//! Semantic queries over the whole index, a root, or a single file.

use super::*;

impl SemanticIndex {
    /// Query the index for the top-k nearest neighbours to `embedding`.
    /// Uses cosine similarity computed in Rust (O(n) over all stored vectors).
    pub fn query(&self, embedding: &[f32], top_k: usize) -> anyhow::Result<Vec<IndexedChunk>> {
        self.query_scoped(embedding, top_k, SemanticQueryScope::Corpus)
    }
    pub fn query_scoped(
        &self,
        embedding: &[f32],
        top_k: usize,
        scope: SemanticQueryScope<'_>,
    ) -> anyhow::Result<Vec<IndexedChunk>> {
        match scope {
            SemanticQueryScope::Corpus => self.query_corpus(embedding, top_k),
            SemanticQueryScope::Root(root) => self.query_root(embedding, top_k, root),
            SemanticQueryScope::File(path) => self.query_file(embedding, top_k, path),
        }
    }
    /// Apply document eligibility and exclusions before the caller's top-k boundary.
    /// Filtered queries widen the nearest-neighbour request only when filtered
    /// documents leave too few results, avoiding a corpus-wide scan for the
    /// common case of one or two exclusions.
    pub fn query_scoped_filtered(
        &self,
        embedding: &[f32],
        top_k: usize,
        scope: SemanticQueryScope<'_>,
        eligible_paths: Option<&std::collections::HashSet<PathBuf>>,
        excluded_paths: Option<&std::collections::HashSet<PathBuf>>,
    ) -> anyhow::Result<Vec<IndexedChunk>> {
        if eligible_paths.is_some_and(|paths| paths.is_empty()) {
            return Ok(Vec::new());
        }
        if eligible_paths.is_none() && excluded_paths.is_none_or(|paths| paths.is_empty()) {
            return self.query_scoped(embedding, top_k, scope);
        }
        let retain_eligible = |chunk: &IndexedChunk| {
            eligible_paths.is_none_or(|paths| paths.contains(&chunk.file_path))
                && excluded_paths.is_none_or(|paths| !paths.contains(&chunk.file_path))
        };
        if top_k == 0 {
            let mut results = self.query_scoped(embedding, 0, scope)?;
            results.retain(retain_eligible);
            return Ok(results);
        }

        let mut candidate_limit = top_k;
        loop {
            let mut results = self.query_scoped(embedding, candidate_limit, scope)?;
            let exhausted = results.len() < candidate_limit;
            results.retain(&retain_eligible);
            if results.len() >= top_k || exhausted {
                results.truncate(top_k);
                return Ok(results);
            }
            let widened = candidate_limit.saturating_mul(2);
            if widened == candidate_limit {
                return Ok(results);
            }
            candidate_limit = widened;
        }
    }
    /// Leading extracted text for a document, read straight from the index.
    ///
    /// The extraction cache only — never a fresh parse. Hovering a row in the
    /// related-documents pane is not a licence to open a PDF.
    pub fn cached_document_excerpt(
        &self,
        path: &Path,
        max_chars: usize,
    ) -> anyhow::Result<Option<String>> {
        let key = self
            .path_key_for_existing_path(path)
            .to_string_lossy()
            .into_owned();
        let mut stmt = self.conn.prepare(
            "SELECT c.chunk_text
             FROM files f JOIN chunks c ON c.file_id = f.id
             WHERE f.file_path = ?1
             ORDER BY c.byte_start ASC
             LIMIT 8",
        )?;
        let rows = stmt.query_map([&key], |row| row.get::<_, String>(0))?;

        let mut excerpt = String::new();
        for row in rows {
            if excerpt.chars().count() >= max_chars {
                break;
            }
            if !excerpt.is_empty() {
                excerpt.push(' ');
            }
            excerpt.push_str(row?.trim());
        }
        if excerpt.trim().is_empty() {
            return Ok(None);
        }
        Ok(Some(
            crate::generate::truncate_chars(excerpt.trim(), max_chars).to_string(),
        ))
    }
    pub fn related_documents(
        &self,
        root: &Path,
        source_path: &Path,
        limit: usize,
        supported_extensions: &[String],
        whole_corpus: bool,
    ) -> anyhow::Result<Vec<RelatedDocument>> {
        self.related_documents_filtered(
            root,
            source_path,
            limit,
            supported_extensions,
            whole_corpus,
            None,
        )
    }
    pub fn related_documents_filtered(
        &self,
        root: &Path,
        source_path: &Path,
        limit: usize,
        supported_extensions: &[String],
        whole_corpus: bool,
        eligible_paths: Option<&std::collections::HashSet<PathBuf>>,
    ) -> anyhow::Result<Vec<RelatedDocument>> {
        let source_key = self
            .path_key_for_existing_path(source_path)
            .to_string_lossy()
            .into_owned();
        let limit = if limit == 0 { 8 } else { limit };
        let (sql, root_id) = if whole_corpus {
            (
                "SELECT f.file_path, v.embedding
                 FROM files f
                 JOIN chunks c ON c.file_id = f.id
                 JOIN vec_chunks v ON v.rowid = c.id",
                None,
            )
        } else {
            let Some(root_id) = self.root_id_for_path(root)? else {
                return Ok(Vec::new());
            };
            (
                "SELECT f.file_path, v.embedding
                 FROM root_files rf
                 JOIN files f ON f.id = rf.file_id
                 JOIN chunks c ON c.file_id = f.id
                 JOIN vec_chunks v ON v.rowid = c.id
                 WHERE rf.root_id = ?1",
                Some(root_id),
            )
        };

        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params_from_iter(root_id), |row| {
            let file_path: String = row.get(0)?;
            let embedding_blob: Vec<u8> = row.get(1)?;
            Ok((file_path, embedding_blob))
        })?;
        let mut by_file = HashMap::new();
        self.accumulate_document_vectors(rows, &mut by_file)?;

