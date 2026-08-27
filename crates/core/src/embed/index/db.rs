use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use ignore::WalkBuilder;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, ToSql};
use tracing::error;

use crate::extract::ExtractorRegistry;
use crate::metadata::cache::FileIdentity;
use crate::types::{
    BoundingBox, ByteRange, ChunkTopicMember, EmbeddingEngine, FileType, IndexStatus,
    IndexingConfig, RelatedDocument, SourceMap, SourceOrigin, SourceSegment,
};

use super::super::Embedder;
use super::chunk::{chunk_content, ensure_chunks_reconstruct, Chunk};
use crate::embed::identity::{
    chunk_ref, rendition_id, sha256_bytes, sha256_file, snapshot_id, ChunkDescriptor,
};
use crate::embed::{
    ChunkRef, DocumentSnapshotId, EmbeddingSpaceId, EmbeddingSpaceIdentity, ExtractionRecipe,
    IndexEmbeddingMetadata, RenditionId,
};
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

fn replacement_backup_path(data_dir: &Path) -> PathBuf {
    data_dir.join("semantic_index.db.replacement-backup")
}

fn remove_sqlite_sidecars(path: &Path) {
    let mut wal = path.as_os_str().to_owned();
    wal.push("-wal");
    let mut shm = path.as_os_str().to_owned();
    shm.push("-shm");
    let _ = std::fs::remove_file(wal);
    let _ = std::fs::remove_file(shm);
}

/// Complete or roll back a replacement interrupted after the old database was
/// moved aside. A fully created temporary database is renamed before the
/// backup is removed, so every crash point retains at least one complete copy.
fn recover_interrupted_index_replacement(data_dir: &Path) -> anyhow::Result<()> {
    let final_path = db_path(data_dir);
    let backup_path = replacement_backup_path(data_dir);
    if !backup_path.exists() {
        return Ok(());
    }
    if !final_path.exists() {
        std::fs::rename(&backup_path, &final_path).with_context(|| {
            format!(
                "Failed to restore interrupted index replacement from {}",
                backup_path.display()
            )
        })?;
        return Ok(());
    }

    let replacement_is_complete = (|| -> anyhow::Result<bool> {
        let conn = Connection::open(&final_path)?;
        configure_connection(&conn, &final_path)?;
        let quick_check: String = conn.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
        let schema_version: Option<String> = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        Ok(quick_check == "ok"
            && schema_version
                .as_deref()
                .and_then(|version| version.parse::<i64>().ok())
                == Some(SCHEMA_VERSION))
    })()
    .unwrap_or(false);

    if replacement_is_complete {
        std::fs::remove_file(&backup_path).with_context(|| {
            format!(
                "Failed to finish index replacement by removing {}",
                backup_path.display()
            )
        })?;
    } else {
        std::fs::remove_file(&final_path).with_context(|| {
            format!(
                "Failed to discard incomplete replacement {}",
                final_path.display()
            )
        })?;
        remove_sqlite_sidecars(&final_path);
        std::fs::rename(&backup_path, &final_path).with_context(|| {
            format!(
                "Failed to roll back index replacement from {}",
                backup_path.display()
            )
        })?;
    }
    Ok(())
}

