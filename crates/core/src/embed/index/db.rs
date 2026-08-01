use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use ignore::WalkBuilder;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension};
use tracing::error;

use crate::extract::ExtractorRegistry;
use crate::metadata::cache::FileIdentity;
use crate::types::{
    BoundingBox, ByteRange, EmbeddingEngine, FileType, IndexStatus, IndexingConfig,
    RelatedDocument, SourceOrigin,
};

use super::super::Embedder;
use super::chunk::{chunk_content, Chunk};
use crate::models::progress::{EmbedProgress, IndexBuildProgress, ProgressTx};

fn system_time_ms(value: SystemTime) -> Option<i64> {
    value
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
}

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
const SCHEMA_VERSION: i64 = 3;

fn configure_connection(conn: &Connection, path: &Path) -> anyhow::Result<()> {
    conn.busy_timeout(SQLITE_BUSY_TIMEOUT)
        .with_context(|| format!("Failed to configure busy timeout for {}", path.display()))?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")
        .with_context(|| format!("Failed to enable foreign keys for {}", path.display()))?;
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

    fn canon(path: &Path) -> PathBuf {
        std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
    }

    fn test_chunk(path: &Path, text: &str) -> Chunk {
        Chunk {
            file_path: path.to_path_buf(),
            text: text.to_string(),
            byte_range: ByteRange {
                start: 0,
                end: text.len(),
            },
            origin: SourceOrigin::TextFile { line: 1, col: 1 },
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
            dimension: 128,
            active_root: None,
            active_root_id: None,
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
        assert_eq!(idx.status().model_id, model);
        assert_eq!(idx.dimension, dim);
        assert!(idx.active_root.is_some());
        assert!(idx.active_root_id.is_some());
        drop(idx);

        // Open
        let idx2 = SemanticIndex::open(dir.path(), model, dim).unwrap();
        assert_eq!(idx2.status().model_id, model);
        assert_eq!(idx2.dimension, dim);
        assert!(idx2.active_root.is_none());
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
        assert_eq!(results[0].file_path, canon(&new));

        // It is keyed under the new path only: removing the old path is a no-op,
        // removing the new path clears it.
        idx.remove_file(&old).unwrap();
        assert_eq!(idx.query(&[1.0, 0.0, 0.0], 1).unwrap().len(), 1);
        idx.remove_file(&new).unwrap();
        assert_eq!(idx.query(&[1.0, 0.0, 0.0], 1).unwrap().len(), 0);
    }

    #[test]
    fn topic_chunks_bulk_read_is_root_scoped_and_includes_passage_metadata() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("root");
        fs::create_dir(&root).unwrap();
        let path = root.join("notes.txt");
        fs::write(&path, "first second").unwrap();
        let mut idx =
            SemanticIndex::create(dir.path(), "m", 3, EmbeddingEngine::Candle, Some(&root))
                .unwrap();
        idx.write_file(PreparedFile {
            path: path.clone(),
            chunks: vec![
                (
                    Chunk {
                        file_path: path.clone(),
                        text: "first".into(),
                        byte_range: ByteRange { start: 0, end: 5 },
                        origin: SourceOrigin::TextFile { line: 1, col: 1 },
                    },
                    vec![1.0, 0.0, 0.0],
                ),
                (
                    Chunk {
                        file_path: path.clone(),
                        text: "second".into(),
                        byte_range: ByteRange { start: 6, end: 12 },
                        origin: SourceOrigin::TextFile { line: 1, col: 7 },
                    },
                    vec![0.0, 1.0, 0.0],
                ),
            ],
        })
        .unwrap();

        let chunks = idx.topic_chunks_for_root(&root).unwrap();
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].file_path, canon(&path));
        assert_eq!(chunks[0].chunk_text, "first");
        assert_eq!(chunks[0].embedding, vec![1.0, 0.0, 0.0]);
        assert!(chunks[0].chunk_id > 0);
        assert!(idx
            .topic_chunks_for_root(&root.join("missing"))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn test_rename_directory_rekeys_descendant_chunks_and_keeps_embeddings() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut idx =
            SemanticIndex::create(root, "m", 3, EmbeddingEngine::Candle, Some(root)).unwrap();

        let old_dir = root.join("old");
        fs::create_dir(&old_dir).unwrap();
        let old_file = old_dir.join("paper.txt");
        fs::write(&old_file, "hello world").unwrap();
        idx.write_file(PreparedFile {
            path: old_file.clone(),
            chunks: vec![(
                Chunk {
                    file_path: old_file.clone(),
                    text: "hello world".to_string(),
                    byte_range: ByteRange { start: 0, end: 11 },
                    origin: SourceOrigin::TextFile { line: 1, col: 1 },
                },
                vec![1.0, 0.0, 0.0],
            )],
        })
        .unwrap();

        let new_dir = root.join("new");
        fs::rename(&old_dir, &new_dir).unwrap();
        idx.rename_file(&old_dir, &new_dir).unwrap();

        // The descendant's chunk survives under the renamed directory.
        let new_file = new_dir.join("paper.txt");
        let results = idx.query(&[1.0, 0.0, 0.0], 1).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].file_path, canon(&new_file));

        // Keyed under the new path only.
        idx.remove_file(&old_file).unwrap();
        assert_eq!(idx.query(&[1.0, 0.0, 0.0], 1).unwrap().len(), 1);
        idx.remove_file(&new_file).unwrap();
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
            dimension: 1,
            active_root: None,
            active_root_id: None,
        };

        let abs = root.join("subdir/file.txt");
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(&abs, "hello").unwrap();
        let rel = index.path_key_for_existing_path(&abs);
        assert_eq!(rel, canon(&abs));

        let abs2 = index.key_to_display_path(&abs.to_string_lossy());
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
        assert_eq!(global[0].file_path, canon(&other_path));

        let eligible = std::collections::HashSet::from([canon(&scoped_path)]);
        let filtered = idx
            .query_scoped_filtered(&[1.0, 0.0], 1, SemanticQueryScope::Corpus, Some(&eligible))
            .unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].file_path, canon(&scoped_path));

        let scoped = idx
            .query_scoped(&[1.0, 0.0], 1, SemanticQueryScope::File(&scoped_path))
            .unwrap();
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].file_path, canon(&scoped_path));
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
            .related_documents(root, &source, 10, &["txt".to_string()], false)
            .unwrap();

        assert_eq!(
            related
                .iter()
                .map(|doc| doc.entry.path.clone())
                .collect::<Vec<_>>(),
            vec![canon(&close), canon(&far)]
        );
        assert!(related[0].score > related[1].score);
        assert!(related[0].entry.size_bytes > 0);

        let eligible = std::collections::HashSet::from([canon(&far)]);
        let filtered = idx
            .related_documents_filtered(
                root,
                &source,
                1,
                &["txt".to_string()],
                false,
                Some(&eligible),
            )
            .unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].entry.path, canon(&far));
    }

    #[test]
    fn test_related_documents_missing_source_errors() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let idx = SemanticIndex::create(root, "m", 2, EmbeddingEngine::Candle, Some(root)).unwrap();
        let source = root.join("source.txt");
        fs::write(&source, "content").unwrap();

        let err = idx
            .related_documents(root, &source, 10, &["txt".to_string()], false)
            .unwrap_err();

        assert!(err.to_string().contains("not present"));
    }

    #[test]
    fn test_related_documents_can_search_the_whole_index() {
        let dir = tempdir().unwrap();
        let first_root = dir.path().join("first");
        let second_root = dir.path().join("second");
        fs::create_dir_all(&first_root).unwrap();
        fs::create_dir_all(&second_root).unwrap();
        let source = first_root.join("source.txt");
        let other = second_root.join("other.txt");
        let stale = second_root.join("stale.txt");
        fs::write(&source, "source").unwrap();
        fs::write(&other, "other").unwrap();
        fs::write(&stale, "stale").unwrap();

        let mut idx = SemanticIndex::create(
            dir.path(),
            "m",
            2,
            EmbeddingEngine::Candle,
            Some(&first_root),
        )
        .unwrap();
        let prepared = |path: &Path, text: &str| PreparedFile {
            path: path.to_path_buf(),
            chunks: vec![(
                Chunk {
                    file_path: path.to_path_buf(),
                    text: text.to_string(),
                    byte_range: ByteRange {
                        start: 0,
                        end: text.len(),
                    },
                    origin: SourceOrigin::TextFile { line: 1, col: 1 },
                },
                vec![1.0, 0.0],
            )],
        };
        idx.write_file(prepared(&source, "source")).unwrap();
        idx.activate_root(&second_root).unwrap();
        idx.write_file(prepared(&other, "other")).unwrap();
        idx.write_file(prepared(&stale, "stale")).unwrap();
        fs::remove_file(&stale).unwrap();

        let current = idx
            .related_documents(&first_root, &source, 10, &["txt".to_string()], false)
            .unwrap();
        let whole_library = idx
            .related_documents(&first_root, &source, 10, &["txt".to_string()], true)
            .unwrap();

        assert!(current.is_empty());
        assert_eq!(whole_library.len(), 1);
        assert_eq!(whole_library[0].entry.path, canon(&other));
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
        assert_eq!(results[0].file_path, canon(&path));
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
        assert_eq!(results[0].file_path, canon(&new));
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
        assert_eq!(results[0].file_path, canon(&path));
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
    fn test_build_second_root_preserves_first_root() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path().join("data");
        let root_a = dir.path().join("a");
        let root_b = dir.path().join("b");
        fs::create_dir_all(&root_a).unwrap();
        fs::create_dir_all(&root_b).unwrap();
        fs::write(root_a.join("a.txt"), "alpha").unwrap();
        fs::write(root_b.join("b.txt"), "beta").unwrap();

        let registry = ExtractorRegistry::new();
        let embedder = CountingEmbedder::new();
        let indexing = txt_indexing();
        let (tx, _rx) = tokio::sync::mpsc::channel(10);

        SemanticIndex::build(
            &data_dir,
            &root_a,
            &[root_a.join("a.txt")],
            &registry,
            &embedder,
            tx.clone(),
            Arc::new(AtomicBool::new(false)),
            &indexing,
        )
        .unwrap();
        let idx = SemanticIndex::build(
            &data_dir,
            &root_b,
            &[root_b.join("b.txt")],
            &registry,
            &embedder,
            tx,
            Arc::new(AtomicBool::new(false)),
            &indexing,
        )
        .unwrap();

        assert_eq!(idx.status().indexed_files, 2);
        assert_eq!(idx.status_for_root(Some(&root_a)).indexed_files, 1);
        assert_eq!(idx.status_for_root(Some(&root_b)).indexed_files, 1);
    }

    #[test]
    fn test_overlapping_roots_share_file_rows_but_keep_root_coverage() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("root");
        let nested = root.join("nested");
        fs::create_dir_all(&nested).unwrap();
        let shared = nested.join("shared.txt");
        fs::write(&shared, "shared").unwrap();

        let registry = ExtractorRegistry::new();
        let embedder = CountingEmbedder::new();
        let indexing = txt_indexing();
        let (tx, _rx) = tokio::sync::mpsc::channel(10);

        let idx = SemanticIndex::build(
            dir.path(),
            &root,
            std::slice::from_ref(&shared),
            &registry,
            &embedder,
            tx.clone(),
            Arc::new(AtomicBool::new(false)),
            &indexing,
        )
        .unwrap();
        assert_eq!(idx.status().indexed_files, 1);
        assert_eq!(embedder.calls(), 1);

        let mut idx = SemanticIndex::build(
            dir.path(),
            &nested,
            std::slice::from_ref(&shared),
            &registry,
            &embedder,
            tx,
            Arc::new(AtomicBool::new(false)),
            &indexing,
        )
        .unwrap();

        assert_eq!(idx.status().indexed_files, 1);
        assert_eq!(idx.status_for_root(Some(&root)).indexed_files, 1);
        assert_eq!(idx.status_for_root(Some(&nested)).indexed_files, 1);
        assert_eq!(embedder.calls(), 1);

        idx.delete_root(&nested).unwrap();
        assert_eq!(idx.status().indexed_files, 1);
        assert_eq!(idx.status_for_root(Some(&root)).indexed_files, 1);
        assert_eq!(idx.status_for_root(Some(&nested)).indexed_files, 0);
    }

    #[test]
    fn test_root_query_is_scoped_by_root_files() {
        let dir = tempdir().unwrap();
        let root_a = dir.path().join("a");
        let root_b = dir.path().join("b");
        fs::create_dir_all(&root_a).unwrap();
        fs::create_dir_all(&root_b).unwrap();
        let a = root_a.join("a.txt");
        let b = root_b.join("b.txt");
        fs::write(&a, "alpha").unwrap();
        fs::write(&b, "beta").unwrap();

        let mut idx =
            SemanticIndex::create(dir.path(), "m", 2, EmbeddingEngine::Candle, Some(&root_a))
                .unwrap();
        idx.write_file(PreparedFile {
            path: a.clone(),
            chunks: vec![(
                test_chunk(&a, "close outside requested root"),
                vec![1.0, 0.0],
            )],
        })
        .unwrap();
        idx.activate_root(&root_b).unwrap();
        idx.write_file(PreparedFile {
            path: b.clone(),
            chunks: vec![(test_chunk(&b, "inside requested root"), vec![0.5, 0.5])],
        })
        .unwrap();

        let root_results = idx
            .query_scoped(&[1.0, 0.0], 10, SemanticQueryScope::Root(&root_b))
            .unwrap();
        assert_eq!(root_results.len(), 1);
        assert_eq!(root_results[0].file_path, canon(&b));

        let corpus_results = idx.query(&[1.0, 0.0], 1).unwrap();
        assert_eq!(corpus_results[0].file_path, canon(&a));
    }

    #[test]
    fn test_cancelled_build_preserves_existing_root_coverage() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path().join("data");
        let root = dir.path().join("root");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("doc.txt");
        fs::write(&path, "old content").unwrap();

        let registry = ExtractorRegistry::new();
        let embedder = CountingEmbedder::new();
        let indexing = txt_indexing();
        let (tx, _rx) = tokio::sync::mpsc::channel(10);

        let mut idx = SemanticIndex::create(
            &data_dir,
            "counting",
            1,
            EmbeddingEngine::Candle,
            Some(&root),
        )
        .unwrap();
        idx.write_file(PreparedFile {
            path: path.clone(),
            chunks: vec![(test_chunk(&path, "old content"), vec![1.0])],
        })
        .unwrap();
        drop(idx);

        fs::write(&path, "new content").unwrap();
        let cancelled = Arc::new(AtomicBool::new(true));
        let result = SemanticIndex::build(
            &data_dir,
            &root,
            std::slice::from_ref(&path),
            &registry,
            &embedder,
            tx,
            cancelled,
            &indexing,
        );
        let err = match result {
            Ok(_) => panic!("expected cancelled build to fail"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("cancelled"));

        let idx = SemanticIndex::open(&data_dir, "counting", 1).unwrap();
        assert_eq!(idx.status_for_root(Some(&root)).indexed_files, 1);
        let results = idx
            .query_scoped(&[1.0], 1, SemanticQueryScope::Root(&root))
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].chunk_text, "old content");
    }

    #[test]
    fn test_cancelled_overlapping_root_retries_without_reembedding() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("root");
        let nested = root.join("nested");
        fs::create_dir_all(&nested).unwrap();
        let shared = nested.join("shared.txt");
        fs::write(&shared, "shared").unwrap();

        let registry = ExtractorRegistry::new();
        let embedder = CountingEmbedder::new();
        let indexing = txt_indexing();
        let (tx, _rx) = tokio::sync::mpsc::channel(10);

        SemanticIndex::build(
            dir.path(),
            &root,
            std::slice::from_ref(&shared),
            &registry,
            &embedder,
            tx.clone(),
            Arc::new(AtomicBool::new(false)),
            &indexing,
        )
        .unwrap();
        assert_eq!(embedder.calls(), 1);

        let cancelled = SemanticIndex::build(
            dir.path(),
            &nested,
            std::slice::from_ref(&shared),
            &registry,
            &embedder,
            tx.clone(),
            Arc::new(AtomicBool::new(true)),
            &indexing,
        );
        assert!(cancelled.is_err());

        let idx = SemanticIndex::open(dir.path(), "counting", 1).unwrap();
        assert_eq!(idx.status_for_root(Some(&root)).indexed_files, 1);
        assert_eq!(idx.status_for_root(Some(&nested)).indexed_files, 0);
        assert_eq!(embedder.calls(), 1);
        drop(idx);

        let idx = SemanticIndex::build(
            dir.path(),
            &nested,
            std::slice::from_ref(&shared),
            &registry,
            &embedder,
            tx,
            Arc::new(AtomicBool::new(false)),
            &indexing,
        )
        .unwrap();
        assert_eq!(idx.status_for_root(Some(&root)).indexed_files, 1);
        assert_eq!(idx.status_for_root(Some(&nested)).indexed_files, 1);
        assert_eq!(embedder.calls(), 1);
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

