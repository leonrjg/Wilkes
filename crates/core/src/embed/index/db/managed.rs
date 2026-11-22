//! Lookups over the managed corpus: documents, renditions, import keys and
//! vectors that can be adopted.

use super::*;

impl SemanticIndex {
    pub fn managed_document_for_path(
        &self,
        path: &Path,
    ) -> anyhow::Result<Option<ManagedDocumentData>> {
        let key = self.path_key_for_known_path(path);
        let rendition: Option<String> = self
            .conn
            .query_row(
                "SELECT rendition_id FROM files
                 WHERE file_path = ?1 AND admission_state = 'ready'",
                params![key.to_string_lossy()],
                |row| row.get(0),
            )
            .optional()?;
        rendition
            .map(|rendition| self.managed_document_by_rendition(&rendition))
            .transpose()
            .map(Option::flatten)
    }
    /// Every retained snapshot this corpus has admitted, with the rendition it
    /// was admitted as, in a stable order.
    ///
    /// The membership authority for a catch-up, deliberately, rather than the
    /// contents of `managed_sources/`: the directory can hold bytes the corpus
    /// never admitted — an interrupted import, a file someone dropped in — and
    /// projecting those would either fail the whole pass or invent membership
    /// the canonical corpus does not have.
    ///
    /// The rendition rides along because a catch-up is a catch-up *to* one:
    /// the caller has to name which rendition it is projecting before it can
    /// ask whether that work is already done, and reading it here costs a
    /// column on a row already being read.
    pub fn managed_admitted_renditions(&self) -> anyhow::Result<Vec<(PathBuf, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT file_path, rendition_id FROM files
              WHERE admission_state = 'ready' AND rendition_id IS NOT NULL
              ORDER BY source_sha256, id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut admitted = Vec::new();
        for row in rows {
            let (path, rendition) = row?;
            admitted.push((PathBuf::from(path), rendition));
        }
        Ok(admitted)
    }
    /// Read and verify the canonical, vector-free structure of one managed
    /// document for projection into another embedding space.
    ///
    /// The returned [`PreparedFile`] deliberately carries empty vectors. Its
    /// chunks and full text come from the admitted canonical rendition, so a
    /// projection can embed those exact passages without reading, extracting,
    /// or chunking the source again. The caller supplies `target_path` because
    /// projection indexes may share the canonical immutable snapshot while
    /// retaining their own file-row identity.
    ///
    /// The recipe id of that rendition comes back with it, and is not asked
    /// for. A caller here is *reading* a rendition, not producing one, so it
    /// has no recipe of its own to assert and nothing to check the answer
    /// against; writing the passages back under this id is what makes a
    /// projection's `chunk_ref`s identical to the canonical's by derivation
    /// rather than by coincidence. It used to take the caller's recipe and
    /// refuse when the two differed, which turned every runtime recipe change
    /// — a recognizer installed, an extractor version bumped — into a
    /// projection that could never be brought level again.
    pub fn managed_file_structure_for_reembedding(
        &self,
        path: &Path,
        target_path: &Path,
    ) -> anyhow::Result<Option<(PreparedFile, String)>> {
        let Some(document) = self.managed_document_for_path(path)? else {
            return Ok(None);
        };
        let extraction_recipe_id = document.extraction_recipe_id.clone();
        let key = self.path_key_for_known_path(path);
        let full_text: Option<String> = self
            .conn
            .query_row(
                "SELECT full_text FROM files
                 WHERE file_path = ?1 AND admission_state = 'ready'",
                params![key.to_string_lossy()],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        let full_text = full_text.ok_or_else(|| {
            consumer_error(
                ConsumerErrorCode::DocumentIndexIncomplete,
                "retained text is absent",
            )
        })?;
        consumer_ensure!(
            sha256_bytes(full_text.as_bytes()) == document.extracted_content_sha256,
            ConsumerErrorCode::DocumentIndexIncomplete,
            "extracted-content hash does not match retained text",
        );
        let chunks = document
            .chunks
            .into_iter()
            .map(|chunk| {
                (
                    Chunk {
                        file_path: target_path.to_path_buf(),
                        text: chunk.text,
                        byte_range: chunk.extraction_byte_range,
                        origin: chunk.origin,
                    },
                    Vec::new(),
                )
            })
            .collect();
        Ok(Some((
            PreparedFile {
                path: target_path.to_path_buf(),
                chunks,
                full_text,
                // Part of the rendition being reused, exactly like the chunks
                // above: re-deriving them would mean extracting the source
                // again, which is what this whole path exists not to do.
                retained: self.retained_extraction_for_key(&key.to_string_lossy())?,
            },
            extraction_recipe_id,
        )))
    }
    pub fn managed_document_for_import_key(
        &self,
        idempotency_key: &str,
        source_sha256: &str,
        extraction_recipe_id: &str,
    ) -> anyhow::Result<Option<ManagedDocumentData>> {
        let binding = self
            .conn
            .query_row(
                "SELECT source_sha256, extraction_recipe_id, rendition_id
                 FROM managed_import_keys WHERE idempotency_key = ?1",
                params![idempotency_key],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((bound_source, bound_recipe, rendition)) = binding else {
            return Ok(None);
        };
        consumer_ensure!(
            bound_source == source_sha256 && bound_recipe == extraction_recipe_id,
            ConsumerErrorCode::IdempotencyKeyConflict,
            "managed import key is already bound to different content",
        );
        self.managed_document_by_rendition(&rendition)
    }
    pub fn bind_managed_import_key(
        &mut self,
        idempotency_key: &str,
        document: &ManagedDocumentData,
    ) -> anyhow::Result<()> {
        consumer_ensure!(
            !idempotency_key.trim().is_empty(),
            ConsumerErrorCode::IdempotencyKeyConflict,
            "idempotency key is empty",
        );
        let existing = self
            .conn
            .query_row(
                "SELECT source_sha256, extraction_recipe_id, rendition_id
                 FROM managed_import_keys WHERE idempotency_key = ?1",
                params![idempotency_key],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        if let Some((source, recipe, rendition)) = existing {
            consumer_ensure!(
                source == document.source_sha256
                    && recipe == document.extraction_recipe_id
                    && rendition == document.rendition_id.as_str(),
                ConsumerErrorCode::IdempotencyKeyConflict,
                "managed import key is already bound to a different rendition",
            );
            return Ok(());
        }
        self.conn.execute(
            "INSERT INTO managed_import_keys
                (idempotency_key, source_sha256, extraction_recipe_id, rendition_id, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                idempotency_key,
                document.source_sha256,
                document.extraction_recipe_id,
                document.rendition_id.as_str(),
                Self::now_ms(),
            ],
        )?;
        Ok(())
    }
    pub fn managed_document_by_rendition(
        &self,
        rendition_id: &str,
    ) -> anyhow::Result<Option<ManagedDocumentData>> {
        let header = self
            .conn
            .query_row(
                "SELECT source_sha256, snapshot_id, extraction_recipe_id, rendition_id,
                        full_text, extracted_content_sha256
                 FROM files WHERE rendition_id = ?1",
                params![rendition_id],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            source_sha256,
            snapshot,
            extraction_recipe_id,
            rendition,
            full_text,
            extracted_content_sha256,
        )) = header
        else {
            return Ok(None);
        };
        let source_sha256 = source_sha256.ok_or_else(|| {
            consumer_error(
                ConsumerErrorCode::DocumentIndexIncomplete,
                "source hash is absent",
            )
        })?;
        let snapshot = snapshot.ok_or_else(|| {
            consumer_error(
                ConsumerErrorCode::DocumentIndexIncomplete,
                "snapshot id is absent",
            )
        })?;
        let extraction_recipe_id = extraction_recipe_id.ok_or_else(|| {
            consumer_error(
                ConsumerErrorCode::DocumentIndexIncomplete,
                "extraction recipe id is absent",
            )
        })?;
        let rendition = rendition.ok_or_else(|| {
            consumer_error(
                ConsumerErrorCode::DocumentIndexIncomplete,
                "rendition id is absent",
            )
        })?;
        let full_text = full_text.ok_or_else(|| {
            consumer_error(
                ConsumerErrorCode::DocumentIndexIncomplete,
                "retained text is absent",
            )
        })?;
        let extracted_content_sha256 = extracted_content_sha256.ok_or_else(|| {
            consumer_error(
                ConsumerErrorCode::DocumentIndexIncomplete,
                "extracted-content hash is absent",
            )
        })?;
        consumer_ensure!(
            sha256_bytes(full_text.as_bytes()) == extracted_content_sha256,
            ConsumerErrorCode::DocumentIndexIncomplete,
            "extracted-content hash does not match retained text",
        );
        consumer_ensure!(
            snapshot_id(&source_sha256).as_str() == snapshot,
            ConsumerErrorCode::DocumentIndexIncomplete,
            "snapshot identity does not match source hash",
        );

        let mut stmt = self.conn.prepare(
            // No vectors. `ManagedDocumentData` carries chunk refs, text and
            // geometry — a consumer reads passages here and embeds its own
            // text — so the coordinates this used to read were decoded,
            // validated and dropped on the floor. Reading them also inner-
            // joined `vec_chunks`, which meant a corpus that owned passages
            // and no vectors could not export a single document.
            "SELECT c.chunk_ref, c.chunk_idx, c.chunk_text, c.text_sha256,
                    c.byte_start, c.byte_end, c.origin_type, c.page, c.line, c.col,
                    c.bbox_x, c.bbox_y, c.bbox_w, c.bbox_h
             FROM chunks c JOIN files f ON f.id = c.file_id
             WHERE f.rendition_id = ?1
             ORDER BY c.chunk_idx",
        )?;
        let rows = stmt
            .query_map(params![rendition_id], |row| {
                let origin_type: String = row.get(6)?;
                let origin = source_origin_from_parts(
                    &origin_type,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                    row.get(10)?,
                    row.get(11)?,
                    row.get(12)?,
                    row.get(13)?,
                )
                .ok_or_else(|| {
                    rusqlite::Error::InvalidColumnType(
                        6,
                        "origin_type".to_string(),
                        rusqlite::types::Type::Text,
                    )
                })?;
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    origin,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        consumer_ensure!(
            !rows.is_empty(),
            ConsumerErrorCode::DocumentIndexIncomplete,
            "rendition has no complete chunk/vector mapping",
        );
        let mut chunks = Vec::with_capacity(rows.len());
        let mut descriptors = Vec::with_capacity(rows.len());
        for (expected_ordinal, row) in rows.into_iter().enumerate() {
            let (stored_ref, ordinal, text, stored_text_sha256, byte_start, byte_end, origin) = row;
            consumer_ensure!(
                ordinal >= 0 && ordinal as usize == expected_ordinal,
                ConsumerErrorCode::DocumentIndexIncomplete,
                "chunk ordinals are not contiguous",
            );
            consumer_ensure!(
                byte_start >= 0 && byte_end >= byte_start,
                ConsumerErrorCode::DocumentIndexIncomplete,
                "invalid chunk byte range",
            );
            let text_sha256 = sha256_bytes(text.as_bytes());
            consumer_ensure!(
                text_sha256 == stored_text_sha256,
                ConsumerErrorCode::DocumentIndexIncomplete,
                "chunk text hash mismatch",
            );
            let byte_range = ByteRange {
                start: byte_start as usize,
                end: byte_end as usize,
            };
            descriptors.push(ChunkDescriptor {
                ordinal: expected_ordinal,
                text_sha256: text_sha256.clone(),
                byte_range: byte_range.clone(),
                origin: origin.clone(),
            });
            chunks.push(ManagedChunkData {
                chunk_ref: ChunkRef(stored_ref),
                ordinal: expected_ordinal,
                text,
                text_sha256,
                extraction_byte_range: byte_range,
                origin,
            });
        }
        let recomputed_rendition = crate::embed::identity::rendition_id(
            &DocumentSnapshotId(snapshot.clone()),
            &extraction_recipe_id,
            &descriptors,
        );
        consumer_ensure!(
            recomputed_rendition.as_str() == rendition,
            ConsumerErrorCode::DocumentIndexIncomplete,
            "rendition identity mismatch",
        );
        for chunk in &chunks {
            consumer_ensure!(
                chunk.chunk_ref == chunk_ref(&recomputed_rendition, chunk.ordinal),
                ConsumerErrorCode::DocumentIndexIncomplete,
                "stable chunk reference mismatch",
            );
        }
        Ok(Some(ManagedDocumentData {
            source_sha256,
            snapshot_id: DocumentSnapshotId(snapshot),
            extraction_recipe_id,
            rendition_id: RenditionId(rendition),
            extracted_content_sha256,
            chunks,
        }))
    }
    /// Materialize an exactly verified whole rendition for copying into a
    /// different index. The returned vectors are owned values; the target will
    /// allocate fresh rowids and never depend on this index's lifecycle.
    /// What this index holds for one source document, per rendition.
    ///
    /// A diagnostic, and it exists because "0 of 605 chunks adopted" is a
    /// number with several possible causes and no way to tell them apart. Two
    /// indexes in the same space adopt nothing from each other when they read
    /// the document differently: the text moves, so the hashes differ — and
    /// the vectors genuinely are for other spans, which is not a refusal to be
    /// relaxed but a fact to be reported.
    ///
    /// The recipe is the whole reading, not just the chunking: extractor
    /// version, chunk size and overlap, and the image analyzer each sit in it,
    /// and any one of them differing is enough. A workspace indexed by an
    /// older runtime holds a different reading of the same file at the same
    /// chunk geometry.
    ///
    /// Returns the recipe each rendition was produced under and how many
    /// chunks it holds, so a caller that found nothing can say whether this
    /// index had never seen the document or had read it another way.
    pub fn renditions_for_source(&self, source_sha256: &str) -> anyhow::Result<Vec<(String, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT f.extraction_recipe_id, COUNT(c.id)
               FROM files f LEFT JOIN chunks c ON c.file_id = f.id
              WHERE f.source_sha256 = ?1
              GROUP BY f.id, f.extraction_recipe_id",
        )?;
        let rows = stmt
            .query_map(params![source_sha256], |row| {
                Ok((row.get::<_, Option<String>>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows
            .into_iter()
            .map(|(recipe, chunks)| (recipe.unwrap_or_else(|| "unrecorded".into()), chunks))
            .collect())
    }
    /// Vectors this index already holds for the given chunk texts, by hash.
    ///
    /// An embedding is a pure function of its text and its model, so a chunk
    /// carrying the same text in the same space carries the same vector
    /// wherever it was computed. That makes the text hash *direct* evidence
    /// for reuse, where the extraction recipe was only ever a proxy for it —
    /// and a poor one in both directions. Two recipes that agree do not
    /// guarantee identical text: an extractor whose behaviour changes without
    /// a version bump slips straight through. Two that differ may have
    /// produced identical text anyway, and today that costs a full re-embed.
    ///
    /// Deliberately says nothing about chunk structure. The caller keeps its
    /// own chunks — their ordinals, their byte ranges, their `chunk_ref`s —
    /// and takes only the coordinates. That is what makes this safe to read
    /// across workspaces: nothing a citation points at comes from here.
    ///
    /// Misses are simply absent from the map. A caller embeds what it did not
    /// find, so a partial hit is a partial saving rather than a failure.
    pub fn vectors_for_text_hashes(
        &self,
        wanted: &[String],
    ) -> anyhow::Result<std::collections::HashMap<String, Vec<f32>>> {
        let mut found = std::collections::HashMap::new();
        if wanted.is_empty() {
            return Ok(found);
        }
        // Chunked to stay under SQLite's bound-variable limit on every build.
        for batch in wanted.chunks(400) {
            let placeholders = std::iter::repeat_n("?", batch.len())
                .collect::<Vec<_>>()
                .join(",");
            let mut stmt = self.conn.prepare(&format!(
                "SELECT c.text_sha256, v.embedding
                   FROM chunks c JOIN vec_chunks v ON v.rowid = c.id
                  WHERE c.text_sha256 IN ({placeholders})"
            ))?;
            let rows = stmt
                .query_map(rusqlite::params_from_iter(batch.iter()), |row| {
                    Ok((row.get::<_, Option<String>>(0)?, row.get::<_, Vec<u8>>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            for (hash, blob) in rows {
                let Some(hash) = hash else { continue };
                if found.contains_key(&hash) {
                    continue;
                }
                let embedding = f32_slice_from_bytes(&blob)?;
                // A stored vector of the wrong width or with a non-finite
                // component is not adoptable. Logged rather than dropped in
                // silence: it means this index disagrees with its own
                // recorded dimension, which nothing else would report.
                if embedding.len() != self.dimension {
                    tracing::warn!(
                        "chunk {hash} holds a {}-wide vector in a {}-wide index; not adopting it",
                        embedding.len(),
                        self.dimension
                    );
                    continue;
                }
                if embedding.iter().any(|value| !value.is_finite()) {
                    tracing::warn!("chunk {hash} holds a non-finite vector; not adopting it");
                    continue;
                }
                found.insert(hash, embedding);
            }
        }
        Ok(found)
    }
    pub fn verified_file_for_adoption(
        &self,
        source_sha256: &str,
        recipe: &ExtractionRecipe,
        expected_space_id: &EmbeddingSpaceId,
        target_path: &Path,
    ) -> anyhow::Result<Option<PreparedFile>> {
        let Some(exact_identity) = self.embedding_metadata()?.exact_identity else {
            return Ok(None);
        };
        if &exact_identity.id() != expected_space_id {
            return Ok(None);
        }
        let recipe_id = recipe.id();
        let file = self
            .conn
            .query_row(
                "SELECT id, full_text, rendition_id, extracted_content_sha256 FROM files
                 WHERE source_sha256 = ?1 AND extraction_recipe_id = ?2
                 LIMIT 1",
                params![source_sha256, recipe_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((file_id, full_text, stored_rendition, stored_extracted_content_sha256)) = file
        else {
            return Ok(None);
        };
        let (Some(full_text), Some(stored_rendition), Some(stored_extracted_content_sha256)) =
            (full_text, stored_rendition, stored_extracted_content_sha256)
        else {
            return Ok(None);
        };
        if sha256_bytes(full_text.as_bytes()) != stored_extracted_content_sha256 {
            return Ok(None);
        }
        let Some(chunks) = self.verified_rendition_chunks(
            file_id,
            source_sha256,
            &recipe_id,
            &stored_rendition,
            target_path,
        )?
        else {
            return Ok(None);
        };
        Ok(Some(PreparedFile {
            path: target_path.to_path_buf(),
            chunks,
            full_text,
            retained: self.retained_extraction_for_file(file_id)?,
        }))
    }
}
