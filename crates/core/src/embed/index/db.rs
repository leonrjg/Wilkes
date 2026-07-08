use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use ignore::WalkBuilder;
use rusqlite::{params, Connection};
use tracing::error;

use crate::extract::ExtractorRegistry;
use crate::metadata::cache::FileIdentity;
use crate::types::{
    BoundingBox, ByteRange, EmbeddingEngine, FileType, IndexStatus, IndexingConfig,
    RelatedDocument, SourceOrigin,
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

const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(3);
const SCHEMA_VERSION: i64 = 2;

fn configure_connection(conn: &Connection, path: &Path) -> anyhow::Result<()> {
    conn.busy_timeout(SQLITE_BUSY_TIMEOUT)
        .with_context(|| format!("Failed to configure busy timeout for {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use std::sync::atomic::AtomicUsize;
    use tempfile::tempdir;

    struct CountingEmbedder {
        calls: AtomicUsize,
    }

    impl CountingEmbedder {
        fn new() -> Self {
            Self {
                calls: AtomicUsize::new(0),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::Relaxed)
        }
    }

    impl Embedder for CountingEmbedder {
        fn embed(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            self.calls.fetch_add(texts.len(), Ordering::Relaxed);
            Ok(vec![vec![1.0]; texts.len()])
        }

        fn model_id(&self) -> &str {
            "counting"
        }

        fn dimension(&self) -> usize {
            1
        }

        fn engine(&self) -> EmbeddingEngine {
            EmbeddingEngine::Candle
        }
    }

    fn txt_indexing() -> IndexingConfig {
        IndexingConfig {
            chunk_size: 100,
            chunk_overlap: 0,
            supported_extensions: vec!["txt".to_string()],
        }
    }

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
            root_canonical_path: None,
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
            INSERT INTO meta VALUES ('schema_version', '2');
            CREATE TABLE files (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                file_path TEXT NOT NULL UNIQUE,
                size_bytes INTEGER NOT NULL,
                modified_at_ms INTEGER NOT NULL,
                indexed_at_ms INTEGER NOT NULL
            );
            CREATE TABLE chunks (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                file_id INTEGER NOT NULL,
                chunk_idx INTEGER NOT NULL,
                byte_start INTEGER NOT NULL,
                byte_end INTEGER NOT NULL,
                origin_type TEXT NOT NULL,
                chunk_text TEXT NOT NULL
            );
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
    fn test_read_status_from_path_retries_locked_db() {
        let dir = tempfile::tempdir().unwrap();
        let path = db_path(dir.path());
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "
            CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT);
            INSERT INTO meta VALUES ('model_id', 'm1');
            INSERT INTO meta VALUES ('dimension', '512');
            INSERT INTO meta VALUES ('engine', 'sbert');
            INSERT INTO meta VALUES ('schema_version', '2');
            CREATE TABLE files (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                file_path TEXT NOT NULL UNIQUE,
                size_bytes INTEGER NOT NULL,
                modified_at_ms INTEGER NOT NULL,
                indexed_at_ms INTEGER NOT NULL
            );
            CREATE TABLE chunks (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                file_id INTEGER NOT NULL,
                chunk_idx INTEGER NOT NULL,
                byte_start INTEGER NOT NULL,
                byte_end INTEGER NOT NULL,
                origin_type TEXT NOT NULL,
                chunk_text TEXT NOT NULL
            );
            CREATE TABLE vec_chunks (id INTEGER PRIMARY KEY);
        ",
        )
        .unwrap();
        drop(conn);

        let lock_path = path.clone();
        let lock_handle = std::thread::spawn(move || {
            let conn = Connection::open(&lock_path).unwrap();
            conn.execute_batch("BEGIN EXCLUSIVE").unwrap();
            std::thread::sleep(Duration::from_millis(150));
            conn.execute_batch("COMMIT").unwrap();
        });

        std::thread::sleep(Duration::from_millis(50));

        let status = SemanticIndex::read_status_from_path(dir.path()).unwrap();
        assert_eq!(status.model_id, "m1");

        lock_handle.join().unwrap();
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
        fs::write(&file_path, "hello world\nfoo bar").unwrap();
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
    fn test_rename_file_rekeys_chunks_and_keeps_embeddings() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut idx =
            SemanticIndex::create(root, "m", 3, EmbeddingEngine::Candle, Some(root)).unwrap();

        let old = root.join("old.txt");
        let new = root.join("new.txt");
        fs::write(&old, "hello world").unwrap();
        idx.write_file(PreparedFile {
            path: old.clone(),
            chunks: vec![(
                Chunk {
                    file_path: old.clone(),
                    text: "hello world".to_string(),
                    byte_range: ByteRange { start: 0, end: 11 },
                    origin: SourceOrigin::TextFile { line: 1, col: 1 },
                },
                vec![1.0, 0.0, 0.0],
            )],
        })
        .unwrap();

        fs::rename(&old, &new).unwrap();
        idx.rename_file(&old, &new).unwrap();

        // The chunk (and its embedding) survives the rename and is queryable.
        let results = idx.query(&[1.0, 0.0, 0.0], 1).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].file_path, new);

        // It is keyed under the new path only: removing the old path is a no-op,
        // removing the new path clears it.
        idx.remove_file(&old).unwrap();
        assert_eq!(idx.query(&[1.0, 0.0, 0.0], 1).unwrap().len(), 1);
        idx.remove_file(&new).unwrap();
        assert_eq!(idx.query(&[1.0, 0.0, 0.0], 1).unwrap().len(), 0);
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
            INSERT INTO meta VALUES ('schema_version', '1'); -- expected 2
            CREATE TABLE vec_chunks (id INTEGER PRIMARY KEY);
            CREATE TABLE files (id INTEGER PRIMARY KEY);
        ",
        )
        .unwrap();
        drop(conn);

        let res = SemanticIndex::open(dir.path(), "any", 0);
        match res {
            Err(e) => assert!(e.to_string().contains("schema version 1 is not supported")),
            Ok(_) => panic!("Expected schema version error"),
        }
    }

    #[test]
    fn test_to_rel_abs_path() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let index = SemanticIndex {
            conn: Connection::open_in_memory().unwrap(),
            model_id: "m".to_string(),
            dimension: 1,
            root_path: Some(root.to_path_buf()),
            root_canonical_path: Some(std::fs::canonicalize(root).unwrap()),
        };

        let abs = root.join("subdir/file.txt");
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(&abs, "hello").unwrap();
        let rel = index.path_key_for_existing_path(&abs);
        assert_eq!(rel, Path::new("subdir/file.txt"));

        let abs2 = index.key_to_display_path("subdir/file.txt");
        assert_eq!(abs2, abs);

        // Path outside root
        let outside = Path::new("/other/file.txt");
        let rel_outside = index.path_key_for_known_path(outside);
        assert_eq!(rel_outside, outside);
    }

    #[test]
    fn test_write_file_pdf_origin() {
        let dir = tempdir().unwrap();
        let mut idx =
            SemanticIndex::create(dir.path(), "m", 1, EmbeddingEngine::Candle, None).unwrap();

        let path = dir.path().join("test.pdf");
        fs::write(&path, "page content").unwrap();
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
    fn test_query_file_scope_selects_within_file_before_top_k() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let mut idx =
            SemanticIndex::create(root, "m", 2, EmbeddingEngine::Candle, Some(root)).unwrap();
        let scoped_path = root.join("scoped.txt");
        let other_path = root.join("other.txt");
        fs::write(&scoped_path, "scoped chunk").unwrap();
        fs::write(&other_path, "other chunk").unwrap();

        idx.write_file(PreparedFile {
            path: scoped_path.clone(),
            chunks: vec![(
                Chunk {
                    file_path: scoped_path.clone(),
                    text: "scoped chunk".to_string(),
                    byte_range: ByteRange { start: 0, end: 12 },
                    origin: SourceOrigin::TextFile { line: 1, col: 1 },
                },
                vec![0.0, 1.0],
            )],
        })
        .unwrap();
        idx.write_file(PreparedFile {
            path: other_path.clone(),
            chunks: vec![(
                Chunk {
                    file_path: other_path.clone(),
                    text: "other chunk".to_string(),
                    byte_range: ByteRange { start: 0, end: 11 },
                    origin: SourceOrigin::TextFile { line: 1, col: 1 },
                },
                vec![1.0, 0.0],
            )],
        })
        .unwrap();

        let global = idx.query(&[1.0, 0.0], 1).unwrap();
        assert_eq!(global.len(), 1);
        assert_eq!(global[0].file_path, other_path);

        let scoped = idx
            .query_scoped(&[1.0, 0.0], 1, SemanticQueryScope::File(&scoped_path))
            .unwrap();
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].file_path, scoped_path);
        assert_eq!(scoped[0].chunk_text, "scoped chunk");
    }

    #[test]
    fn test_related_documents_ranks_by_centroid_similarity_and_filters() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let mut idx =
            SemanticIndex::create(root, "m", 2, EmbeddingEngine::Candle, Some(root)).unwrap();
        let source = root.join("source.txt");
        let close = root.join("close.txt");
        let far = root.join("far.txt");
        let unsupported = root.join("image.bin");
        for path in [&source, &close, &far, &unsupported] {
            fs::write(path, "content").unwrap();
        }

        let chunk = |path: &Path, text: &str| Chunk {
            file_path: path.to_path_buf(),
            text: text.to_string(),
            byte_range: ByteRange {
                start: 0,
                end: text.len(),
            },
            origin: SourceOrigin::TextFile { line: 1, col: 1 },
        };

        idx.write_file(PreparedFile {
            path: source.clone(),
            chunks: vec![
                (chunk(&source, "source one"), vec![1.0, 0.0]),
                (chunk(&source, "source two"), vec![1.0, 0.0]),
            ],
        })
        .unwrap();
        idx.write_file(PreparedFile {
            path: close.clone(),
            chunks: vec![(chunk(&close, "close"), vec![0.9, 0.1])],
        })
        .unwrap();
        idx.write_file(PreparedFile {
            path: far.clone(),
            chunks: vec![(chunk(&far, "far"), vec![0.0, 1.0])],
        })
        .unwrap();
        idx.write_file(PreparedFile {
            path: unsupported.clone(),
            chunks: vec![(chunk(&unsupported, "unsupported"), vec![1.0, 0.0])],
        })
        .unwrap();

        let related = idx
            .related_documents(&source, 10, &["txt".to_string()])
            .unwrap();

        assert_eq!(
            related
                .iter()
                .map(|doc| doc.path.clone())
                .collect::<Vec<_>>(),
            vec![close, far]
        );
        assert!(related[0].score > related[1].score);
        assert_eq!(related[0].indexed_chunks, 1);
    }

    #[test]
    fn test_related_documents_missing_source_errors() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let idx = SemanticIndex::create(root, "m", 2, EmbeddingEngine::Candle, Some(root)).unwrap();
        let source = root.join("source.txt");
        fs::write(&source, "content").unwrap();

        let err = idx
            .related_documents(&source, 10, &["txt".to_string()])
            .unwrap_err();

        assert!(err.to_string().contains("not present"));
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
    fn test_query_skips_unknown_origin_types() {
        let dir = tempdir().unwrap();
        let mut idx =
            SemanticIndex::create(dir.path(), "m", 1, EmbeddingEngine::Candle, None).unwrap();

        let path = dir.path().join("mystery.txt");
        fs::write(&path, "mystery").unwrap();
        let prepared = PreparedFile {
            path: path.clone(),
            chunks: vec![(
                Chunk {
                    file_path: path.clone(),
                    text: "mystery".to_string(),
                    byte_range: ByteRange { start: 0, end: 7 },
                    origin: SourceOrigin::TextFile { line: 1, col: 1 },
                },
                vec![1.0],
            )],
        };
        idx.write_file(prepared).unwrap();

        idx.conn
            .execute("UPDATE chunks SET origin_type = 'mystery'", [])
            .unwrap();

        let results = idx.query(&[1.0], 1).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_reconcile_indexes_new_file_missed_while_offline() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let mut idx =
            SemanticIndex::create(root, "counting", 1, EmbeddingEngine::Candle, Some(root))
                .unwrap();
        let embedder = CountingEmbedder::new();
        let registry = ExtractorRegistry::new();
        let indexing = txt_indexing();

        let path = root.join("new.txt");
        fs::write(&path, "new content").unwrap();

        let errors = idx
            .reconcile_root(root, &registry, &embedder, &indexing)
            .unwrap();
        assert!(errors.is_empty());
        assert_eq!(embedder.calls(), 1);

        let results = idx.query(&[1.0], 1).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].file_path, path);
        assert_eq!(results[0].chunk_text, "new content");
    }

    #[test]
    fn test_reconcile_rekeys_offline_rename_without_reembedding() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let mut idx =
            SemanticIndex::create(root, "counting", 1, EmbeddingEngine::Candle, Some(root))
                .unwrap();
        let embedder = CountingEmbedder::new();
        let registry = ExtractorRegistry::new();
        let indexing = txt_indexing();

        let old = root.join("old.txt");
        let new = root.join("new.txt");
        fs::write(&old, "stable content").unwrap();
        idx.write_file(PreparedFile {
            path: old.clone(),
            chunks: vec![(
                Chunk {
                    file_path: old.clone(),
                    text: "stable content".to_string(),
                    byte_range: ByteRange { start: 0, end: 14 },
                    origin: SourceOrigin::TextFile { line: 1, col: 1 },
                },
                vec![1.0],
            )],
        })
        .unwrap();

        fs::rename(&old, &new).unwrap();

        let errors = idx
            .reconcile_root(root, &registry, &embedder, &indexing)
            .unwrap();
        assert!(errors.is_empty());
        assert_eq!(embedder.calls(), 0);

        let results = idx.query(&[1.0], 1).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].file_path, new);
        assert_eq!(idx.status().indexed_files, 1);
    }

    #[test]
    fn test_reconcile_deletes_file_missing_after_offline_delete() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let mut idx =
            SemanticIndex::create(root, "counting", 1, EmbeddingEngine::Candle, Some(root))
                .unwrap();
        let embedder = CountingEmbedder::new();
        let registry = ExtractorRegistry::new();
        let indexing = txt_indexing();

        let path = root.join("gone.txt");
        fs::write(&path, "delete me").unwrap();
        idx.write_file(PreparedFile {
            path: path.clone(),
            chunks: vec![(
                Chunk {
                    file_path: path.clone(),
                    text: "delete me".to_string(),
                    byte_range: ByteRange { start: 0, end: 9 },
                    origin: SourceOrigin::TextFile { line: 1, col: 1 },
                },
                vec![1.0],
            )],
        })
        .unwrap();

        fs::remove_file(&path).unwrap();

        let errors = idx
            .reconcile_root(root, &registry, &embedder, &indexing)
            .unwrap();
        assert!(errors.is_empty());
        assert_eq!(idx.query(&[1.0], 1).unwrap().len(), 0);
        assert_eq!(idx.status().indexed_files, 0);
    }

    #[test]
    fn test_reconcile_reindexes_changed_file_at_same_path() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let mut idx =
            SemanticIndex::create(root, "counting", 1, EmbeddingEngine::Candle, Some(root))
                .unwrap();
        let embedder = CountingEmbedder::new();
        let registry = ExtractorRegistry::new();
        let indexing = txt_indexing();

        let path = root.join("changed.txt");
        fs::write(&path, "old").unwrap();
        idx.write_file(PreparedFile {
            path: path.clone(),
            chunks: vec![(
                Chunk {
                    file_path: path.clone(),
                    text: "old".to_string(),
                    byte_range: ByteRange { start: 0, end: 3 },
                    origin: SourceOrigin::TextFile { line: 1, col: 1 },
                },
                vec![0.5],
            )],
        })
        .unwrap();

        fs::write(&path, "changed content").unwrap();

        let errors = idx
            .reconcile_root(root, &registry, &embedder, &indexing)
            .unwrap();
        assert!(errors.is_empty());
        assert_eq!(embedder.calls(), 1);

        let results = idx.query(&[1.0], 1).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].file_path, path);
        assert_eq!(results[0].chunk_text, "changed content");
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
    fn test_build_skips_file_on_embedding_dimension_mismatch() {
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

        let idx = SemanticIndex::build(
            &data_dir,
            &root,
            &[root.join("test.txt")],
            &registry,
            &WrongDimEmbedder,
            tx,
            Arc::new(AtomicBool::new(false)),
            &indexing,
        )
        .unwrap();

        assert_eq!(idx.status().total_chunks, 0);
    }

    #[test]
    fn test_is_fatal_embedder_error_detects_worker_failures() {
        assert!(SemanticIndex::is_fatal_embedder_error(&anyhow::anyhow!(
            "Worker error: failed to spawn"
        )));
        assert!(SemanticIndex::is_fatal_embedder_error(&anyhow::anyhow!(
            "Failed to send command to manager: closed"
        )));
        assert!(SemanticIndex::is_fatal_embedder_error(&anyhow::anyhow!(
            "Worker finished without returning embeddings"
        )));
        assert!(!SemanticIndex::is_fatal_embedder_error(&anyhow::anyhow!(
            "dimension mismatch"
        )));
    }

    #[test]
    fn test_write_file_is_atomic_on_failure() {
        let dir = tempdir().unwrap();
        let mut idx =
            SemanticIndex::create(dir.path(), "m", 1, EmbeddingEngine::Candle, None).unwrap();

        let path = dir.path().join("test.txt");
        fs::write(&path, "original").unwrap();
        let original = PreparedFile {
            path: path.clone(),
            chunks: vec![(
                Chunk {
                    file_path: path.clone(),
                    text: "original".to_string(),
                    byte_range: ByteRange { start: 0, end: 8 },
                    origin: SourceOrigin::TextFile { line: 1, col: 1 },
                },
                vec![1.0],
            )],
        };
        idx.write_file(original).unwrap();

        let replacement = PreparedFile {
            path: path.clone(),
            chunks: vec![(
                Chunk {
                    file_path: path.clone(),
                    text: "replacement".to_string(),
                    byte_range: ByteRange { start: 0, end: 11 },
                    origin: SourceOrigin::TextFile { line: 1, col: 1 },
                },
                vec![1.0, 2.0],
            )],
        };
        assert!(idx.write_file(replacement).is_err());

        let results = idx.query(&[1.0], 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].chunk_text, "original");
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

