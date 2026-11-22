use super::*;

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
fn test_delete_non_existent() {
    let dir = tempdir().unwrap();
    let idx = SemanticIndex::create(dir.path(), "m", 3, EmbeddingEngine::Candle, None).unwrap();
    fs::remove_file(dir.path().join("semantic_index.db")).unwrap();
    assert!(idx.delete(dir.path()).is_ok());
}
