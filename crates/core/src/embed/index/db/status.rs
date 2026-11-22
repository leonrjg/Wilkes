//! Index status, read from an open index or straight from disk, and deleting
//! the index.

use super::*;

impl SemanticIndex {
    /// Read index metadata without re-validating model_id/dimension.
    pub fn status(&self) -> IndexStatus {
        self.status_for_root(None)
    }
    pub fn status_for_root(&self, root: Option<&Path>) -> IndexStatus {
        let engine_str: String = self
            .conn
            .query_row("SELECT value FROM meta WHERE key = 'engine'", [], |r| {
                r.get(0)
            })
            .unwrap_or_else(|_| "candle".to_string());

        let engine = match engine_str.as_str() {
            "sbert" | "python" => EmbeddingEngine::SBERT,
            "fastembed" => EmbeddingEngine::Fastembed,
            _ => EmbeddingEngine::Candle,
        };

        let model_id = self
            .conn
            .query_row("SELECT value FROM meta WHERE key = 'model_id'", [], |r| {
                r.get(0)
            })
            .unwrap_or_default();

        let dimension: usize = self
            .conn
            .query_row("SELECT value FROM meta WHERE key = 'dimension'", [], |r| {
                let s: String = r.get(0)?;
                Ok(s.parse().unwrap_or(0))
            })
            .unwrap_or(0);

        let built_at: Option<u64> = self
            .conn
            .query_row("SELECT value FROM meta WHERE key = 'built_at'", [], |r| {
                let s: String = r.get(0)?;
                Ok(s.parse::<u64>().ok())
            })
            .unwrap_or(None);

        let build_duration_ms: Option<u64> = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'build_duration_ms'",
                [],
                |r| {
                    let s: String = r.get(0)?;
                    Ok(s.parse::<u64>().ok())
                },
            )
            .unwrap_or(None);

