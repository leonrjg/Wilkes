//! Indexed roots: activation, membership, merging and reconciliation with the
//! files on disk.

use super::*;

impl SemanticIndex {
    pub fn activate_root(&mut self, root: &Path) -> anyhow::Result<i64> {
        let root = Self::canonical_path(root);
        let root_str = root.to_string_lossy().into_owned();
        let now = Self::now_ms();
        self.conn.execute(
            "INSERT OR IGNORE INTO indexed_roots (path, built_at, last_reconciled_at)
             VALUES (?1, ?2, ?2)",
            params![root_str, now],
        )?;
        let root_id: i64 = self.conn.query_row(
            "SELECT id FROM indexed_roots WHERE path = ?1",
            params![root.to_string_lossy()],
            |row| row.get(0),
        )?;
        self.active_root = Some(root);
        self.active_root_id = Some(root_id);
        Ok(root_id)
    }
    pub(super) fn root_id_for_path(&self, root: &Path) -> anyhow::Result<Option<i64>> {
        let root = Self::canonical_path(root);
        self.conn
            .query_row(
                "SELECT id FROM indexed_roots WHERE path = ?1",
                params![root.to_string_lossy()],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }
    pub(super) fn prune_unreferenced_files(&mut self) -> anyhow::Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM files
             WHERE id NOT IN (SELECT DISTINCT file_id FROM root_files)",
            [],
        )?;
        tx.commit()?;
        Ok(())
    }
    /// Copy `root`'s coverage out of `source` into this index.
    ///
    /// `membership` decides what happens to documents `source` does not carry.
    /// [`RootMembership::Authoritative`] drops them, which is how a completed
    /// whole-root build removes documents that have left the disk.
    /// [`RootMembership::Additive`] keeps them, which is the only correct
    /// reading when the source is a stopped or partial build: it looked at some
    /// of the root, and silence about the rest is not a claim that the rest is
    /// gone.
    pub(super) fn merge_root_from(
        &mut self,
        source: &SemanticIndex,
        root: &Path,
        membership: RootMembership,
    ) -> anyhow::Result<()> {
        consumer_ensure!(
            self.embedding_space_id()? == source.embedding_space_id()?,
            ConsumerErrorCode::EmbeddingSpaceMismatch,
            "refusing cross-index vector copy",
        );
        let target_root_id = self.activate_root(root)?;
        let source_root_id = source.root_id_for_path(root)?.ok_or_else(|| {
            anyhow::anyhow!("Source index has no coverage for {}", root.display())
        })?;

        let mut source_stmt = source.conn.prepare(
            "SELECT f.id, f.file_path, f.size_bytes, f.modified_at_ms, f.indexed_at_ms, f.full_text,
                    f.source_sha256, f.snapshot_id, f.extraction_recipe_id, f.rendition_id,
                    f.extracted_content_sha256, f.embedding_reused_chunks,
                    f.embedding_computed_chunks, f.managed_snapshot_relative_path,
                    f.original_source_provenance_json, f.admission_state
             FROM root_files rf
             JOIN files f ON f.id = rf.file_id
             WHERE rf.root_id = ?1",
        )?;
        let source_files = source_stmt
            .query_map(params![source_root_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, Option<i64>>(11)?,
                    row.get::<_, Option<i64>>(12)?,
                    row.get::<_, Option<String>>(13)?,
                    row.get::<_, Option<String>>(14)?,
                    row.get::<_, Option<String>>(15)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let tx = self.conn.transaction()?;
        if membership == RootMembership::Authoritative {
            tx.execute(
                "DELETE FROM root_files WHERE root_id = ?1",
                params![target_root_id],
            )?;
        }

        for (
            source_file_id,
            file_path,
            size_bytes,
            modified_at_ms,
            indexed_at_ms,
            full_text,
            source_sha256,
            snapshot_id,
            extraction_recipe_id,
            rendition_id,
            extracted_content_sha256,
            embedding_reused_chunks,
            embedding_computed_chunks,
            managed_snapshot_relative_path,
            original_source_provenance_json,
            admission_state,
        ) in source_files
        {
            let target_file_id: Option<i64> = tx
                .query_row(
                    "SELECT id FROM files WHERE file_path = ?1",
                    params![file_path],
                    |row| row.get(0),
                )
                .optional()?;
            let target_file_id = if let Some(file_id) = target_file_id {
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
                        size_bytes,
                        modified_at_ms,
                        indexed_at_ms,
                        full_text,
                        source_sha256,
                        snapshot_id,
                        extraction_recipe_id,
                        rendition_id,
                        extracted_content_sha256,
                        embedding_reused_chunks,
                        embedding_computed_chunks,
                        managed_snapshot_relative_path,
                        original_source_provenance_json,
                        admission_state,
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
                    params![file_path, size_bytes, modified_at_ms, indexed_at_ms, full_text,
                            source_sha256, snapshot_id, extraction_recipe_id, rendition_id,
                            extracted_content_sha256, embedding_reused_chunks,
                            embedding_computed_chunks, managed_snapshot_relative_path,
                            original_source_provenance_json, admission_state],
                )?;
                tx.last_insert_rowid()
            };

            tx.execute(
                "INSERT OR REPLACE INTO root_files (root_id, file_id, seen_at_ms)
                 VALUES (?1, ?2, ?3)",
                params![target_root_id, target_file_id, Self::now_ms()],
            )?;

            let mut chunk_stmt = source.conn.prepare(
                "SELECT c.chunk_idx, c.byte_start, c.byte_end,
                        c.origin_type, c.page, c.line, c.col,
                        c.bbox_x, c.bbox_y, c.bbox_w, c.bbox_h, c.chunk_text,
                        c.chunk_ref, c.text_sha256, v.embedding
                 FROM chunks c
                 JOIN vec_chunks v ON v.rowid = c.id
                 WHERE c.file_id = ?1
                 ORDER BY c.chunk_idx",
            )?;
            let chunks = chunk_stmt
                .query_map(params![source_file_id], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                        row.get::<_, Option<i64>>(5)?,
                        row.get::<_, Option<i64>>(6)?,
                        row.get::<_, Option<f64>>(7)?,
                        row.get::<_, Option<f64>>(8)?,
                        row.get::<_, Option<f64>>(9)?,
                        row.get::<_, Option<f64>>(10)?,
                        row.get::<_, String>(11)?,
                        row.get::<_, Option<String>>(12)?,
                        row.get::<_, Option<String>>(13)?,
                        row.get::<_, Vec<u8>>(14)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;

            for (
                chunk_idx,
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
                stable_ref,
                text_sha256,
                embedding,
            ) in chunks
            {
                tx.execute(
                    "INSERT INTO chunks (file_id, chunk_idx, byte_start, byte_end,
                                         origin_type, page, line, col,
                                         bbox_x, bbox_y, bbox_w, bbox_h, chunk_text,
                                         chunk_ref, text_sha256)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                    params![
                        target_file_id,
                        chunk_idx,
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
                        stable_ref,
                        text_sha256,
                    ],
                )?;
                let chunk_id = tx.last_insert_rowid();
                tx.execute(
                    "INSERT INTO vec_chunks(rowid, embedding) VALUES (?1, ?2)",
                    params![chunk_id, embedding],
                )?;
            }

            // The retained extraction travels with the text and chunks it
            // belongs to. A merged root that dropped it would serve a document
            // whose formulas silently went back to being glyph runs and whose
            // figures stopped existing. Carried as the stored string: this is
            // the same rendition, so nothing here needs to understand it.
            let retained: Option<String> = source
                .conn
                .query_row(
                    "SELECT retained_extraction_json FROM files WHERE id = ?1",
                    params![source_file_id],
                    |row| row.get(0),
                )
                .optional()?
                .flatten();
            tx.execute(
                "UPDATE files SET retained_extraction_json = ?2 WHERE id = ?1",
                params![target_file_id, retained],
            )?;
        }

        tx.execute(
            "DELETE FROM files
             WHERE id NOT IN (SELECT DISTINCT file_id FROM root_files)",
            [],
        )?;
        tx.execute(
            "UPDATE indexed_roots
             SET built_at = COALESCE(built_at, ?2), last_reconciled_at = ?2
             WHERE id = ?1",
            params![target_root_id, Self::now_ms()],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub(super) fn finish_active_root_build(&mut self) -> anyhow::Result<()> {
        if let Some(root_id) = self.active_root_id {
            let now = Self::now_ms();
            self.conn.execute(
                "UPDATE indexed_roots
                 SET built_at = COALESCE(built_at, ?2), last_reconciled_at = ?2
                 WHERE id = ?1",
                params![root_id, now],
            )?;
        }
        Ok(())
    }
    pub fn delete_root(&mut self, root: &Path) -> anyhow::Result<()> {
        let Some(root_id) = self.root_id_for_path(root)? else {
            return Ok(());
        };
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM root_files WHERE root_id = ?1",
            params![root_id],
        )?;
        tx.execute("DELETE FROM indexed_roots WHERE id = ?1", params![root_id])?;
        tx.execute(
            "DELETE FROM files
             WHERE id NOT IN (SELECT DISTINCT file_id FROM root_files)",
            [],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub(super) fn indexed_files_for_root(
        &self,
        root_id: i64,
    ) -> anyhow::Result<Vec<IndexedFileRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT f.file_path, f.size_bytes, f.modified_at_ms
             FROM root_files rf
             JOIN files f ON f.id = rf.file_id
             WHERE rf.root_id = ?1",
        )?;
        let rows = stmt
            .query_map(params![root_id], |row| {
                let path: String = row.get(0)?;
                Ok(IndexedFileRecord {
                    path_key: PathBuf::from(path),
                    identity: FileIdentity {
                        size_bytes: row.get(1)?,
                        modified_at_ms: row.get(2)?,
                    },
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
    /// Which of `paths` this index does *not* hold under `root`.
    ///
    /// A read of `root_files` and `files` only: no extraction, no embedder, no
    /// model. Presence is the test, not identity — a file whose bytes changed
    /// since it was read is held but stale, and staleness is the directory
    /// watcher's and a rebuild's business, not this list's.
    ///
    /// A root the index has never seen holds nothing, so every path comes back
    /// rather than an error: "never indexed" is an answer this is asked for.
    pub fn unindexed_paths_for_root(
        &self,
        root: &Path,
        paths: &[PathBuf],
    ) -> anyhow::Result<Vec<PathBuf>> {
        let Some(root_id) = self.root_id_for_path(root)? else {
            return Ok(paths.to_vec());
        };
        let indexed: HashSet<PathBuf> = self
            .indexed_files_for_root(root_id)?
            .into_iter()
            .map(|row| row.path_key)
            .collect();
        Ok(paths
            .iter()
            .filter(|path| !indexed.contains(&Self::canonical_path(path)))
            .cloned()
            .collect())
    }
    pub(super) fn rename_key(&mut self, old_key: &Path, new_key: &Path) -> anyhow::Result<()> {
        let old_rel = old_key.to_string_lossy().into_owned();
        let new_rel = new_key.to_string_lossy().into_owned();
        if old_rel == new_rel {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        Self::delete_file_by_key_tx(&tx, &new_rel)?;
        tx.execute(
            "UPDATE files SET file_path = ?1 WHERE file_path = ?2",
            params![new_rel, old_rel],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn reconcile_root(
        &mut self,
        root: &Path,
        extractors: &ExtractorRegistry,
        embedder: &dyn Embedder,
        indexing: &IndexingConfig,
    ) -> anyhow::Result<Vec<String>> {
        let mut errors = Vec::new();
        let root_id = self.activate_root(root)?;
        let indexed = self.indexed_files_for_root(root_id)?;
        let by_path: HashMap<PathBuf, FileIdentity> = indexed
            .iter()
            .map(|row| (row.path_key.clone(), row.identity))
            .collect();
        let mut missing_by_identity: HashMap<(i64, i64), Vec<PathBuf>> = HashMap::new();
        for row in &indexed {
            let display_path = self.key_to_display_path(&row.path_key.to_string_lossy());
            if !display_path.exists() {
                missing_by_identity
                    .entry((row.identity.size_bytes, row.identity.modified_at_ms))
                    .or_default()
                    .push(row.path_key.clone());
            }
        }

        let mut seen = HashSet::new();
        for entry in WalkBuilder::new(root)
            .hidden(false)
            .git_ignore(true)
            .build()
        {
            let entry = match entry {
                Ok(entry) => entry,
                Err(e) => {
                    errors.push(format!("walk failed: {e}"));
                    continue;
                }
            };
            let path = entry.path();
            if !path.is_file()
                || crate::types::FileType::detect(path, &indexing.supported_extensions).is_none()
            {
                continue;
            }
            let key = self.path_key_for_existing_path(path);
            let identity = match FileIdentity::for_path(path) {
                Some(identity) => identity,
                None => {
                    errors.push(format!("identity unavailable for {}", path.display()));
                    continue;
                }
            };
            if by_path.get(&key).is_some_and(|stored| *stored == identity) {
                seen.insert(key);
                continue;
            }

            if by_path.contains_key(&key) {
                if let Err(e) = self.index_file(
                    path,
                    extractors,
                    embedder,
                    indexing.chunk_size,
                    indexing.chunk_overlap,
                ) {
                    if Self::is_fatal_embedder_error(&e) {
                        return Err(e);
                    }
                    errors.push(format!("reindex {}: {e:#}", path.display()));
                }
                seen.insert(key);
                continue;
            }

            let identity_key = (identity.size_bytes, identity.modified_at_ms);
            if let Some(candidates) = missing_by_identity.get(&identity_key) {
                if candidates.len() == 1 {
                    let old_key = &candidates[0];
                    if let Err(e) = self.rename_key(old_key, &key) {
                        errors.push(format!(
                            "rename indexed path {} -> {}: {e:#}",
                            old_key.display(),
                            key.display()
                        ));
                    } else {
                        seen.insert(key);
                        continue;
                    }
                }
            }

            if let Err(e) = self.index_file(
                path,
                extractors,
                embedder,
                indexing.chunk_size,
                indexing.chunk_overlap,
            ) {
                if Self::is_fatal_embedder_error(&e) {
                    return Err(e);
                }
                errors.push(format!("index {}: {e:#}", path.display()));
            }
            seen.insert(key);
        }

        for row in indexed {
            if !seen.contains(&row.path_key) {
                let key = row.path_key.to_string_lossy().into_owned();
                let tx = self.conn.transaction()?;
                tx.execute(
                    "DELETE FROM root_files
                     WHERE root_id = ?1
                       AND file_id IN (SELECT id FROM files WHERE file_path = ?2)",
                    params![root_id, key],
                )?;
                tx.commit()?;
            }
        }
        self.prune_unreferenced_files()?;
        self.finish_active_root_build()?;

        Ok(errors)
    }
}
