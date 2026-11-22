//! Chunks gathered for topic clustering, and topic coverage across the library.

use super::*;

/// A document-local topic projected across the configured indexed library.
/// `mean_member_embedding` is the arithmetic mean of normalized member
/// embeddings, so its dot product with a normalized candidate is that
/// candidate's average cosine similarity to the topic members.
#[derive(Clone, Debug)]
pub struct TopicCoveragePrototype {
    pub mean_member_embedding: Vec<f32>,
    pub cohesion: f32,
}

#[derive(Clone, Debug, Default)]
pub struct TopicCoverageResult {
    pub eligible_document_count: usize,
    pub related_document_counts: Vec<usize>,
    pub related_chunks: Vec<Vec<ChunkTopicMember>>,
}

pub(super) const TOPIC_COVERAGE_CHUNKS_PER_DOCUMENT: usize = 15;

#[derive(Clone, Debug)]
pub(super) struct RankedCoverageChunk {
    pub(super) score: f32,
    pub(super) chunk: ChunkTopicMember,
}

impl SemanticIndex {
    /// Bulk-read all chunk vectors and passage metadata belonging to one
    /// indexed root. This is deliberately separate from ANN querying: topic
    /// discovery needs the bounded input set itself, not nearest neighbours to
    /// a query vector.
    pub fn topic_chunks_for_root(&self, root: &Path) -> anyhow::Result<Vec<TopicChunkData>> {
        let Some(root_id) = self.root_id_for_path(root)? else {
            return Ok(Vec::new());
        };
        self.topic_chunks_filtered("rf.root_id = ?1", &[&root_id])
    }
    /// How many exportable passages each document of one root holds, keyed by
    /// the document's canonical path.
    ///
    /// The joins are the export's own (`root_files` → `files` → `chunks` →
    /// `vec_chunks`), so a path this reports with a non-zero count is a path
    /// `topic_chunks_for_file` will answer for. A file row alone is not the
    /// question a caller is asking: a document that was walked but whose
    /// passages never made it into the index would export nothing, and saying
    /// otherwise would move the disappointment to the export call.
    ///
    /// A root the index has never seen is an empty map rather than an error —
    /// "no passages here" is what an unindexed root truthfully has.
    pub fn indexed_chunk_counts_for_root(
        &self,
        root: &Path,
    ) -> anyhow::Result<HashMap<PathBuf, usize>> {
        let Some(root_id) = self.root_id_for_path(root)? else {
            return Ok(HashMap::new());
        };
        let mut stmt = self.conn.prepare(
            "SELECT f.file_path, count(*)
             FROM root_files rf
             JOIN files f ON f.id = rf.file_id
             JOIN chunks c ON c.file_id = f.id
             JOIN vec_chunks v ON v.rowid = c.id
             WHERE rf.root_id = ?1
             GROUP BY f.file_path",
        )?;
        let rows = stmt.query_map(params![root_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        let mut counts = HashMap::new();
        for row in rows {
            let (stored, count) = row?;
            counts.insert(
                self.key_to_display_path(&stored),
                usize::try_from(count).unwrap_or(0),
            );
        }
        Ok(counts)
    }
    /// Bulk-read vectors and passage metadata for one indexed document while
    /// proving that the file belongs to the requested root. Filtering in SQL
    /// avoids materialising the entire root for a within-document cloud.
    pub fn topic_chunks_for_file(
        &self,
        root: &Path,
        path: &Path,
    ) -> anyhow::Result<Vec<TopicChunkData>> {
        let Some(root_id) = self.root_id_for_path(root)? else {
            return Ok(Vec::new());
        };
        let path_key = self
            .path_key_for_existing_path(path)
            .to_string_lossy()
            .into_owned();
        self.topic_chunks_filtered(
            "rf.root_id = ?1 AND f.file_path = ?2",
            &[&root_id, &path_key],
        )
    }
    /// Find other documents in the configured library roots that contain a
    /// passage at least as similar to each topic as the topic's own members are
    /// to one another, retaining the strongest passages from every match.
    ///
    /// Only roots that still exist in the semantic index participate. The
    /// membership `EXISTS` clause prevents a file shared by overlapping roots
    /// from duplicating its chunks. All prototypes are evaluated in one scan,
    /// keeping coverage linear in the eligible indexed passage count.
    pub fn topic_library_coverage(
        &self,
        roots: &[PathBuf],
        source_path: &Path,
        prototypes: &[TopicCoveragePrototype],
        cancelled: &AtomicBool,
    ) -> anyhow::Result<TopicCoverageResult> {
        if prototypes.is_empty() {
            return Ok(TopicCoverageResult::default());
        }
        for prototype in prototypes {
            anyhow::ensure!(
                prototype.mean_member_embedding.len() == self.dimension,
                "Topic coverage prototype dimension mismatch: expected {}, received {}",
                self.dimension,
                prototype.mean_member_embedding.len()
            );
            anyhow::ensure!(
                prototype
                    .mean_member_embedding
                    .iter()
                    .all(|value| value.is_finite())
                    && prototype.cohesion.is_finite(),
                "Topic coverage prototype contains non-finite values"
            );
        }
        let mut root_ids = roots
            .iter()
            .map(|root| self.root_id_for_path(root))
            .collect::<anyhow::Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        root_ids.sort_unstable();
        root_ids.dedup();
        if root_ids.is_empty() {
            return Ok(TopicCoverageResult {
                eligible_document_count: 0,
                related_document_counts: vec![0; prototypes.len()],
                related_chunks: vec![Vec::new(); prototypes.len()],
            });
        }
        let source_key = self
            .path_key_for_existing_path(source_path)
            .to_string_lossy()
            .into_owned();
        let root_placeholders = std::iter::repeat_n("?", root_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT c.id, f.id, f.file_path, c.byte_start, c.byte_end,
                    c.origin_type, c.page, c.line, c.col,
                    c.bbox_x, c.bbox_y, c.bbox_w, c.bbox_h, c.chunk_text,
                    v.embedding
             FROM files f
             JOIN chunks c ON c.file_id = f.id
             JOIN vec_chunks v ON v.rowid = c.id
             WHERE EXISTS (
                 SELECT 1
                 FROM root_files rf
                 WHERE rf.file_id = f.id
                   AND rf.root_id IN ({root_placeholders})
             )
             ORDER BY f.id, c.chunk_idx, c.id"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(&root_ids), |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<i64>>(6)?,
                row.get::<_, Option<i64>>(7)?,
                row.get::<_, Option<i64>>(8)?,
                row.get::<_, Option<f64>>(9)?,
                row.get::<_, Option<f64>>(10)?,
                row.get::<_, Option<f64>>(11)?,
                row.get::<_, Option<f64>>(12)?,
                row.get::<_, String>(13)?,
                row.get::<_, Vec<u8>>(14)?,
            ))
        })?;