        let root_path = root.map(Self::canonical_path);
        let root_id = root_path
            .as_ref()
            .and_then(|root| self.root_id_for_path(root).ok().flatten());
        let (indexed_files, total_chunks) = if let Some(root_id) = root_id {
            let indexed_files: usize = self
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM root_files WHERE root_id = ?1",
                    params![root_id],
                    |r| r.get(0),
                )
                .map(|n: i64| n as usize)
                .unwrap_or(0);
            let total_chunks: usize = self
                .conn
                .query_row(
                    "SELECT COUNT(*)
                     FROM root_files rf
                     JOIN chunks c ON c.file_id = rf.file_id
                     WHERE rf.root_id = ?1",
                    params![root_id],
                    |r| r.get(0),
                )
                .map(|n: i64| n as usize)
                .unwrap_or(0);
            (indexed_files, total_chunks)
        } else if root_path.is_some() {
            (0, 0)
        } else {
            let indexed_files: usize = self
                .conn
                .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
                .map(|n: i64| n as usize)
                .unwrap_or(0);
            let total_chunks: usize = self
                .conn
                .query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))
                .map(|n: i64| n as usize)
                .unwrap_or(0);
            (indexed_files, total_chunks)
        };

        IndexStatus {
            indexed_files,
            total_chunks,
            built_at,
            build_duration_ms,
            engine,
            model_id,
            dimension,
            root_path,
            db_size_bytes: None,
        }
    }
    /// Read index status directly from the DB file without opening a full SemanticIndex.
    /// Does not validate model_id/dimension against any embedder.
    pub fn read_status_from_path(data_dir: &Path) -> anyhow::Result<IndexStatus> {
        Self::read_status_from_path_for_root(data_dir, None)
    }
    pub fn read_status_from_path_for_root(
        data_dir: &Path,
        root: Option<&Path>,
    ) -> anyhow::Result<IndexStatus> {
        load_sqlite_vec();
        recover_interrupted_index_replacement(data_dir)?;
        let path = db_path(data_dir);
        anyhow::ensure!(path.exists(), "No semantic index found");
        let conn = Connection::open(&path)
            .with_context(|| format!("Failed to open index at {}", path.display()))?;
        configure_connection(&conn, &path)?;

        let has_files_table: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='files'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n > 0)
            .unwrap_or(false);
        anyhow::ensure!(
            has_files_table,
            "Index uses legacy schema (no files table); rebuild the index"
        );

        let mut schema_version: i64 = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |row| {
                    let s: String = row.get(0)?;
                    Ok(s.parse::<i64>().unwrap_or(0))
                },
            )
            .unwrap_or(0);
        if schema_version == 2 {
            Self::migrate_v2_to_v3(&conn)?;
            schema_version = 3;
        }
        if schema_version == 3 {
            Self::migrate_v3_to_v4(&conn)?;
            schema_version = 4;
        }
        if schema_version == 4 {
            Self::migrate_v4_to_v5(&conn)?;
            schema_version = 5;
        }
        if schema_version == 5 {
            Self::migrate_v5_to_v6(&conn)?;
            schema_version = 6;
        }
        if schema_version == 6 {
            Self::migrate_v6_to_v7(&conn)?;
            schema_version = 7;
        }
        if schema_version == 7 {
            Self::migrate_v7_to_v8(&conn)?;
            schema_version = 8;
        }
        if schema_version == 8 {
            Self::migrate_v8_to_v9(&conn)?;
            schema_version = 9;
        }
        if schema_version == 9 {
            Self::migrate_v9_to_v10(&conn)?;
            schema_version = 10;
        }
        if schema_version == 10 {
            Self::migrate_v10_to_v11(&conn)?;
            schema_version = 11;
        }
        if schema_version == 11 {
            Self::migrate_v11_to_v12(&conn)?;
            schema_version = 12;
        }
        anyhow::ensure!(
            schema_version == SCHEMA_VERSION,
            "Index schema version {} is not supported (expected {}); rebuild the index",
            schema_version,
            SCHEMA_VERSION
        );

        let db_size_bytes = std::fs::metadata(&path).ok().map(|m| m.len());

        let engine_str: String = conn
            .query_row("SELECT value FROM meta WHERE key = 'engine'", [], |r| {
                r.get(0)
            })
            .unwrap_or_else(|_| "candle".to_string());

        let engine = match engine_str.as_str() {
            "sbert" | "python" => EmbeddingEngine::SBERT,
            "fastembed" => EmbeddingEngine::Fastembed,
            _ => EmbeddingEngine::Candle,
        };

        let model_id: String = conn
            .query_row("SELECT value FROM meta WHERE key = 'model_id'", [], |r| {
                r.get(0)
            })
            .unwrap_or_default();

        let dimension: usize = conn
            .query_row("SELECT value FROM meta WHERE key = 'dimension'", [], |r| {
                let s: String = r.get(0)?;
                Ok(s.parse().unwrap_or(0))
            })
            .unwrap_or(0);

        let built_at: Option<u64> = conn
            .query_row("SELECT value FROM meta WHERE key = 'built_at'", [], |r| {
                let s: String = r.get(0)?;
                Ok(s.parse::<u64>().ok())
            })
            .unwrap_or(None);

        let build_duration_ms: Option<u64> = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'build_duration_ms'",
                [],
                |r| {
                    let s: String = r.get(0)?;
                    Ok(s.parse::<u64>().ok())
                },
            )
            .unwrap_or(None);

        let root_path = root.map(Self::canonical_path);
        let root_id: Option<i64> = if let Some(root) = &root_path {
            conn.query_row(
                "SELECT id FROM indexed_roots WHERE path = ?1",
                params![root.to_string_lossy()],
                |row| row.get(0),
            )
            .optional()?
        } else {
            None
        };
        let (indexed_files, total_chunks) = if let Some(root_id) = root_id {
            let indexed_files: usize = conn
                .query_row(
                    "SELECT COUNT(*) FROM root_files WHERE root_id = ?1",
                    params![root_id],
                    |r| r.get(0),
                )
                .map(|n: i64| n as usize)
                .unwrap_or(0);
            let total_chunks: usize = conn
                .query_row(
                    "SELECT COUNT(*)
                     FROM root_files rf
                     JOIN chunks c ON c.file_id = rf.file_id
                     WHERE rf.root_id = ?1",
                    params![root_id],
                    |r| r.get(0),
                )
                .map(|n: i64| n as usize)
                .unwrap_or(0);
            (indexed_files, total_chunks)
        } else if root_path.is_some() {
            (0, 0)
        } else {
            let indexed_files: usize = conn
                .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
                .map(|n: i64| n as usize)
                .unwrap_or(0);
            let total_chunks: usize = conn
                .query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))
                .map(|n: i64| n as usize)
                .unwrap_or(0);
            (indexed_files, total_chunks)
        };

        Ok(IndexStatus {
            indexed_files,
            total_chunks,
            built_at,
            build_duration_ms,
            engine,
            model_id,
            dimension,
            root_path,
            db_size_bytes,
        })
    }
    /// Delete the index from disk. Consumes `self` so it cannot be used after deletion.
    pub fn delete(self, data_dir: &Path) -> anyhow::Result<()> {
        drop(self.conn);
        let path = db_path(data_dir);
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        remove_sqlite_sidecars(&path);
        let backup = replacement_backup_path(data_dir);
        if backup.exists() {
            std::fs::remove_file(&backup)?;
        }
        remove_sqlite_sidecars(&backup);
        Ok(())
    }
}
