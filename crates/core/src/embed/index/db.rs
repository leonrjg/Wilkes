use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use rusqlite::{params, Connection};
use tracing::error;

use crate::extract::ExtractorRegistry;
use crate::types::{
    BoundingBox, ByteRange, EmbeddingEngine, IndexStatus, IndexingConfig, SourceOrigin,
};

use super::super::models::installer::{EmbedProgress, IndexBuildProgress, ProgressTx};
use super::super::Embedder;
use super::chunk::{chunk_content, Chunk};

// ── sqlite-vec extension loading ──────────────────────────────────────────────

fn load_sqlite_vec() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe {
        // sqlite3_vec_init is declared as fn() but sqlite3_auto_extension expects
        // the full 3-argument extension init signature. transmute bridges the gap;
        // this is the canonical pattern shown in the sqlite-vec crate's own tests.
        rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
            *const (),
            unsafe extern "C" fn(
                *mut rusqlite::ffi::sqlite3,
                *mut *const std::ffi::c_char,
                *const rusqlite::ffi::sqlite3_api_routines,
            ) -> i32,
        >(
            sqlite_vec::sqlite3_vec_init as *const ()
        )));
    });
}

// ── File path of the SQLite DB ────────────────────────────────────────────────

fn db_path(data_dir: &Path) -> PathBuf {
    data_dir.join("semantic_index.db")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_db_path() {
        let p = db_path(Path::new("/tmp/data"));
        assert_eq!(p, PathBuf::from("/tmp/data/semantic_index.db"));
    }

    #[test]
    fn test_status_default() {
        let dir = tempfile::tempdir().unwrap();
        let conn = Connection::open(db_path(dir.path())).unwrap();

        conn.execute_batch(
            "
            CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT);
            INSERT INTO meta VALUES ('model_id', 'test-model');
            INSERT INTO meta VALUES ('dimension', '128');
            INSERT INTO meta VALUES ('engine', 'fastembed');
            CREATE TABLE vec_chunks (id INTEGER PRIMARY KEY);
        ",
        )
        .unwrap();

        let index = SemanticIndex {
            conn,
            model_id: "test-model".to_string(),
            dimension: 128,
            root_path: None,
        };

        let status = index.status();
        assert_eq!(status.model_id, "test-model");
        assert_eq!(status.dimension, 128);
    }

    #[test]
    fn test_read_status_from_path() {
        let dir = tempfile::tempdir().unwrap();
        let conn = Connection::open(db_path(dir.path())).unwrap();
        conn.execute_batch(
            "
            CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT);
            INSERT INTO meta VALUES ('model_id', 'm1');
            INSERT INTO meta VALUES ('dimension', '512');
            INSERT INTO meta VALUES ('engine', 'sbert');
            CREATE TABLE vec_chunks (id INTEGER PRIMARY KEY);
        ",
        )
        .unwrap();
        drop(conn);

        let status = SemanticIndex::read_status_from_path(dir.path()).unwrap();
        assert_eq!(status.model_id, "m1");
        assert_eq!(status.dimension, 512);
    }

    #[test]
    fn test_open_missing_error() {
        let dir = tempfile::tempdir().unwrap();
        let res = SemanticIndex::open(dir.path(), "any", 0);
        assert!(res.is_err());
    }

    #[test]
    fn test_create_and_open() {
        let dir = tempfile::tempdir().unwrap();
        let root = Path::new("/search/root");
        let model = "test-model";
        let dim = 128;
        let engine = EmbeddingEngine::Candle;

        // Create
        let idx = SemanticIndex::create(dir.path(), model, dim, engine, Some(root)).unwrap();
        assert_eq!(idx.model_id, model);
        assert_eq!(idx.dimension, dim);
        assert_eq!(idx.root_path, Some(root.to_path_buf()));
        drop(idx);

        // Open
        let idx2 = SemanticIndex::open(dir.path(), model, dim).unwrap();
        assert_eq!(idx2.model_id, model);
        assert_eq!(idx2.dimension, dim);
        assert_eq!(idx2.root_path, Some(root.to_path_buf()));
    }

    #[test]
    fn test_write_and_query() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let model = "test-model";
        let dim = 3;
        let engine = EmbeddingEngine::Candle;

        let mut idx = SemanticIndex::create(dir.path(), model, dim, engine, Some(root)).unwrap();

        let file_path = root.join("test.txt");
        let prepared = PreparedFile {
            path: file_path.clone(),
            chunks: vec![
                (
                    Chunk {
                        file_path: file_path.clone(),
                        text: "hello world".to_string(),
                        byte_range: ByteRange { start: 0, end: 11 },
                        origin: SourceOrigin::TextFile { line: 1, col: 1 },
                    },
                    vec![1.0, 0.0, 0.0],
                ),
                (
                    Chunk {
                        file_path: file_path.clone(),
                        text: "foo bar".to_string(),
                        byte_range: ByteRange { start: 12, end: 19 },
                        origin: SourceOrigin::TextFile { line: 2, col: 1 },
                    },
                    vec![0.0, 1.0, 0.0],
                ),
            ],
        };

        idx.write_file(prepared).unwrap();

        // Query for "hello" (vec [1, 0, 0])
        let results = idx.query(&[1.0, 0.0, 0.0], 1).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].chunk_text, "hello world");
        assert!(results[0].score > 0.99);

        // Query for "foo" (vec [0, 1, 0])
        let results2 = idx.query(&[0.0, 1.0, 0.0], 1).unwrap();
        assert_eq!(results2.len(), 1);
        assert_eq!(results2[0].chunk_text, "foo bar");

        // Remove file
        idx.remove_file(&file_path).unwrap();
        let results3 = idx.query(&[1.0, 0.0, 0.0], 10).unwrap();
        assert_eq!(results3.len(), 0);
    }

    #[test]
    fn test_index_delete() {
        let dir = tempdir().unwrap();
        let idx_dir = dir.path().join("idx");
        fs::create_dir_all(&idx_dir).unwrap();

        let idx = SemanticIndex::create(&idx_dir, "m", 3, EmbeddingEngine::Candle, None).unwrap();
        let db_file = idx_dir.join("semantic_index.db");
        assert!(db_file.exists());

        idx.delete(&idx_dir).unwrap();
        assert!(!db_file.exists());
    }

    #[test]
    fn test_delete_non_existent() {
        let dir = tempdir().unwrap();
        let idx = SemanticIndex::create(dir.path(), "m", 3, EmbeddingEngine::Candle, None).unwrap();
        fs::remove_file(dir.path().join("semantic_index.db")).unwrap();
        assert!(idx.delete(dir.path()).is_ok());
    }

    #[test]
    fn test_open_legacy_schema() {
        let dir = tempdir().unwrap();
        let path = db_path(dir.path());
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT);")
            .unwrap();
        // Missing vec_chunks table
        drop(conn);

        let res = SemanticIndex::open(dir.path(), "any", 0);
        match res {
            Err(e) => assert!(e.to_string().contains("legacy schema")),
            Ok(_) => panic!("Expected legacy schema error"),
        }
    }

    #[test]
    fn test_open_dimension_mismatch() {
        let dir = tempdir().unwrap();
        let model = "m1";
        let engine = EmbeddingEngine::Candle;

        // Create with dim 128
        SemanticIndex::create(dir.path(), model, 128, engine, None).unwrap();

        // Try open with dim 256
        let res = SemanticIndex::open(dir.path(), model, 256);
        match res {
            Err(e) => assert!(e.to_string().contains("dimension mismatch")),
            Ok(_) => panic!("Expected dimension mismatch error"),
        }
    }

    #[test]
    fn test_extract_chunks_fallback() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.txt");
        fs::write(&path, "hello world").unwrap();

        let registry = ExtractorRegistry::new(); // empty registry
        let chunks = SemanticIndex::extract_chunks(&path, &registry, 100, 10).unwrap();

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, "hello world");
    }

    #[test]
    fn test_f32_slice_to_bytes() {
        let v = vec![1.0f32, -2.5f32];
        let bytes = f32_slice_to_bytes(&v);
        assert_eq!(bytes.len(), 8);
        assert_eq!(bytes[0..4], 1.0f32.to_le_bytes());
        assert_eq!(bytes[4..8], (-2.5f32).to_le_bytes());
    }

    #[test]
    fn test_open_schema_version_mismatch() {
        let dir = tempdir().unwrap();
        let path = db_path(dir.path());
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "
            CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT);
            INSERT INTO meta VALUES ('schema_version', '2'); -- expected 1
            CREATE TABLE vec_chunks (id INTEGER PRIMARY KEY);
        ",
        )
        .unwrap();
        drop(conn);

        let res = SemanticIndex::open(dir.path(), "any", 0);
        match res {
            Err(e) => assert!(e.to_string().contains("schema version 2 is not supported")),
            Ok(_) => panic!("Expected schema version error"),
        }
    }

    #[test]
    fn test_to_rel_abs_path() {
        let root = Path::new("/search/root");
        let index = SemanticIndex {
            conn: Connection::open_in_memory().unwrap(),
            model_id: "m".to_string(),
            dimension: 1,
            root_path: Some(root.to_path_buf()),
        };

        let abs = root.join("subdir/file.txt");
        let rel = index.to_rel_path(&abs);
        assert_eq!(rel, Path::new("subdir/file.txt"));

        let abs2 = index.to_abs_path("subdir/file.txt");
        assert_eq!(abs2, abs);

        // Path outside root
        let outside = Path::new("/other/file.txt");
        let rel_outside = index.to_rel_path(outside);
        assert_eq!(rel_outside, outside);
    }

    #[test]
    fn test_write_file_pdf_origin() {
        let dir = tempdir().unwrap();
        let mut idx =
            SemanticIndex::create(dir.path(), "m", 1, EmbeddingEngine::Candle, None).unwrap();

        let path = PathBuf::from("test.pdf");
        let prepared = PreparedFile {
            path: path.clone(),
            chunks: vec![(
                Chunk {
                    file_path: path.clone(),
                    text: "page content".to_string(),
                    byte_range: ByteRange { start: 0, end: 12 },
                    origin: SourceOrigin::PdfPage {
                        page: 5,
                        bbox: Some(BoundingBox {
                            x: 1.0,
                            y: 2.0,
                            width: 3.0,
                            height: 4.0,
                        }),
                    },
                },
                vec![1.0],
            )],
        };

        idx.write_file(prepared).unwrap();

        let results = idx.query(&[1.0], 1).unwrap();
        assert_eq!(results.len(), 1);
        match &results[0].origin {
            SourceOrigin::PdfPage { page, bbox } => {
                assert_eq!(*page, 5);
                let b = bbox.as_ref().unwrap();
                assert_eq!(b.x, 1.0);
                assert_eq!(b.y, 2.0);
            }
            _ => panic!("Expected PdfPage origin"),
        }
    }

    #[test]
    fn test_query_dimension_mismatch() {
        let dir = tempdir().unwrap();
        let idx = SemanticIndex::create(dir.path(), "m", 1, EmbeddingEngine::Candle, None).unwrap();
        let res = idx.query(&[1.0, 2.0], 1);
        match res {
            Err(e) => assert!(e
                .to_string()
                .contains("Expected 1 dimensions but received 2")),
            Ok(_) => panic!("Expected query dimension mismatch"),
        }
    }

    #[test]
    fn test_build_full() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("root");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("test.txt"), "build test content").unwrap();

        let data_dir = dir.path().join("data");
        fs::create_dir(&data_dir).unwrap();

        let registry = ExtractorRegistry::new();
        struct MockEmbedder;
        impl Embedder for MockEmbedder {
            fn embed(&self, _texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
                Ok(vec![vec![1.0]])
            }
            fn model_id(&self) -> &str {
                "mock"
            }
            fn dimension(&self) -> usize {
                1
            }
            fn engine(&self) -> EmbeddingEngine {
                EmbeddingEngine::Candle
            }
        }

        let (tx, mut rx) = tokio::sync::mpsc::channel(10);
        let indexing = IndexingConfig {
            chunk_size: 100,
            chunk_overlap: 0,
            supported_extensions: vec!["txt".to_string()],
        };

        let idx = SemanticIndex::build(
            &data_dir,
            &root,
            &[root.join("test.txt")],
            &registry,
            &MockEmbedder,
            tx,
            Arc::new(AtomicBool::new(false)),
            &indexing,
        )
        .unwrap();

        assert_eq!(idx.status().total_chunks, 1);

        // Check progress messages
        let mut progress_count = 0;
        while let Ok(_p) = rx.try_recv() {
            progress_count += 1;
        }
        assert!(progress_count >= 2);
    }

    #[test]
    fn test_build_fails_on_embedding_dimension_mismatch() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("root");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("test.txt"), "build test content").unwrap();

        let data_dir = dir.path().join("data");
        fs::create_dir(&data_dir).unwrap();

        let registry = ExtractorRegistry::new();
        struct WrongDimEmbedder;
        impl Embedder for WrongDimEmbedder {
            fn embed(&self, _texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
                Ok(vec![vec![1.0, 2.0]])
            }
            fn model_id(&self) -> &str {
                "mock"
            }
            fn dimension(&self) -> usize {
                1
            }
            fn engine(&self) -> EmbeddingEngine {
                EmbeddingEngine::Candle
            }
        }

        let (tx, _rx) = tokio::sync::mpsc::channel(10);
        let indexing = IndexingConfig {
            chunk_size: 100,
            chunk_overlap: 0,
            supported_extensions: vec!["txt".to_string()],
        };

        let res = SemanticIndex::build(
            &data_dir,
            &root,
            &[root.join("test.txt")],
            &registry,
            &WrongDimEmbedder,
            tx,
            Arc::new(AtomicBool::new(false)),
            &indexing,
        );

        match res {
            Err(e) => assert!(e
                .to_string()
                .contains("Failed to write index entry")),
            Ok(_) => panic!("Expected build failure on embedding dimension mismatch"),
        }
    }
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
    pub fn open(
        data_dir: &Path,
        model_id: &str,
        expected_dimension: usize,
    ) -> anyhow::Result<Self> {
        load_sqlite_vec();

        let path = db_path(data_dir);
        anyhow::ensure!(
            path.exists(),
            "No semantic index found at {}",
            path.display()
        );

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

        let schema_version: i64 = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |row| {
                    let s: String = row.get(0)?;
                    Ok(s.parse::<i64>().unwrap_or(0))
                },
            )
            .unwrap_or(0);
        anyhow::ensure!(
            schema_version == 1,
            "Index schema version {} is not supported (expected 1); rebuild the index",
            schema_version
        );

        let stored_model_id: String = conn
            .query_row("SELECT value FROM meta WHERE key = 'model_id'", [], |row| {
                row.get(0)
            })
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
        root_path: Option<&Path>,
    ) -> anyhow::Result<Self> {
        load_sqlite_vec();

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        if path.exists() {
            std::fs::remove_file(path)?;
        }

        // Remove orphaned WAL/SHM files for this specific path.
        let mut wal = path.as_os_str().to_owned();
        wal.push("-wal");
        let mut shm = path.as_os_str().to_owned();
        shm.push("-shm");
        let _ = std::fs::remove_file(wal);
        let _ = std::fs::remove_file(shm);

        let conn = Connection::open(path)
            .with_context(|| format!("Failed to create index at {}", path.display()))?;

        Self::create_schema(&conn, model_id, dimension, engine)?;

        if let Some(rp) = root_path {
            conn.execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES ('root_path', ?1)",
                params![rp.to_string_lossy()],
            )?;
        }

        let built_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('built_at', ?1)",
            params![built_at.to_string()],
        )?;

        Ok(Self {
            conn,
            model_id: model_id.to_string(),
            dimension,
            root_path: root_path.map(|p| p.to_path_buf()),
        })
    }

    /// Create a new empty index at `data_dir` (schema only, no files indexed).
    /// Removes any existing index at that path.
    pub fn create(
        data_dir: &Path,
        model_id: &str,
        dimension: usize,
        engine: EmbeddingEngine,
        root_path: Option<&Path>,
    ) -> anyhow::Result<Self> {
        Self::create_at_path(&db_path(data_dir), model_id, dimension, engine, root_path)
    }

    /// Full build: creates the database at `data_dir`, indexes every path, and
    /// returns the open index.
    pub fn build(
        data_dir: &Path,
        root_path: &Path,
        paths: &[PathBuf],
        extractors: &ExtractorRegistry,
        embedder: &dyn Embedder,
        tx: ProgressTx,
        cancel_flag: Arc<AtomicBool>,
        indexing: &IndexingConfig,
    ) -> anyhow::Result<Self> {
        let start_time = Instant::now();
        let total_files = paths.len();

        let final_path = db_path(data_dir);
        let tmp_path = data_dir.join("semantic_index.db.tmp");

        let mut idx = Self::create_at_path(
            &tmp_path,
            embedder.model_id(),
            embedder.dimension(),
            embedder.engine(),
            Some(root_path),
        )?;

        // Extract, embed, and write one file at a time so peak memory is bounded
        // to a single file's chunks + embeddings on top of the model weights.
        for (i, path) in paths.iter().enumerate() {
            anyhow::ensure!(
                !cancel_flag.load(Ordering::Relaxed),
                "Index build cancelled"
            );
            let _ = tx.blocking_send(EmbedProgress::Build(IndexBuildProgress {
                files_processed: i,
                total_files,
                message: format!("Indexing {} of {}...", i + 1, total_files),
                done: false,
            }));

            let chunks = match Self::extract_chunks(
                path,
                extractors,
                indexing.chunk_size,
                indexing.chunk_overlap,
            ) {
                Ok(c) if !c.is_empty() => c,
                _ => continue,
            };

            let texts: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();
            let embeddings = embedder.embed_passages(&texts)?;
            anyhow::ensure!(
                !cancel_flag.load(Ordering::Relaxed),
                "Index build cancelled"
            );

            let prepared = PreparedFile {
                path: path.clone(),
                chunks: chunks.into_iter().zip(embeddings).collect(),
            };
            idx.write_file(prepared)
                .with_context(|| format!("Failed to write index entry for {}", path.display()))?;
        }

        let duration_ms = start_time.elapsed().as_millis() as u64;
        idx.conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('build_duration_ms', ?1)",
            params![duration_ms.to_string()],
        )?;

        // Success! Close connection and rename.
        let model_id = idx.model_id.clone();
        let dimension = idx.dimension;
        drop(idx);

        // Remove old files if they exist to avoid rename errors on some systems.
        let _ = std::fs::remove_file(&final_path);
        let _ = std::fs::remove_file(data_dir.join("semantic_index.db-wal"));
        let _ = std::fs::remove_file(data_dir.join("semantic_index.db-shm"));

        std::fs::rename(&tmp_path, &final_path).with_context(|| {
            format!(
                "Failed to rename {} to {}",
                tmp_path.display(),
                final_path.display()
            )
        })?;

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
                origin_type TEXT    NOT NULL,
                page        INTEGER,
                line        INTEGER,
                col         INTEGER,
                bbox_x      REAL,
                bbox_y      REAL,
                bbox_w      REAL,
                bbox_h      REAL,
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
        conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('schema_version', '1')",
            [],
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
                    source_map: crate::types::SourceMap {
                        segments: Vec::new(),
                    },
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
        Ok(chunk_content(
            &content,
            path.to_path_buf(),
            chunk_size,
            chunk_overlap,
        ))
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
            return Ok(PreparedFile {
                path: path.to_path_buf(),
                chunks: Vec::new(),
            });
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
        Ok(PreparedFile {
            path: path.to_path_buf(),
            chunks,
        })
    }

    /// Convert an absolute path to a root-relative path for storage.
    /// Falls back to the absolute path if no root_path is set or stripping fails.
    fn to_rel_path<'a>(&self, path: &'a Path) -> std::borrow::Cow<'a, Path> {
        if let Some(root) = &self.root_path {
            if let Ok(rel) = path.strip_prefix(root) {
                return std::borrow::Cow::Owned(rel.to_path_buf());
            }
        }
        std::borrow::Cow::Borrowed(path)
    }

    /// Reconstruct an absolute path from a stored (possibly relative) path.
    fn to_abs_path(&self, stored: &str) -> PathBuf {
        let p = Path::new(stored);
        if p.is_absolute() {
            return p.to_path_buf();
        }
        if let Some(root) = &self.root_path {
            root.join(p)
        } else {
            p.to_path_buf()
        }
    }

    /// Write previously prepared chunks into the index, removing any existing chunks
    /// for that path first.
    pub fn write_file(&mut self, prepared: PreparedFile) -> anyhow::Result<()> {
        let abs_path_str = prepared.path.to_string_lossy().into_owned();
        let rel_path = self.to_rel_path(&prepared.path);
        let rel_path_str = rel_path.to_string_lossy();

        // Validate dimensions before starting transaction.
        for (_, embedding) in &prepared.chunks {
            anyhow::ensure!(
                embedding.len() == self.dimension,
                "Dimension mismatch: expected {}, received {} for path {}",
                self.dimension,
                embedding.len(),
                abs_path_str
            );
        }

        // Delete vectors first (vec0 has no FK cascade), then the chunk rows.
        self.conn.execute(
            "DELETE FROM vec_chunks WHERE rowid IN (SELECT id FROM chunks WHERE file_path = ?1)",
            params![rel_path_str],
        )?;
        self.conn.execute(
            "DELETE FROM chunks WHERE file_path = ?1",
            params![rel_path_str],
        )?;

        let tx = self.conn.transaction()?;
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
            tx.execute(
                "INSERT INTO chunks (file_path, chunk_idx, byte_start, byte_end,
                                     origin_type, page, line, col,
                                     bbox_x, bbox_y, bbox_w, bbox_h, chunk_text)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    rel_path_str,
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
        let rel = self.to_rel_path(path);
        let rel_str = rel.to_string_lossy();
        self.conn.execute(
            "DELETE FROM vec_chunks WHERE rowid IN (SELECT id FROM chunks WHERE file_path = ?1)",
            params![rel_str],
        )?;
        self.conn
            .execute("DELETE FROM chunks WHERE file_path = ?1", params![rel_str])?;
        Ok(())
    }

    /// Query the index for the top-k nearest neighbours to `embedding`.
    /// Uses cosine similarity computed in Rust (O(n) over all stored vectors).
    pub fn query(&self, embedding: &[f32], top_k: usize) -> anyhow::Result<Vec<IndexedChunk>> {
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
            "SELECT v.rowid, v.distance, c.file_path, c.byte_start, c.byte_end,
                    c.origin_type, c.page, c.line, c.col,
                    c.bbox_x, c.bbox_y, c.bbox_w, c.bbox_h, c.chunk_text
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
                    let abs_path = self.to_abs_path(&file_path);
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

    /// Read index metadata without re-validating model_id/dimension.
    pub fn status(&self) -> IndexStatus {
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

        let indexed_files: usize = self
            .conn
            .query_row("SELECT COUNT(DISTINCT file_path) FROM chunks", [], |r| {
                r.get(0)
            })
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
            db_size_bytes: None,
        }
    }

    /// Read index status directly from the DB file without opening a full SemanticIndex.
    /// Does not validate model_id/dimension against any embedder.
    pub fn read_status_from_path(data_dir: &Path) -> anyhow::Result<IndexStatus> {
        let path = db_path(data_dir);
        anyhow::ensure!(path.exists(), "No semantic index found");
        let conn = Connection::open(&path)?;

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

        let indexed_files: usize = conn
            .query_row("SELECT COUNT(DISTINCT file_path) FROM chunks", [], |r| {
                r.get(0)
            })
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
        Ok(())
    }
}

// ── Vector utilities ──────────────────────────────────────────────────────────

fn f32_slice_to_bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}