const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(3);
const SCHEMA_VERSION: i64 = 10;

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
        fn embedding_space_identity(&self) -> crate::embed::EmbeddingSpaceIdentity {
            crate::embed::EmbeddingSpaceIdentity::for_test(
                self.engine(),
                self.model_id(),
                self.dimension(),
            )
        }

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
    fn test_open_v4_normalizes_empty_full_text_only_when_chunks_exist() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let legacy_path = root.join("legacy.txt");
        let empty_path = root.join("empty.txt");
        fs::write(&legacy_path, "legacy body").unwrap();
        fs::write(&empty_path, "").unwrap();

        let mut idx =
            SemanticIndex::create(root, "m", 1, EmbeddingEngine::Candle, Some(root)).unwrap();
        idx.write_file(PreparedFile {
            path: legacy_path.clone(),
            full_text: String::new(),
            chunks: vec![(test_chunk(&legacy_path, "legacy body"), vec![1.0])],
        })
        .unwrap();
        idx.write_file(PreparedFile {
            path: empty_path.clone(),
            full_text: String::new(),
            chunks: Vec::new(),
        })
        .unwrap();
        idx.conn
            .execute(
                "UPDATE meta SET value = '4' WHERE key = 'schema_version'",
                [],
            )
            .unwrap();
        drop(idx);

        let idx = SemanticIndex::open(root, "m", 1).unwrap();
        let stored_text = |path: &Path| -> Option<String> {
            idx.conn
                .query_row(
                    "SELECT full_text FROM files WHERE file_path = ?1",
                    params![canon(path).to_string_lossy()],
                    |row| row.get(0),
                )
                .unwrap()
        };

        assert_eq!(stored_text(&legacy_path), None);
        assert_eq!(stored_text(&empty_path), Some(String::new()));
        assert!(idx.embedding_metadata().unwrap().exact_identity.is_none());
        assert_eq!(
            idx.conn
                .query_row(
                    "SELECT value FROM meta WHERE key = 'schema_version'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "10"
        );
    }

    #[test]
    fn creating_an_index_with_an_unresolved_identity_is_refused() {
        let dir = tempdir().unwrap();
        let unresolved =
            EmbeddingSpaceIdentity::for_runtime(EmbeddingEngine::Candle, "placeholder-model", 1);
        let error = SemanticIndex::create_exact(dir.path(), &unresolved, None)
            .err()
            .expect("an unresolved identity must be refused");
        assert!(
            error.to_string().contains("UNRESOLVED_EMBEDDING_SPACE"),
            "unexpected error: {error:#}"
        );
        // Refused before anything is written, so no half-built index is left
        // for the next open to adopt.
        assert!(!db_path(dir.path()).exists());
    }

    #[test]
    fn v9_unresolved_identity_migrates_to_legacy_metadata_and_remains_locally_usable() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let file = root.join("legacy.txt");
        fs::write(&file, "legacy body").unwrap();
        let unresolved =
            EmbeddingSpaceIdentity::for_runtime(EmbeddingEngine::Candle, "legacy-model", 1);
        // Creating an index with an unresolved identity is refused now, so
        // reproduce the v9 on-disk shape the way it actually arose: a valid
        // index whose meta rows carry the placeholder a v9 runtime wrote.
        let mut index = SemanticIndex::create_exact(
            root,
            &EmbeddingSpaceIdentity::for_test(EmbeddingEngine::Candle, "legacy-model", 1),
            Some(root),
        )
        .unwrap();
        index
            .write_file(PreparedFile {
                path: file.clone(),
                full_text: "legacy body".to_string(),
                chunks: vec![(test_chunk(&file, "legacy body"), vec![1.0])],
            })
            .unwrap();
        index
            .conn
            .execute(
                "INSERT OR REPLACE INTO meta(key, value) VALUES ('embedding_space_id', ?1)",
                params![unresolved.id().0],
            )
            .unwrap();
        index
            .conn
            .execute(
                "INSERT OR REPLACE INTO meta(key, value) VALUES ('embedding_space_identity_json', ?1)",
                params![serde_json::to_string(&unresolved).unwrap()],
            )
            .unwrap();
        index
            .conn
            .execute_batch(
                "DELETE FROM meta WHERE key = 'index_embedding_metadata_json';
                 UPDATE meta SET value = '9' WHERE key = 'schema_version';",
            )
            .unwrap();
        drop(index);

        let migrated = SemanticIndex::open(root, "legacy-model", 1).unwrap();
        let metadata = migrated.embedding_metadata().unwrap();
        assert_eq!(metadata.engine, EmbeddingEngine::Candle);
        assert_eq!(metadata.model_id, "legacy-model");
        assert_eq!(metadata.dimension, 1);
        assert!(metadata.exact_identity.is_none());
        assert_eq!(migrated.query(&[1.0], 1).unwrap().len(), 1);

        let current = EmbeddingSpaceIdentity::with_artifact_revision(
            EmbeddingEngine::Candle,
            "legacy-model",
            1,
            "artifact-sha256:current".to_string(),
        );
        migrated.validate_local_embedding_space(&current).unwrap();
        assert!(migrated.validate_embedding_space(&current).is_err());
        assert!(migrated
            .verified_file_for_adoption(
                "unused",
                &ExtractionRecipe::new(100, 0),
                &current.id(),
                &file,
            )
            .unwrap()
            .is_none());
    }

    #[test]
    fn v9_resolved_identity_migrates_without_losing_exact_evidence() {
        let dir = tempdir().unwrap();
        let identity = EmbeddingSpaceIdentity::with_artifact_revision(
            EmbeddingEngine::Fastembed,
            "exact-model",
            3,
            "artifact-sha256:resolved".to_string(),
        );
        let index = SemanticIndex::create_exact(dir.path(), &identity, None).unwrap();
        index
            .conn
            .execute(
                "INSERT OR REPLACE INTO meta(key, value) VALUES ('embedding_space_id', ?1)",
                params![identity.id().0],
            )
            .unwrap();
        index
            .conn
            .execute(
                "INSERT OR REPLACE INTO meta(key, value) VALUES ('embedding_space_identity_json', ?1)",
                params![serde_json::to_string(&identity).unwrap()],
            )
            .unwrap();
        index
            .conn
            .execute_batch(
                "DELETE FROM meta WHERE key = 'index_embedding_metadata_json';
                 UPDATE meta SET value = '9' WHERE key = 'schema_version';",
            )
            .unwrap();
        drop(index);

        let migrated = SemanticIndex::open_exact(dir.path(), &identity).unwrap();
        assert_eq!(
            migrated.embedding_metadata().unwrap().exact_identity,
            Some(identity)
        );
    }

    #[test]
    fn interrupted_replacement_restores_the_preserved_index() {
        let dir = tempdir().unwrap();
        let index = SemanticIndex::create(
            dir.path(),
            "preserved-model",
            1,
            EmbeddingEngine::Candle,
            None,
        )
        .unwrap();
        drop(index);
        let final_path = db_path(dir.path());
        let backup_path = replacement_backup_path(dir.path());
        fs::rename(&final_path, &backup_path).unwrap();

        let restored = SemanticIndex::open(dir.path(), "preserved-model", 1).unwrap();
        assert_eq!(restored.status().model_id, "preserved-model");
        assert!(final_path.exists());
        assert!(!backup_path.exists());
    }

    #[test]
    fn corrupt_published_replacement_rolls_back_to_the_preserved_index() {
        let dir = tempdir().unwrap();
        let index = SemanticIndex::create(
            dir.path(),
            "preserved-model",
            1,
            EmbeddingEngine::Candle,
            None,
        )
        .unwrap();
        drop(index);
        let final_path = db_path(dir.path());
        let backup_path = replacement_backup_path(dir.path());
        fs::rename(&final_path, &backup_path).unwrap();
        fs::write(&final_path, b"incomplete replacement").unwrap();

        let restored = SemanticIndex::open(dir.path(), "preserved-model", 1).unwrap();
        assert_eq!(restored.status().model_id, "preserved-model");
        assert!(!backup_path.exists());
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
            full_text: String::new(),
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
        let backup_file = replacement_backup_path(&idx_dir);
        fs::write(&backup_file, b"stale replacement backup").unwrap();
        assert!(db_file.exists());

        idx.delete(&idx_dir).unwrap();
        assert!(!db_file.exists());
        assert!(!backup_file.exists());
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
            full_text: String::new(),
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
    fn indexed_document_for_path_serves_stored_text_and_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut idx =
            SemanticIndex::create(root, "m", 3, EmbeddingEngine::Candle, Some(root)).unwrap();

        let path = root.join("doc.txt");
        fs::write(&path, "the quick brown fox").unwrap();
        idx.write_file(PreparedFile {
            path: path.clone(),
            full_text: "the quick brown fox".to_string(),
            chunks: vec![(
                Chunk {
                    file_path: path.clone(),
                    text: "the quick brown fox".to_string(),
                    byte_range: ByteRange { start: 0, end: 19 },
                    origin: SourceOrigin::PdfPage {
                        page: 2,
                        bbox: None,
                    },
                },
                vec![1.0, 0.0, 0.0],
            )],
        })
        .unwrap();

        // Happy path: stored text and a chunk-granular source map come back, and
        // the map resolves a match offset to the chunk's page.
        let (text, map) = idx
            .indexed_document_for_path(&path)
            .unwrap()
            .expect("indexed document should be served");
        assert_eq!(text, "the quick brown fox");
        match map.resolve_range(ByteRange { start: 4, end: 9 }) {
            Some(SourceOrigin::PdfPage { page, .. }) => assert_eq!(page, 2),
            other => panic!("expected page-2 origin, got {other:?}"),
        }

        // Stale on-disk change: identity no longer matches, so the caller is told
        // to extract live rather than be served vanished content.
        fs::write(&path, "completely different content now").unwrap();
        assert!(idx.indexed_document_for_path(&path).unwrap().is_none());

        // Absent file: nothing to serve.
        assert!(idx
            .indexed_document_for_path(&root.join("missing.txt"))
            .unwrap()
            .is_none());
    }

    #[test]
    fn indexed_document_for_path_none_when_full_text_missing_or_inconsistent() {
        // Simulates a row written before schema v4: chunks exist but full_text is
        // NULL, so grep must fall back to live extraction.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut idx =
            SemanticIndex::create(root, "m", 3, EmbeddingEngine::Candle, Some(root)).unwrap();

        let path = root.join("legacy.txt");
        fs::write(&path, "legacy body").unwrap();
        idx.write_file(PreparedFile {
            path: path.clone(),
            full_text: "legacy body".to_string(),
            chunks: vec![(
                Chunk {
                    file_path: path.clone(),
                    text: "legacy body".to_string(),
                    byte_range: ByteRange { start: 0, end: 11 },
                    origin: SourceOrigin::TextFile { line: 1, col: 1 },
                },
                vec![1.0, 0.0, 0.0],
            )],
        })
        .unwrap();

        // Force the column back to NULL to emulate a pre-v4 row.
        idx.conn
            .execute("UPDATE files SET full_text = NULL", [])
            .unwrap();

        assert!(idx.indexed_document_for_path(&path).unwrap().is_none());

        // Older rebuilds coerced that NULL to an empty string while retaining
        // chunks. This is still missing text, not a genuinely empty document.
        idx.conn
            .execute("UPDATE files SET full_text = ''", [])
            .unwrap();

        assert!(idx.indexed_document_for_path(&path).unwrap().is_none());
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
            full_text: String::new(),
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
        let file_chunks = idx.topic_chunks_for_file(&root, &path).unwrap();
        assert_eq!(file_chunks.len(), 2);
        assert!(file_chunks
            .iter()
            .all(|chunk| chunk.file_path == canon(&path)));
        assert!(idx
            .topic_chunks_for_file(&root, &root.join("missing.txt"))
            .unwrap()
            .is_empty());
        assert!(idx
            .topic_chunks_for_root(&root.join("missing"))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn topic_library_coverage_spans_configured_roots_and_excludes_stale_roots() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        let other_root = dir.path().join("other-root");
        let stale_root = dir.path().join("stale-root");
        let unindexed_root = dir.path().join("unindexed-root");
        for path in [&root, &other_root, &stale_root, &unindexed_root] {
            fs::create_dir(path).unwrap();
        }
        let source = root.join("source.txt");
        let matching = root.join("matching.txt");
        let boundary = root.join("boundary.txt");
        let unrelated = root.join("unrelated.txt");
        let cross_root_match = other_root.join("cross-root-match.txt");
        let stale_match = stale_root.join("stale-match.txt");
        for path in [
            &source,
            &matching,
            &boundary,
            &unrelated,
            &cross_root_match,
            &stale_match,
        ] {
            fs::write(path, "indexed text").unwrap();
        }
        let mut index =
            SemanticIndex::create(dir.path(), "m", 2, EmbeddingEngine::Candle, Some(&root))
                .unwrap();
        index
            .write_file(PreparedFile {
                full_text: "source".into(),
                path: source.clone(),
                chunks: vec![(test_chunk(&source, "source"), vec![1.0, 0.0])],
            })
            .unwrap();
        index
            .write_file(PreparedFile {
                full_text: "matching".into(),
                path: matching.clone(),
                // More than the per-document cap still counts as one document,
                // and only the fifteen strongest passages are retained.
                chunks: (0..17)
                    .map(|number| {
                        (
                            test_chunk(&matching, &format!("matching {number:02}")),
                            vec![1.0, number as f32 * 0.01],
                        )
                    })
                    .collect(),
            })
            .unwrap();
        index
            .write_file(PreparedFile {
                full_text: "boundary".into(),
                path: boundary.clone(),
                chunks: vec![(test_chunk(&boundary, "boundary"), vec![0.8, 0.6])],
            })
            .unwrap();
        index
            .write_file(PreparedFile {
                full_text: "unrelated".into(),
                path: unrelated.clone(),
                chunks: vec![(test_chunk(&unrelated, "unrelated"), vec![0.0, 1.0])],
            })
            .unwrap();
        index.activate_root(&other_root).unwrap();
        index
            .write_file(PreparedFile {
                full_text: "cross root".into(),
                path: cross_root_match.clone(),
                chunks: vec![(test_chunk(&cross_root_match, "cross root"), vec![1.0, 0.0])],
            })
            .unwrap();
        index.activate_root(&stale_root).unwrap();
        index
            .write_file(PreparedFile {
                full_text: "stale".into(),
                path: stale_match.clone(),
                chunks: vec![(test_chunk(&stale_match, "stale"), vec![1.0, 0.0])],
            })
            .unwrap();

        let coverage = index
            .topic_library_coverage(
                &[root.clone(), root.clone(), other_root, unindexed_root],
                &source,
                &[
                    TopicCoveragePrototype {
                        mean_member_embedding: vec![1.0, 0.0],
                        cohesion: 0.8,
                    },
                    TopicCoveragePrototype {
                        mean_member_embedding: vec![0.0, 1.0],
                        cohesion: 0.9,
                    },
                ],
                &AtomicBool::new(false),
            )
            .unwrap();

        assert_eq!(coverage.eligible_document_count, 4);
        assert_eq!(coverage.related_document_counts, vec![3, 1]);
        let matching_chunks = coverage.related_chunks[0]
            .iter()
            .filter(|chunk| chunk.file_path == canon(&matching))
            .map(|chunk| chunk.chunk_text.as_str())
            .collect::<Vec<_>>();
        assert_eq!(matching_chunks.len(), 15);
        assert_eq!(matching_chunks.first(), Some(&"matching 00"));
        assert_eq!(matching_chunks.last(), Some(&"matching 14"));
        assert!(!coverage.related_chunks[0]
            .iter()
            .any(|chunk| chunk.chunk_text == "matching 15" || chunk.chunk_text == "matching 16"));
        assert_eq!(coverage.related_chunks[1].len(), 1);
        assert_eq!(coverage.related_chunks[1][0].chunk_text, "unrelated");

        let cancelled = AtomicBool::new(true);
        let error = index
            .topic_library_coverage(
                std::slice::from_ref(&root),
                &source,
                &[TopicCoveragePrototype {
                    mean_member_embedding: vec![1.0, 0.0],
                    cohesion: 0.8,
                }],
                &cancelled,
            )
            .unwrap_err();
        assert!(error.to_string().contains("cancelled"));
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
            full_text: String::new(),
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
        let (full_text, chunks) = SemanticIndex::extract_chunks(&path, &registry, 100, 10).unwrap();

        assert_eq!(full_text, "hello world");
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
            full_text: String::new(),
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
            full_text: String::new(),
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
            full_text: String::new(),
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
            .query_scoped_filtered(
                &[1.0, 0.0],
                1,
                SemanticQueryScope::Corpus,
                Some(&eligible),
                None,
            )
            .unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].file_path, canon(&scoped_path));

        let excluded = std::collections::HashSet::from([canon(&other_path)]);
        let without_other = idx
            .query_scoped_filtered(
                &[1.0, 0.0],
                1,
                SemanticQueryScope::Corpus,
                None,
                Some(&excluded),
            )
            .unwrap();
        assert_eq!(without_other.len(), 1);
        assert_eq!(without_other[0].file_path, canon(&scoped_path));

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
            full_text: String::new(),
            path: source.clone(),
            chunks: vec![
                (chunk(&source, "source one"), vec![1.0, 0.0]),
                (chunk(&source, "source two"), vec![1.0, 0.0]),
            ],
        })
        .unwrap();
        idx.write_file(PreparedFile {
            full_text: String::new(),
            path: close.clone(),
            chunks: vec![(chunk(&close, "close"), vec![0.9, 0.1])],
        })
        .unwrap();
        idx.write_file(PreparedFile {
            full_text: String::new(),
            path: far.clone(),
            chunks: vec![(chunk(&far, "far"), vec![0.0, 1.0])],
        })
        .unwrap();
        idx.write_file(PreparedFile {
            full_text: String::new(),
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
    fn test_chunk_centroids_weighs_passages_not_magnitudes() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let mut idx =
            SemanticIndex::create(root, "m", 2, EmbeddingEngine::Candle, Some(root)).unwrap();
        let doc = root.join("doc.txt");
        fs::write(&doc, "content").unwrap();

        // The long/short asymmetry the pre-normalisation exists for: a raw
        // mean of these two would land at (0.75, 0.25) and call the region
        // "mostly the first passage" on the strength of a vector norm.
        idx.write_file(PreparedFile {
            full_text: String::new(),
            path: doc.clone(),
            chunks: vec![
                (test_chunk(&doc, "long"), vec![3.0, 0.0]),
                (test_chunk(&doc, "short"), vec![0.0, 1.0]),
            ],
        })
        .unwrap();

        let ids: Vec<i64> = idx
            .topic_chunks_for_file(root, &doc)
            .unwrap()
            .iter()
            .map(|chunk| chunk.chunk_id)
            .collect();
        assert_eq!(ids.len(), 2);

        let centroids = idx
            .chunk_centroids(&[vec![ids[0], ids[1]], vec![ids[0]]])
            .unwrap();
        assert_eq!(centroids.len(), 2);
        let halfway = std::f32::consts::FRAC_1_SQRT_2;
        assert!(
            (centroids[0][0] - halfway).abs() < 1e-5,
            "{:?}",
            centroids[0]
        );
        assert!(
            (centroids[0][1] - halfway).abs() < 1e-5,
            "{:?}",
            centroids[0]
        );
        // A group of one is that chunk's direction, magnitude discarded.
        assert!((centroids[1][0] - 1.0).abs() < 1e-5, "{:?}", centroids[1]);
    }

    #[test]
    fn managed_refs_resolve_and_aggregate_without_exposing_rowids() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("managed_sources");
        fs::create_dir_all(&root).unwrap();
        let document = root.join("document.txt");
        fs::write(&document, "east north").unwrap();
        let mut index =
            SemanticIndex::create(dir.path(), "m", 2, EmbeddingEngine::Candle, Some(&root))
                .unwrap();
        let recipe = ExtractionRecipe::new(100, 0);
        let managed = index
            .write_file_with_recipe(
                PreparedFile {
                    path: document.clone(),
                    full_text: "east north".to_string(),
                    chunks: vec![
                        (test_chunk(&document, "east"), vec![3.0, 0.0]),
                        (test_chunk(&document, "north"), vec![0.0, 4.0]),
                    ],
                },
                &recipe,
                Some(Path::new("managed_sources/document.txt")),
                Some(&serde_json::json!({"kind": "path"})),
                true,
                false,
                Some("job-1"),
            )
            .unwrap();

        assert_eq!(managed.chunks.len(), 2);
        assert_eq!(index.managed_embedding_work_totals().unwrap(), (0, 2));
        assert!(index
            .managed_document_for_import_key(
                "job-1",
                &managed.source_sha256,
                &managed.extraction_recipe_id,
            )
            .unwrap()
            .is_some());
        assert!(index
            .managed_document_for_import_key(
                "job-1",
                "different-source",
                &managed.extraction_recipe_id,
            )
            .unwrap_err()
            .to_string()
            .contains("IDEMPOTENCY_KEY_CONFLICT"));
        assert_ne!(managed.chunks[0].chunk_ref, managed.chunks[1].chunk_ref);
        let groups = vec![vec![
            managed.chunks[0].chunk_ref.clone(),
            managed.chunks[1].chunk_ref.clone(),
        ]];
        let accumulated = index.accumulate_chunk_refs(&groups).unwrap();
        assert_eq!(accumulated[0].member_count, 2);
        assert_eq!(accumulated[0].sum, vec![1.0, 1.0]);
        let resolved = index
            .managed_chunks_for_refs(&groups[0])
            .expect("stable refs resolve");
        assert_eq!(resolved[0].text, "east");
        assert!(index
            .accumulate_chunk_refs(&[vec![ChunkRef("chunk-missing".to_string())]])
            .unwrap_err()
            .to_string()
            .contains("CHUNK_REF_NOT_FOUND"));
    }

    #[test]
    fn managed_projection_reuses_structure_and_generation_but_not_vectors() {
        let canonical_dir = tempdir().unwrap();
        let canonical_root = canonical_dir.path().join("managed_sources");
        fs::create_dir_all(&canonical_root).unwrap();
        let document = canonical_root.join("document.txt");
        fs::write(&document, "canonical passage").unwrap();
        let recipe = ExtractionRecipe::new(100, 0);
        let mut canonical = SemanticIndex::create(
            canonical_dir.path(),
            "primary",
            2,
            EmbeddingEngine::Candle,
            Some(&canonical_root),
        )
        .unwrap();
        canonical
            .write_file_with_recipe(
                PreparedFile {
                    path: document.clone(),
                    chunks: vec![(test_chunk(&document, "canonical passage"), vec![1.0, 0.0])],
                    full_text: "canonical passage".to_string(),
                },
                &recipe,
                Some(Path::new("managed_sources/document.txt")),
                None,
                true,
                false,
                Some("canonical-import"),
            )
            .unwrap();

        let projection_dir = tempdir().unwrap();
        let projection_root = projection_dir.path().join("managed_sources");
        fs::create_dir_all(&projection_root).unwrap();
        let mut projection = SemanticIndex::create(
            projection_dir.path(),
            "secondary",
            2,
            EmbeddingEngine::Candle,
            Some(&projection_root),
        )
        .unwrap();
        let mut prepared = canonical
            .managed_file_structure_for_reembedding(&document, &document, &recipe)
            .unwrap()
            .unwrap();
        assert_eq!(prepared.full_text, "canonical passage");
        assert_eq!(prepared.chunks.len(), 1);
        assert!(prepared.chunks[0].1.is_empty());
        prepared.chunks[0].1 = vec![0.0, 1.0];
        projection
            .write_file_with_recipe(
                prepared,
                &recipe,
                None,
                Some(&serde_json::json!({"kind": "managed_corpus_projection"})),
                true,
                false,
                Some("projection-import"),
            )
            .unwrap();

        assert_eq!(
            canonical.managed_snapshot_sha256().unwrap(),
            projection.managed_snapshot_sha256().unwrap(),
            "membership generation excludes model-specific coordinates"
        );
        assert_eq!(
            projection.query(&[0.0, 1.0], 1).unwrap()[0].chunk_text,
            "canonical passage",
            "the projection stores its own vectors over canonical chunks"
        );
    }

    #[test]
    fn exact_whole_document_adoption_survives_source_index_deletion_and_rebuild() {
        let recipe = ExtractionRecipe::new(100, 0);
        let target_dir = tempdir().unwrap();
        let target_root = target_dir.path().join("managed_sources");
        fs::create_dir_all(&target_root).unwrap();
        let target_path = target_root.join("document.txt");
        fs::write(&target_path, "retained body").unwrap();

        let adopted =
            {
                let source_dir = tempdir().unwrap();
                let source_path = source_dir.path().join("document.txt");
                fs::write(&source_path, "retained body").unwrap();
                let mut source = SemanticIndex::create(
                    source_dir.path(),
                    "m",
                    2,
                    EmbeddingEngine::Candle,
                    Some(source_dir.path()),
                )
                .unwrap();
                source
                    .write_file_with_recipe(
                        PreparedFile {
                            path: source_path,
                            full_text: "retained body".to_string(),
                            chunks: vec![
                                (test_chunk(&target_path, "retained"), vec![1.0, 0.0]),
                                (test_chunk(&target_path, "body"), vec![0.0, 1.0]),
                            ],
                        },
                        &recipe,
                        None,
                        None,
                        false,
                        false,
                        None,
                    )
                    .unwrap();
                let space = source.embedding_space_id().unwrap();
                assert!(source
                    .verified_file_for_adoption(
                        &sha256_file(&target_path).unwrap(),
                        &ExtractionRecipe::new(50, 0),
                        &space,
                        &target_path,
                    )
                    .unwrap()
                    .is_none());
                assert!(source
                    .verified_file_for_adoption(
                        &sha256_file(&target_path).unwrap(),
                        &recipe,
                        &EmbeddingSpaceIdentity::for_runtime(
                            EmbeddingEngine::Candle,
                            "other-model",
                            2,
                        )
                        .id(),
                        &target_path,
                    )
                    .unwrap()
                    .is_none());
                source
                    .verified_file_for_adoption(
                        &sha256_file(&target_path).unwrap(),
                        &recipe,
                        &space,
                        &target_path,
                    )
                    .unwrap()
                    .expect("exact rendition is adoptable")
            };

        let mut target = SemanticIndex::create(
            target_dir.path(),
            "m",
            2,
            EmbeddingEngine::Candle,
            Some(&target_root),
        )
        .unwrap();
        let first = target
            .write_file_with_recipe(
                adopted,
                &recipe,
                Some(Path::new("managed_sources/document.txt")),
                Some(&serde_json::json!({"kind": "wilkes_file"})),
                true,
                true,
                Some("adoption-job"),
            )
            .unwrap();
        let refs: Vec<_> = first
            .chunks
            .iter()
            .map(|chunk| chunk.chunk_ref.clone())
            .collect();
        assert_eq!(target.managed_embedding_work_totals().unwrap(), (2, 0));
        assert_eq!(target.managed_chunks_for_refs(&refs).unwrap().len(), 2);

        let rebuilt_dir = tempdir().unwrap();
        let rebuilt_root = rebuilt_dir.path().join("managed_sources");
        fs::create_dir_all(&rebuilt_root).unwrap();
        let rebuilt_path = rebuilt_root.join("document.txt");
        fs::write(&rebuilt_path, "retained body").unwrap();
        let mut rebuilt = SemanticIndex::create(
            rebuilt_dir.path(),
            "m",
            2,
            EmbeddingEngine::Candle,
            Some(&rebuilt_root),
        )
        .unwrap();
        let second = rebuilt
            .write_file_with_recipe(
                PreparedFile {
                    path: rebuilt_path.clone(),
                    full_text: "retained body".to_string(),
                    chunks: vec![
                        (test_chunk(&rebuilt_path, "retained"), vec![1.0, 0.0]),
                        (test_chunk(&rebuilt_path, "body"), vec![0.0, 1.0]),
                    ],
                },
                &recipe,
                Some(Path::new("managed_sources/document.txt")),
                None,
                true,
                false,
                None,
            )
            .unwrap();
        assert_eq!(
            refs,
            second
                .chunks
                .iter()
                .map(|chunk| chunk.chunk_ref.clone())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_chunk_similarity_answers_both_directions_and_leaves_the_probe_as_given() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let mut idx =
            SemanticIndex::create(root, "m", 2, EmbeddingEngine::Candle, Some(root)).unwrap();
        let doc = root.join("doc.txt");
        fs::write(&doc, "content").unwrap();
        idx.write_file(PreparedFile {
            full_text: String::new(),
            path: doc.clone(),
            chunks: vec![
                (test_chunk(&doc, "east"), vec![5.0, 0.0]),
                (test_chunk(&doc, "north"), vec![0.0, 1.0]),
            ],
        })
        .unwrap();
        let ids: Vec<i64> = idx
            .topic_chunks_for_file(root, &doc)
            .unwrap()
            .iter()
            .map(|chunk| chunk.chunk_id)
            .collect();

        let found = idx
            .chunk_similarity(
                &[
                    SimilarityProbe {
                        vector: vec![1.0, 0.0],
                        scope: vec![],
                    },
                    SimilarityProbe {
                        vector: vec![0.0, 1.0],
                        scope: vec![],
                    },
                    // Half a unit vector: a caller sending the unnormalized
                    // mean of a group wants the mean of the group's cosines
                    // back, so nothing here may renormalize it.
                    SimilarityProbe {
                        vector: vec![0.5, 0.0],
                        scope: ids.clone(),
                    },
                ],
                &ids,
            )
            .unwrap();

        assert_eq!(found.probes[0].nearest_chunk_id, Some(ids[0]));
        assert!((found.probes[0].similarity.unwrap() - 1.0).abs() < 1e-5);
        assert_eq!(found.probes[1].nearest_chunk_id, Some(ids[1]));
        // The chunk's own norm never enters: `east` is five units long and
        // still reads 1.0 against the direction it points in.
        assert_eq!(found.chunks[0].probe, 0);
        assert!((found.chunks[0].similarity - 1.0).abs() < 1e-5);
        assert_eq!(found.chunks[1].probe, 1);
        // mean over {east, north} of dot((0.5, 0), ·) = (0.5 + 0) / 2.
        let mean = found.probes[2].scope_mean.unwrap();
        assert!((mean - 0.25).abs() < 1e-5, "{mean}");
        assert_eq!(found.probes[2].scope_size, 2);
        assert!(found.probes[0].scope_mean.is_none(), "no scope, no mean");
    }

    #[test]
    fn test_chunk_similarity_refuses_stale_ids_and_wrong_dimensions() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let mut idx =
            SemanticIndex::create(root, "m", 2, EmbeddingEngine::Candle, Some(root)).unwrap();
        let doc = root.join("doc.txt");
        fs::write(&doc, "content").unwrap();
        idx.write_file(PreparedFile {
            full_text: String::new(),
            path: doc.clone(),
            chunks: vec![(test_chunk(&doc, "only"), vec![1.0, 0.0])],
        })
        .unwrap();
        let id = idx.topic_chunks_for_file(root, &doc).unwrap()[0].chunk_id;
        let probe = |scope: Vec<i64>| SimilarityProbe {
            vector: vec![1.0, 0.0],
            scope,
        };

        // A stale id in the searched set, and a stale id hiding in a scope,
        // are the same failure: a reading over the ids that happened to
        // survive, with nothing in the reply to say which.
        assert!(idx
            .chunk_similarity(&[probe(vec![])], &[id, 999_999])
            .is_err());
        let error = idx
            .chunk_similarity(&[probe(vec![999_999])], &[id])
            .expect_err("a stale scope id must refuse");
        assert!(format!("{error:#}").contains("999999"), "{error:#}");

        let error = idx
            .chunk_similarity(
                &[SimilarityProbe {
                    vector: vec![1.0, 0.0, 0.0],
                    scope: vec![],
                }],
                &[id],
            )
            .expect_err("a probe from another space must refuse");
        assert!(format!("{error:#}").contains("dimension"), "{error:#}");
    }

    #[test]
    /// Renamed from `…_preserves_probe_scale`, which pinned the opposite rule:
    /// a probe of magnitude 0.5 scored 0.5 against a chunk pointing the same
    /// way, so the field called `similarity` was cosine × ‖probe‖ and the
    /// shared `min_similarity` floor filtered a short probe harder than a long
    /// one. No caller wants that quantity — Underdog reads the value as a
    /// cosine and two of its thresholds are calibrated as cosines — and every
    /// live probe is engine-normalized, so the rule was invisible rather than
    /// load-bearing.
    #[test]
    fn test_managed_chunk_search_covers_corpus_and_scores_cosine() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let mut idx =
            SemanticIndex::create(root, "m", 2, EmbeddingEngine::Candle, Some(root)).unwrap();
        for (name, text, vector) in [
            ("east.txt", "east", vec![4.0, 0.0]),
            ("north.txt", "north", vec![0.0, 2.0]),
        ] {
            let path = root.join(name);
            fs::write(&path, text).unwrap();
            idx.write_file_with_recipe(
                PreparedFile {
                    full_text: text.to_string(),
                    path: path.clone(),
                    chunks: vec![(test_chunk(&path, text), vector)],
                },
                &ExtractionRecipe::new(100, 0),
                Some(Path::new(name)),
                None,
                true,
                false,
                None,
            )
            .unwrap();
        }

        let found = idx.managed_chunk_search(&[vec![0.5, 0.0]], 8, 0.1).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].len(), 1, "minimum similarity filters north");
        assert_eq!(found[0][0].ordinal, 0);
        assert!(
            (found[0][0].similarity - 1.0).abs() < 1e-5,
            "a probe pointing the same way as the chunk scores 1, whatever its magnitude"
        );
        let unit = idx.managed_chunk_search(&[vec![1.0, 0.0]], 8, 0.1).unwrap();
        assert!(
            (unit[0][0].similarity - found[0][0].similarity).abs() < 1e-6,
            "probe magnitude does not move the score"
        );
        idx.managed_chunk_search(&[vec![0.0, 0.0]], 8, 0.0)
            .expect_err("a probe with no direction is not a query");
        assert!(!found[0][0].chunk_ref.as_str().is_empty());
        assert!(!found[0][0].snapshot_id.as_str().is_empty());
        assert!(!found[0][0].rendition_id.as_str().is_empty());

        assert!(idx.managed_chunk_search(&[vec![1.0]], 8, 0.0).is_err());
        assert!(idx.managed_chunk_search(&[vec![1.0, 0.0]], 0, 0.0).is_err());
    }

    #[test]
    fn test_chunk_centroids_refuse_ids_the_index_does_not_hold() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let mut idx =
            SemanticIndex::create(root, "m", 2, EmbeddingEngine::Candle, Some(root)).unwrap();
        let doc = root.join("doc.txt");
        fs::write(&doc, "content").unwrap();
        idx.write_file(PreparedFile {
            full_text: String::new(),
            path: doc.clone(),
            chunks: vec![(test_chunk(&doc, "only"), vec![1.0, 0.0])],
        })
        .unwrap();
        let id = idx.topic_chunks_for_file(root, &doc).unwrap()[0].chunk_id;

        // A stale id must not quietly reduce the group to the chunks that
        // survived: that answer is a vector too, and nothing about it says so.
        let error = idx
            .chunk_centroids(&[vec![id, 999_999]])
            .expect_err("stale id must refuse");
        assert!(format!("{error:#}").contains("999999"), "{error:#}");

        assert!(idx.chunk_centroids(&[vec![]]).is_err(), "empty group");
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
            full_text: String::new(),
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
            full_text: String::new(),
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
            full_text: String::new(),
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
            full_text: String::new(),
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
            full_text: String::new(),
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
            fn embedding_space_identity(&self) -> crate::embed::EmbeddingSpaceIdentity {
                crate::embed::EmbeddingSpaceIdentity::for_test(
                    self.engine(),
                    self.model_id(),
                    self.dimension(),
                )
            }

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
    fn test_build_reembeds_legacy_rows_without_exact_identity() {
        for missing_text in [None, Some("")] {
            let dir = tempdir().unwrap();
            let data_dir = dir.path().join("data");
            let root = dir.path().join("root");
            fs::create_dir_all(&root).unwrap();
            let path = root.join("legacy.txt");
            fs::write(&path, "legacy body").unwrap();

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
                full_text: "legacy body".to_string(),
                chunks: vec![(test_chunk(&path, "legacy body"), vec![1.0])],
            })
            .unwrap();
            idx.conn
                .execute("UPDATE files SET full_text = ?1", params![missing_text])
                .unwrap();
            drop(idx);

            let registry = ExtractorRegistry::new();
            let embedder = CountingEmbedder::new();
            let (tx, _rx) = tokio::sync::mpsc::channel(10);
            let rebuilt = SemanticIndex::build(
                &data_dir,
                &root,
                std::slice::from_ref(&path),
                &registry,
                &embedder,
                tx,
                Arc::new(AtomicBool::new(false)),
                &txt_indexing(),
            )
            .unwrap();

            // Matching model/dimension and unchanged path metadata are not a
            // compatibility proof. This row lacks source/recipe/rendition
            // identity, so a fresh embedding is the safe outcome.
            assert_eq!(embedder.calls(), 1);
            let (full_text, _) = rebuilt
                .indexed_document_for_path(&path)
                .unwrap()
                .expect("rebuilt document should have stored full text");
            assert_eq!(full_text, "legacy body");
        }
    }

    #[test]
    fn test_backfill_missing_full_text_fills_only_unchanged_stale_rows() {
        use std::sync::Mutex;

        let dir = tempdir().unwrap();
        let data_dir = dir.path().join("data");
        let root = dir.path().join("root");
        fs::create_dir_all(&root).unwrap();

        // A legacy row: chunks present, full_text NULL, on-disk identity intact.
        let stale_path = root.join("stale.txt");
        fs::write(&stale_path, "stale body").unwrap();
        // A row whose file is edited after indexing: its stored text is stale, so
        // backfill must leave it for the watcher rather than persist old content.
        let changed_path = root.join("changed.txt");
        fs::write(&changed_path, "original").unwrap();

        let mut idx = SemanticIndex::create(
            &data_dir,
            "counting",
            1,
            EmbeddingEngine::Candle,
            Some(&root),
        )
        .unwrap();
        idx.write_file(PreparedFile {
            path: stale_path.clone(),
            full_text: "stale body".to_string(),
            chunks: vec![(test_chunk(&stale_path, "stale body"), vec![1.0])],
        })
        .unwrap();
        idx.write_file(PreparedFile {
            path: changed_path.clone(),
            full_text: "original".to_string(),
            chunks: vec![(test_chunk(&changed_path, "original"), vec![1.0])],
        })
        .unwrap();
        // Strip stored text from both rows to reproduce a pre-v4 index.
        idx.conn
            .execute("UPDATE files SET full_text = NULL", [])
            .unwrap();
        // Change one file on disk so its identity no longer matches the index.
        fs::write(&changed_path, "original body is now longer").unwrap();

        let index = Arc::new(Mutex::new(Some(idx)));
        let registry = ExtractorRegistry::new();

        let filled = SemanticIndex::backfill_missing_full_text(&index, &registry);
        assert_eq!(filled, 1, "only the unchanged legacy row is backfilled");

        let guard = index.lock().unwrap();
        let idx = guard.as_ref().unwrap();
        // The unchanged file's text is restored and now served from the index.
        let (text, _) = idx
            .indexed_document_for_path(&stale_path)
            .unwrap()
            .expect("stale row should now have stored full text");
        assert_eq!(text, "stale body");
        // The edited file was skipped: still no stored text, so grep stays live.
        assert!(idx
            .indexed_document_for_path(&changed_path)
            .unwrap()
            .is_none());
        drop(guard);

        // Idempotent: the filled row is no longer stale and the edited row is
        // still correctly skipped, so a second pass writes nothing.
        assert_eq!(
            SemanticIndex::backfill_missing_full_text(&index, &registry),
            0
        );
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
            full_text: String::new(),
            path: a.clone(),
            chunks: vec![(
                test_chunk(&a, "close outside requested root"),
                vec![1.0, 0.0],
            )],
        })
        .unwrap();
        idx.activate_root(&root_b).unwrap();
        idx.write_file(PreparedFile {
            full_text: String::new(),
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
            full_text: String::new(),
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
            fn embedding_space_identity(&self) -> crate::embed::EmbeddingSpaceIdentity {
                crate::embed::EmbeddingSpaceIdentity::for_test(
                    self.engine(),
                    self.model_id(),
                    self.dimension(),
                )
            }

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
            full_text: String::new(),
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
            full_text: String::new(),
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
    /// The document's full extracted text, stored verbatim so exact (grep)
    /// search can scan it without re-extracting the source file. Empty when the
    /// document yielded no text.
    pub full_text: String,
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

/// Stable, vector-free passage export used by managed-corpus consumers.
#[derive(Clone, Debug)]
pub struct ManagedChunkData {
    pub chunk_ref: ChunkRef,
    pub ordinal: usize,
    pub text: String,
    pub text_sha256: String,
    pub extraction_byte_range: ByteRange,
    pub origin: SourceOrigin,
}

#[derive(Clone, Debug)]
pub struct ManagedDocumentData {
    pub source_sha256: String,
    pub snapshot_id: DocumentSnapshotId,
    pub extraction_recipe_id: String,
    pub rendition_id: RenditionId,
    pub extracted_content_sha256: String,
    pub chunks: Vec<ManagedChunkData>,
}

#[derive(Clone, Debug)]
pub struct ChunkAccumulation {
    pub sum: Vec<f32>,
    pub member_count: usize,
}

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

const TOPIC_COVERAGE_CHUNKS_PER_DOCUMENT: usize = 15;

#[derive(Clone, Debug)]
struct RankedCoverageChunk {
    score: f32,
    chunk: ChunkTopicMember,
}

#[derive(Clone, Debug)]
struct IndexedFileRecord {
    path_key: PathBuf,
    identity: FileIdentity,
}

struct FileSemanticIdentity {
    source_sha256: String,
    snapshot_id: DocumentSnapshotId,
    extraction_recipe_id: String,
    rendition_id: RenditionId,
    extracted_content_sha256: String,
    embedding_reused_chunks: Option<usize>,
    embedding_computed_chunks: Option<usize>,
    idempotency_key: Option<String>,
    chunk_descriptors: Vec<ChunkDescriptor>,
    managed_snapshot_relative_path: Option<String>,
    original_source_provenance_json: Option<String>,
    admission_state: Option<String>,
}

#[derive(Clone, Copy)]
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

    pub fn embedding_metadata(&self) -> anyhow::Result<IndexEmbeddingMetadata> {
        let json: String = self.conn.query_row(
            "SELECT value FROM meta WHERE key = 'index_embedding_metadata_json'",
            [],
            |row| row.get(0),
        )?;
        let metadata: IndexEmbeddingMetadata = serde_json::from_str(&json)?;
        metadata.validate()?;
        Ok(metadata)
    }

    pub fn embedding_space_identity(&self) -> anyhow::Result<EmbeddingSpaceIdentity> {
        self.embedding_metadata()?
            .exact_identity
            .ok_or_else(|| anyhow::anyhow!("INDEX_EMBEDDING_IDENTITY_UNVERIFIED"))
    }

    pub fn embedding_space_id(&self) -> anyhow::Result<EmbeddingSpaceId> {
        Ok(self.embedding_space_identity()?.id())
    }

    pub fn validate_embedding_space(
        &self,
        expected: &EmbeddingSpaceIdentity,
    ) -> anyhow::Result<()> {
        let actual = self.embedding_space_identity()?;
        anyhow::ensure!(
            &actual == expected,
            "EMBEDDING_SPACE_MISMATCH: index={}, runtime={}",
            actual.id().as_str(),
            expected.id().as_str()
        );
        Ok(())
    }

    /// Validate an index for ordinary Wilkes semantic search. Exact indexes
    /// still require a full identity match; migrated legacy indexes retain the
    /// historical engine/model/dimension compatibility rule without thereby
    /// becoming eligible for managed-corpus vector reuse.
    pub fn validate_local_embedding_space(
        &self,
        expected: &EmbeddingSpaceIdentity,
    ) -> anyhow::Result<()> {
        let metadata = self.embedding_metadata()?;
        anyhow::ensure!(
            metadata.is_locally_compatible_with(expected),
            "EMBEDDING_SPACE_MISMATCH: index metadata is incompatible with runtime {}",
            expected.id().as_str()
        );
        Ok(())
    }

    pub fn managed_completeness(&self) -> anyhow::Result<(usize, usize, usize)> {
        let ready_documents = self.conn.query_row(
            "SELECT COUNT(*) FROM files WHERE admission_state = 'ready'",
            [],
            |row| row.get::<_, i64>(0),
        )? as usize;
        let required_chunks = self.conn.query_row(
            "SELECT COUNT(*) FROM chunks c JOIN files f ON f.id = c.file_id
             WHERE f.admission_state = 'ready'",
            [],
            |row| row.get::<_, i64>(0),
        )? as usize;
        let embedded_chunks = self.conn.query_row(
            "SELECT COUNT(*) FROM chunks c JOIN files f ON f.id = c.file_id
             JOIN vec_chunks v ON v.rowid = c.id
             WHERE f.admission_state = 'ready'",
            [],
            |row| row.get::<_, i64>(0),
        )? as usize;
        Ok((ready_documents, required_chunks, embedded_chunks))
    }

    /// Stable identity of the managed corpus membership represented by this
    /// projection. Embeddings and rowids are deliberately excluded: two
    /// embedding spaces are synchronized when they contain the same ready
    /// renditions and stable chunks, not when their coordinates match.
    pub fn managed_snapshot_sha256(&self) -> anyhow::Result<String> {
        fn field(target: &mut Vec<u8>, value: &str) {
            target.extend_from_slice(&(value.len() as u64).to_be_bytes());
            target.extend_from_slice(value.as_bytes());
        }

        let mut bytes = Vec::new();
        let mut stmt = self.conn.prepare(
            "SELECT f.source_sha256, f.rendition_id, c.chunk_idx,
                    c.chunk_ref, c.text_sha256
               FROM files f
               JOIN chunks c ON c.file_id = f.id
              WHERE f.admission_state = 'ready'
              ORDER BY f.source_sha256, f.rendition_id, c.chunk_idx",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        for row in rows {
            let (source, rendition, ordinal, chunk_ref, text_sha256) = row?;
            field(&mut bytes, &source);
            field(&mut bytes, &rendition);
            field(&mut bytes, &ordinal.to_string());
            field(&mut bytes, &chunk_ref);
            field(&mut bytes, &text_sha256);
        }
        Ok(sha256_bytes(&bytes))
    }

    pub fn managed_embedding_work_totals(&self) -> anyhow::Result<(usize, usize)> {
        let (reused, computed) = self.conn.query_row(
            "SELECT COALESCE(SUM(embedding_reused_chunks), 0),
                    COALESCE(SUM(embedding_computed_chunks), 0)
             FROM files WHERE admission_state = 'ready'",
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )?;
        Ok((reused as usize, computed as usize))
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
        recover_interrupted_index_replacement(data_dir)?;

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

    pub fn open_exact(data_dir: &Path, expected: &EmbeddingSpaceIdentity) -> anyhow::Result<Self> {
        let index = Self::open(data_dir, &expected.model_id, expected.dimension)?;
        index.validate_embedding_space(expected)?;
        Ok(index)
    }

    pub fn open_for_maintenance(data_dir: &Path) -> anyhow::Result<Self> {
        load_sqlite_vec();
        recover_interrupted_index_replacement(data_dir)?;
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
    ///
    /// Test-only: production code creates an index from the identity of the
    /// embedder that will fill it, never from a model name alone.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn create_at_path(
        path: &Path,
        model_id: &str,
        dimension: usize,
        engine: EmbeddingEngine,
        root_path: Option<&Path>,
    ) -> anyhow::Result<Self> {
        Self::create_at_path_exact(
            path,
            &EmbeddingSpaceIdentity::for_test(engine, model_id, dimension),
            root_path,
        )
    }

    fn create_at_path_exact(
        path: &Path,
        embedding_identity: &EmbeddingSpaceIdentity,
        root_path: Option<&Path>,
    ) -> anyhow::Result<Self> {
        // An index records its identity as exact evidence. A placeholder
        // revision names the model but not the artifacts, so no reader can
        // reproduce it — refuse to write one rather than publish an index that
        // only the process that built it can open.
        anyhow::ensure!(
            embedding_identity.is_resolved(),
            "UNRESOLVED_EMBEDDING_SPACE: refusing to create an index with artifact revision '{}'",
            embedding_identity.artifact_revision
        );
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

        Self::create_schema(&conn, embedding_identity)?;

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
            dimension: embedding_identity.dimension,
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
    ///
    /// Test-only, for the same reason as [`Self::create_at_path`].
    #[cfg(any(test, feature = "test-utils"))]
    pub fn create(
        data_dir: &Path,
        model_id: &str,
        dimension: usize,
        engine: EmbeddingEngine,
        root_path: Option<&Path>,
    ) -> anyhow::Result<Self> {
        Self::create_at_path(&db_path(data_dir), model_id, dimension, engine, root_path)
    }

    pub fn create_exact(
        data_dir: &Path,
        identity: &EmbeddingSpaceIdentity,
        root_path: Option<&Path>,
    ) -> anyhow::Result<Self> {
        Self::create_at_path_exact(&db_path(data_dir), identity, root_path)
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
        let embedding_identity = embedder.embedding_space_identity();
        let reusable = Self::open_exact(data_dir, &embedding_identity).ok();

        let mut idx = Self::create_at_path_exact(&tmp_path, &embedding_identity, Some(root_path))?;

        // Extract, embed, and write one file at a time so peak memory is bounded
        // to a single file's chunks + embeddings on top of the model weights.
        for (i, path) in paths.iter().enumerate() {
            let extraction_recipe =
                ExtractionRecipe::for_path(path, indexing.chunk_size, indexing.chunk_overlap);
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
                match idx.reuse_unchanged_file_from(source, path, extractors, &extraction_recipe) {
                    Ok(true) => continue,
                    Ok(false) => {}
                    Err(e) => error!(
                        "[SemanticIndex::build] could not reuse {}: {e:#}",
                        path.display()
                    ),
                }
            }

            let (full_text, chunks) = match Self::extract_chunks(
                path,
                extractors,
                indexing.chunk_size,
                indexing.chunk_overlap,
            ) {
                Ok((text, c)) if !c.is_empty() => (text, c),
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
                full_text,
            };
            if let Err(e) = idx.write_file_with_recipe(
                prepared,
                &extraction_recipe,
                None,
                None,
                false,
                false,
                None,
            ) {
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
        idx.validate_embedding_space(&embedding_identity)?;
        let integrity: String = idx
            .conn
            .query_row("PRAGMA quick_check", [], |row| row.get(0))?;
        anyhow::ensure!(
            integrity == "ok",
            "Temporary semantic index failed integrity check: {integrity}"
        );

        let _ = tx.blocking_send(EmbedProgress::Build(IndexBuildProgress {
            files_processed: total_files,
            total_files,
            message: "Done!".to_string(),
            done: true,
        }));

        if let Some(source) = reusable.as_ref() {
            source
                .conn
                .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        }
        drop(reusable);

        let mut live = match Self::open_exact(data_dir, &embedding_identity) {
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
                drop(idx);

                let backup_path = replacement_backup_path(data_dir);
                anyhow::ensure!(
                    !backup_path.exists(),
                    "Cannot replace index while recovery backup exists at {}",
                    backup_path.display()
                );
                remove_sqlite_sidecars(&final_path);
                if final_path.exists() {
                    std::fs::rename(&final_path, &backup_path).with_context(|| {
                        format!(
                            "Failed to preserve existing index at {}",
                            backup_path.display()
                        )
                    })?;
                }
                if let Err(rename_error) = std::fs::rename(&tmp_path, &final_path) {
                    if backup_path.exists() {
                        let _ = std::fs::rename(&backup_path, &final_path);
                    }
                    return Err(rename_error).with_context(|| {
                        format!(
                            "Failed to publish replacement index from {}",
                            tmp_path.display()
                        )
                    });
                }
                Self::open_exact(data_dir, &embedding_identity)?
            }
        };
        live.activate_root(root_path)?;
        Ok(live)
    }

    fn create_schema(conn: &Connection, identity: &EmbeddingSpaceIdentity) -> anyhow::Result<()> {
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
                admission_state TEXT
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
    fn migrate_v3_to_v4(conn: &Connection) -> anyhow::Result<()> {
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
    fn migrate_v4_to_v5(conn: &Connection) -> anyhow::Result<()> {
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
    fn migrate_v5_to_v6(conn: &Connection) -> anyhow::Result<()> {
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
    fn migrate_v6_to_v7(conn: &Connection) -> anyhow::Result<()> {
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
    fn migrate_v7_to_v8(conn: &Connection) -> anyhow::Result<()> {
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
    fn migrate_v8_to_v9(conn: &Connection) -> anyhow::Result<()> {
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
    fn migrate_v9_to_v10(conn: &Connection) -> anyhow::Result<()> {
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
            .transpose()?
            .filter(EmbeddingSpaceIdentity::is_resolved);
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

    /// Extract a file's canonical content without chunking or embedding it.
    fn extract_content(
        path: &Path,
        extractors: &ExtractorRegistry,
    ) -> anyhow::Result<crate::types::ExtractedContent> {
        let content = match extractors.find(path, None) {
            Some(ext) => ext.extract(path)?,
            None => {
                // Plain-text fallback: read raw bytes.
                let text = std::fs::read_to_string(path)
                    .with_context(|| format!("Failed to read {}", path.display()))?;
                crate::types::ExtractedContent {
                    text,
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
        Ok(content)
    }

    /// Extract and chunk a file without embedding, returning both the document's
    /// full extracted text and its chunks. The text is stored verbatim so exact
    /// (grep) search can scan it without re-extracting the file.
    pub fn extract_chunks(
        path: &Path,
        extractors: &ExtractorRegistry,
        chunk_size: usize,
        chunk_overlap: usize,
    ) -> anyhow::Result<(String, Vec<Chunk>)> {
        let content = Self::extract_content(path, extractors)?;
        let chunks = chunk_content(&content, path.to_path_buf(), chunk_size, chunk_overlap);
        ensure_chunks_reconstruct(
            &content.text,
            chunks
                .iter()
                .map(|chunk| (&chunk.byte_range, chunk.text.as_str())),
        )
        .with_context(|| {
            format!(
                "DOCUMENT_INDEX_INCOMPLETE: chunks of {} do not rebuild its extracted content",
                path.display()
            )
        })?;
        Ok((content.text, chunks))
    }

    /// Extract, chunk, and embed a file without holding the index lock.
    pub fn prepare_file(
        path: &Path,
        extractors: &ExtractorRegistry,
        embedder: &dyn Embedder,
        chunk_size: usize,
        chunk_overlap: usize,
    ) -> anyhow::Result<PreparedFile> {
        let (full_text, raw_chunks) =
            Self::extract_chunks(path, extractors, chunk_size, chunk_overlap)?;
        if raw_chunks.is_empty() {
            return Ok(PreparedFile {
                path: path.to_path_buf(),
                chunks: Vec::new(),
                full_text,
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
            full_text,
        })
    }

    /// Copy an unchanged file's chunks and embeddings from another compatible
    /// index. Returns `false` when reuse is unsafe so the caller can embed it.
    fn reuse_unchanged_file_from(
        &mut self,
        source: &SemanticIndex,
        path: &Path,
        extractors: &ExtractorRegistry,
        recipe: &ExtractionRecipe,
    ) -> anyhow::Result<bool> {
        if self.embedding_space_id()? != source.embedding_space_id()? {
            return Ok(false);
        }
        let key = Self::canonical_path(path);
        let key_str = key.to_string_lossy().into_owned();
        let identity = Self::identity_for_path(path)?;
        let source_sha256 = sha256_file(path)?;
        let source_file = source
            .conn
            .query_row(
                "SELECT id, size_bytes, modified_at_ms, full_text,
                        source_sha256, extraction_recipe_id, rendition_id,
                        extracted_content_sha256
                 FROM files
                 WHERE file_path = ?1",
                params![key_str],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, Option<String>>(7)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            source_file_id,
            size_bytes,
            modified_at_ms,
            full_text,
            stored_source_sha256,
            stored_recipe_id,
            stored_rendition_id,
            stored_extracted_content_sha256,
        )) = source_file
        else {
            return Ok(false);
        };
        if size_bytes != identity.size_bytes
            || modified_at_ms != identity.modified_at_ms
            || stored_source_sha256.as_deref() != Some(source_sha256.as_str())
            || stored_recipe_id.as_deref() != Some(recipe.id().as_str())
            || stored_rendition_id.is_none()
        {
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

        // Pre-v4 rows have no stored full text. Some builds made those rows look
        // populated by coercing NULL to an empty string even though they still
        // had chunks. Extract only the missing text while retaining the valid
        // chunks and embeddings copied above.
        let full_text = match full_text {
            Some(text) if !text.is_empty() => text,
            _ => Self::extract_content(path, extractors)?.text,
        };
        if stored_extracted_content_sha256.as_deref()
            != Some(sha256_bytes(full_text.as_bytes()).as_str())
        {
            return Ok(false);
        }

        let descriptors: Vec<ChunkDescriptor> = chunks
            .iter()
            .enumerate()
            .map(|(ordinal, (chunk, _))| ChunkDescriptor {
                ordinal,
                text_sha256: sha256_bytes(chunk.text.as_bytes()),
                byte_range: chunk.byte_range.clone(),
                origin: chunk.origin.clone(),
            })
            .collect();
        let expected_rendition =
            rendition_id(&snapshot_id(&source_sha256), &recipe.id(), &descriptors);
        if stored_rendition_id.as_deref() != Some(expected_rendition.as_str()) {
            return Ok(false);
        }

        self.write_file_with_recipe(
            PreparedFile {
                path: path.to_path_buf(),
                chunks,
                full_text,
            },
            recipe,
            None,
            None,
            false,
            false,
            None,
        )?;
        Ok(true)
    }

    /// Fill `full_text` for rows that carry chunks but no stored text — legacy
    /// rows indexed before schema v4 that have neither changed on disk nor been
    /// rebuilt since, so no existing path (fresh build, build-time reuse, or the
    /// incremental watcher) has ever backfilled them. Until filled they force
    /// exact (grep) search to re-extract the file live on every query — exactly
    /// the cost the column exists to remove.
    ///
    /// Only text is written; chunks and embeddings are left untouched. Extraction
    /// runs without holding the index lock — only the initial read of the stale
    /// set and each per-file write briefly take it — so concurrent search stays
    /// responsive. A row whose file is gone or whose on-disk identity no longer
    /// matches is skipped so stale text is never stored, and the write is guarded
    /// on that identity so a concurrent incremental refresh always wins. Returns
    /// the number of rows filled.
    pub fn backfill_missing_full_text(
        index: &Arc<std::sync::Mutex<Option<SemanticIndex>>>,
        extractors: &ExtractorRegistry,
    ) -> usize {
        // Phase 1: snapshot the stale set under a brief lock, then release it so
        // extraction never blocks search.
        let stale: Vec<(i64, PathBuf, i64, i64)> = {
            let guard = match index.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            let Some(idx) = guard.as_ref() else {
                return 0;
            };
            let mut stmt = match idx.conn.prepare(
                "SELECT f.id, f.file_path, f.size_bytes, f.modified_at_ms
                   FROM files f
                  WHERE (f.full_text IS NULL OR f.full_text = '')
                    AND EXISTS (SELECT 1 FROM chunks c WHERE c.file_id = f.id)",
            ) {
                Ok(stmt) => stmt,
                Err(e) => {
                    error!("[SemanticIndex::backfill] prepare stale query: {e:#}");
                    return 0;
                }
            };
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    PathBuf::from(row.get::<_, String>(1)?),
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            });
            match rows.and_then(|r| r.collect::<Result<Vec<_>, _>>()) {
                Ok(rows) => rows,
                Err(e) => {
                    error!("[SemanticIndex::backfill] read stale rows: {e:#}");
                    return 0;
                }
            }
        };
        if stale.is_empty() {
            return 0;
        }

        let mut filled = 0usize;
        for (file_id, path, size_bytes, modified_at_ms) in stale {
            // Phase 2: extract with no lock held. Skip files that vanished or
            // changed since indexing so we never persist text that no longer
            // matches what is on disk.
            let Some(identity) = FileIdentity::for_path(&path) else {
                continue;
            };
            if identity.size_bytes != size_bytes || identity.modified_at_ms != modified_at_ms {
                continue;
            }
            let text = match Self::extract_content(&path, extractors) {
                Ok(content) => content.text,
                Err(e) => {
                    error!(
                        "[SemanticIndex::backfill] extract {}: {e:#}",
                        path.display()
                    );
                    continue;
                }
            };
            // A chunked row cannot legitimately extract to empty text; empty now
            // means the file changed underneath us, so skip rather than store an
            // empty string that would later read as "populated".
            if text.is_empty() {
                continue;
            }

            // Phase 3: write under a brief lock, guarded on the identity we read.
            // The guard makes a concurrent incremental refresh authoritative: if
            // it already rewrote the row (new identity + text), this update finds
            // no matching row and changes nothing.
            let guard = match index.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            let Some(idx) = guard.as_ref() else {
                break;
            };
            match idx.conn.execute(
                "UPDATE files
                    SET full_text = ?1
                  WHERE id = ?2
                    AND size_bytes = ?3
                    AND modified_at_ms = ?4
                    AND (full_text IS NULL OR full_text = '')",
                params![text, file_id, size_bytes, modified_at_ms],
            ) {
                Ok(changed) => filled += changed,
                Err(e) => error!("[SemanticIndex::backfill] write {}: {e:#}", path.display()),
            }
        }
        filled
    }

    /// Return a file's stored extracted text plus a chunk-granular `SourceMap`,
    /// for exact (grep) search that reads from the index instead of
    /// re-extracting. Returns `None` — so the caller falls back to live
    /// extraction — when the file is absent, has no stored `full_text` (indexed
    /// before schema v4), or has changed on disk since it was indexed (stale
    /// text must never be served as a match).
    pub fn indexed_document_for_path(
        &self,
        path: &Path,
    ) -> anyhow::Result<Option<(String, SourceMap)>> {
        let key = Self::canonical_path(path);
        let key_str = key.to_string_lossy().into_owned();

        let row = self
            .conn
            .query_row(
                "SELECT id, size_bytes, modified_at_ms, full_text
                 FROM files WHERE file_path = ?1",
                params![key_str],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((file_id, size_bytes, modified_at_ms, full_text)) = row else {
            return Ok(None);
        };
        let Some(full_text) = full_text else {
            return Ok(None);
        };

        // Extracted empty documents have no chunks. Empty text alongside chunks
        // is a legacy row produced by the old NULL-to-empty reuse bug, so do not
        // suppress the caller's live-extraction fallback.
        if full_text.is_empty() {
            let has_chunks: bool = self.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM chunks WHERE file_id = ?1)",
                params![file_id],
                |row| row.get(0),
            )?;
            if has_chunks {
                return Ok(None);
            }
        }

        // Reject stale text: a file edited after indexing would otherwise yield
        // matches against content that no longer exists on disk.
        let identity = Self::identity_for_path(path)?;
        if size_bytes != identity.size_bytes || modified_at_ms != identity.modified_at_ms {
            return Ok(None);
        }

        let mut stmt = self.conn.prepare(
            "SELECT byte_start, byte_end, origin_type, page, line, col,
                    bbox_x, bbox_y, bbox_w, bbox_h
             FROM chunks WHERE file_id = ?1 ORDER BY chunk_idx",
        )?;
        let segments = stmt
            .query_map(params![file_id], |row| {
                let byte_start = row.get::<_, i64>(0)? as usize;
                let byte_end = row.get::<_, i64>(1)? as usize;
                let origin = source_origin_from_parts(
                    &row.get::<_, String>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                    row.get::<_, Option<f64>>(6)?,
                    row.get::<_, Option<f64>>(7)?,
                    row.get::<_, Option<f64>>(8)?,
                    row.get::<_, Option<f64>>(9)?,
                );
                Ok((byte_start, byte_end, origin))
            })?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter_map(|(start, end, origin)| {
                origin.map(|origin| SourceSegment {
                    text_range: ByteRange { start, end },
                    origin,
                })
            })
            .collect();

        Ok(Some((full_text, SourceMap { segments })))
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
        anyhow::ensure!(
            self.embedding_space_id()? == source.embedding_space_id()?,
            "EMBEDDING_SPACE_MISMATCH: refusing cross-index vector copy"
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
        tx.execute(
            "DELETE FROM root_files WHERE root_id = ?1",
            params![target_root_id],
        )?;

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

    /// Write a generic workspace file. Stable managed identities are omitted
    /// unless the caller supplies the extraction recipe through
    /// [`Self::write_file_with_recipe`]; legacy callers therefore cannot
    /// accidentally authorize managed reuse with guessed metadata.
    pub fn write_file(&mut self, prepared: PreparedFile) -> anyhow::Result<()> {
        self.write_file_internal(prepared, None).map(|_| ())
    }

    /// Write a file with the complete source/rendition/chunk identity used by
    /// exact whole-document adoption and managed APIs.
    pub fn write_file_with_recipe(
        &mut self,
        prepared: PreparedFile,
        recipe: &ExtractionRecipe,
        managed_snapshot_relative_path: Option<&Path>,
        original_source_provenance: Option<&serde_json::Value>,
        admitted: bool,
        reused: bool,
        idempotency_key: Option<&str>,
    ) -> anyhow::Result<ManagedDocumentData> {
        let source_sha256 = sha256_file(&prepared.path)?;
        let snapshot = snapshot_id(&source_sha256);
        let extraction_recipe_id = recipe.id();
        let descriptors: Vec<ChunkDescriptor> = prepared
            .chunks
            .iter()
            .enumerate()
            .map(|(ordinal, (chunk, _))| ChunkDescriptor {
                ordinal,
                text_sha256: sha256_bytes(chunk.text.as_bytes()),
                byte_range: chunk.byte_range.clone(),
                origin: chunk.origin.clone(),
            })
            .collect();
        let rendition = rendition_id(&snapshot, &extraction_recipe_id, &descriptors);
        let identity = FileSemanticIdentity {
            source_sha256,
            snapshot_id: snapshot,
            extraction_recipe_id,
            rendition_id: rendition,
            extracted_content_sha256: sha256_bytes(prepared.full_text.as_bytes()),
            embedding_reused_chunks: admitted.then_some(if reused {
                prepared.chunks.len()
            } else {
                0
            }),
            embedding_computed_chunks: admitted.then_some(if reused {
                0
            } else {
                prepared.chunks.len()
            }),
            idempotency_key: idempotency_key.map(str::to_string),
            chunk_descriptors: descriptors,
            managed_snapshot_relative_path: managed_snapshot_relative_path
                .map(|path| path.to_string_lossy().into_owned()),
            original_source_provenance_json: original_source_provenance
                .map(serde_json::to_string)
                .transpose()?,
            admission_state: admitted.then(|| "ready".to_string()),
        };
        self.write_file_internal(prepared, Some(identity))?
            .ok_or_else(|| anyhow::anyhow!("DOCUMENT_INDEX_INCOMPLETE: identity was not published"))
    }

    fn write_file_internal(
        &mut self,
        prepared: PreparedFile,
        semantic_identity: Option<FileSemanticIdentity>,
    ) -> anyhow::Result<Option<ManagedDocumentData>> {
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
            anyhow::ensure!(
                embedding.iter().all(|value| value.is_finite()),
                "DOCUMENT_INDEX_INCOMPLETE: non-finite embedding for path {}",
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
                 SET size_bytes = ?2, modified_at_ms = ?3, indexed_at_ms = ?4, full_text = ?5,
                     source_sha256 = ?6, snapshot_id = ?7, extraction_recipe_id = ?8,
                     rendition_id = ?9, extracted_content_sha256 = ?10,
                     embedding_reused_chunks = ?11, embedding_computed_chunks = ?12,
                     managed_snapshot_relative_path = ?13,
                     original_source_provenance_json = ?14, admission_state = ?15
                 WHERE id = ?1",
                params![
                    file_id,
                    identity.size_bytes,
                    identity.modified_at_ms,
                    now,
                    prepared.full_text,
                    semantic_identity
                        .as_ref()
                        .map(|value| value.source_sha256.as_str()),
                    semantic_identity
                        .as_ref()
                        .map(|value| value.snapshot_id.as_str()),
                    semantic_identity
                        .as_ref()
                        .map(|value| value.extraction_recipe_id.as_str()),
                    semantic_identity
                        .as_ref()
                        .map(|value| value.rendition_id.as_str()),
                    semantic_identity
                        .as_ref()
                        .map(|value| value.extracted_content_sha256.as_str()),
                    semantic_identity
                        .as_ref()
                        .and_then(|value| value.embedding_reused_chunks),
                    semantic_identity
                        .as_ref()
                        .and_then(|value| value.embedding_computed_chunks),
                    semantic_identity
                        .as_ref()
                        .and_then(|value| value.managed_snapshot_relative_path.as_deref()),
                    semantic_identity
                        .as_ref()
                        .and_then(|value| value.original_source_provenance_json.as_deref()),
                    semantic_identity
                        .as_ref()
                        .and_then(|value| value.admission_state.as_deref()),
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
                params![
                    key_str,
                    identity.size_bytes,
                    identity.modified_at_ms,
                    now,
                    prepared.full_text,
                    semantic_identity.as_ref().map(|value| value.source_sha256.as_str()),
                    semantic_identity.as_ref().map(|value| value.snapshot_id.as_str()),
                    semantic_identity.as_ref().map(|value| value.extraction_recipe_id.as_str()),
                    semantic_identity.as_ref().map(|value| value.rendition_id.as_str()),
                    semantic_identity.as_ref().map(|value| value.extracted_content_sha256.as_str()),
                    semantic_identity.as_ref().and_then(|value| value.embedding_reused_chunks),
                    semantic_identity.as_ref().and_then(|value| value.embedding_computed_chunks),
                    semantic_identity.as_ref().and_then(|value| value.managed_snapshot_relative_path.as_deref()),
                    semantic_identity.as_ref().and_then(|value| value.original_source_provenance_json.as_deref()),
                    semantic_identity.as_ref().and_then(|value| value.admission_state.as_deref()),
                ],
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
            let descriptor = semantic_identity
                .as_ref()
                .and_then(|value| value.chunk_descriptors.get(i));
            let stable_ref = semantic_identity
                .as_ref()
                .map(|value| chunk_ref(&value.rendition_id, i));
            tx.execute(
                "INSERT INTO chunks (file_id, chunk_idx, byte_start, byte_end,
                                     origin_type, page, line, col,
                                     bbox_x, bbox_y, bbox_w, bbox_h, chunk_text,
                                     chunk_ref, text_sha256)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
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
                    stable_ref.as_ref().map(ChunkRef::as_str),
                    descriptor.map(|value| value.text_sha256.as_str()),
                ],
            )?;
            let chunk_id = tx.last_insert_rowid();
            let blob = f32_slice_to_bytes(&embedding);
            tx.execute(
                "INSERT INTO vec_chunks(rowid, embedding) VALUES (?1, ?2)",
                params![chunk_id, blob],
            )?;
        }
        if let Some(identity) = semantic_identity.as_ref() {
            if let Some(idempotency_key) = identity.idempotency_key.as_deref() {
                let existing = tx
                    .query_row(
                        "SELECT source_sha256, extraction_recipe_id, rendition_id
                         FROM managed_import_keys WHERE idempotency_key = ?1",
                        params![idempotency_key],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, String>(2)?,
                            ))
                        },
                    )
                    .optional()?;
                if let Some((source, recipe, rendition)) = existing {
                    anyhow::ensure!(
                        source == identity.source_sha256
                            && recipe == identity.extraction_recipe_id
                            && rendition == identity.rendition_id.as_str(),
                        "IDEMPOTENCY_KEY_CONFLICT: managed import key is already bound to a different rendition"
                    );
                } else {
                    tx.execute(
                        "INSERT INTO managed_import_keys
                            (idempotency_key, source_sha256, extraction_recipe_id, rendition_id, created_at_ms)
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![
                            idempotency_key,
                            identity.source_sha256,
                            identity.extraction_recipe_id,
                            identity.rendition_id.as_str(),
                            Self::now_ms(),
                        ],
                    )?;
                }
            }
        }
        tx.commit()?;
        semantic_identity
            .map(|identity| {
                self.managed_document_by_rendition(identity.rendition_id.as_str())?
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "DOCUMENT_INDEX_INCOMPLETE: published rendition cannot be read"
                        )
                    })
            })
            .transpose()
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
        self.validate_local_embedding_space(&embedder.embedding_space_identity())?;
        let prepared = Self::prepare_file(path, extractors, embedder, chunk_size, chunk_overlap)?;
        self.write_file_with_recipe(
            prepared,
            &ExtractionRecipe::for_path(path, chunk_size, chunk_overlap),
            None,
            None,
            false,
            false,
            None,
        )?;
        Ok(())
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

    pub fn managed_document_for_path(
        &self,
        path: &Path,
    ) -> anyhow::Result<Option<ManagedDocumentData>> {
        let key = self.path_key_for_known_path(path);
        let rendition: Option<String> = self
            .conn
            .query_row(
                "SELECT rendition_id FROM files
                 WHERE file_path = ?1 AND admission_state = 'ready'",
                params![key.to_string_lossy()],
                |row| row.get(0),
            )
            .optional()?;
        rendition
            .map(|rendition| self.managed_document_by_rendition(&rendition))
            .transpose()
            .map(Option::flatten)
    }

    pub fn managed_path_for_source_sha256(
        &self,
        source_sha256: &str,
    ) -> anyhow::Result<Option<PathBuf>> {
        let path: Option<String> = self
            .conn
            .query_row(
                "SELECT file_path FROM files
                 WHERE source_sha256 = ?1 AND admission_state = 'ready'
                 ORDER BY id LIMIT 1",
                params![source_sha256],
                |row| row.get(0),
            )
            .optional()?;
        Ok(path.map(PathBuf::from))
    }

    /// Read and verify the canonical, vector-free structure of one managed
    /// document for projection into another embedding space.
    ///
    /// The returned [`PreparedFile`] deliberately carries empty vectors. Its
    /// chunks and full text come from the admitted canonical rendition, so a
    /// projection can embed those exact passages without reading, extracting,
    /// or chunking the source again. The caller supplies `target_path` because
    /// projection indexes may share the canonical immutable snapshot while
    /// retaining their own file-row identity.
    pub fn managed_file_structure_for_reembedding(
        &self,
        path: &Path,
        target_path: &Path,
        expected_recipe: &ExtractionRecipe,
    ) -> anyhow::Result<Option<PreparedFile>> {
        let Some(document) = self.managed_document_for_path(path)? else {
            return Ok(None);
        };
        anyhow::ensure!(
            document.extraction_recipe_id == expected_recipe.id(),
            "DOCUMENT_INDEX_INCOMPLETE: canonical rendition uses a different extraction recipe"
        );
        let key = self.path_key_for_known_path(path);
        let full_text: Option<String> = self
            .conn
            .query_row(
                "SELECT full_text FROM files
                 WHERE file_path = ?1 AND admission_state = 'ready'",
                params![key.to_string_lossy()],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        let full_text = full_text
            .ok_or_else(|| anyhow::anyhow!("DOCUMENT_INDEX_INCOMPLETE: retained text is absent"))?;
        anyhow::ensure!(
            sha256_bytes(full_text.as_bytes()) == document.extracted_content_sha256,
            "DOCUMENT_INDEX_INCOMPLETE: extracted-content hash does not match retained text"
        );
        let chunks = document
            .chunks
            .into_iter()
            .map(|chunk| {
                (
                    Chunk {
                        file_path: target_path.to_path_buf(),
                        text: chunk.text,
                        byte_range: chunk.extraction_byte_range,
                        origin: chunk.origin,
                    },
                    Vec::new(),
                )
            })
            .collect();
        Ok(Some(PreparedFile {
            path: target_path.to_path_buf(),
            chunks,
            full_text,
        }))
    }

    pub fn managed_document_for_import_key(
        &self,
        idempotency_key: &str,
        source_sha256: &str,
        extraction_recipe_id: &str,
    ) -> anyhow::Result<Option<ManagedDocumentData>> {
        let binding = self
            .conn
            .query_row(
                "SELECT source_sha256, extraction_recipe_id, rendition_id
                 FROM managed_import_keys WHERE idempotency_key = ?1",
                params![idempotency_key],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((bound_source, bound_recipe, rendition)) = binding else {
            return Ok(None);
        };
        anyhow::ensure!(
            bound_source == source_sha256 && bound_recipe == extraction_recipe_id,
            "IDEMPOTENCY_KEY_CONFLICT: managed import key is already bound to different content"
        );
        self.managed_document_by_rendition(&rendition)
    }

    pub fn bind_managed_import_key(
        &mut self,
        idempotency_key: &str,
        document: &ManagedDocumentData,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            !idempotency_key.trim().is_empty(),
            "IDEMPOTENCY_KEY_CONFLICT: idempotency key is empty"
        );
        let existing = self
            .conn
            .query_row(
                "SELECT source_sha256, extraction_recipe_id, rendition_id
                 FROM managed_import_keys WHERE idempotency_key = ?1",
                params![idempotency_key],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        if let Some((source, recipe, rendition)) = existing {
            anyhow::ensure!(
                source == document.source_sha256
                    && recipe == document.extraction_recipe_id
                    && rendition == document.rendition_id.as_str(),
                "IDEMPOTENCY_KEY_CONFLICT: managed import key is already bound to a different rendition"
            );
            return Ok(());
        }
        self.conn.execute(
            "INSERT INTO managed_import_keys
                (idempotency_key, source_sha256, extraction_recipe_id, rendition_id, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                idempotency_key,
                document.source_sha256,
                document.extraction_recipe_id,
                document.rendition_id.as_str(),
                Self::now_ms(),
            ],
        )?;
        Ok(())
    }

    pub fn managed_document_by_rendition(
        &self,
        rendition_id: &str,
    ) -> anyhow::Result<Option<ManagedDocumentData>> {
        let header = self
            .conn
            .query_row(
                "SELECT source_sha256, snapshot_id, extraction_recipe_id, rendition_id,
                        full_text, extracted_content_sha256
                 FROM files WHERE rendition_id = ?1",
                params![rendition_id],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            source_sha256,
            snapshot,
            extraction_recipe_id,
            rendition,
            full_text,
            extracted_content_sha256,
        )) = header
        else {
            return Ok(None);
        };
        let source_sha256 = source_sha256
            .ok_or_else(|| anyhow::anyhow!("DOCUMENT_INDEX_INCOMPLETE: source hash is absent"))?;
        let snapshot = snapshot
            .ok_or_else(|| anyhow::anyhow!("DOCUMENT_INDEX_INCOMPLETE: snapshot id is absent"))?;
        let extraction_recipe_id = extraction_recipe_id.ok_or_else(|| {
            anyhow::anyhow!("DOCUMENT_INDEX_INCOMPLETE: extraction recipe id is absent")
        })?;
        let rendition = rendition
            .ok_or_else(|| anyhow::anyhow!("DOCUMENT_INDEX_INCOMPLETE: rendition id is absent"))?;
        let full_text = full_text
            .ok_or_else(|| anyhow::anyhow!("DOCUMENT_INDEX_INCOMPLETE: retained text is absent"))?;
        let extracted_content_sha256 = extracted_content_sha256.ok_or_else(|| {
            anyhow::anyhow!("DOCUMENT_INDEX_INCOMPLETE: extracted-content hash is absent")
        })?;
        anyhow::ensure!(
            sha256_bytes(full_text.as_bytes()) == extracted_content_sha256,
            "DOCUMENT_INDEX_INCOMPLETE: extracted-content hash does not match retained text"
        );
        anyhow::ensure!(
            snapshot_id(&source_sha256).as_str() == snapshot,
            "DOCUMENT_INDEX_INCOMPLETE: snapshot identity does not match source hash"
        );

        let mut stmt = self.conn.prepare(
            "SELECT c.chunk_ref, c.chunk_idx, c.chunk_text, c.text_sha256,
                    c.byte_start, c.byte_end, c.origin_type, c.page, c.line, c.col,
                    c.bbox_x, c.bbox_y, c.bbox_w, c.bbox_h, v.embedding
             FROM chunks c JOIN files f ON f.id = c.file_id
             JOIN vec_chunks v ON v.rowid = c.id
             WHERE f.rendition_id = ?1
             ORDER BY c.chunk_idx",
        )?;
        let rows = stmt
            .query_map(params![rendition_id], |row| {
                let origin_type: String = row.get(6)?;
                let origin = source_origin_from_parts(
                    &origin_type,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                    row.get(10)?,
                    row.get(11)?,
                    row.get(12)?,
                    row.get(13)?,
                )
                .ok_or_else(|| {
                    rusqlite::Error::InvalidColumnType(
                        6,
                        "origin_type".to_string(),
                        rusqlite::types::Type::Text,
                    )
                })?;
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    origin,
                    row.get::<_, Vec<u8>>(14)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        anyhow::ensure!(
            !rows.is_empty(),
            "DOCUMENT_INDEX_INCOMPLETE: rendition has no complete chunk/vector mapping"
        );
        let mut chunks = Vec::with_capacity(rows.len());
        let mut descriptors = Vec::with_capacity(rows.len());
        for (expected_ordinal, row) in rows.into_iter().enumerate() {
            let (stored_ref, ordinal, text, stored_text_sha256, byte_start, byte_end, origin, blob) =
                row;
            anyhow::ensure!(
                ordinal >= 0 && ordinal as usize == expected_ordinal,
                "DOCUMENT_INDEX_INCOMPLETE: chunk ordinals are not contiguous"
            );
            anyhow::ensure!(
                byte_start >= 0 && byte_end >= byte_start,
                "DOCUMENT_INDEX_INCOMPLETE: invalid chunk byte range"
            );
            let text_sha256 = sha256_bytes(text.as_bytes());
            anyhow::ensure!(
                text_sha256 == stored_text_sha256,
                "DOCUMENT_INDEX_INCOMPLETE: chunk text hash mismatch"
            );
            let embedding = f32_slice_from_bytes(&blob)?;
            anyhow::ensure!(
                embedding.len() == self.dimension
                    && embedding.iter().all(|value| value.is_finite()),
                "DOCUMENT_INDEX_INCOMPLETE: invalid stored vector"
            );
            let byte_range = ByteRange {
                start: byte_start as usize,
                end: byte_end as usize,
            };
            descriptors.push(ChunkDescriptor {
                ordinal: expected_ordinal,
                text_sha256: text_sha256.clone(),
                byte_range: byte_range.clone(),
                origin: origin.clone(),
            });
            chunks.push(ManagedChunkData {
                chunk_ref: ChunkRef(stored_ref),
                ordinal: expected_ordinal,
                text,
                text_sha256,
                extraction_byte_range: byte_range,
                origin,
            });
        }
        let recomputed_rendition = crate::embed::identity::rendition_id(
            &DocumentSnapshotId(snapshot.clone()),
            &extraction_recipe_id,
            &descriptors,
        );
        anyhow::ensure!(
            recomputed_rendition.as_str() == rendition,
            "DOCUMENT_INDEX_INCOMPLETE: rendition identity mismatch"
        );
        for chunk in &chunks {
            anyhow::ensure!(
                chunk.chunk_ref == chunk_ref(&recomputed_rendition, chunk.ordinal),
                "DOCUMENT_INDEX_INCOMPLETE: stable chunk reference mismatch"
            );
        }
        Ok(Some(ManagedDocumentData {
            source_sha256,
            snapshot_id: DocumentSnapshotId(snapshot),
            extraction_recipe_id,
            rendition_id: RenditionId(rendition),
            extracted_content_sha256,
            chunks,
        }))
    }

    /// Materialize an exactly verified whole rendition for copying into a
    /// different index. The returned vectors are owned values; the target will
    /// allocate fresh rowids and never depend on this index's lifecycle.
    pub fn verified_file_for_adoption(
        &self,
        source_sha256: &str,
        recipe: &ExtractionRecipe,
        expected_space_id: &EmbeddingSpaceId,
        target_path: &Path,
    ) -> anyhow::Result<Option<PreparedFile>> {
        let Some(exact_identity) = self.embedding_metadata()?.exact_identity else {
            return Ok(None);
        };
        if &exact_identity.id() != expected_space_id {
            return Ok(None);
        }
        let recipe_id = recipe.id();
        let file = self
            .conn
            .query_row(
                "SELECT id, full_text, rendition_id, extracted_content_sha256 FROM files
                 WHERE source_sha256 = ?1 AND extraction_recipe_id = ?2
                 LIMIT 1",
                params![source_sha256, recipe_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((file_id, full_text, stored_rendition, stored_extracted_content_sha256)) = file
        else {
            return Ok(None);
        };
        let (Some(full_text), Some(stored_rendition), Some(stored_extracted_content_sha256)) =
            (full_text, stored_rendition, stored_extracted_content_sha256)
        else {
            return Ok(None);
        };
        if sha256_bytes(full_text.as_bytes()) != stored_extracted_content_sha256 {
            return Ok(None);
        }
        let mut stmt = self.conn.prepare(
            "SELECT c.chunk_idx, c.byte_start, c.byte_end, c.origin_type,
                    c.page, c.line, c.col, c.bbox_x, c.bbox_y, c.bbox_w, c.bbox_h,
                    c.chunk_text, c.text_sha256, v.embedding
             FROM chunks c JOIN vec_chunks v ON v.rowid = c.id
             WHERE c.file_id = ?1 ORDER BY c.chunk_idx",
        )?;
        let rows = stmt
            .query_map(params![file_id], |row| {
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
                    row.get::<_, Vec<u8>>(13)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        if rows.is_empty() {
            return Ok(None);
        }
        let mut prepared_chunks = Vec::with_capacity(rows.len());
        let mut descriptors = Vec::with_capacity(rows.len());
        for (
            ordinal,
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
            stored_text_sha256,
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
                return Ok(None);
            };
            let text_sha256 = sha256_bytes(text.as_bytes());
            if stored_text_sha256.as_deref() != Some(text_sha256.as_str()) {
                return Ok(None);
            }
            let embedding = f32_slice_from_bytes(&embedding_blob)?;
            if embedding.len() != self.dimension || embedding.iter().any(|value| !value.is_finite())
            {
                return Ok(None);
            }
            let byte_range = ByteRange {
                start: byte_start as usize,
                end: byte_end as usize,
            };
            descriptors.push(ChunkDescriptor {
                ordinal: ordinal as usize,
                text_sha256,
                byte_range: byte_range.clone(),
                origin: origin.clone(),
            });
            prepared_chunks.push((
                Chunk {
                    file_path: target_path.to_path_buf(),
                    text,
                    byte_range,
                    origin,
                },
                embedding,
            ));
        }
        let recomputed = rendition_id(&snapshot_id(source_sha256), &recipe.id(), &descriptors);
        if recomputed.as_str() != stored_rendition {
            return Ok(None);
        }
        Ok(Some(PreparedFile {
            path: target_path.to_path_buf(),
            chunks: prepared_chunks,
            full_text,
        }))
    }

    /// Resolve opaque refs to local execution rowids. All refs must resolve;
    /// rowids never leave this method's callers in the managed surface.
    pub fn resolve_chunk_refs(
        &self,
        refs: &HashSet<ChunkRef>,
    ) -> anyhow::Result<HashMap<ChunkRef, i64>> {
        const REFS_PER_QUERY: usize = 400;
        let refs: Vec<ChunkRef> = refs.iter().cloned().collect();
        let mut found = HashMap::with_capacity(refs.len());
        for batch in refs.chunks(REFS_PER_QUERY) {
            let placeholders = vec!["?"; batch.len()].join(",");
            let mut stmt = self.conn.prepare(&format!(
                "SELECT chunk_ref, id FROM chunks
                 WHERE chunk_ref IN ({placeholders})"
            ))?;
            let rows = stmt.query_map(
                params_from_iter(batch.iter().map(ChunkRef::as_str)),
                |row| Ok((ChunkRef(row.get(0)?), row.get::<_, i64>(1)?)),
            )?;
            for row in rows {
                let (stable_ref, rowid) = row?;
                found.insert(stable_ref, rowid);
            }
        }
        if found.len() != refs.len() {
            let mut missing: Vec<&str> = refs
                .iter()
                .filter(|stable_ref| !found.contains_key(*stable_ref))
                .map(ChunkRef::as_str)
                .collect();
            missing.sort_unstable();
            anyhow::bail!(
                "CHUNK_REF_NOT_FOUND: {} reference(s) do not belong to this corpus: {}",
                missing.len(),
                missing.into_iter().take(10).collect::<Vec<_>>().join(", ")
            );
        }
        Ok(found)
    }

    pub fn managed_chunks_for_refs(
        &self,
        refs: &[ChunkRef],
    ) -> anyhow::Result<Vec<ManagedChunkData>> {
        let wanted: HashSet<ChunkRef> = refs.iter().cloned().collect();
        self.resolve_chunk_refs(&wanted)?;
        const REFS_PER_QUERY: usize = 300;
        let unique: Vec<ChunkRef> = wanted.into_iter().collect();
        let mut found = HashMap::with_capacity(unique.len());
        for batch in unique.chunks(REFS_PER_QUERY) {
            let placeholders = vec!["?"; batch.len()].join(",");
            let mut stmt = self.conn.prepare(&format!(
                "SELECT chunk_ref, chunk_idx, chunk_text, text_sha256,
                        byte_start, byte_end, origin_type, page, line, col,
                        bbox_x, bbox_y, bbox_w, bbox_h
                 FROM chunks WHERE chunk_ref IN ({placeholders})"
            ))?;
            let rows = stmt.query_map(
                params_from_iter(batch.iter().map(ChunkRef::as_str)),
                |row| {
                    let origin = source_origin_from_parts(
                        &row.get::<_, String>(6)?,
                        row.get(7)?,
                        row.get(8)?,
                        row.get(9)?,
                        row.get(10)?,
                        row.get(11)?,
                        row.get(12)?,
                        row.get(13)?,
                    )
                    .ok_or_else(|| {
                        rusqlite::Error::InvalidColumnType(
                            6,
                            "origin_type".to_string(),
                            rusqlite::types::Type::Text,
                        )
                    })?;
                    Ok(ManagedChunkData {
                        chunk_ref: ChunkRef(row.get(0)?),
                        ordinal: row.get::<_, i64>(1)? as usize,
                        text: row.get(2)?,
                        text_sha256: row.get(3)?,
                        extraction_byte_range: ByteRange {
                            start: row.get::<_, i64>(4)? as usize,
                            end: row.get::<_, i64>(5)? as usize,
                        },
                        origin,
                    })
                },
            )?;
            for row in rows {
                let chunk = row?;
                found.insert(chunk.chunk_ref.clone(), chunk);
            }
        }
        Ok(refs
            .iter()
            .map(|stable_ref| found[stable_ref].clone())
            .collect())
    }

    pub fn accumulate_chunk_refs(
        &self,
        groups: &[Vec<ChunkRef>],
    ) -> anyhow::Result<Vec<ChunkAccumulation>> {
        anyhow::ensure!(
            groups.iter().all(|group| !group.is_empty()),
            "An aggregate over no chunks is not a vector"
        );
        let wanted: HashSet<ChunkRef> = groups.iter().flatten().cloned().collect();
        if wanted.is_empty() {
            return Ok(Vec::new());
        }
        let rowids = self.resolve_chunk_refs(&wanted)?;
        let rowid_groups: Vec<Vec<i64>> = groups
            .iter()
            .map(|group| group.iter().map(|stable_ref| rowids[stable_ref]).collect())
            .collect();
        self.accumulate_chunk_ids(&rowid_groups, "Aggregate request")
    }

    /// The stored vectors of named chunks, L2-normalized, keyed by chunk id.
    ///
    /// Shared by every endpoint that answers a question *about* named chunks
    /// rather than handing the chunks out, because the rule they all need is
    /// the same one and it is not a detail: a chunk id this index does not
    /// hold refuses the whole request, naming the ids. Chunk ids are rowids
    /// reissued when a file is re-indexed, so the alternative is answering
    /// over whichever of the caller's ids happened to survive — a perfectly
    /// well-formed number with nothing in it to say which passages it is
    /// about.
    ///
    /// `asked_for` opens the refusal message, so a caller reads "Centroid
    /// request names 3 chunks…" rather than a sentence that could have come
    /// from any of them.
    fn normalized_chunk_vectors(
        &self,
        wanted: &HashSet<i64>,
        asked_for: &str,
    ) -> anyhow::Result<HashMap<i64, Vec<f32>>> {
        // Batched rather than one IN-list: the number of ids is the caller's,
        // and SQLite's bound-variable ceiling is the build's. A limit that
        // depends on how the library was compiled is not one this can honour
        // by asserting.
        const IDS_PER_QUERY: usize = 500;
        let ids: Vec<i64> = wanted.iter().copied().collect();
        let mut normalized: HashMap<i64, Vec<f32>> = HashMap::with_capacity(ids.len());
        for batch in ids.chunks(IDS_PER_QUERY) {
            let placeholders = vec!["?"; batch.len()].join(",");
            let mut stmt = self.conn.prepare(&format!(
                "SELECT c.id, v.embedding
                 FROM chunks c
                 JOIN vec_chunks v ON v.rowid = c.id
                 WHERE c.id IN ({placeholders})"
            ))?;
            let rows = stmt.query_map(params_from_iter(batch.iter()), |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
            })?;
            for row in rows {
                let (chunk_id, blob) = row?;
                let embedding = f32_slice_from_bytes(&blob)?;
                anyhow::ensure!(
                    embedding.len() == self.dimension,
                    "Stored embedding dimension mismatch for chunk {}. Expected {}, received {}.",
                    chunk_id,
                    self.dimension,
                    embedding.len()
                );
                normalized.insert(chunk_id, normalized_vector(&embedding));
            }
        }

        if normalized.len() != wanted.len() {
            let mut missing: Vec<i64> = wanted
                .iter()
                .filter(|id| !normalized.contains_key(id))
                .copied()
                .collect();
            missing.sort_unstable();
            let shown: Vec<String> = missing.iter().take(10).map(i64::to_string).collect();
            anyhow::bail!(
                "{} names {} chunk{} this index does not hold ({}{}) — they were re-indexed \
                 since those ids were recorded.",
                asked_for,
                missing.len(),
                if missing.len() == 1 { "" } else { "s" },
                shown.join(", "),
                if missing.len() > shown.len() {
                    ", …"
                } else {
                    ""
                },
            );
        }
        Ok(normalized)
    }

    fn accumulate_chunk_ids(
        &self,
        groups: &[Vec<i64>],
        asked_for: &str,
    ) -> anyhow::Result<Vec<ChunkAccumulation>> {
        anyhow::ensure!(
            groups.iter().all(|group| !group.is_empty()),
            "An aggregate over no chunks is not a vector"
        );
        let wanted: HashSet<i64> = groups.iter().flatten().copied().collect();
        if wanted.is_empty() {
            return Ok(Vec::new());
        }
        let normalized = self.normalized_chunk_vectors(&wanted, asked_for)?;
        Ok(groups
            .iter()
            .map(|group| {
                let mut sum = vec![0.0; self.dimension];
                for chunk_id in group {
                    for (total, value) in sum.iter_mut().zip(&normalized[chunk_id]) {
                        *total += value;
                    }
                }
                ChunkAccumulation {
                    sum,
                    member_count: group.len(),
                }
            })
            .collect())
    }

    /// The normalized mean of the stored vectors of named chunks, one mean per
    /// group, in the order the groups arrived.
    ///
    /// The same accumulation [`Self::related_documents`] does at document
    /// granularity — normalize each member, sum, divide, normalize the sum —
    /// with the membership named by the caller instead of read off `file_id`.
    /// Members are normalized *before* the mean because otherwise a long chunk
    /// with a large-norm vector would out-vote several short ones, and the
    /// question being asked ("what region do these passages occupy") weighs
    /// passages, not magnitudes.
    ///
    /// A chunk id the index does not hold is an error, never a skipped member.
    /// Chunk ids are rowids reissued when a file is re-indexed, so a caller
    /// holding ids from an earlier index would otherwise be handed a mean over
    /// whichever of its ids happened to survive — a different number, with
    /// nothing in the reply to say so. The chunk-text lookup on the export
    /// surface refuses stale ids for the same reason; here it matters more,
    /// because a partial mean still looks like a perfectly good vector.
    ///
    /// Groups are scanned together: one pass over the union of the ids, so the
    /// cost is the ids asked for and not the groups they are arranged into.
    pub fn chunk_centroids(&self, groups: &[Vec<i64>]) -> anyhow::Result<Vec<Vec<f32>>> {
        Ok(self
            .accumulate_chunk_ids(groups, "Centroid request")?
            .into_iter()
            .map(|group| normalized_vector(&centroid(&group.sum, group.member_count)))
            .collect())
    }

    /// How close each probe vector sits to a named set of chunks, both ways
    /// round, without any chunk vector leaving this index.
    ///
    /// Two answers from one scan, because they are two readings of the same
    /// matrix and a caller that asked for them separately would pay for it
    /// twice:
    ///
    /// * per probe, the nearest chunk of `chunk_ids` and how near it is;
    /// * per chunk, the nearest probe and how near it is.
    ///
    /// Plus, per probe, the **mean** similarity over its own `scope` — a
    /// second set of chunk ids, unrelated to `chunk_ids`. That exists because
    /// the consumer this was built for (Underdog's §6 conceptual coverage)
    /// judges a reading against a bar it derives from a different set of
    /// passages, and computing that bar on its side would mean receiving the
    /// vectors this function exists to keep here.
    ///
    /// **The probe vector is used exactly as given.** It is dotted with the
    /// L2-normalized chunk vector and nothing else is done to it, so a
    /// normalized probe yields a cosine and an unnormalized mean of unit
    /// vectors yields the mean of the cosines of its members. Normalizing here
    /// would quietly destroy the second, which is the whole of how a caller
    /// asks "how close is this *group* to these passages" in one probe.
    ///
    /// Chunk ids this index does not hold — in `chunk_ids` or in any scope —
    /// refuse the whole request, for the reason
    /// [`Self::normalized_chunk_vectors`] states.
    pub fn chunk_similarity(
        &self,
        probes: &[SimilarityProbe],
        chunk_ids: &[i64],
    ) -> anyhow::Result<ChunkSimilarity> {
        for (at, probe) in probes.iter().enumerate() {
            anyhow::ensure!(
                probe.vector.len() == self.dimension,
                "Probe {} has dimension {}; this index embeds at {}.",
                at,
                probe.vector.len(),
                self.dimension
            );
        }
        let wanted: HashSet<i64> = chunk_ids
            .iter()
            .copied()
            .chain(probes.iter().flat_map(|probe| probe.scope.iter().copied()))
            .collect();
        if wanted.is_empty() || probes.is_empty() {
            return Ok(ChunkSimilarity::default());
        }
        let normalized = self.normalized_chunk_vectors(&wanted, "Similarity request")?;

        // Nearest-probe per chunk accumulates as the probes are walked, so the
        // matrix is never materialised: the reply is O(probes + chunks), which
        // is what makes asking about a whole document affordable.
        let mut chunks: Vec<ChunkNearest> = chunk_ids
            .iter()
            .map(|chunk_id| ChunkNearest {
                chunk_id: *chunk_id,
                probe: 0,
                similarity: f32::NEG_INFINITY,
            })
            .collect();
        let mut answers = Vec::with_capacity(probes.len());
        for (at, probe) in probes.iter().enumerate() {
            let mut nearest: Option<(i64, f32)> = None;
            for held in chunks.iter_mut() {
                let score = dot(&probe.vector, &normalized[&held.chunk_id]);
                // Strictly greater, so a tie keeps the earlier probe and the
                // earlier chunk — the caller's order is the tiebreak, and two
                // runs of one request cannot disagree about it.
                if score > held.similarity {
                    held.similarity = score;
                    held.probe = at;
                }
                if nearest.is_none_or(|(_, best)| score > best) {
                    nearest = Some((held.chunk_id, score));
                }
            }
            let scope_mean = (!probe.scope.is_empty()).then(|| {
                let total: f32 = probe
                    .scope
                    .iter()
                    .map(|chunk_id| dot(&probe.vector, &normalized[chunk_id]))
                    .sum();
                total / probe.scope.len() as f32
            });
            answers.push(ProbeSimilarity {
                nearest_chunk_id: nearest.map(|(chunk_id, _)| chunk_id),
                similarity: nearest.map(|(_, score)| score),
                scope_mean,
                scope_size: probe.scope.len(),
            });
        }

        Ok(ChunkSimilarity {
            probes: answers,
            chunks,
        })
    }

    /// Nearest managed chunks across the entire corpus for each caller-owned
    /// probe. Probes are dotted exactly as provided against L2-normalized
    /// stored chunks; they are deliberately not normalized here because an
    /// unnormalized probe is a meaningful caller choice in the managed API.
    pub fn managed_chunk_search(
        &self,
        probes: &[Vec<f32>],
        top_k: usize,
        min_similarity: f32,
    ) -> anyhow::Result<Vec<Vec<ManagedChunkSearchHit>>> {
        anyhow::ensure!(!probes.is_empty(), "Search request names no probes");
        anyhow::ensure!(top_k > 0, "Search top_k must be greater than zero");
        anyhow::ensure!(
            min_similarity.is_finite(),
            "Search minimum similarity is not finite"
        );
        for (at, probe) in probes.iter().enumerate() {
            anyhow::ensure!(
                probe.len() == self.dimension,
                "Probe {} has dimension {}; this index embeds at {}.",
                at,
                probe.len(),
                self.dimension
            );
            anyhow::ensure!(
                probe.iter().all(|value| value.is_finite()),
                "Probe {at} contains a non-finite value"
            );
            anyhow::ensure!(
                probe.iter().any(|value| *value != 0.0),
                "Probe {at} has zero magnitude and names no direction"
            );
        }

        // The stored side is normalized below, so the dot product is a cosine
        // only if the probe is a unit vector too. Both engines normalize on
        // output, which made that an invariant nobody stated and nothing
        // enforced — while `min_similarity` reads as a cosine floor and two of
        // Underdog's thresholds ride on it. Normalizing here costs one pass
        // over a handful of probes and makes the name true by construction.
        let probes: Vec<Vec<f32>> = probes
            .iter()
            .map(|probe| normalized_vector(probe))
            .collect();

        let mut stmt = self.conn.prepare(
            "SELECT c.chunk_ref, f.snapshot_id, f.rendition_id, c.chunk_idx, v.embedding
               FROM chunks c
               JOIN files f ON f.id = c.file_id
               JOIN vec_chunks v ON v.rowid = c.id
              ORDER BY f.id, c.chunk_idx, c.id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Vec<u8>>(4)?,
            ))
        })?;
        let mut answers = vec![Vec::new(); probes.len()];
        for row in rows {
            let (chunk_ref, snapshot_id, rendition_id, ordinal, blob) = row?;
            let chunk_ref = chunk_ref
                .ok_or_else(|| anyhow::anyhow!("DOCUMENT_INDEX_INCOMPLETE: chunk ref is absent"))?;
            let snapshot_id = snapshot_id.ok_or_else(|| {
                anyhow::anyhow!("DOCUMENT_INDEX_INCOMPLETE: snapshot id is absent")
            })?;
            let rendition_id = rendition_id.ok_or_else(|| {
                anyhow::anyhow!("DOCUMENT_INDEX_INCOMPLETE: rendition id is absent")
            })?;
            anyhow::ensure!(
                ordinal >= 0,
                "DOCUMENT_INDEX_INCOMPLETE: negative chunk ordinal"
            );
            let vector = f32_slice_from_bytes(&blob)?;
            anyhow::ensure!(
                vector.len() == self.dimension && vector.iter().all(|value| value.is_finite()),
                "DOCUMENT_INDEX_INCOMPLETE: invalid stored vector"
            );
            let vector = normalized_vector(&vector);
            for (probe_index, probe) in probes.iter().enumerate() {
                let similarity = dot(probe, &vector);
                if similarity < min_similarity {
                    continue;
                }
                answers[probe_index].push(ManagedChunkSearchHit {
                    chunk_ref: ChunkRef(chunk_ref.clone()),
                    snapshot_id: DocumentSnapshotId(snapshot_id.clone()),
                    rendition_id: RenditionId(rendition_id.clone()),
                    ordinal: ordinal as usize,
                    similarity,
                });
            }
        }
        for hits in &mut answers {
            hits.sort_by(|left, right| {
                right
                    .similarity
                    .total_cmp(&left.similarity)
                    .then(left.rendition_id.as_str().cmp(right.rendition_id.as_str()))
                    .then(left.ordinal.cmp(&right.ordinal))
            });
            hits.truncate(top_k);
        }
        Ok(answers)
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

    fn topic_chunks_filtered(
        &self,
        predicate: &str,
        parameters: &[&dyn ToSql],
    ) -> anyhow::Result<Vec<TopicChunkData>> {
        let sql = format!(
            "SELECT c.id, f.id, f.file_path, c.byte_start, c.byte_end,
                    c.origin_type, c.page, c.line, c.col,
                    c.bbox_x, c.bbox_y, c.bbox_w, c.bbox_h, c.chunk_text,
                    v.embedding
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

/// One question for [`SemanticIndex::chunk_similarity`]: a vector in the
/// index's own space, and optionally a set of chunk ids to average it over.
///
/// `scope` is not a filter on the search — the nearest-chunk answer always
/// ranges over the request's whole `chunk_ids`. It is a separate question
/// asked in the same pass, because the caller that needs it needs both about
/// the same probe.
#[derive(Clone, Debug)]
pub struct SimilarityProbe {
    pub vector: Vec<f32>,
    pub scope: Vec<i64>,
}

/// What one probe found. `None` for the nearest pair when the request named no
/// chunks to search, which is a legitimate request: a caller may want only the
/// scope means.
#[derive(Clone, Debug)]
pub struct ProbeSimilarity {
    pub nearest_chunk_id: Option<i64>,
    pub similarity: Option<f32>,
    pub scope_mean: Option<f32>,
    /// How many ids the mean was taken over, reported so a caller can tell a
    /// mean of two from a mean of two hundred without keeping its own copy of
    /// what it asked for.
    pub scope_size: usize,
}

/// What one chunk found: the probe it sits closest to, by index into the
/// request's probe list.
#[derive(Clone, Debug)]
pub struct ChunkNearest {
    pub chunk_id: i64,
    pub probe: usize,
    pub similarity: f32,
}

/// One full-corpus vector hit addressed entirely by managed, stable ids.
#[derive(Clone, Debug, PartialEq)]
pub struct ManagedChunkSearchHit {
    pub chunk_ref: ChunkRef,
    pub snapshot_id: DocumentSnapshotId,
    pub rendition_id: RenditionId,
    pub ordinal: usize,
    pub similarity: f32,
}

/// Both directions of one similarity request. `chunks` follows the order the
/// ids were asked in; `probes` follows the probe order.
#[derive(Clone, Debug, Default)]
pub struct ChunkSimilarity {
    pub probes: Vec<ProbeSimilarity>,
    pub chunks: Vec<ChunkNearest>,
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
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
