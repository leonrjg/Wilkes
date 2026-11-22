//! Schema creation and the migrations that bring an older index up to
//! `SCHEMA_VERSION`.

use super::*;

impl SemanticIndex {
    pub(super) fn create_schema(
        conn: &Connection,
        identity: &EmbeddingSpaceIdentity,
    ) -> anyhow::Result<()> {
        let model_id = &identity.model_id;
        let dimension = identity.dimension;
        let engine = identity.engine;
        conn.execute_batch(
            "
            PRAGMA journal_mode = WAL;
            CREATE TABLE IF NOT EXISTS meta (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS files (
                id             INTEGER PRIMARY KEY AUTOINCREMENT,
                file_path      TEXT    NOT NULL UNIQUE,
                size_bytes     INTEGER NOT NULL,
                modified_at_ms INTEGER NOT NULL,
                indexed_at_ms  INTEGER NOT NULL,
                full_text      TEXT,
                source_sha256  TEXT,
                snapshot_id    TEXT,
                extraction_recipe_id TEXT,
                rendition_id   TEXT,
                extracted_content_sha256 TEXT,
                embedding_reused_chunks INTEGER,
                embedding_computed_chunks INTEGER,
                managed_snapshot_relative_path TEXT,
                original_source_provenance_json TEXT,
                admission_state TEXT,
                retained_extraction_json TEXT
            );
            CREATE TABLE IF NOT EXISTS indexed_roots (
                id                  INTEGER PRIMARY KEY AUTOINCREMENT,
                path                TEXT NOT NULL UNIQUE,
                built_at            INTEGER,
                last_reconciled_at  INTEGER
            );
            CREATE TABLE IF NOT EXISTS root_files (
                root_id    INTEGER NOT NULL REFERENCES indexed_roots(id) ON DELETE CASCADE,
                file_id    INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                seen_at_ms INTEGER NOT NULL,
                PRIMARY KEY(root_id, file_id)
            );
            CREATE TABLE IF NOT EXISTS managed_import_keys (
                idempotency_key TEXT PRIMARY KEY,
                source_sha256 TEXT NOT NULL,
                extraction_recipe_id TEXT NOT NULL,
                rendition_id TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS chunks (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                file_id     INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                chunk_idx   INTEGER NOT NULL,
                byte_start  INTEGER NOT NULL,
                byte_end    INTEGER NOT NULL,
                origin_type TEXT    NOT NULL,
                page        INTEGER,
                line        INTEGER,
                col         INTEGER,
                bbox_x      REAL,
                bbox_y      REAL,
                bbox_w      REAL,
                bbox_h      REAL,
                chunk_text  TEXT    NOT NULL,
                chunk_ref   TEXT,
                text_sha256 TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_files_identity
                ON files(size_bytes, modified_at_ms);
            CREATE INDEX IF NOT EXISTS idx_chunks_file_id ON chunks(file_id);
            CREATE INDEX IF NOT EXISTS idx_chunks_text_sha256
                ON chunks(text_sha256) WHERE text_sha256 IS NOT NULL;
            CREATE INDEX IF NOT EXISTS idx_files_rendition_id
                ON files(rendition_id) WHERE rendition_id IS NOT NULL;
            CREATE INDEX IF NOT EXISTS idx_chunks_chunk_ref
                ON chunks(chunk_ref) WHERE chunk_ref IS NOT NULL;
            CREATE INDEX IF NOT EXISTS idx_root_files_file_id ON root_files(file_id);
            PRAGMA foreign_keys = ON;
            ",
        )?;

        // vec0 DDL requires the dimension to be a literal in the column type, so
        // it cannot be parameterised and must be interpolated as a string.
        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE IF NOT EXISTS vec_chunks \
             USING vec0(embedding float[{dimension}] distance_metric=cosine);"
        ))?;

        conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('engine', ?1)",
            params![engine.as_str()],
        )?;
        conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('model_id', ?1)",
            params![model_id],
        )?;
        conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('dimension', ?1)",
            params![dimension.to_string()],
        )?;
        conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('index_embedding_metadata_json', ?1)",
            params![serde_json::to_string(&IndexEmbeddingMetadata::exact(
                identity.clone()
            ))?],
        )?;
        conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('schema_version', ?1)",
            params![SCHEMA_VERSION.to_string()],
        )?;
        Ok(())
    }
    pub(super) fn migrate_v2_to_v3(conn: &Connection) -> anyhow::Result<()> {
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS indexed_roots (
                id                  INTEGER PRIMARY KEY AUTOINCREMENT,
                path                TEXT NOT NULL UNIQUE,
                built_at            INTEGER,
                last_reconciled_at  INTEGER
            );
            CREATE TABLE IF NOT EXISTS root_files (
                root_id    INTEGER NOT NULL REFERENCES indexed_roots(id) ON DELETE CASCADE,
                file_id    INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                seen_at_ms INTEGER NOT NULL,
                PRIMARY KEY(root_id, file_id)
            );
            CREATE INDEX IF NOT EXISTS idx_root_files_file_id ON root_files(file_id);
            ",
        )?;

        let root: Option<PathBuf> = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'root_canonical_path'",
                [],
                |row| {
                    let s: String = row.get(0)?;
                    Ok(PathBuf::from(s))
                },
            )
            .or_else(|_| {
                conn.query_row(
                    "SELECT value FROM meta WHERE key = 'root_path'",
                    [],
                    |row| {
                        let s: String = row.get(0)?;
                        Ok(PathBuf::from(s))
                    },
                )
            })
            .ok();

        if let Some(root) = root {
            let root = std::fs::canonicalize(&root).unwrap_or(root);
            let root_str = root.to_string_lossy().into_owned();
            let now = Self::now_ms();
            conn.execute(
                "INSERT OR IGNORE INTO indexed_roots (path, built_at, last_reconciled_at)
                 VALUES (?1, ?2, ?2)",
                params![root_str, now],
            )?;
            let root_id: i64 = conn.query_row(
                "SELECT id FROM indexed_roots WHERE path = ?1",
                params![root.to_string_lossy()],
                |row| row.get(0),
            )?;

            let mut stmt = conn.prepare("SELECT id, file_path FROM files")?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            for (file_id, stored_path) in rows {
                let path = PathBuf::from(&stored_path);
                let canonical = if path.is_absolute() {
                    std::fs::canonicalize(&path).unwrap_or(path)
                } else {
                    let joined = root.join(path);
                    std::fs::canonicalize(&joined).unwrap_or(joined)
                };
                let canonical_str = canonical.to_string_lossy().into_owned();
                conn.execute(
                    "UPDATE files SET file_path = ?1 WHERE id = ?2",
                    params![canonical_str, file_id],
                )?;
                conn.execute(
                    "INSERT OR IGNORE INTO root_files (root_id, file_id, seen_at_ms)
                     VALUES (?1, ?2, ?3)",
                    params![root_id, file_id, now],
                )?;
            }
        }

        // Stamp the concrete version this step produces, not the crate's current
        // SCHEMA_VERSION: a v2 index must still pass through the v3->v4 step.
        conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('schema_version', '3')",
            [],
        )?;
        Ok(())
    }
    /// v3 -> v4: add the `files.full_text` column that lets exact (grep) search
    /// read a document's text from the index instead of re-extracting it.
    /// Existing rows keep `full_text = NULL` and are backfilled the next time the
    /// file is rebuilt or reindexed; until then grep falls back to live extraction.
    pub(super) fn migrate_v3_to_v4(conn: &Connection) -> anyhow::Result<()> {
        let has_column: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('files') WHERE name = 'full_text'")?
            .exists([])?;
        if !has_column {
            conn.execute_batch("ALTER TABLE files ADD COLUMN full_text TEXT;")?;
        }
        conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('schema_version', '4')",
            [],
        )?;
        Ok(())
    }
    /// v4 -> v5: repair rows whose missing full text was incorrectly coerced
    /// from NULL to an empty string while their chunks were reused. A genuinely
    /// empty extracted document cannot have chunks.
    pub(super) fn migrate_v4_to_v5(conn: &Connection) -> anyhow::Result<()> {
        conn.execute(
            "UPDATE files
             SET full_text = NULL
             WHERE full_text = ''
               AND EXISTS (SELECT 1 FROM chunks WHERE chunks.file_id = files.id)",
            [],
        )?;
        conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('schema_version', '5')",
            [],
        )?;
        Ok(())
    }
    /// v5 -> v6: introduce durable semantic identities. Existing file/chunk
    /// rows deliberately remain unidentified: the historical index never
    /// recorded enough extraction facts to prove a rendition, so treating
    /// them as reusable would turn missing evidence into a compatibility hit.
    pub(super) fn migrate_v5_to_v6(conn: &Connection) -> anyhow::Result<()> {
        fn has_column(conn: &Connection, table: &str, column: &str) -> anyhow::Result<bool> {
            let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
            let names = stmt
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(names.iter().any(|name| name == column))
        }
        for (table, column) in [
            ("files", "source_sha256"),
            ("files", "snapshot_id"),
            ("files", "extraction_recipe_id"),
            ("files", "rendition_id"),
            ("files", "managed_snapshot_relative_path"),
            ("files", "original_source_provenance_json"),
            ("files", "admission_state"),
            ("chunks", "chunk_ref"),
            ("chunks", "text_sha256"),
        ] {
            if !has_column(conn, table, column)? {
                conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} TEXT;"))?;
            }
        }
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_files_rendition_id
                ON files(rendition_id) WHERE rendition_id IS NOT NULL;
             CREATE INDEX IF NOT EXISTS idx_chunks_chunk_ref
                ON chunks(chunk_ref) WHERE chunk_ref IS NOT NULL;",
        )?;

        let engine: String =
            conn.query_row("SELECT value FROM meta WHERE key = 'engine'", [], |row| {
                row.get(0)
            })?;
        let engine = match engine.as_str() {
            "sbert" | "python" => EmbeddingEngine::SBERT,
            "fastembed" => EmbeddingEngine::Fastembed,
            _ => EmbeddingEngine::Candle,
        };
        let model_id: String =
            conn.query_row("SELECT value FROM meta WHERE key = 'model_id'", [], |row| {
                row.get(0)
            })?;
        let dimension: usize = conn.query_row(
            "SELECT value FROM meta WHERE key = 'dimension'",
            [],
            |row| {
                let value: String = row.get(0)?;
                Ok(value.parse().unwrap_or(0))
            },
        )?;
        let identity = EmbeddingSpaceIdentity::for_runtime(engine, &model_id, dimension);
        conn.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES ('embedding_space_id', ?1)",
            params![identity.id().0],
        )?;
        conn.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES ('embedding_space_identity_json', ?1)",
            params![serde_json::to_string(&identity)?],
        )?;
        conn.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES ('identity_schema_version', ?1)",
            params![identity.identity_schema_version.to_string()],
        )?;
        conn.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES ('schema_version', '6')",
            [],
        )?;
        Ok(())
    }
    /// v6 -> v7: retain a digest of the extracted full text so a managed
    /// export can prove that its replay body is the one Wilkes admitted.
    pub(super) fn migrate_v6_to_v7(conn: &Connection) -> anyhow::Result<()> {
        let mut stmt = conn.prepare("PRAGMA table_info(files)")?;
        let columns = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        if !columns
            .iter()
            .any(|column| column == "extracted_content_sha256")
        {
            conn.execute_batch("ALTER TABLE files ADD COLUMN extracted_content_sha256 TEXT;")?;
        }
        let rows = {
            let mut stmt = conn.prepare(
                "SELECT id, full_text FROM files
                 WHERE source_sha256 IS NOT NULL AND full_text IS NOT NULL
                   AND extracted_content_sha256 IS NULL",
            )?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };
        for (file_id, full_text) in rows {
            conn.execute(
                "UPDATE files SET extracted_content_sha256 = ?2 WHERE id = ?1",
                params![file_id, sha256_bytes(full_text.as_bytes())],
            )?;
        }
        conn.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES ('schema_version', '7')",
            [],
        )?;
        Ok(())
    }
    /// v7 -> v8: retain per-document import work diagnostics. These values do
    /// not affect identity; they explain whether the ready managed mapping was
    /// copied exactly or computed by the corpus embedder.
    pub(super) fn migrate_v7_to_v8(conn: &Connection) -> anyhow::Result<()> {
        let mut stmt = conn.prepare("PRAGMA table_info(files)")?;
        let columns = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        for column in ["embedding_reused_chunks", "embedding_computed_chunks"] {
            if !columns.iter().any(|existing| existing == column) {
                conn.execute_batch(&format!("ALTER TABLE files ADD COLUMN {column} INTEGER;"))?;
            }
        }
        conn.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES ('schema_version', '8')",
            [],
        )?;
        Ok(())
    }
    /// v8 -> v9: make a managed import job's retry identity durable rather
    /// than relying only on content deduplication.
    pub(super) fn migrate_v8_to_v9(conn: &Connection) -> anyhow::Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS managed_import_keys (
                idempotency_key TEXT PRIMARY KEY,
                source_sha256 TEXT NOT NULL,
                extraction_recipe_id TEXT NOT NULL,
                rendition_id TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL
            );",
        )?;
        conn.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES ('schema_version', '9')",
            [],
        )?;
        Ok(())
    }
    /// v9 -> v10: distinguish exact embedding evidence from the display tuple
    /// available in historical indexes. Earlier migrations synthesized an
    /// `unresolved-runtime` identity for legacy rows; that value is deliberately
    /// downgraded to `exact_identity: None` rather than being treated as proof.
    pub(super) fn migrate_v9_to_v10(conn: &Connection) -> anyhow::Result<()> {
        let engine_name: String =
            conn.query_row("SELECT value FROM meta WHERE key = 'engine'", [], |row| {
                row.get(0)
            })?;
        let engine = match engine_name.as_str() {
            "sbert" | "python" => EmbeddingEngine::SBERT,
            "fastembed" => EmbeddingEngine::Fastembed,
            _ => EmbeddingEngine::Candle,
        };
        let model_id: String =
            conn.query_row("SELECT value FROM meta WHERE key = 'model_id'", [], |row| {
                row.get(0)
            })?;
        let dimension: usize = conn.query_row(
            "SELECT value FROM meta WHERE key = 'dimension'",
            [],
            |row| {
                let value: String = row.get(0)?;
                Ok(value.parse().unwrap_or(0))
            },
        )?;

        let old_identity_json: Option<String> = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'embedding_space_identity_json'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let exact_identity = old_identity_json
            .map(|json| serde_json::from_str::<EmbeddingSpaceIdentity>(&json))
            .transpose()?;
        if let Some(identity) = exact_identity.as_ref() {
            anyhow::ensure!(
                identity.engine == engine
                    && identity.model_id == model_id
                    && identity.dimension == dimension,
                "Index embedding metadata contradicts its exact identity"
            );
            if let Some(stored_id) = conn
                .query_row(
                    "SELECT value FROM meta WHERE key = 'embedding_space_id'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
            {
                anyhow::ensure!(
                    identity.id().as_str() == stored_id,
                    "Index embedding-space identity is corrupt"
                );
            }
        }

        let metadata = IndexEmbeddingMetadata {
            engine,
            model_id,
            dimension,
            exact_identity,
        };
        metadata.validate()?;
        conn.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES ('index_embedding_metadata_json', ?1)",
            params![serde_json::to_string(&metadata)?],
        )?;
        conn.execute_batch(
            "DELETE FROM meta WHERE key IN (
                'embedding_space_id',
                'embedding_space_identity_json',
                'identity_schema_version'
             );",
        )?;
        conn.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES ('schema_version', '10')",
            [],
        )?;
        Ok(())
    }
    /// v10 -> v11: keep the page positions of the reading's own areas.
    ///
    /// Existing rows gain no regions and cannot: unlike `full_text`, these
    /// cannot be recovered from a stored column — they are what a recognizer
    /// read, and recovering them means running one. A document indexed before
    /// this migration therefore reads exactly as it did until it is next
    /// extracted, which is what enabling image analysis already does to every
    /// document with a picture in it.
    pub(super) fn migrate_v10_to_v11(conn: &Connection) -> anyhow::Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS reading_regions (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                file_id    INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                area_id    TEXT    NOT NULL,
                ordinal    INTEGER NOT NULL,
                page       INTEGER NOT NULL,
                bbox_x     REAL    NOT NULL,
                bbox_y     REAL    NOT NULL,
                bbox_w     REAL    NOT NULL,
                bbox_h     REAL    NOT NULL,
                byte_start INTEGER NOT NULL,
                byte_end   INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_reading_regions_file_id
                ON reading_regions(file_id);",
        )?;
        conn.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES ('schema_version', '11')",
            [],
        )?;
        Ok(())
    }
    /// v11 -> v12: retain the extraction as one object rather than one table
    /// per thing extracted.
    ///
    /// `reading_regions` was a materialized view of `ExtractedContent` —
    /// `serialize::reading_regions` computes it — and the image inventory this
    /// schema now also keeps would have been a second table of the same kind,
    /// with formula anchors a third. Each costs a migration, a write path, a
    /// delete and a copy in the adoption loop. One retained object costs a
    /// serde field.
    ///
    /// The rows are carried over rather than dropped: they are a rendition's
    /// page positions and cannot be recovered without running a recognizer
    /// again. What migrated rows cannot gain is the image inventory, which is
    /// why it is `None` here and not an empty list — a document indexed before
    /// this reads as *unknown*, and re-extracting is what fills it in.
    pub(super) fn migrate_v11_to_v12(conn: &Connection) -> anyhow::Result<()> {
        let has_column = conn
            .prepare("PRAGMA table_info(files)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?
            .iter()
            .any(|column| column == "retained_extraction_json");
        if !has_column {
            conn.execute_batch("ALTER TABLE files ADD COLUMN retained_extraction_json TEXT;")?;
        }

        let mut per_file: HashMap<i64, Vec<ReadingRegion>> = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT file_id, area_id, page, bbox_x, bbox_y, bbox_w, bbox_h,
                        byte_start, byte_end
                 FROM reading_regions ORDER BY file_id, ordinal",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    ReadingRegion {
                        area_id: row.get::<_, String>(1)?,
                        page: row.get::<_, i64>(2)? as u32,
                        bbox: BoundingBox {
                            x: row.get::<_, f64>(3)? as f32,
                            y: row.get::<_, f64>(4)? as f32,
                            width: row.get::<_, f64>(5)? as f32,
                            height: row.get::<_, f64>(6)? as f32,
                        },
                        text_range: ByteRange {
                            start: row.get::<_, i64>(7)? as usize,
                            end: row.get::<_, i64>(8)? as usize,
                        },
                    },
                ))
            })?;
            for row in rows {
                let (file_id, region) = row?;
                per_file.entry(file_id).or_default().push(region);
            }
        }

        for (file_id, reading_regions) in per_file {
            let retained = RetainedExtraction {
                version: RETAINED_EXTRACTION_VERSION.to_string(),
                reading_regions,
                images: None,
            };
            conn.execute(
                "UPDATE files SET retained_extraction_json = ?2 WHERE id = ?1",
                params![file_id, serde_json::to_string(&retained)?],
            )?;
        }

        conn.execute_batch("DROP TABLE reading_regions;")?;
        conn.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES ('schema_version', '12')",
            [],
        )?;
        Ok(())
    }
}
