use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use rusqlite::{Connection, params};
use tracing::error;

use crate::extract::ExtractorRegistry;
use crate::types::{ByteRange, EmbeddingEngine, IndexStatus, SourceOrigin};

use super::Embedder;
use super::chunk::{Chunk, chunk_content};
use super::installer::{EmbedProgress, IndexBuildProgress, ProgressTx};

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
    #[allow(dead_code)]
    model_id: String,
    #[allow(dead_code)]
    dimension: usize,
}

impl SemanticIndex {
    /// Open an existing index. Returns `Err` if no index exists at `data_dir` or
    /// if `model_id`/`dimension` in the stored metadata mismatches the parameters.
    pub fn open(data_dir: &Path, model_id: &str, dimension: usize) -> anyhow::Result<Self> {
        let path = db_path(data_dir);
        anyhow::ensure!(path.exists(), "No semantic index found at {}", path.display());

        let conn = Connection::open(&path)
            .with_context(|| format!("Failed to open index at {}", path.display()))?;

        let _stored_engine: String = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'engine'",
                [],
                |row| row.get(0),
            )
            .unwrap_or_else(|_| "candle".to_string()); // Fallback for legacy indexes

        let stored_model_id: String = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'model_id'",
                [],
                |row| row.get(0),
            )
            .context("Index is missing model_id metadata")?;

        let stored_dimension: usize = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'dimension'",
                [],
                |row| {
                    let s: String = row.get(0)?;
                    Ok(s.parse::<usize>().unwrap_or(0))
                },
            )
            .context("Index is missing dimension metadata")?;

        anyhow::ensure!(
            stored_model_id == model_id,
            "Index was built with model '{}' but requested is '{}'; rebuild the index",
            stored_model_id,
            model_id
        );
        anyhow::ensure!(
            stored_dimension == dimension,
            "Index dimension {} does not match requested dimension {}; rebuild the index",
            stored_dimension,
            dimension
        );

        Ok(Self {
            conn,
            model_id: stored_model_id,
            dimension: stored_dimension,
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
        std::fs::create_dir_all(data_dir)?;
        let path = db_path(data_dir);
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        // Remove orphaned WAL/SHM files left by a previous unclean shutdown.
        // If the main DB was deleted but these remain, SQLite will try to replay
        // them into the new database and fail with a disk I/O error.
        let _ = std::fs::remove_file(data_dir.join("semantic_index.db-wal"));
        let _ = std::fs::remove_file(data_dir.join("semantic_index.db-shm"));

        let conn = Connection::open(&path)
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
        })
    }

    /// Full build: creates the database at `data_dir`, indexes every path, and
    /// returns the open index.
    pub fn build(
        data_dir: &Path,
        paths: &[PathBuf],
        extractors: &ExtractorRegistry,
        embedder: &dyn Embedder,
        engine: EmbeddingEngine,
        tx: ProgressTx,
        chunk_size: usize,
        chunk_overlap: usize,
    ) -> anyhow::Result<Self> {
        let start_time = std::time::Instant::now();
        let total_files = paths.len();
        let mut idx = Self::create(data_dir, embedder.model_id(), embedder.dimension(), engine)?;

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

        let _ = tx.blocking_send(EmbedProgress::Build(IndexBuildProgress {
            files_processed: total_files,
            total_files,
            message: "Done!".to_string(),
            done: true,
        }));

        Ok(idx)
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
            CREATE TABLE IF NOT EXISTS chunk_vectors (
                chunk_id  INTEGER PRIMARY KEY REFERENCES chunks(id) ON DELETE CASCADE,
                embedding BLOB    NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_chunks_file_path ON chunks(file_path);
            PRAGMA foreign_keys = ON;
            ",
        )?;

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
                    text,
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
                "INSERT INTO chunk_vectors (chunk_id, embedding) VALUES (?1, ?2)",
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
            "DELETE FROM chunks WHERE file_path = ?1",
            params![path_str],
        )?;
        Ok(())
    }

    /// Query the index for the top-k nearest neighbours to `embedding`.
    /// Uses cosine similarity computed in Rust (O(n) over all stored vectors).
    pub fn query(&self, embedding: &[f32], top_k: usize) -> anyhow::Result<Vec<IndexedChunk>> {
        // Load all chunk ids + vectors.
        let mut stmt = self.conn.prepare(
            "SELECT cv.chunk_id, cv.embedding, c.file_path, c.byte_start, c.byte_end,
                    c.origin_json, c.chunk_text
             FROM chunk_vectors cv
             JOIN chunks c ON c.id = cv.chunk_id",
        )?;

        let mut scored: Vec<(f32, IndexedChunk)> = stmt
            .query_map([], |row| {
                let chunk_id: i64 = row.get(0)?;
                let blob: Vec<u8> = row.get(1)?;
                let file_path: String = row.get(2)?;
                let byte_start: i64 = row.get(3)?;
                let byte_end: i64 = row.get(4)?;
                let origin_json: String = row.get(5)?;
                let chunk_text: String = row.get(6)?;
                Ok((chunk_id, blob, file_path, byte_start, byte_end, origin_json, chunk_text))
            })?
            .filter_map(|r| r.ok())
            .filter_map(|(_, blob, file_path, byte_start, byte_end, origin_json, chunk_text)| {
                let stored = bytes_to_f32_vec(&blob);
                let score = cosine_similarity(embedding, &stored);
                let origin: SourceOrigin = serde_json::from_str(&origin_json).ok()?;
                Some((
                    score,
                    IndexedChunk {
                        file_path: PathBuf::from(file_path),
                        chunk_text,
                        extraction_byte_range: ByteRange {
                            start: byte_start as usize,
                            end: byte_end as usize,
                        },
                        origin,
                        score,
                    },
                ))
            })
            .collect();

        scored.retain(|(score, _)| *score >= 0.2);
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(top_k);

        Ok(scored.into_iter().map(|(_, c)| c).collect())
    }

    /// Read index metadata without re-validating model_id/dimension.
    pub fn status(&self) -> IndexStatus {
        let engine_str: String = self
            .conn
            .query_row("SELECT value FROM meta WHERE key = 'engine'", [], |r| r.get(0))
            .unwrap_or_else(|_| "candle".to_string());
        
        let engine = match engine_str.as_str() {
            "python" => EmbeddingEngine::Python,
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

        IndexStatus {
            indexed_files,
            total_chunks,
            built_at,
            build_duration_ms,
            engine,
            model_id,
            dimension,
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
            "python" => EmbeddingEngine::Python,
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

        Ok(IndexStatus {
            indexed_files,
            total_chunks,
            built_at,
            build_duration_ms,
            engine,
            model_id,
            dimension,
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

fn bytes_to_f32_vec(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}
