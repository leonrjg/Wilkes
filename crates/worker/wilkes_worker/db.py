import sqlite3
import sys

def get_connection(db_path):
    import sqlite_vec
    conn = sqlite3.connect(db_path)
    conn.enable_load_extension(True)
    sqlite_vec.load(conn)
    conn.enable_load_extension(False)
    conn.execute("PRAGMA journal_mode=WAL")
    conn.execute("PRAGMA foreign_keys=ON")
    return conn

def init_schema(conn, dimension):
    # Ensure meta exists before reading from it (it may not on first run).
    conn.execute("""
        CREATE TABLE IF NOT EXISTS meta (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        )
    """)

    # If the existing index was built with a different dimension (i.e. a different model),
    # the vec_chunks virtual table schema is incompatible. Drop both tables so they are
    # recreated below with the correct dimension.
    stored_dim = conn.execute("SELECT value FROM meta WHERE key='dimension'").fetchone()
    if stored_dim is not None and int(stored_dim[0]) != dimension:
        sys.stderr.write(
            f"Dimension mismatch: stored={stored_dim[0]}, new={dimension}. "
            "Dropping incompatible index tables.\n"
        )
        conn.executescript("DROP TABLE IF EXISTS vec_chunks; DROP TABLE IF EXISTS chunks;")

    conn.executescript(f"""
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
        CREATE VIRTUAL TABLE IF NOT EXISTS vec_chunks
            USING vec0(embedding float[{dimension}] distance_metric=cosine);
        CREATE INDEX IF NOT EXISTS idx_chunks_file_path ON chunks(file_path);
    """)

def insert_meta(conn: sqlite3.Connection, model_id: str, dimension: int, built_at: int, build_duration_ms: int, root_path: str) -> None:
    conn.execute("INSERT OR REPLACE INTO meta (key, value) VALUES ('engine', 'sbert')")
    conn.execute("INSERT OR REPLACE INTO meta (key, value) VALUES ('model_id', ?)", (model_id,))
    conn.execute("INSERT OR REPLACE INTO meta (key, value) VALUES ('dimension', ?)", (str(dimension),))
    conn.execute("INSERT OR REPLACE INTO meta (key, value) VALUES ('built_at', ?)", (str(built_at),))
    conn.execute("INSERT OR REPLACE INTO meta (key, value) VALUES ('build_duration_ms', ?)", (str(build_duration_ms),))
    conn.execute("INSERT OR REPLACE INTO meta (key, value) VALUES ('root_path', ?)", (str(root_path),))

def delete_existing_chunks(conn: sqlite3.Connection, path_strs: Set[str]) -> None:
    cur = conn.cursor()
    cur.executemany(
        "DELETE FROM vec_chunks WHERE rowid IN (SELECT id FROM chunks WHERE file_path = ?)",
        [(p,) for p in path_strs]
    )
    cur.executemany(
        "DELETE FROM chunks WHERE file_path = ?",
        [(p,) for p in path_strs]
    )