        let mut eligible_documents = HashSet::new();
        let mut related_by_document =
            vec![BTreeMap::<i64, Vec<RankedCoverageChunk>>::new(); prototypes.len()];
        for (row_number, row) in rows.enumerate() {
            if row_number.is_multiple_of(256) && cancelled.load(Ordering::Relaxed) {
                anyhow::bail!("Chunk topic operation cancelled");
            }
            let (
                chunk_id,
                file_id,
                file_path,
                byte_start,
                byte_end,
                origin_type,
                page,
                line,
                col,
                bbox_x,
                bbox_y,
                bbox_w,
                bbox_h,
                chunk_text,
                embedding_bytes,
            ) = row?;
            if file_path == source_key {
                continue;
            }
            eligible_documents.insert(file_id);
            let embedding = normalized_vector(&f32_slice_from_bytes(&embedding_bytes)?);
            anyhow::ensure!(
                embedding.len() == self.dimension,
                "Stored embedding dimension mismatch for {}. Expected {}, received {}.",
                file_path,
                self.dimension,
                embedding.len()
            );
            let mut matches = Vec::new();
            for (index, prototype) in prototypes.iter().enumerate() {
                let average_similarity = embedding
                    .iter()
                    .zip(&prototype.mean_member_embedding)
                    .map(|(candidate, member_mean)| candidate * member_mean)
                    .sum::<f32>();
                if average_similarity >= prototype.cohesion {
                    matches.push((index, average_similarity));
                }
            }
            if matches.is_empty() {
                continue;
            }
            let origin = source_origin_from_parts(
                &origin_type,
                page,
                line,
                col,
                bbox_x,
                bbox_y,
                bbox_w,
                bbox_h,
            )
            .ok_or_else(|| anyhow::anyhow!("Unknown chunk origin type '{origin_type}'"))?;
            let chunk = ChunkTopicMember {
                chunk_id,
                file_path: self.key_to_display_path(&file_path),
                chunk_text,
                extraction_byte_range: ByteRange {
                    start: byte_start as usize,
                    end: byte_end as usize,
                },
                origin,
            };
            for (index, score) in matches {
                let ranked = related_by_document[index].entry(file_id).or_default();
                ranked.push(RankedCoverageChunk {
                    score,
                    chunk: chunk.clone(),
                });
                ranked.sort_by(|left, right| {
                    right
                        .score
                        .total_cmp(&left.score)
                        .then_with(|| left.chunk.chunk_id.cmp(&right.chunk.chunk_id))
                });
                ranked.truncate(TOPIC_COVERAGE_CHUNKS_PER_DOCUMENT);
            }
        }
        if cancelled.load(Ordering::Relaxed) {
            anyhow::bail!("Chunk topic operation cancelled");
        }
        let related_document_counts = related_by_document.iter().map(BTreeMap::len).collect();
        let related_chunks = related_by_document
            .into_iter()
            .map(|documents| {
                documents
                    .into_values()
                    .flat_map(|chunks| chunks.into_iter().map(|ranked| ranked.chunk))
                    .collect()
            })
            .collect();
        Ok(TopicCoverageResult {
            eligible_document_count: eligible_documents.len(),
            related_document_counts,
            related_chunks,
        })
    }
    pub(super) fn topic_chunks_filtered(
        &self,
        predicate: &str,
        parameters: &[&dyn ToSql],
    ) -> anyhow::Result<Vec<TopicChunkData>> {
        let sql = format!(
            "SELECT c.id, f.id, f.file_path, c.byte_start, c.byte_end,
                    c.origin_type, c.page, c.line, c.col,
                    c.bbox_x, c.bbox_y, c.bbox_w, c.bbox_h, c.chunk_text,
                    v.embedding, c.chunk_ref, c.text_sha256
             FROM root_files rf
             JOIN files f ON f.id = rf.file_id
             JOIN chunks c ON c.file_id = f.id
             JOIN vec_chunks v ON v.rowid = c.id
             WHERE {predicate}
             ORDER BY f.file_path, c.chunk_idx, c.id"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(parameters, |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<i64>>(6)?,
                row.get::<_, Option<i64>>(7)?,
                row.get::<_, Option<i64>>(8)?,
                row.get::<_, Option<f64>>(9)?,
                row.get::<_, Option<f64>>(10)?,
                row.get::<_, Option<f64>>(11)?,
                row.get::<_, Option<f64>>(12)?,
                row.get::<_, String>(13)?,
                row.get::<_, Vec<u8>>(14)?,
                row.get::<_, Option<String>>(15)?,
                row.get::<_, Option<String>>(16)?,
            ))
        })?;

        let mut chunks = Vec::new();
        for row in rows {
            let (
                chunk_id,
                file_id,
                file_path,
                byte_start,
                byte_end,
                origin_type,
                page,
                line,
                col,
                bbox_x,
                bbox_y,
                bbox_w,
                bbox_h,
                chunk_text,
                embedding_bytes,
                chunk_ref,
                text_sha256,
            ) = row?;
            let origin = source_origin_from_parts(
                &origin_type,
                page,
                line,
                col,
                bbox_x,
                bbox_y,
                bbox_w,
                bbox_h,
            )
            .ok_or_else(|| anyhow::anyhow!("Unknown chunk origin type '{origin_type}'"))?;
            let embedding = f32_slice_from_bytes(&embedding_bytes)?;
            anyhow::ensure!(
                embedding.len() == self.dimension,
                "Chunk {chunk_id} has dimension {}; expected {}",
                embedding.len(),
                self.dimension
            );
            chunks.push(TopicChunkData {
                chunk_id,
                file_id,
                file_path: self.key_to_display_path(&file_path),
                chunk_text,
                chunk_ref: chunk_ref.map(ChunkRef),
                text_sha256,
                extraction_byte_range: ByteRange {
                    start: byte_start as usize,
                    end: byte_end as usize,
                },
                origin,
                embedding,
            });
        }
        Ok(chunks)
    }
}
