use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH, Instant};

use anyhow::Context;
use rusqlite::{Connection, params};
use tracing::error;

use crate::extract::ExtractorRegistry;
use crate::types::{ByteRange, EmbeddingEngine, IndexStatus, SourceOrigin};

use super::Embedder;
use super::chunk::{Chunk, chunk_content};
use super::installer::{EmbedProgress, IndexBuildProgress, ProgressTx};

// ── sqlite-vec extension loading ──────────────────────────────────────────────

fn load_sqlite_vec() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe {
        // sqlite3_vec_init is declared as fn() but sqlite3_auto_extension expects
        // the full 3-argument extension init signature. transmute bridges the gap;
        // this is the canonical pattern shown in the sqlite-vec crate's own tests.
        rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute(
            sqlite_vec::sqlite3_vec_init as *const (),
        )));
    });
}

// ── File path of the SQLite DB ────────────────────────────────────────────────

fn db_path(data_dir: &Path) -> PathBuf {
    data_dir.join("semantic_index.db")
}

// ── Prepared file (ready to write) ───────────────────────────────────────────

pub struct PreparedFile {
    pub path: PathBuf,
    /// Pairs of (chunk metadata, embedding vector), ready to write.
    pub chunks: Vec<(Chunk, Vec<f32>)>,
}

// ── Indexed chunk (query result) ──────────────────────────────────────────────

pub struct IndexedChunk {
    pub file_path: PathBuf,
    pub chunk_text: String,
    /// Byte range into `ExtractedContent.text`.
    pub extraction_byte_range: ByteRange,
    pub origin: SourceOrigin,
    pub score: f32,
}

// ── SemanticIndex ─────────────────────────────────────────────────────────────

pub struct SemanticIndex {
    conn: Connection,
    model_id: String,
    dimension: usize,
    root_path: Option<PathBuf>,
}

impl SemanticIndex {
    /// Open an existing index. Returns `Err` if no index exists at `data_dir` or
    /// if `model_id` in the stored metadata mismatches the parameter.
    /// The dimension is read from the DB; callers can inspect it via `status()`.
    pub fn open(data_dir: &Path, model_id: &str, expected_dimension: usize) -> anyhow::Result<Self> {
        load_sqlite_vec();

        let path = db_path(data_dir);
        anyhow::ensure!(path.exists(), "No semantic index found at {}", path.display());

        let conn = Connection::open(&path)
            .with_context(|| format!("Failed to open index at {}", path.display()))?;

        // Require the sqlite-vec schema. A missing vec_chunks table means the index
        // was built before this migration; the caller should rebuild.
        let has_vec_table: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='vec_chunks'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n > 0)
            .unwrap_or(false);
        anyhow::ensure!(
            has_vec_table,
            "Index uses legacy schema (no vec_chunks table); rebuild the index"
        );