#[derive(Clone, Debug)]
struct IndexedFileRecord {
    path_key: PathBuf,
    identity: FileIdentity,
}

pub enum SemanticQueryScope<'a> {
    Corpus,
    File(&'a Path),
}

// ── SemanticIndex ─────────────────────────────────────────────────────────────

pub struct SemanticIndex {
    conn: Connection,
    model_id: String,
    dimension: usize,
    root_path: Option<PathBuf>,
    root_canonical_path: Option<PathBuf>,
}

impl SemanticIndex {
    fn is_fatal_embedder_error(err: &anyhow::Error) -> bool {
        let msg = err.to_string();
        msg.starts_with("Worker error:")
            || msg.starts_with("Failed to send command to manager:")
            || msg.starts_with("Worker finished without returning embeddings")
    }

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
        configure_connection(&conn, &path)?;

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
            schema_version == SCHEMA_VERSION,
            "Index schema version {} is not supported (expected {}); rebuild the index",
            schema_version,
            SCHEMA_VERSION
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
        let root_canonical_path: Option<PathBuf> = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'root_canonical_path'",
                [],
                |row| {
                    let s: String = row.get(0)?;
                    Ok(Some(PathBuf::from(s)))
                },
            )
            .unwrap_or_else(|_| {
                root_path
                    .as_ref()
                    .and_then(|p| std::fs::canonicalize(p).ok())
            });

        Ok(Self {
            conn,
            model_id: stored_model_id,
            dimension: stored_dimension,
            root_path,
            root_canonical_path,
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
        configure_connection(&conn, path)?;

        Self::create_schema(&conn, model_id, dimension, engine)?;

        let root_canonical_path =
            root_path.map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()));
        if let Some(rp) = root_path {
            conn.execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES ('root_path', ?1)",
                params![rp.to_string_lossy()],
            )?;
        }
        if let Some(rp) = &root_canonical_path {
            conn.execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES ('root_canonical_path', ?1)",
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
            root_canonical_path,
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
    #[allow(clippy::too_many_arguments)]
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
            let embeddings = match embedder.embed_passages(&texts) {
                Ok(embeddings) => embeddings,
                Err(e) => {
                    if Self::is_fatal_embedder_error(&e) {
                        return Err(e.context(format!(
                            "Fatal embedder error while indexing {}",
                            path.display()
                        )));
                    }
                    error!(
                        "[SemanticIndex::build] skipping {}: embed_passages failed: {e:#}",
                        path.display()
                    );
                    continue;
                }
            };
            if embeddings.len() != chunks.len() {
                error!(
                    "[SemanticIndex::build] skipping {}: embedder returned {} embeddings for {} chunks",
                    path.display(),
                    embeddings.len(),
                    chunks.len()
                );
                continue;
            }
            anyhow::ensure!(
                !cancel_flag.load(Ordering::Relaxed),
                "Index build cancelled"
            );

            let prepared = PreparedFile {
                path: path.clone(),
                chunks: chunks.into_iter().zip(embeddings).collect(),
            };
            if let Err(e) = idx.write_file(prepared) {
                error!(
                    "[SemanticIndex::build] skipping {}: failed to write index entry: {e:#}",
                    path.display()
                );
            }
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
            CREATE TABLE IF NOT EXISTS files (
                id             INTEGER PRIMARY KEY AUTOINCREMENT,
                file_path      TEXT    NOT NULL UNIQUE,
                size_bytes     INTEGER NOT NULL,
                modified_at_ms INTEGER NOT NULL,
                indexed_at_ms  INTEGER NOT NULL
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
                chunk_text  TEXT    NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_files_identity
                ON files(size_bytes, modified_at_ms);
            CREATE INDEX IF NOT EXISTS idx_chunks_file_id ON chunks(file_id);
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
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('schema_version', ?1)",
            params![SCHEMA_VERSION.to_string()],
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

    fn path_key_for_existing_path(&self, path: &Path) -> PathBuf {
        let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if let Some(root) = &self.root_canonical_path {
            if let Ok(rel) = canonical.strip_prefix(root) {
                return rel.to_path_buf();
            }
        }
        if let Some(root) = &self.root_path {
            if let Ok(rel) = path.strip_prefix(root) {
                return rel.to_path_buf();
            }
        }
        canonical
    }

    fn path_key_for_known_path(&self, path: &Path) -> PathBuf {
        if path.exists() {
            return self.path_key_for_existing_path(path);
        }
        if let Some(root) = &self.root_path {
            if let Ok(rel) = path.strip_prefix(root) {
                return rel.to_path_buf();
            }
        }
        if let Some(root) = &self.root_canonical_path {
            if let Ok(rel) = path.strip_prefix(root) {
                return rel.to_path_buf();
            }
        }
        path.to_path_buf()
    }

    fn key_to_display_path(&self, stored: &str) -> PathBuf {
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

    fn identity_for_path(path: &Path) -> anyhow::Result<FileIdentity> {
        FileIdentity::for_path(path)
            .ok_or_else(|| anyhow::anyhow!("Could not derive file identity for {}", path.display()))
    }

    fn now_ms() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|d| i64::try_from(d.as_millis()).ok())
            .unwrap_or(0)
    }

    fn delete_file_by_key_tx(tx: &rusqlite::Transaction<'_>, key: &str) -> anyhow::Result<()> {
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

    fn indexed_files(&self) -> anyhow::Result<Vec<IndexedFileRecord>> {
        let mut stmt = self
            .conn
            .prepare("SELECT file_path, size_bytes, modified_at_ms FROM files")?;
        let rows = stmt
            .query_map([], |row| {
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

    fn rename_key(&mut self, old_key: &Path, new_key: &Path) -> anyhow::Result<()> {
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
        let indexed = self.indexed_files()?;
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
                Self::delete_file_by_key_tx(&tx, &key)?;
                tx.commit()?;
            }
        }

        Ok(errors)
    }

    /// Write previously prepared chunks into the index, removing any existing chunks
    /// for that path first.
    pub fn write_file(&mut self, prepared: PreparedFile) -> anyhow::Result<()> {
        let abs_path_str = prepared.path.to_string_lossy().into_owned();
        let key = self.path_key_for_existing_path(&prepared.path);
        let key_str = key.to_string_lossy().into_owned();
        let identity = Self::identity_for_path(&prepared.path)?;

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

        let tx = self.conn.transaction()?;

        Self::delete_file_by_key_tx(&tx, &key_str)?;
        tx.execute(
            "INSERT INTO files (file_path, size_bytes, modified_at_ms, indexed_at_ms)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                key_str,
                identity.size_bytes,
                identity.modified_at_ms,
                Self::now_ms()
            ],
        )?;
        let file_id = tx.last_insert_rowid();
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
                "INSERT INTO chunks (file_id, chunk_idx, byte_start, byte_end,
                                     origin_type, page, line, col,
                                     bbox_x, bbox_y, bbox_w, bbox_h, chunk_text)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
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
        let key = self.path_key_for_known_path(path);
        let key_str = key.to_string_lossy().into_owned();
        let tx = self.conn.transaction()?;
        Self::delete_file_by_key_tx(&tx, &key_str)?;
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
        Self::delete_file_by_key_tx(&tx, &new_rel)?;
        tx.execute(
            "UPDATE files SET file_path = ?1 WHERE file_path = ?2",
            params![new_rel, old_rel],
        )?;
        tx.commit()?;
        Ok(())
    }

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
            SemanticQueryScope::File(path) => self.query_file(embedding, top_k, path),
        }
    }

    pub fn related_documents(
        &self,
        source_path: &Path,
        limit: usize,
        supported_extensions: &[String],
    ) -> anyhow::Result<Vec<RelatedDocument>> {
        let source_key = self
            .path_key_for_existing_path(source_path)
            .to_string_lossy()
            .into_owned();
        let limit = if limit == 0 { 8 } else { limit };

        let mut stmt = self.conn.prepare(
            "SELECT f.file_path, v.embedding
             FROM chunks c
             JOIN vec_chunks v ON v.rowid = c.id
             JOIN files f ON f.id = c.file_id",
        )?;

        let rows = stmt.query_map([], |row| {
            let file_path: String = row.get(0)?;
            let embedding_blob: Vec<u8> = row.get(1)?;
            Ok((file_path, embedding_blob))
        })?;

        #[derive(Default)]
        struct Accumulator {
            sum: Vec<f32>,
            chunks: usize,
        }

        let mut by_file: HashMap<String, Accumulator> = HashMap::new();
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
            let entry = by_file.entry(file_path).or_insert_with(|| Accumulator {
                sum: vec![0.0; self.dimension],
                chunks: 0,
            });
            for (total, value) in entry.sum.iter_mut().zip(normalized) {
                *total += value;
            }
            entry.chunks += 1;
        }

        let Some(source_acc) = by_file.get(&source_key) else {
            anyhow::bail!(
                "Source file is not present in the semantic index: {}",
                source_path.display()
            );
        };
        let source_centroid = centroid(&source_acc.sum, source_acc.chunks);

        let mut related = Vec::new();
        for (file_path, acc) in by_file {
            if file_path == source_key || acc.chunks == 0 {
                continue;
            }
            let abs_path = self.key_to_display_path(&file_path);
            let Some(file_type) = FileType::detect(&abs_path, supported_extensions) else {
                continue;
            };
            let candidate_centroid = centroid(&acc.sum, acc.chunks);
            let score = cosine_similarity(&source_centroid, &candidate_centroid);
            related.push(RelatedDocument {
                path: abs_path,
                file_type,
                score,
                indexed_chunks: acc.chunks,
            });
        }

        related.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| {
                    let left = a
                        .path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or_default();
                    let right = b
                        .path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or_default();
                    left.cmp(right)
                })
                .then_with(|| a.path.cmp(&b.path))
        });
        related.truncate(limit);
        Ok(related)
    }

    fn query_corpus(&self, embedding: &[f32], top_k: usize) -> anyhow::Result<Vec<IndexedChunk>> {
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

    fn query_file(
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
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
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

        let indexed_files: usize = conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
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

fn f32_slice_from_bytes(bytes: &[u8]) -> anyhow::Result<Vec<f32>> {
    anyhow::ensure!(
        bytes.len() % std::mem::size_of::<f32>() == 0,
        "Invalid embedding byte length: {}",
        bytes.len()
    );
    Ok(bytes
        .chunks_exact(std::mem::size_of::<f32>())
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect())
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let (mut dot, mut norm_a, mut norm_b) = (0.0f32, 0.0f32, 0.0f32);
    for (left, right) in a.iter().zip(b) {
        dot += left * right;
        norm_a += left * left;
        norm_b += right * right;
    }
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a.sqrt() * norm_b.sqrt())
}

fn normalized_vector(v: &[f32]) -> Vec<f32> {
    let norm = v.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm == 0.0 {
        return vec![0.0; v.len()];
    }
    v.iter().map(|value| value / norm).collect()
}

fn centroid(sum: &[f32], count: usize) -> Vec<f32> {
    if count == 0 {
        return vec![0.0; sum.len()];
    }
    let count = count as f32;
    sum.iter().map(|value| value / count).collect()
}

fn source_origin_from_parts(
    origin_type: &str,
    page: Option<i64>,
    line: Option<i64>,
    col: Option<i64>,
    bbox_x: Option<f64>,
    bbox_y: Option<f64>,
    bbox_w: Option<f64>,
    bbox_h: Option<f64>,
) -> Option<SourceOrigin> {
    match origin_type {
        "text_file" => Some(SourceOrigin::TextFile {
            line: line.unwrap_or(0) as u32,
            col: col.unwrap_or(0) as u32,
        }),
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
            Some(SourceOrigin::PdfPage {
                page: page.unwrap_or(0) as u32,
                bbox,
            })
        }
        _ => None,
    }
}