/// A complete indexed chunk row used by corpus-wide consumers such as the
/// topic cloud. Unlike `IndexedChunk`, this is not an ANN result and therefore
/// carries the stored vector and stable database ids.
#[derive(Clone, Debug)]
pub struct TopicChunkData {
    pub chunk_id: i64,
    pub file_id: i64,
    pub file_path: PathBuf,
    pub chunk_text: String,
    pub extraction_byte_range: ByteRange,
    pub origin: SourceOrigin,
    pub embedding: Vec<f32>,
}

#[derive(Clone, Debug)]
struct IndexedFileRecord {
    path_key: PathBuf,
    identity: FileIdentity,
}

pub enum SemanticQueryScope<'a> {
    Corpus,
    Root(&'a Path),
    File(&'a Path),
}

// ── SemanticIndex ─────────────────────────────────────────────────────────────

pub struct SemanticIndex {
    conn: Connection,
    dimension: usize,
    active_root: Option<PathBuf>,
    active_root_id: Option<i64>,
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
            schema_version = SCHEMA_VERSION;
        }
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

        Ok(Self {
            conn,
            dimension: stored_dimension,
            active_root: None,
            active_root_id: None,
        })
    }

    pub fn open_for_maintenance(data_dir: &Path) -> anyhow::Result<Self> {
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
            schema_version = SCHEMA_VERSION;
        }
        anyhow::ensure!(
            schema_version == SCHEMA_VERSION,
            "Index schema version {} is not supported (expected {}); rebuild the index",
            schema_version,
            SCHEMA_VERSION
        );
        let dimension = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'dimension'",
                [],
                |row| {
                    let s: String = row.get(0)?;
                    Ok(s.parse::<usize>().unwrap_or(0))
                },
            )
            .unwrap_or(0);
        Ok(Self {
            conn,
            dimension,
            active_root: None,
            active_root_id: None,
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

        let built_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('built_at', ?1)",
            params![built_at.to_string()],
        )?;

        let mut index = Self {
            conn,
            dimension,
            active_root: None,
            active_root_id: None,
        };
        if let Some(root) = root_path {
            index.activate_root(root)?;
        }
        Ok(index)
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

        // Reuse compatible global embeddings while keeping the temporary index
        // as the atomic root-membership boundary.
        let reusable = Self::open(data_dir, embedder.model_id(), embedder.dimension()).ok();

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

            if let Some(source) = reusable.as_ref() {
                match idx.reuse_unchanged_file_from(source, path) {
                    Ok(true) => continue,
                    Ok(false) => {}
                    Err(e) => error!(
                        "[SemanticIndex::build] could not reuse {}: {e:#}",
                        path.display()
                    ),
                }
            }

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

        idx.finish_active_root_build()?;

        let _ = tx.blocking_send(EmbedProgress::Build(IndexBuildProgress {
            files_processed: total_files,
            total_files,
            message: "Done!".to_string(),
            done: true,
        }));

        let mut live = match Self::open(data_dir, embedder.model_id(), embedder.dimension()) {
            Ok(mut live) => {
                live.merge_root_from(&idx, root_path)?;
                drop(idx);
                let _ = std::fs::remove_file(&tmp_path);
                let _ = std::fs::remove_file(data_dir.join("semantic_index.db.tmp-wal"));
                let _ = std::fs::remove_file(data_dir.join("semantic_index.db.tmp-shm"));
                live
            }
            Err(err) => {
                tracing::info!(
                    "SemanticIndex::build: replacing index because existing DB could not be reused: {err:#}"
                );
                let model_id = embedder.model_id().to_string();
                let dimension = embedder.dimension();
                drop(idx);

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
                Self::open(data_dir, &model_id, dimension)?
            }
        };
        live.activate_root(root_path)?;
        Ok(live)
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
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('schema_version', ?1)",
            params![SCHEMA_VERSION.to_string()],
        )?;
        Ok(())
    }

    fn migrate_v2_to_v3(conn: &Connection) -> anyhow::Result<()> {
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

    /// Copy an unchanged file's chunks and embeddings from another compatible
    /// index. Returns `false` when reuse is unsafe so the caller can embed it.
    fn reuse_unchanged_file_from(
        &mut self,
        source: &SemanticIndex,
        path: &Path,
    ) -> anyhow::Result<bool> {
        let key = Self::canonical_path(path);
        let key_str = key.to_string_lossy().into_owned();
        let identity = Self::identity_for_path(path)?;
        let source_file = source
            .conn
            .query_row(
                "SELECT id, size_bytes, modified_at_ms
                 FROM files
                 WHERE file_path = ?1",
                params![key_str],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((source_file_id, size_bytes, modified_at_ms)) = source_file else {
            return Ok(false);
        };
        if size_bytes != identity.size_bytes || modified_at_ms != identity.modified_at_ms {
            return Ok(false);
        }

        let mut stmt = source.conn.prepare(
            "SELECT c.byte_start, c.byte_end,
                    c.origin_type, c.page, c.line, c.col,
                    c.bbox_x, c.bbox_y, c.bbox_w, c.bbox_h, c.chunk_text,
                    v.embedding
             FROM chunks c
             JOIN vec_chunks v ON v.rowid = c.id
             WHERE c.file_id = ?1
             ORDER BY c.chunk_idx",
        )?;
        let rows = stmt
            .query_map(params![source_file_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                    row.get::<_, Option<f64>>(6)?,
                    row.get::<_, Option<f64>>(7)?,
                    row.get::<_, Option<f64>>(8)?,
                    row.get::<_, Option<f64>>(9)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, Vec<u8>>(11)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        if rows.is_empty() {
            return Ok(false);
        }

        let mut chunks = Vec::with_capacity(rows.len());
        for (
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
            text,
            embedding_blob,
        ) in rows
        {
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
                return Ok(false);
            };
            let embedding = f32_slice_from_bytes(&embedding_blob)?;
            if embedding.len() != self.dimension {
                return Ok(false);
            }
            chunks.push((
                Chunk {
                    file_path: path.to_path_buf(),
                    text,
                    byte_range: ByteRange {
                        start: byte_start as usize,
                        end: byte_end as usize,
                    },
                    origin,
                },
                embedding,
            ));
        }

        self.write_file(PreparedFile {
            path: path.to_path_buf(),
            chunks,
        })?;
        Ok(true)
    }

    fn canonical_path(path: &Path) -> PathBuf {
        std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
    }

    fn path_key_for_existing_path(&self, path: &Path) -> PathBuf {
        Self::canonical_path(path)
    }

    fn path_key_for_known_path(&self, path: &Path) -> PathBuf {
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

    fn key_to_display_path(&self, stored: &str) -> PathBuf {
        PathBuf::from(stored)
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

    fn delete_chunks_for_file_tx(
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
        Ok(())
    }

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

    fn root_id_for_path(&self, root: &Path) -> anyhow::Result<Option<i64>> {
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

    fn prune_unreferenced_files(&mut self) -> anyhow::Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM files
             WHERE id NOT IN (SELECT DISTINCT file_id FROM root_files)",
            [],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn merge_root_from(&mut self, source: &SemanticIndex, root: &Path) -> anyhow::Result<()> {
        let target_root_id = self.activate_root(root)?;
        let source_root_id = source.root_id_for_path(root)?.ok_or_else(|| {
            anyhow::anyhow!("Source index has no coverage for {}", root.display())
        })?;

        let mut source_stmt = source.conn.prepare(
            "SELECT f.id, f.file_path, f.size_bytes, f.modified_at_ms, f.indexed_at_ms
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
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM root_files WHERE root_id = ?1",
            params![target_root_id],
        )?;

        for (source_file_id, file_path, size_bytes, modified_at_ms, indexed_at_ms) in source_files {
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
                     SET size_bytes = ?2, modified_at_ms = ?3, indexed_at_ms = ?4
                     WHERE id = ?1",
                    params![file_id, size_bytes, modified_at_ms, indexed_at_ms],
                )?;
                file_id
            } else {
                tx.execute(
                    "INSERT INTO files (file_path, size_bytes, modified_at_ms, indexed_at_ms)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![file_path, size_bytes, modified_at_ms, indexed_at_ms],
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
                        v.embedding
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
                        row.get::<_, Vec<u8>>(12)?,
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
                embedding,
            ) in chunks
            {
                tx.execute(
                    "INSERT INTO chunks (file_id, chunk_idx, byte_start, byte_end,
                                         origin_type, page, line, col,
                                         bbox_x, bbox_y, bbox_w, bbox_h, chunk_text)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
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
                    ],
                )?;
                let chunk_id = tx.last_insert_rowid();
                tx.execute(
                    "INSERT INTO vec_chunks(rowid, embedding) VALUES (?1, ?2)",
                    params![chunk_id, embedding],
                )?;
            }
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

    fn finish_active_root_build(&mut self) -> anyhow::Result<()> {
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

    fn indexed_files_for_root(&self, root_id: i64) -> anyhow::Result<Vec<IndexedFileRecord>> {
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
                 SET size_bytes = ?2, modified_at_ms = ?3, indexed_at_ms = ?4
                 WHERE id = ?1",
                params![file_id, identity.size_bytes, identity.modified_at_ms, now],
            )?;
            file_id
        } else {
            tx.execute(
                "INSERT INTO files (file_path, size_bytes, modified_at_ms, indexed_at_ms)
                 VALUES (?1, ?2, ?3, ?4)",
                params![key_str, identity.size_bytes, identity.modified_at_ms, now],
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
    fn rekey_descendant_files_tx(
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

    /// Apply document eligibility before the caller's top-k boundary. The
    /// current index implementations already scan in Rust for root/file scope;
    /// corpus scope requests all vector candidates only when a collection is
    /// active, then performs the single authoritative eligibility cut.
    pub fn query_scoped_filtered(
        &self,
        embedding: &[f32],
        top_k: usize,
        scope: SemanticQueryScope<'_>,
        eligible_paths: Option<&std::collections::HashSet<PathBuf>>,
    ) -> anyhow::Result<Vec<IndexedChunk>> {
        let Some(eligible_paths) = eligible_paths else {
            return self.query_scoped(embedding, top_k, scope);
        };
        let mut results = self.query_scoped(embedding, 0, scope)?;
        results.retain(|chunk| eligible_paths.contains(&chunk.file_path));
        if top_k > 0 {
            results.truncate(top_k);
        }
        Ok(results)
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
            if eligible_paths.is_some_and(|eligible| !eligible.contains(&abs_path)) {
                continue;
            }
            let Some(file_type) = FileType::detect(&abs_path, supported_extensions) else {
                continue;
            };
            let candidate_centroid = centroid(&acc.sum, acc.chunks);
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

    /// Bulk-read all chunk vectors and passage metadata belonging to one
    /// indexed root. This is deliberately separate from ANN querying: topic
    /// discovery needs the bounded input set itself, not nearest neighbours to
    /// a query vector.
    pub fn topic_chunks_for_root(&self, root: &Path) -> anyhow::Result<Vec<TopicChunkData>> {
        let Some(root_id) = self.root_id_for_path(root)? else {
            return Ok(Vec::new());
        };
        let mut stmt = self.conn.prepare(
            "SELECT c.id, f.id, f.file_path, c.byte_start, c.byte_end,
                    c.origin_type, c.page, c.line, c.col,
                    c.bbox_x, c.bbox_y, c.bbox_w, c.bbox_h, c.chunk_text,
                    v.embedding
             FROM root_files rf
             JOIN files f ON f.id = rf.file_id
             JOIN chunks c ON c.file_id = f.id
             JOIN vec_chunks v ON v.rowid = c.id
             WHERE rf.root_id = ?1
             ORDER BY f.file_path, c.chunk_idx, c.id",
        )?;
        let rows = stmt.query_map(params![root_id], |row| {
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

    fn query_root(
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
            schema_version = SCHEMA_VERSION;
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
        Ok(())
    }
}

// ── Vector utilities ──────────────────────────────────────────────────────────

fn f32_slice_to_bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

fn f32_slice_from_bytes(bytes: &[u8]) -> anyhow::Result<Vec<f32>> {
    anyhow::ensure!(
        bytes.len().is_multiple_of(std::mem::size_of::<f32>()),
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

#[allow(clippy::too_many_arguments)]
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
