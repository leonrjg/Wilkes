import sys
import json
import os
import sqlite3
import logging
from pathlib import Path

# Configure logging to stderr so the Rust side can capture and display it
logging.basicConfig(
    level=logging.INFO,
    format="%(name)s - %(levelname)s - %(message)s",
    stream=sys.stderr
)

# Ensure transformers is also verbose enough
try:
    from transformers.utils import logging as tf_logging
    tf_logging.set_verbosity_warning()
    tf_logging.enable_default_handler()
except ImportError:
    pass

SUPPORTED_EXTENSIONS = {
    ".txt", ".md", ".markdown", ".rst", ".py", ".js", ".ts", ".jsx", ".tsx",
    ".java", ".c", ".cpp", ".h", ".cs", ".go", ".rs", ".rb", ".swift", ".kt",
    ".html", ".css", ".json", ".yaml", ".yml", ".toml", ".xml", ".sh",
    ".pdf",
}

def emit(event):
    print(json.dumps(event), flush=True)

MODEL_CACHE = {}

def get_model(model_id, device):
    import time
    cache_key = (model_id, device)
    if cache_key not in MODEL_CACHE:
        emit({"Progress": {"Build": {
            "files_processed": 0,
            "total_files": 0,
            "message": "Initializing embedding engine...",
            "done": False
        }}})
        from sentence_transformers import SentenceTransformer
        try:
            # Try optimized SDPA first
            model = SentenceTransformer(
                model_id,
                device=None if device == "auto" else device,
                trust_remote_code=True,
                model_kwargs={"attn_implementation": "sdpa"}
            )
        except (ValueError, Exception):
            # Fallback to default attention if SDPA is not supported by this architecture
            model = SentenceTransformer(
                model_id,
                device=None if device == "auto" else device,
                trust_remote_code=True
            )
        MODEL_CACHE[cache_key] = model
    return MODEL_CACHE[cache_key]

def extract_text(path):
    suffix = Path(path).suffix.lower()
    if suffix == ".pdf":
        try:
            import fitz  # PyMuPDF
            doc = fitz.open(path)
            text = ""
            for page in doc:
                text += page.get_text()
            return text
        except Exception as e:
            return f"Error extracting PDF: {e}"
    else:
        try:
            with open(path, "r", encoding="utf-8", errors="ignore") as f:
                return f.read()
        except Exception as e:
            return f"Error reading file: {e}"

def _safe_encode(model, texts, **kwargs):
    """
    Centralized encoding helper that handles both modern task-based API 
    and legacy SentenceTransformer models with a robust fallback.
    """
    # Default settings for indexing/retrieval
    params = {
        "normalize_embeddings": True,
        "convert_to_numpy": True,
        "show_progress_bar": False,
    }
    params.update(kwargs)
    
    try:
        # Try modern API with task='retrieval'
        return model.encode(texts, task='retrieval', **params)
    except (TypeError, ValueError):
        # Fallback for models that don't support 'task'
        return model.encode(texts, **params)