        let Some(source_acc) = by_file.get(&source_key) else {
            anyhow::bail!(
                "Source file is not present in the semantic index: {}",
                source_path.display()
            );
        };
        let source_centroid = source_acc.centroid();

        let mut related = Vec::new();
        for (file_path, acc) in by_file {
            if file_path == source_key || acc.chunks == 0 {
                continue;
            }
            let abs_path = self.key_to_display_path(&file_path);
            if eligible_paths.is_some_and(|eligible| !eligible.contains(&abs_path)) {
                continue;
            }
            let Some(file_type) = FileType::detect(&abs_path, supported_extensions) else {
                continue;
            };
            let candidate_centroid = acc.centroid();
            let score = cosine_similarity(&source_centroid, &candidate_centroid);
            let metadata = match std::fs::metadata(&abs_path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    error!(
                        file = %abs_path.display(),
                        error = %error,
                        "Skipping missing related-document candidate"
                    );
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            let extension = abs_path
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or_default()
                .to_string();
            related.push(RelatedDocument {
                entry: crate::types::FileEntry {
                    path: abs_path,
                    size_bytes: metadata.len(),
                    file_type,
                    extension,
                    created_at_ms: metadata.created().ok().and_then(system_time_ms),
                    modified_at_ms: metadata.modified().ok().and_then(system_time_ms),
                    title: None,
                    author: None,
                    doi: None,
                    publication_date: None,
                    citation_count: None,
                    metadata_conflicts: Default::default(),
                    tags: Vec::new(),
                },
                score,
            });
        }

        related.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| {
                    let left = a
                        .entry
                        .path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or_default();
                    let right = b
                        .entry
                        .path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or_default();
                    left.cmp(right)
                })
                .then_with(|| a.entry.path.cmp(&b.entry.path))
        });
        related.truncate(limit);
        Ok(related)
    }
    /// The document vector of each of `paths` that the index holds: the mean
    /// of its normalised chunk vectors, as [`Self::related_documents`] compares
    /// documents by. A path the index does not hold is absent from the answer,
    /// so the caller can say which documents it could not place.
    pub fn document_vectors(
        &self,
        paths: &[PathBuf],
    ) -> anyhow::Result<HashMap<PathBuf, Vec<f32>>> {
        let mut stmt = self.conn.prepare(
            "SELECT f.file_path, v.embedding
             FROM files f
             JOIN chunks c ON c.file_id = f.id
             JOIN vec_chunks v ON v.rowid = c.id
             WHERE f.file_path = ?1",
        )?;
        let mut by_file = HashMap::new();
        let mut requested = HashMap::with_capacity(paths.len());
        for path in paths {
            let key = self
                .path_key_for_existing_path(path)
                .to_string_lossy()
                .into_owned();
            let rows = stmt.query_map([&key], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
            })?;
            self.accumulate_document_vectors(rows, &mut by_file)?;
            requested.insert(key, path.clone());
        }
        Ok(by_file
            .into_iter()
            .filter(|(_, acc)| acc.chunks > 0)
            .filter_map(|(key, acc)| requested.remove(&key).map(|path| (path, acc.centroid())))
            .collect())
    }
    /// Sum each file's normalised chunk vectors into `by_file`, checking every
    /// stored vector has the index's width. The one place a document's vector
    /// is assembled from its chunks.
    pub(super) fn accumulate_document_vectors(
        &self,
        rows: impl Iterator<Item = rusqlite::Result<(String, Vec<u8>)>>,
        by_file: &mut HashMap<String, DocumentVectorSum>,
    ) -> anyhow::Result<()> {
        for row in rows {
            let (file_path, embedding_blob) = row?;
            let embedding = f32_slice_from_bytes(&embedding_blob)?;
            anyhow::ensure!(
                embedding.len() == self.dimension,
                "Stored embedding dimension mismatch for {}. Expected {}, received {}.",
                file_path,
                self.dimension,
                embedding.len()
            );
            let normalized = normalized_vector(&embedding);
            let entry = by_file
                .entry(file_path)
                .or_insert_with(|| DocumentVectorSum {
                    sum: vec![0.0; self.dimension],
                    chunks: 0,
                });
            for (total, value) in entry.sum.iter_mut().zip(normalized) {
                *total += value;
            }
            entry.chunks += 1;
        }
        Ok(())
    }
    pub(super) fn query_corpus(
        &self,
        embedding: &[f32],
        top_k: usize,
    ) -> anyhow::Result<Vec<IndexedChunk>> {
        anyhow::ensure!(
            embedding.len() == self.dimension,
            "Dimension mismatch for query vector for the \"embedding\" column. Expected {} dimensions but received {}.",
            self.dimension,
            embedding.len()
        );

        // cosine distance = 1 - cosine_similarity.
        // No hard threshold: top_k already bounds the result count, and a fixed
        // distance cutoff is model-dependent (short queries on MiniLM-style models
        // produce distances of 0.7–0.85 even for clearly relevant chunks).

        let stored_count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM vec_chunks", [], |r| r.get(0))
            .unwrap_or(0);
        let top_k = if top_k == 0 {
            stored_count as usize
        } else {
            top_k
        };
        tracing::info!(
            "[query] vec_chunks rows={stored_count}, embedding_dim={}, top_k={top_k}",
            embedding.len()
        );

        let blob = f32_slice_to_bytes(embedding);

        let mut stmt = self.conn.prepare(
            "SELECT v.rowid, v.distance, f.file_path, c.byte_start, c.byte_end,
                    c.origin_type, c.page, c.line, c.col,
                    c.bbox_x, c.bbox_y, c.bbox_w, c.bbox_h, c.chunk_text
             FROM vec_chunks v
             JOIN chunks c ON c.id = v.rowid
             JOIN files f ON f.id = c.file_id
             WHERE v.embedding MATCH ?1
               AND v.k = ?2
             ORDER BY v.distance",
        )?;

        let raw_rows: Vec<_> = stmt
            .query_map(params![blob, top_k as i64], |row| {
                let distance: f32 = row.get(1)?;
                let file_path: String = row.get(2)?;
                let byte_start: i64 = row.get(3)?;
                let byte_end: i64 = row.get(4)?;
                let origin_type: String = row.get(5)?;
                let page: Option<i64> = row.get(6)?;
                let line: Option<i64> = row.get(7)?;
                let col: Option<i64> = row.get(8)?;
                let bbox_x: Option<f64> = row.get(9)?;
                let bbox_y: Option<f64> = row.get(10)?;
                let bbox_w: Option<f64> = row.get(11)?;
                let bbox_h: Option<f64> = row.get(12)?;
                let chunk_text: String = row.get(13)?;
                Ok((
                    distance,
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
                ))
            })?
            .map(|r| match r {
                Ok(v) => Some(v),
                Err(e) => {
                    error!("[query] row error: {e}");
                    None
                }
            })
            .collect();

        tracing::info!(
            "[query] sqlite-vec returned {} rows ({} errors)",
            raw_rows.iter().filter(|r| r.is_some()).count(),
            raw_rows.iter().filter(|r| r.is_none()).count()
        );

        let results: Vec<IndexedChunk> = raw_rows
            .into_iter()
            .flatten()
            .filter_map(
                |(
                    distance,
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
                )| {
                    let score = 1.0 - distance;
                    let origin = match origin_type.as_str() {
                        "text_file" => SourceOrigin::TextFile {
                            line: line.unwrap_or(0) as u32,
                            col: col.unwrap_or(0) as u32,
                        },
                        "pdf_page" => {
                            let bbox = match (bbox_x, bbox_y, bbox_w, bbox_h) {
                                (Some(x), Some(y), Some(w), Some(h)) => Some(BoundingBox {
                                    x: x as f32,
                                    y: y as f32,
                                    width: w as f32,
                                    height: h as f32,
                                }),
                                _ => None,
                            };
                            SourceOrigin::PdfPage {
                                page: page.unwrap_or(0) as u32,
                                bbox,
                            }
                        }
                        other => {
                            error!("[query] unknown origin_type '{}' for {file_path}", other);
                            return None;
                        }
                    };
                    let abs_path = self.key_to_display_path(&file_path);
                    Some(IndexedChunk {
                        file_path: abs_path,
                        chunk_text,
                        extraction_byte_range: ByteRange {
                            start: byte_start as usize,
                            end: byte_end as usize,
                        },
                        origin,
                        score,
                    })
                },
            )
            .collect();

        tracing::info!("[query] returning {} results", results.len());
        Ok(results)
    }
    pub(super) fn query_file(
        &self,
        embedding: &[f32],
        top_k: usize,
        path: &Path,
    ) -> anyhow::Result<Vec<IndexedChunk>> {
        anyhow::ensure!(
            embedding.len() == self.dimension,
            "Dimension mismatch for query vector for the \"embedding\" column. Expected {} dimensions but received {}.",
            self.dimension,
            embedding.len()
        );

        let rel_path = self.path_key_for_existing_path(path);
        let rel_path = rel_path.to_string_lossy().into_owned();
        let mut stmt = self.conn.prepare(
            "SELECT f.file_path, c.byte_start, c.byte_end,
                    c.origin_type, c.page, c.line, c.col,
                    c.bbox_x, c.bbox_y, c.bbox_w, c.bbox_h, c.chunk_text,
                    v.embedding
             FROM chunks c
             JOIN vec_chunks v ON v.rowid = c.id
             JOIN files f ON f.id = c.file_id
             WHERE f.file_path = ?1",
        )?;

        let mut scored = Vec::new();
        let rows = stmt.query_map(params![rel_path], |row| {
            let file_path: String = row.get(0)?;
            let byte_start: i64 = row.get(1)?;
            let byte_end: i64 = row.get(2)?;
            let origin_type: String = row.get(3)?;
            let page: Option<i64> = row.get(4)?;
            let line: Option<i64> = row.get(5)?;
            let col: Option<i64> = row.get(6)?;
            let bbox_x: Option<f64> = row.get(7)?;
            let bbox_y: Option<f64> = row.get(8)?;
            let bbox_w: Option<f64> = row.get(9)?;
            let bbox_h: Option<f64> = row.get(10)?;
            let chunk_text: String = row.get(11)?;
            let embedding_blob: Vec<u8> = row.get(12)?;
            Ok((
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
                embedding_blob,
            ))
        })?;

        for row in rows {
            let (
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
                embedding_blob,
            ) = row?;
            let chunk_embedding = f32_slice_from_bytes(&embedding_blob)?;
            anyhow::ensure!(
                chunk_embedding.len() == self.dimension,
                "Stored embedding dimension mismatch for {}. Expected {}, received {}.",
                file_path,
                self.dimension,
                chunk_embedding.len()
            );
            let Some(origin) = source_origin_from_parts(
                &origin_type,
                page,
                line,
                col,
                bbox_x,
                bbox_y,
                bbox_w,
                bbox_h,
            ) else {
                error!(
                    "[query_file] unknown origin_type '{}' for {file_path}",
                    origin_type
                );
                continue;
            };
            let score = cosine_similarity(embedding, &chunk_embedding);
            let abs_path = self.key_to_display_path(&file_path);
            scored.push(IndexedChunk {
                file_path: abs_path,
                chunk_text,
                extraction_byte_range: ByteRange {
                    start: byte_start as usize,
                    end: byte_end as usize,
                },
                origin,
                score,
            });
        }

        scored.sort_by(|a, b| b.score.total_cmp(&a.score));
        if top_k > 0 && scored.len() > top_k {
            scored.truncate(top_k);
        }
        Ok(scored)
    }
    pub(super) fn query_root(
        &self,
        embedding: &[f32],
        top_k: usize,
        root: &Path,
    ) -> anyhow::Result<Vec<IndexedChunk>> {
        anyhow::ensure!(
            embedding.len() == self.dimension,
            "Dimension mismatch for query vector for the \"embedding\" column. Expected {} dimensions but received {}.",
            self.dimension,
            embedding.len()
        );

        let Some(root_id) = self.root_id_for_path(root)? else {
            return Ok(Vec::new());
        };
        let mut stmt = self.conn.prepare(
            "SELECT f.file_path, c.byte_start, c.byte_end,
                    c.origin_type, c.page, c.line, c.col,
                    c.bbox_x, c.bbox_y, c.bbox_w, c.bbox_h, c.chunk_text,
                    v.embedding
             FROM root_files rf
             JOIN files f ON f.id = rf.file_id
             JOIN chunks c ON c.file_id = f.id
             JOIN vec_chunks v ON v.rowid = c.id
             WHERE rf.root_id = ?1",
        )?;

        let rows = stmt.query_map(params![root_id], |row| {
            let file_path: String = row.get(0)?;
            let byte_start: i64 = row.get(1)?;
            let byte_end: i64 = row.get(2)?;
            let origin_type: String = row.get(3)?;
            let page: Option<i64> = row.get(4)?;
            let line: Option<i64> = row.get(5)?;
            let col: Option<i64> = row.get(6)?;
            let bbox_x: Option<f64> = row.get(7)?;
            let bbox_y: Option<f64> = row.get(8)?;
            let bbox_w: Option<f64> = row.get(9)?;
            let bbox_h: Option<f64> = row.get(10)?;
            let chunk_text: String = row.get(11)?;
            let embedding_blob: Vec<u8> = row.get(12)?;
            Ok((
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
                embedding_blob,
            ))
        })?;

        let mut scored = Vec::new();
        for row in rows {
            let (
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
                embedding_blob,
            ) = row?;
            let chunk_embedding = f32_slice_from_bytes(&embedding_blob)?;
            anyhow::ensure!(
                chunk_embedding.len() == self.dimension,
                "Stored embedding dimension mismatch for {}. Expected {}, received {}.",
                file_path,
                self.dimension,
                chunk_embedding.len()
            );
            let Some(origin) = source_origin_from_parts(
                &origin_type,
                page,
                line,
                col,
                bbox_x,
                bbox_y,
                bbox_w,
                bbox_h,
            ) else {
                error!(
                    "[query_root] unknown origin_type '{}' for {file_path}",
                    origin_type
                );
                continue;
            };
            scored.push(IndexedChunk {
                file_path: self.key_to_display_path(&file_path),
                chunk_text,
                extraction_byte_range: ByteRange {
                    start: byte_start as usize,
                    end: byte_end as usize,
                },
                origin,
                score: cosine_similarity(embedding, &chunk_embedding),
            });
        }

        scored.sort_by(|a, b| b.score.total_cmp(&a.score));
        if top_k > 0 && scored.len() > top_k {
            scored.truncate(top_k);
        }
        Ok(scored)
    }
}