        let stored_model_id: String = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'model_id'",
                [],
                |row| row.get(0),
            )
            .context("Index is missing model_id metadata")?;

        anyhow::ensure!(
            stored_model_id == model_id,
            "Index was built with model '{}' but requested is '{}'; rebuild the index",
            stored_model_id,
            model_id
        );

        let stored_dimension: usize = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'dimension'",
                [],
                |row| {
                    let s: String = row.get(0)?;
                    Ok(s.parse::<usize>().unwrap_or(0))
                },
            )
            .unwrap_or(0);

        anyhow::ensure!(
            stored_dimension == expected_dimension,
            "Index dimension mismatch: stored={}, expected={}. Rebuild the index.",
            stored_dimension,
            expected_dimension
        );

        let root_path: Option<PathBuf> = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'root_path'",
                [],
                |row| {
                    let s: String = row.get(0)?;
                    Ok(Some(PathBuf::from(s)))
                },
            )
            .unwrap_or(None);

        Ok(Self {
            conn,
            model_id: stored_model_id,
            dimension: stored_dimension,
            root_path,
        })
    }

    /// Create a new empty index at the specified path.
    pub fn create_at_path(
        path: &Path,
        model_id: &str,
        dimension: usize,
        engine: EmbeddingEngine,
    ) -> anyhow::Result<Self> {
        load_sqlite_vec();

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        
        if path.exists() {
            std::fs::remove_file(path)?;
        }

        // Remove orphaned WAL/SHM files if any.
        let data_dir = path.parent().unwrap_or(Path::new("."));
        let _ = std::fs::remove_file(data_dir.join("semantic_index.db-wal"));
        let _ = std::fs::remove_file(data_dir.join("semantic_index.db-shm"));

        let conn = Connection::open(path)
            .with_context(|| format!("Failed to create index at {}", path.display()))?;

        Self::create_schema(&conn, model_id, dimension, engine)?;

        let built_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        conn.execute(
            "INSERT INTO meta (key, value) VALUES ('built_at', ?1)",
            params![built_at.to_string()],
        )?;

        Ok(Self {
            conn,
            model_id: model_id.to_string(),
            dimension,
            root_path: None,
        })
    }

    /// Create a new empty index at `data_dir` (schema only, no files indexed).
    /// Removes any existing index at that path.
    pub fn create(
        data_dir: &Path,
        model_id: &str,
        dimension: usize,
        engine: EmbeddingEngine,
    ) -> anyhow::Result<Self> {
        Self::create_at_path(&db_path(data_dir), model_id, dimension, engine)
    }

    /// Full build: creates the database at `data_dir`, indexes every path, and
    /// returns the open index.
    pub fn build(
        data_dir: &Path,
        root_path: &Path,
        paths: &[PathBuf],
        extractors: &ExtractorRegistry,
        embedder: &dyn Embedder,
        engine: EmbeddingEngine,
        tx: ProgressTx,
        chunk_size: usize,
        chunk_overlap: usize,
    ) -> anyhow::Result<Self> {
        let start_time = Instant::now();
        let total_files = paths.len();

        let final_path = db_path(data_dir);
        let tmp_path = data_dir.join("semantic_index.db.tmp");

        let mut idx = Self::create_at_path(&tmp_path, embedder.model_id(), embedder.dimension(), engine)?;

        // Phase 1: extract and chunk all files (0% to 30% progress).
        let mut file_chunks: Vec<(PathBuf, Vec<Chunk>)> = Vec::new();
        for (i, path) in paths.iter().enumerate() {
            let _ = tx.blocking_send(EmbedProgress::Build(IndexBuildProgress {
                files_processed: i,
                total_files,
                message: format!("Extracting {} of {}...", i + 1, total_files),
                done: false,
            }));

            match Self::extract_chunks(path, extractors, chunk_size, chunk_overlap) {
                Ok(chunks) if !chunks.is_empty() => file_chunks.push((path.clone(), chunks)),
                _ => {}
            }
        }

        // Phase 2: embed and write per file to bound peak memory usage.
        // Embedding all files at once would hold all chunk texts + all embedding
        // vectors in memory simultaneously; per-file processing keeps peak at
        // (one file's chunks + embeddings) on top of the model weights.
        let total_extracted = file_chunks.len();
        for (i, (path, chunks)) in file_chunks.into_iter().enumerate() {
            let _ = tx.blocking_send(EmbedProgress::Build(IndexBuildProgress {
                files_processed: total_files / 3 + (i * total_files * 2 / (3 * total_extracted.max(1))),
                total_files,
                message: format!("Embedding {} of {}...", i + 1, total_extracted),
                done: false,
            }));

            let texts: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();
            let embeddings = embedder.embed_passages(&texts)?;

            let prepared = PreparedFile {
                path: path.clone(),
                chunks: chunks.into_iter().zip(embeddings).collect(),
            };
            if let Err(e) = idx.write_file(prepared) {
                error!("Failed to write {}: {e:#}", path.display());
            }
        }

        let duration_ms = start_time.elapsed().as_millis() as u64;
        idx.conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('build_duration_ms', ?1)",
            params![duration_ms.to_string()],
        )?;
        idx.conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('root_path', ?1)",
            params![root_path.to_string_lossy()],
        )?;

        // Success! Close connection and rename.
        let model_id = idx.model_id.clone();
        let dimension = idx.dimension;
        drop(idx);

        // Remove old files if they exist to avoid rename errors on some systems.
        let _ = std::fs::remove_file(&final_path);
        let _ = std::fs::remove_file(data_dir.join("semantic_index.db-wal"));
        let _ = std::fs::remove_file(data_dir.join("semantic_index.db-shm"));

        std::fs::rename(&tmp_path, &final_path)
            .with_context(|| format!("Failed to rename {} to {}", tmp_path.display(), final_path.display()))?;

        let _ = tx.blocking_send(EmbedProgress::Build(IndexBuildProgress {
            files_processed: total_files,
            total_files,
            message: "Done!".to_string(),
            done: true,
        }));

        // Reopen at final path
        Self::open(data_dir, &model_id, dimension)
    }

    fn create_schema(
        conn: &Connection,
        model_id: &str,
        dimension: usize,
        engine: EmbeddingEngine,
    ) -> anyhow::Result<()> {
        conn.execute_batch(
            "
            PRAGMA journal_mode = WAL;
            CREATE TABLE IF NOT EXISTS meta (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS chunks (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                file_path   TEXT    NOT NULL,
                chunk_idx   INTEGER NOT NULL,
                byte_start  INTEGER NOT NULL,
                byte_end    INTEGER NOT NULL,
                origin_json TEXT    NOT NULL,
                chunk_text  TEXT    NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_chunks_file_path ON chunks(file_path);
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
        Ok(())
    }

    /// Extract and chunk a file without embedding. Use this to collect chunks
    /// from many files before embedding them all in a single batch.
    pub fn extract_chunks(
        path: &Path,
        extractors: &ExtractorRegistry,
        chunk_size: usize,
        chunk_overlap: usize,
    ) -> anyhow::Result<Vec<Chunk>> {
        let content = match extractors.find(path, None) {
            Some(ext) => ext.extract(path)?,
            None => {
                // Plain-text fallback: read raw bytes.
                let text = std::fs::read_to_string(path)
                    .with_context(|| format!("Failed to read {}", path.display()))?;
                crate::types::ExtractedContent {
                    text: text.clone(),
                    source_map: crate::types::SourceMap { segments: Vec::new() },
                    metadata: crate::types::FileMetadata {
                        path: path.to_path_buf(),
                        size_bytes: 0,
                        mime: None,
                        title: None,
                        page_count: None,
                    },
                }
            }
        };
        Ok(chunk_content(&content, path.to_path_buf(), chunk_size, chunk_overlap))
    }

    /// Extract, chunk, and embed a file without holding the index lock.
    pub fn prepare_file(
        path: &Path,
        extractors: &ExtractorRegistry,
        embedder: &dyn Embedder,
        chunk_size: usize,
        chunk_overlap: usize,
    ) -> anyhow::Result<PreparedFile> {
        let raw_chunks = Self::extract_chunks(path, extractors, chunk_size, chunk_overlap)?;
        if raw_chunks.is_empty() {
            return Ok(PreparedFile { path: path.to_path_buf(), chunks: Vec::new() });
        }

        let texts: Vec<&str> = raw_chunks.iter().map(|c| c.text.as_str()).collect();
        let embeddings = embedder.embed_passages(&texts)?;

        anyhow::ensure!(
            embeddings.len() == raw_chunks.len(),
            "Embedder returned {} embeddings for {} chunks",
            embeddings.len(),
            raw_chunks.len()
        );

        let chunks = raw_chunks.into_iter().zip(embeddings).collect();
        Ok(PreparedFile { path: path.to_path_buf(), chunks })
    }

    /// Write previously prepared chunks into the index, removing any existing chunks
    /// for that path first.
    pub fn write_file(&mut self, prepared: PreparedFile) -> anyhow::Result<()> {
        let path_str = prepared.path.to_string_lossy();

        // Validate dimensions before starting transaction.
        for (_, embedding) in &prepared.chunks {
            anyhow::ensure!(
                embedding.len() == self.dimension,
                "Dimension mismatch: expected {}, received {} for path {}",
                self.dimension,
                embedding.len(),
                path_str
            );
        }

        // Delete vectors first (vec0 has no FK cascade), then the chunk rows.
        self.conn.execute(
            "DELETE FROM vec_chunks WHERE rowid IN (SELECT id FROM chunks WHERE file_path = ?1)",
            params![path_str],
        )?;
        self.conn.execute(
            "DELETE FROM chunks WHERE file_path = ?1",
            params![path_str],
        )?;

        let tx = self.conn.transaction()?;
        for (i, (chunk, embedding)) in prepared.chunks.into_iter().enumerate() {
            let origin_json = serde_json::to_string(&chunk.origin)?;
            tx.execute(
                "INSERT INTO chunks (file_path, chunk_idx, byte_start, byte_end, origin_json, chunk_text)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    path_str,
                    i as i64,
                    chunk.byte_range.start as i64,
                    chunk.byte_range.end as i64,
                    origin_json,
                    chunk.text,
                ],
            )?;
            let chunk_id = tx.last_insert_rowid();
            let blob = f32_slice_to_bytes(&embedding);
            tx.execute(
                "INSERT INTO vec_chunks(rowid, embedding) VALUES (?1, ?2)",
                params![chunk_id, blob],
            )?;
        }
        tx.commit()?;
        Ok(())
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
        let prepared = Self::prepare_file(path, extractors, embedder, chunk_size, chunk_overlap)?;
        self.write_file(prepared)
    }

    /// Remove all chunks for the given path.
    pub fn remove_file(&mut self, path: &Path) -> anyhow::Result<()> {
        let path_str = path.to_string_lossy();
        self.conn.execute(
            "DELETE FROM vec_chunks WHERE rowid IN (SELECT id FROM chunks WHERE file_path = ?1)",
            params![path_str],
        )?;
        self.conn.execute(
            "DELETE FROM chunks WHERE file_path = ?1",
            params![path_str],
        )?;
        Ok(())
    }

    /// Query the index for the top-k nearest neighbours to `embedding`.
    /// Uses cosine similarity computed in Rust (O(n) over all stored vectors).
    pub fn query(&self, embedding: &[f32], top_k: usize) -> anyhow::Result<Vec<IndexedChunk>> {
        // cosine distance = 1 - cosine_similarity.
        // No hard threshold: top_k already bounds the result count, and a fixed
        // distance cutoff is model-dependent (short queries on MiniLM-style models
        // produce distances of 0.7–0.85 even for clearly relevant chunks).

        let stored_count: i64 = self.conn
            .query_row("SELECT COUNT(*) FROM vec_chunks", [], |r| r.get(0))
            .unwrap_or(0);
        tracing::info!("[query] vec_chunks rows={stored_count}, embedding_dim={}, top_k={top_k}", embedding.len());

        let blob = f32_slice_to_bytes(embedding);

        let mut stmt = self.conn.prepare(
            "SELECT v.rowid, v.distance, c.file_path, c.byte_start, c.byte_end,
                    c.origin_json, c.chunk_text
             FROM vec_chunks v
             JOIN chunks c ON c.id = v.rowid
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
                let origin_json: String = row.get(5)?;
                let chunk_text: String = row.get(6)?;
                Ok((distance, file_path, byte_start, byte_end, origin_json, chunk_text))
            })?
            .map(|r| match r {
                Ok(v) => Some(v),
                Err(e) => { error!("[query] row error: {e}"); None }
            })
            .collect();

        tracing::info!("[query] sqlite-vec returned {} rows ({} errors)", raw_rows.iter().filter(|r| r.is_some()).count(), raw_rows.iter().filter(|r| r.is_none()).count());

        let results: Vec<IndexedChunk> = raw_rows.into_iter()
            .flatten()
            .inspect(|(distance, file_path, ..)| {
                // tracing::info!("[query] candidate: distance={distance:.4}, file={file_path}");
            })
            .filter_map(|(distance, file_path, byte_start, byte_end, origin_json, chunk_text)| {
                let score = 1.0 - distance;
                let origin: SourceOrigin = serde_json::from_str(&origin_json)
                    .map_err(|e| error!("[query] origin_json parse error for {file_path}: {e}"))
                    .ok()?;
                Some(IndexedChunk {
                    file_path: PathBuf::from(file_path),
                    chunk_text,
                    extraction_byte_range: ByteRange {
                        start: byte_start as usize,
                        end: byte_end as usize,
                    },
                    origin,
                    score,
                })
            })
            .collect();

        tracing::info!("[query] returning {} results", results.len());
        Ok(results)
    }

    /// Read index metadata without re-validating model_id/dimension.
    pub fn status(&self) -> IndexStatus {
        let engine_str: String = self
            .conn
            .query_row("SELECT value FROM meta WHERE key = 'engine'", [], |r| r.get(0))
            .unwrap_or_else(|_| "candle".to_string());
        
        let engine = match engine_str.as_str() {
            "sbert" | "python" => EmbeddingEngine::SBERT,
            "fastembed" => EmbeddingEngine::Fastembed,
            _ => EmbeddingEngine::Candle,
        };

        let model_id = self
            .conn
            .query_row("SELECT value FROM meta WHERE key = 'model_id'", [], |r| r.get(0))
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
            .query_row("SELECT value FROM meta WHERE key = 'build_duration_ms'", [], |r| {
                let s: String = r.get(0)?;
                Ok(s.parse::<u64>().ok())
            })
            .unwrap_or(None);

        let indexed_files: usize = self
            .conn
            .query_row("SELECT COUNT(DISTINCT file_path) FROM chunks", [], |r| r.get(0))
            .map(|n: i64| n as usize)
            .unwrap_or(0);

        let total_chunks: usize = self
            .conn
            .query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))
            .map(|n: i64| n as usize)
            .unwrap_or(0);

        let root_path: Option<PathBuf> = self
            .conn
            .query_row("SELECT value FROM meta WHERE key = 'root_path'", [], |r| {
                let s: String = r.get(0)?;
                Ok(Some(PathBuf::from(s)))
            })
            .unwrap_or(None);

        IndexStatus {
            indexed_files,
            total_chunks,
            built_at,
            build_duration_ms,
            engine,
            model_id,
            dimension,
            root_path,
        }
    }

    /// Read index status directly from the DB file without opening a full SemanticIndex.
    /// Does not validate model_id/dimension against any embedder.
    pub fn read_status_from_path(data_dir: &Path) -> anyhow::Result<IndexStatus> {
        let path = db_path(data_dir);
        anyhow::ensure!(path.exists(), "No semantic index found");
        let conn = Connection::open(&path)?;

        let engine_str: String = conn
            .query_row("SELECT value FROM meta WHERE key = 'engine'", [], |r| r.get(0))
            .unwrap_or_else(|_| "candle".to_string());

        let engine = match engine_str.as_str() {
            "sbert" | "python" => EmbeddingEngine::SBERT,
            "fastembed" => EmbeddingEngine::Fastembed,
            _ => EmbeddingEngine::Candle,
        };

        let model_id: String = conn
            .query_row("SELECT value FROM meta WHERE key = 'model_id'", [], |r| r.get(0))
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
            .query_row("SELECT value FROM meta WHERE key = 'build_duration_ms'", [], |r| {
                let s: String = r.get(0)?;
                Ok(s.parse::<u64>().ok())
            })
            .unwrap_or(None);

        let indexed_files: usize = conn
            .query_row("SELECT COUNT(DISTINCT file_path) FROM chunks", [], |r| r.get(0))
            .map(|n: i64| n as usize)
            .unwrap_or(0);

        let total_chunks: usize = conn
            .query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))
            .map(|n: i64| n as usize)
            .unwrap_or(0);

        let root_path: Option<PathBuf> = conn
            .query_row("SELECT value FROM meta WHERE key = 'root_path'", [], |r| {
                let s: String = r.get(0)?;
                Ok(Some(PathBuf::from(s)))
            })
            .unwrap_or(None);

        Ok(IndexStatus {
            indexed_files,
            total_chunks,
            built_at,
            build_duration_ms,
            engine,
            model_id,
            dimension,
            root_path,
        })
    }

    /// Delete the index from disk. Consumes `self` so it cannot be used after deletion.
    pub fn delete(self, data_dir: &Path) -> anyhow::Result<()> {
        drop(self.conn);
        let path = db_path(data_dir);
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        Ok(())
    }
}

// ── Vector utilities ──────────────────────────────────────────────────────────

fn f32_slice_to_bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}