def build_index(request):
    import time
    import numpy as np
    from sentence_transformers import SentenceTransformer
    from semantic_text_splitter import TextSplitter
    root = Path(request["root"])
    model_id = request["model"]
    data_dir = Path(request["data_dir"])
    chunk_size = request["chunk_size"]
    device = request.get("device", "auto")
    paths = request.get("paths")
    build_start = time.time()

    db_path = data_dir / "semantic_index.db"
    if not paths:
        actual_db_path = data_dir / "semantic_index.db.tmp"
    else:
        actual_db_path = db_path

    model = get_model(model_id, device)

    splitter = TextSplitter(chunk_size)

    if paths:
        candidates = [Path(p) for p in paths]
    else:
        candidates = [p for p in root.rglob("*") if p.is_file() and not p.name.startswith(".")]

    files = [p for p in candidates if p.suffix.lower() in SUPPORTED_EXTENSIONS]
    total_files = len(files)

    emit({"Progress": {"Build": {
        "files_processed": 0,
        "total_files": total_files,
        "message": "Extracting text...",
        "done": False
    }}})

    # Collect all chunks before embedding so the engine receives the full corpus
    # in one call and can batch optimally across all files.
    # Each entry: (path, chunk_idx, chunk_text, byte_start, byte_end, line_num)
    all_chunks: list[tuple[Path, int, str, int, int, int]] = []
    for path in files:
        try:
            text = extract_text(path)
            if not text:
                continue
            for idx, (char_offset, chunk_text) in enumerate(splitter.chunk_indices(text)):
                byte_start = len(text[:char_offset].encode('utf-8'))
                byte_end = byte_start + len(chunk_text.encode('utf-8'))
                line_num = text[:char_offset].count('\n') + 1
                all_chunks.append((path, idx, chunk_text, byte_start, byte_end, line_num))
        except Exception as e:
            sys.stderr.write(f"Error extracting {path}: {e}\n")

    if not all_chunks:
        emit({"Done": None})
        return

    emit({"Progress": {"Build": {
        "files_processed": 0,
        "total_files": total_files,
        "message": f"Embedding {len(all_chunks)} chunks...",
        "done": False
    }}})

    embeddings = _safe_encode(model, [c[2] for c in all_chunks], show_progress_bar=True)

    import sqlite_vec

    conn = sqlite3.connect(actual_db_path)
    conn.enable_load_extension(True)
    sqlite_vec.load(conn)
    conn.enable_load_extension(False)
    conn.execute("PRAGMA journal_mode=WAL")
    conn.execute("PRAGMA foreign_keys=ON")

    dimension = embeddings.shape[1] if len(embeddings) > 0 else 0

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

    built_at = int(build_start)
    build_duration_ms = int((time.time() - build_start) * 1000)
    conn.execute("INSERT OR REPLACE INTO meta (key, value) VALUES ('engine', 'sbert')")
    conn.execute("INSERT OR REPLACE INTO meta (key, value) VALUES ('model_id', ?)", (model_id,))
    conn.execute("INSERT OR REPLACE INTO meta (key, value) VALUES ('dimension', ?)", (str(dimension),))
    conn.execute("INSERT OR REPLACE INTO meta (key, value) VALUES ('built_at', ?)", (str(built_at),))
    conn.execute("INSERT OR REPLACE INTO meta (key, value) VALUES ('build_duration_ms', ?)", (str(build_duration_ms),))
    conn.execute("INSERT OR REPLACE INTO meta (key, value) VALUES ('root_path', ?)", (str(root),))

    cur = conn.cursor()
    for path_str in {str(p) for p, *_ in all_chunks}:
        cur.execute("DELETE FROM vec_chunks WHERE rowid IN (SELECT id FROM chunks WHERE file_path = ?)", (path_str,))
        cur.execute("DELETE FROM chunks WHERE file_path = ?", (path_str,))

    for (path, chunk_idx, chunk_text, byte_start, byte_end, line_num), embedding in zip(all_chunks, embeddings):
        if path.suffix.lower() == ".pdf":
            origin_json = f'{{"PdfPage": {{"page": {chunk_idx + 1}, "bbox": null}}}}'
        else:
            origin_json = f'{{"TextFile": {{"line": {line_num}, "col": 0}}}}'
        cur.execute(
            "INSERT INTO chunks (file_path, chunk_idx, byte_start, byte_end, origin_json, chunk_text) VALUES (?, ?, ?, ?, ?, ?)",
            (str(path), chunk_idx, byte_start, byte_end, origin_json, chunk_text)
        )
        chunk_id = cur.lastrowid
        cur.execute(
            "INSERT INTO vec_chunks(rowid, embedding) VALUES (?, ?)",
            (chunk_id, sqlite_vec.serialize_float32(np.array(embedding, dtype=np.float32)))
        )

    conn.commit()
    conn.close()

    if not paths:
        # Atomic rename for full build
        import os
        # Remove old files to avoid rename issues
        for suffix in ["", "-wal", "-shm"]:
            try:
                p = str(db_path) + suffix
                if os.path.exists(p):
                    os.remove(p)
            except OSError:
                pass
        os.rename(str(actual_db_path), str(db_path))

    emit({"Progress": {"Build": {
        "files_processed": total_files,
        "total_files": total_files,
        "message": "Done.",
        "done": True
    }}})
    emit({"Done": None})

def embed_texts(request):
    import numpy as np
    model_id = request["model"]
    texts = request.get("texts") or []
    device = request.get("device", "auto")

    if not texts:
        emit({"Embeddings": []})
        emit({"Done": None})
        return

    model = get_model(model_id, device)
    embeddings = _safe_encode(model, texts, show_progress_bar=True)
    emit({"Embeddings": embeddings.tolist()})
    emit({"Done": None})

def main():
    while True:
        line = sys.stdin.readline()
        if not line:
            break

        try:
            request = json.loads(line)
            sys.stderr.write(f"Request: {json.dumps(request)}\n")
            mode = request.get("mode", "build")

            if mode == "build":
                build_index(request)
            elif mode == "embed":
                embed_texts(request)
            elif mode == "info":
                model_id = request["model"]
                device = request.get("device", "auto")
                model = get_model(model_id, device)
                # SentenceTransformer models have a 'get_sentence_embedding_dimension' method
                dim = model.get_sentence_embedding_dimension()
                seq_len = model.get_max_seq_length()

                # Fallback: If metadata is missing, perform a dummy probe to see the actual output shape
                if dim is None:
                    dummy_emb = _safe_encode(model, [""])
                    dim = dummy_emb.shape[1]
                
                if dim is None or int(dim) == 0:
                    emit({"Error": f"Unable to determine dimension for model '{model_id}' via probe."})
                    return
                
                # Handle Infinity for JSON compatibility
                if seq_len == float('inf') or seq_len > 1_000_000:
                    seq_len = 999999
                emit({"Info": {"dimension": int(dim), "max_seq_length": int(seq_len)}})
                emit({"Done": None})
            else:
                emit({"Error": f"Unknown mode: {mode}"})
        except Exception as e:
            import traceback
            emit({"Error": traceback.format_exc()})

if __name__ == "__main__":
    main()
