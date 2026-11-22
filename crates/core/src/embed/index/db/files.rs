//! Per-file records: path keys, rendition stamps, retained extractions, and
//! writing, renaming and removing indexed files.

use super::*;

impl SemanticIndex {
    /// When each file's rendition with servable text was written, by stored
    /// key. A rendition is servable when it holds `full_text` — empty text
    /// counts only for a document that produced no passages, the same rule
    /// [`Self::indexed_document_for_path`] applies. On-disk identity is
    /// returned, not checked: the caller compares it with the file it has.
    ///
    /// The document text index reads this to find renditions worth copying
    /// without opening any of their text.
    pub fn rendition_stamps(&self) -> anyhow::Result<HashMap<PathBuf, RenditionStamp>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT f.file_path, f.size_bytes, f.modified_at_ms, f.indexed_at_ms
               FROM files f WHERE {SERVABLE_RENDITION}"
        ))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    PathBuf::from(row.get::<_, String>(0)?),
                    RenditionStamp {
                        size_bytes: row.get(1)?,
                        modified_at_ms: row.get(2)?,
                        indexed_at_ms: row.get(3)?,
                    },
                ))
            })?
            .collect::<Result<HashMap<_, _>, _>>()?;
        Ok(rows)
    }
    /// [`Self::rendition_stamps`] for one path.
    pub fn rendition_stamp(&self, path: &Path) -> anyhow::Result<Option<RenditionStamp>> {
        let key = Self::canonical_path(path);
        Ok(self
            .conn
            .query_row(
                &format!(
                    "SELECT f.size_bytes, f.modified_at_ms, f.indexed_at_ms
                       FROM files f WHERE f.file_path = ?1 AND {SERVABLE_RENDITION}"
                ),
                params![key.to_string_lossy()],
                |row| {
                    Ok(RenditionStamp {
                        size_bytes: row.get(0)?,
                        modified_at_ms: row.get(1)?,
                        indexed_at_ms: row.get(2)?,
                    })
                },
            )
            .optional()?)
    }
    /// Return a file's stored extracted text plus a chunk-granular `SourceMap`.
    /// Returns `None` when the file is absent, has no stored `full_text`
    /// (indexed before schema v4), or has changed on disk since it was indexed
    /// (stale text must never be served).
    pub fn indexed_document_for_path(
        &self,
        path: &Path,
    ) -> anyhow::Result<Option<(String, SourceMap)>> {
        let key = Self::canonical_path(path);
        let key_str = key.to_string_lossy().into_owned();

        let row = self
            .conn
            .query_row(
                "SELECT id, size_bytes, modified_at_ms, full_text
                 FROM files WHERE file_path = ?1",
                params![key_str],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((file_id, size_bytes, modified_at_ms, full_text)) = row else {
            return Ok(None);
        };
        let Some(full_text) = full_text else {
            return Ok(None);
        };

        // Extracted empty documents have no chunks. Empty text alongside chunks
        // is a legacy row produced by the old NULL-to-empty reuse bug, so do not
        // suppress the caller's live-extraction fallback.
        if full_text.is_empty() {
            let has_chunks: bool = self.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM chunks WHERE file_id = ?1)",
                params![file_id],
                |row| row.get(0),
            )?;
            if has_chunks {
                return Ok(None);
            }
        }

        // Reject stale text: a file edited after indexing would otherwise yield
        // matches against content that no longer exists on disk.
        let identity = Self::identity_for_path(path)?;
        if size_bytes != identity.size_bytes || modified_at_ms != identity.modified_at_ms {
            return Ok(None);
        }

        let mut stmt = self.conn.prepare(
            "SELECT byte_start, byte_end, origin_type, page, line, col,
                    bbox_x, bbox_y, bbox_w, bbox_h
             FROM chunks WHERE file_id = ?1 ORDER BY chunk_idx",
        )?;
        let segments = stmt
            .query_map(params![file_id], |row| {
                let byte_start = row.get::<_, i64>(0)? as usize;
                let byte_end = row.get::<_, i64>(1)? as usize;
                let origin = source_origin_from_parts(
                    &row.get::<_, String>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                    row.get::<_, Option<f64>>(6)?,
                    row.get::<_, Option<f64>>(7)?,
                    row.get::<_, Option<f64>>(8)?,
                    row.get::<_, Option<f64>>(9)?,
                );
                Ok((byte_start, byte_end, origin))
            })?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter_map(|(start, end, origin)| {
                origin.map(|origin| SourceSegment {
                    text_range: ByteRange { start, end },
                    origin,
                    // One segment per chunk, and a chunk can hold native text
                    // and image enrichment together, so no single answer here
                    // would be true. Consumers needing provenance re-extract.
                    provenance: crate::types::TextProvenance::Unrecorded,
                })
            })
            .collect();

        Ok(Some((full_text, SourceMap { segments })))
    }
    /// The page areas one indexed document's reading owns, resolved against
    /// the text it was cut from.
    ///
    /// Empty is the ordinary answer, and deliberately not distinguished from
    /// "not indexed": a reading surface asks this so it knows what *not* to
    /// offer from the page, and the honest reply when nothing is known is that
    /// the page speaks for itself. Guarded on the same on-disk identity as
    /// [`Self::indexed_document_for_path`] — rectangles from a rendition of a
    /// file that has since changed would land on whatever is drawn there now.
    pub fn superseded_areas_for_path(&self, path: &Path) -> anyhow::Result<Vec<SupersededArea>> {
        let key = Self::canonical_path(path);
        let key_str = key.to_string_lossy().into_owned();

        let row = self
            .conn
            .query_row(
                "SELECT id, size_bytes, modified_at_ms, full_text
                 FROM files WHERE file_path = ?1",
                params![key_str],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((file_id, size_bytes, modified_at_ms, Some(full_text))) = row else {
            return Ok(Vec::new());
        };

        let identity = Self::identity_for_path(path)?;
        if size_bytes != identity.size_bytes || modified_at_ms != identity.modified_at_ms {
            return Ok(Vec::new());
        }

        let regions = self.reading_regions_for_file(file_id)?;
        Ok(crate::extract::image::serialize::superseded_areas(
            &full_text, &regions,
        ))
    }
    pub(super) fn canonical_path(path: &Path) -> PathBuf {
        std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
    }
    pub(super) fn path_key_for_existing_path(&self, path: &Path) -> PathBuf {
        Self::canonical_path(path)
    }
    pub(super) fn path_key_for_known_path(&self, path: &Path) -> PathBuf {
        if path.exists() {
            return self.path_key_for_existing_path(path);
        }
        if let Some(file_name) = path.file_name() {
            if let Some(parent) = path.parent() {
                let parent = std::fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf());
                return parent.join(file_name);
            }
        }
        path.to_path_buf()
    }
    pub(super) fn key_to_display_path(&self, stored: &str) -> PathBuf {
        PathBuf::from(stored)
    }
    pub(super) fn identity_for_path(path: &Path) -> anyhow::Result<FileIdentity> {
        FileIdentity::for_path(path)
            .ok_or_else(|| anyhow::anyhow!("Could not derive file identity for {}", path.display()))
    }
    pub(super) fn now_ms() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|d| i64::try_from(d.as_millis()).ok())
            .unwrap_or(0)
    }
    pub(super) fn delete_file_by_key_tx(
        tx: &rusqlite::Transaction<'_>,
        key: &str,
    ) -> anyhow::Result<()> {
        tx.execute(
            "DELETE FROM vec_chunks WHERE rowid IN (
                SELECT c.id FROM chunks c
                JOIN files f ON f.id = c.file_id
                WHERE f.file_path = ?1
            )",
            params![key],
        )?;
        tx.execute(
            "DELETE FROM chunks WHERE file_id IN (SELECT id FROM files WHERE file_path = ?1)",
            params![key],
        )?;
        tx.execute("DELETE FROM files WHERE file_path = ?1", params![key])?;
        Ok(())
    }
    pub(super) fn delete_chunks_for_file_tx(
        tx: &rusqlite::Transaction<'_>,
        file_id: i64,
    ) -> anyhow::Result<()> {
        tx.execute(
            "DELETE FROM vec_chunks WHERE rowid IN (
                SELECT id FROM chunks WHERE file_id = ?1
            )",
            params![file_id],
        )?;
        tx.execute("DELETE FROM chunks WHERE file_id = ?1", params![file_id])?;
        // The retained extraction is ranges into the text written in the same
        // breath as these chunks. Leaving it behind would point the next
        // rendition's rectangles and anchors at somebody else's bytes.
        tx.execute(
            "UPDATE files SET retained_extraction_json = NULL WHERE id = ?1",
            params![file_id],
        )?;
        Ok(())
    }
    /// Store one file's retained extraction.
    ///
    /// Written as its own statement rather than as columns on the file insert
    /// above, because it belongs to the rendition rather than to the file
    /// identity, and because the two callers that reuse a rendition hand over
    /// a stored string they never deserialize.
    pub(super) fn write_retained_extraction_tx(
        tx: &rusqlite::Transaction<'_>,
        file_id: i64,
        retained: &RetainedExtraction,
    ) -> anyhow::Result<()> {
        tx.execute(
            "UPDATE files SET retained_extraction_json = ?2 WHERE id = ?1",
            params![file_id, serde_json::to_string(retained)?],
        )?;
        Ok(())
    }
    /// One file's retained extraction, by path key.
    pub(super) fn retained_extraction_for_key(
        &self,
        key: &str,
    ) -> anyhow::Result<RetainedExtraction> {
        let file_id: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM files WHERE file_path = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()?;
        match file_id {
            Some(file_id) => self.retained_extraction_for_file(file_id),
            None => Ok(RetainedExtraction::default()),
        }
    }
    /// One file's retained extraction, by row id.
    ///
    /// A row with nothing stored, and a row whose stored shape is of another
    /// version, both come back as the default: no regions, and an image
    /// inventory of `None` — which reads as *unknown*, never as "this document
    /// draws nothing".
    pub(super) fn retained_extraction_for_file(
        &self,
        file_id: i64,
    ) -> anyhow::Result<RetainedExtraction> {
        let json: Option<String> = self
            .conn
            .query_row(
                "SELECT retained_extraction_json FROM files WHERE id = ?1",
                params![file_id],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        Ok(json
            .as_deref()
            .and_then(RetainedExtraction::parse)
            .unwrap_or_default())
    }
    /// One file's reading regions, in reading order.
    pub(super) fn reading_regions_for_file(
        &self,
        file_id: i64,
    ) -> anyhow::Result<Vec<ReadingRegion>> {
        Ok(self.retained_extraction_for_file(file_id)?.reading_regions)
    }
    /// Write a generic workspace file. Stable managed identities are omitted
    /// unless the caller supplies the extraction recipe through
    /// [`Self::write_file_with_recipe`]; legacy callers therefore cannot
    /// accidentally authorize managed reuse with guessed metadata.
    pub fn write_file(&mut self, prepared: PreparedFile) -> anyhow::Result<()> {
        self.write_file_internal(prepared, None).map(|_| ())
    }
    /// Write a file with the complete source/rendition/chunk identity used by
    /// exact whole-document adoption and managed APIs.
    #[allow(clippy::too_many_arguments)]
    pub fn write_file_with_recipe(
        &mut self,
        prepared: PreparedFile,
        recipe: &ExtractionRecipe,
        managed_snapshot_relative_path: Option<&Path>,
        original_source_provenance: Option<&serde_json::Value>,
        admitted: bool,
        reused: bool,
        idempotency_key: Option<&str>,
    ) -> anyhow::Result<ManagedDocumentData> {
        self.write_file_with_recipe_id(
            prepared,
            &recipe.id(),
            managed_snapshot_relative_path,
            original_source_provenance,
            admitted,
            reused,
            idempotency_key,
        )
    }
    /// The same write, under a recipe id the caller already holds.
    ///
    /// A recipe reaches this function only to be hashed, and a projection has
    /// no recipe to hash: what it holds is the id of the rendition it is
    /// reproducing, read off that rendition. Recomputing one from its own
    /// runtime would derive a different `rendition_id` for the same passages,
    /// which is the one thing a projection may not do — every space's
    /// `chunk_ref`s are the canonical's.
    #[allow(clippy::too_many_arguments)]
    pub fn write_file_with_recipe_id(
        &mut self,
        prepared: PreparedFile,
        extraction_recipe_id: &str,
        managed_snapshot_relative_path: Option<&Path>,
        original_source_provenance: Option<&serde_json::Value>,
        admitted: bool,
        reused: bool,
        idempotency_key: Option<&str>,
    ) -> anyhow::Result<ManagedDocumentData> {
        let source_sha256 = sha256_file(&prepared.path)?;
        let snapshot = snapshot_id(&source_sha256);
        let extraction_recipe_id = extraction_recipe_id.to_string();
        let descriptors: Vec<ChunkDescriptor> = prepared
            .chunks
            .iter()
            .enumerate()
            .map(|(ordinal, (chunk, _))| ChunkDescriptor {
                ordinal,
                text_sha256: sha256_bytes(chunk.text.as_bytes()),
                byte_range: chunk.byte_range.clone(),
                origin: chunk.origin.clone(),
            })
            .collect();
        let rendition = rendition_id(&snapshot, &extraction_recipe_id, &descriptors);
        let identity = FileSemanticIdentity {
            source_sha256,
            snapshot_id: snapshot,
            extraction_recipe_id,
            rendition_id: rendition,
            extracted_content_sha256: sha256_bytes(prepared.full_text.as_bytes()),
            embedding_reused_chunks: admitted.then_some(if reused {
                prepared.chunks.len()
            } else {
                0
            }),
            embedding_computed_chunks: admitted.then_some(if reused {
                0
            } else {
                prepared.chunks.len()
            }),
            idempotency_key: idempotency_key.map(str::to_string),
            chunk_descriptors: descriptors,
            managed_snapshot_relative_path: managed_snapshot_relative_path
                .map(|path| path.to_string_lossy().into_owned()),
            original_source_provenance_json: original_source_provenance
                .map(serde_json::to_string)
                .transpose()?,
            admission_state: admitted.then(|| "ready".to_string()),
        };
        self.write_file_internal(prepared, Some(identity))?
            .ok_or_else(|| {
                consumer_error(
                    ConsumerErrorCode::DocumentIndexIncomplete,
                    "identity was not published",
                )
            })
    }
    pub(super) fn write_file_internal(
        &mut self,
        prepared: PreparedFile,
        semantic_identity: Option<FileSemanticIdentity>,
    ) -> anyhow::Result<Option<ManagedDocumentData>> {
        let abs_path_str = prepared.path.to_string_lossy().into_owned();
        let key = self.path_key_for_existing_path(&prepared.path);
        let key_str = key.to_string_lossy().into_owned();
        let identity = Self::identity_for_path(&prepared.path)?;

        // Validate dimensions before starting transaction. An empty vector is
        // not a wrong-width one: it says this chunk carries no coordinates,
        // which is how the canonical managed corpus writes every chunk it
        // owns. A width is only checked where there is one.
        for (_, embedding) in &prepared.chunks {
            anyhow::ensure!(
                embedding.is_empty() || embedding.len() == self.dimension,
                "Dimension mismatch: expected {}, received {} for path {}",
                self.dimension,
                embedding.len(),
                abs_path_str
            );
            consumer_ensure!(
                embedding.iter().all(|value| value.is_finite()),
                ConsumerErrorCode::DocumentIndexIncomplete,
                "non-finite embedding for path {}",
                abs_path_str,
            );
        }

        let tx = self.conn.transaction()?;

        let now = Self::now_ms();
        let file_id: Option<i64> = tx
            .query_row(
                "SELECT id FROM files WHERE file_path = ?1",
                params![key_str],
                |row| row.get(0),
            )
            .optional()?;
        let file_id = if let Some(file_id) = file_id {
            Self::delete_chunks_for_file_tx(&tx, file_id)?;
            tx.execute(
                "UPDATE files
                 SET size_bytes = ?2, modified_at_ms = ?3, indexed_at_ms = ?4, full_text = ?5,
                     source_sha256 = ?6, snapshot_id = ?7, extraction_recipe_id = ?8,
                     rendition_id = ?9, extracted_content_sha256 = ?10,
                     embedding_reused_chunks = ?11, embedding_computed_chunks = ?12,
                     managed_snapshot_relative_path = ?13,
                     original_source_provenance_json = ?14, admission_state = ?15
                 WHERE id = ?1",
                params![
                    file_id,
                    identity.size_bytes,
                    identity.modified_at_ms,
                    now,
                    prepared.full_text,
                    semantic_identity
                        .as_ref()
                        .map(|value| value.source_sha256.as_str()),
                    semantic_identity
                        .as_ref()
                        .map(|value| value.snapshot_id.as_str()),
                    semantic_identity
                        .as_ref()
                        .map(|value| value.extraction_recipe_id.as_str()),
                    semantic_identity
                        .as_ref()
                        .map(|value| value.rendition_id.as_str()),
                    semantic_identity
                        .as_ref()
                        .map(|value| value.extracted_content_sha256.as_str()),
                    semantic_identity
                        .as_ref()
                        .and_then(|value| value.embedding_reused_chunks),
                    semantic_identity
                        .as_ref()
                        .and_then(|value| value.embedding_computed_chunks),
                    semantic_identity
                        .as_ref()
                        .and_then(|value| value.managed_snapshot_relative_path.as_deref()),
                    semantic_identity
                        .as_ref()
                        .and_then(|value| value.original_source_provenance_json.as_deref()),
                    semantic_identity
                        .as_ref()
                        .and_then(|value| value.admission_state.as_deref()),
                ],
            )?;
            file_id
        } else {
            tx.execute(
                "INSERT INTO files (file_path, size_bytes, modified_at_ms, indexed_at_ms, full_text,
                                    source_sha256, snapshot_id, extraction_recipe_id, rendition_id,
                                    extracted_content_sha256, embedding_reused_chunks,
                                    embedding_computed_chunks, managed_snapshot_relative_path,
                                    original_source_provenance_json, admission_state)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                params![
                    key_str,
                    identity.size_bytes,
                    identity.modified_at_ms,
                    now,
                    prepared.full_text,
                    semantic_identity.as_ref().map(|value| value.source_sha256.as_str()),
                    semantic_identity.as_ref().map(|value| value.snapshot_id.as_str()),
                    semantic_identity.as_ref().map(|value| value.extraction_recipe_id.as_str()),
                    semantic_identity.as_ref().map(|value| value.rendition_id.as_str()),
                    semantic_identity.as_ref().map(|value| value.extracted_content_sha256.as_str()),
                    semantic_identity.as_ref().and_then(|value| value.embedding_reused_chunks),
                    semantic_identity.as_ref().and_then(|value| value.embedding_computed_chunks),
                    semantic_identity.as_ref().and_then(|value| value.managed_snapshot_relative_path.as_deref()),
                    semantic_identity.as_ref().and_then(|value| value.original_source_provenance_json.as_deref()),
                    semantic_identity.as_ref().and_then(|value| value.admission_state.as_deref()),
                ],
            )?;
            tx.last_insert_rowid()
        };
        if let Some(root_id) = self.active_root_id {
            tx.execute(
                "INSERT OR REPLACE INTO root_files (root_id, file_id, seen_at_ms)
                 VALUES (?1, ?2, ?3)",
                params![root_id, file_id, now],
            )?;
        }
        for (i, (chunk, embedding)) in prepared.chunks.into_iter().enumerate() {
            let (origin_type, page, line, col, bbox_x, bbox_y, bbox_w, bbox_h) = match &chunk.origin
            {
                SourceOrigin::TextFile { line, col } => (
                    "text_file",
                    None::<i64>,
                    Some(*line as i64),
                    Some(*col as i64),
                    None::<f64>,
                    None,
                    None,
                    None,
                ),
                SourceOrigin::PdfPage { page, bbox } => {
                    let (bx, by, bw, bh) = bbox
                        .as_ref()
                        .map(|b| {
                            (
                                Some(b.x as f64),
                                Some(b.y as f64),
                                Some(b.width as f64),
                                Some(b.height as f64),
                            )
                        })
                        .unwrap_or((None, None, None, None));
                    ("pdf_page", Some(*page as i64), None, None, bx, by, bw, bh)
                }
            };
            let descriptor = semantic_identity
                .as_ref()
                .and_then(|value| value.chunk_descriptors.get(i));
            let stable_ref = semantic_identity
                .as_ref()
                .map(|value| chunk_ref(&value.rendition_id, i));
            tx.execute(
                "INSERT INTO chunks (file_id, chunk_idx, byte_start, byte_end,
                                     origin_type, page, line, col,
                                     bbox_x, bbox_y, bbox_w, bbox_h, chunk_text,
                                     chunk_ref, text_sha256)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                params![
                    file_id,
                    i as i64,
                    chunk.byte_range.start as i64,
                    chunk.byte_range.end as i64,
                    origin_type,
                    page,
                    line,
                    col,
                    bbox_x,
                    bbox_y,
                    bbox_w,
                    bbox_h,
                    chunk.text,
                    stable_ref.as_ref().map(ChunkRef::as_str),
                    descriptor.map(|value| value.text_sha256.as_str()),
                ],
            )?;
            let chunk_id = tx.last_insert_rowid();
            // No vector, no row. The canonical managed corpus writes its
            // chunks this way: it owns passages and their identity, and no
            // coordinate system, so there is nothing to store here and a
            // zero-length blob would not fit a `float[dimension]` column
            // anyway. A projection supplies the coordinates for these very
            // chunks, matched back by `chunk_ref`.
            if embedding.is_empty() {
                continue;
            }
            let blob = f32_slice_to_bytes(&embedding);
            tx.execute(
                "INSERT INTO vec_chunks(rowid, embedding) VALUES (?1, ?2)",
                params![chunk_id, blob],
            )?;
        }
        // In the same transaction as the text it indexes into: a rendition
        // whose retained extraction was written separately could be read back
        // with one half of it stale.
        Self::write_retained_extraction_tx(&tx, file_id, &prepared.retained)?;
        if let Some(identity) = semantic_identity.as_ref() {
            if let Some(idempotency_key) = identity.idempotency_key.as_deref() {
                let existing = tx
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
                        source == identity.source_sha256
                            && recipe == identity.extraction_recipe_id
                            && rendition == identity.rendition_id.as_str(),
                        ConsumerErrorCode::IdempotencyKeyConflict,
                        "managed import key is already bound to a different rendition",
                    );
                } else {
                    tx.execute(
                        "INSERT INTO managed_import_keys
                            (idempotency_key, source_sha256, extraction_recipe_id, rendition_id, created_at_ms)
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![
                            idempotency_key,
                            identity.source_sha256,
                            identity.extraction_recipe_id,
                            identity.rendition_id.as_str(),
                            Self::now_ms(),
                        ],
                    )?;
                }
            }
        }
        tx.commit()?;
        semantic_identity
            .map(|identity| {
                self.managed_document_by_rendition(identity.rendition_id.as_str())?
                    .ok_or_else(|| {
                        consumer_error(
                            ConsumerErrorCode::DocumentIndexIncomplete,
                            "published rendition cannot be read",
                        )
                    })
            })
            .transpose()
    }
    /// Convenience: `prepare_file` then `write_file`.
    pub fn index_file(
        &mut self,
        path: &Path,
        extractors: &ExtractorRegistry,
        embedder: &dyn Embedder,
        chunk_size: usize,
        chunk_overlap: usize,
    ) -> anyhow::Result<()> {
        self.validate_embedding_space(&embedder.embedding_space_identity())?;
        let prepared = Self::prepare_file(path, extractors, embedder, chunk_size, chunk_overlap)?;
        self.write_file_with_recipe(
            prepared,
            &ExtractionRecipe::for_path(path, extractors, chunk_size, chunk_overlap),
            None,
            None,
            false,
            false,
            None,
        )?;
        Ok(())
    }
    /// Remove all chunks for the given path.
    pub fn remove_file(&mut self, path: &Path) -> anyhow::Result<()> {
        let key = self.path_key_for_known_path(path);
        let key_str = key.to_string_lossy().into_owned();
        let tx = self.conn.transaction()?;
        if let Some(root_id) = self.active_root_id {
            tx.execute(
                "DELETE FROM root_files
                 WHERE root_id = ?1
                   AND file_id IN (SELECT id FROM files WHERE file_path = ?2)",
                params![root_id, key_str],
            )?;
            tx.execute(
                "DELETE FROM files
                 WHERE file_path = ?1
                   AND id NOT IN (SELECT DISTINCT file_id FROM root_files)",
                params![key_str],
            )?;
        } else {
            Self::delete_file_by_key_tx(&tx, &key_str)?;
        }
        tx.commit()?;
        Ok(())
    }
    /// Re-key all chunks for a renamed file from `old` to `new` without
    /// re-extracting or re-embedding. A rename preserves file content, so the
    /// embeddings (`vec_chunks`, keyed by chunk rowid) stay valid untouched;
    /// only the `file_path` column changes. Any chunks already stored under
    /// `new` are removed first so the destination path is not duplicated.
    pub fn rename_file(&mut self, old: &Path, new: &Path) -> anyhow::Result<()> {
        let old_rel = self
            .path_key_for_known_path(old)
            .to_string_lossy()
            .into_owned();
        let new_rel = self
            .path_key_for_existing_path(new)
            .to_string_lossy()
            .into_owned();
        if old_rel == new_rel {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        // A file rename: the renamed path itself is an indexed file.
        Self::delete_file_by_key_tx(&tx, &new_rel)?;
        tx.execute(
            "UPDATE files SET file_path = ?1 WHERE file_path = ?2",
            params![new_rel, old_rel],
        )?;
        // A directory rename: every indexed file beneath it keeps its embeddings
        // but must move to the new path prefix.
        Self::rekey_descendant_files_tx(&tx, &old_rel, &new_rel)?;
        tx.commit()?;
        Ok(())
    }
    /// Rewrites the keys of indexed files living under a renamed directory. Keys
    /// are absolute paths, so a directory rename shifts every descendant's
    /// prefix. Embeddings are preserved — only `file_path` changes. Rows already
    /// occupying a destination key are dropped first to honour the
    /// `UNIQUE(file_path)` constraint.
    pub(super) fn rekey_descendant_files_tx(
        tx: &rusqlite::Transaction<'_>,
        old_key: &str,
        new_key: &str,
    ) -> anyhow::Result<()> {
        for separator in ['/', '\\'] {
            let old_prefix = format!("{old_key}{separator}");
            let new_prefix = format!("{new_key}{separator}");
            let old_len = old_prefix.chars().count() as i64;
            let new_len = new_prefix.chars().count() as i64;
            tx.execute(
                "DELETE FROM vec_chunks WHERE rowid IN (
                    SELECT c.id FROM chunks c JOIN files f ON f.id = c.file_id
                    WHERE substr(f.file_path, 1, ?1) = ?2
                )",
                params![new_len, new_prefix],
            )?;
            tx.execute(
                "DELETE FROM chunks WHERE file_id IN (
                    SELECT id FROM files WHERE substr(file_path, 1, ?1) = ?2
                )",
                params![new_len, new_prefix],
            )?;
            tx.execute(
                "DELETE FROM files WHERE substr(file_path, 1, ?1) = ?2",
                params![new_len, new_prefix],
            )?;
            tx.execute(
                "UPDATE files SET file_path = ?1 || substr(file_path, ?2 + 1)
                 WHERE substr(file_path, 1, ?2) = ?3",
                params![new_prefix, old_len, old_prefix],
            )?;
        }
        Ok(())
    }
}
