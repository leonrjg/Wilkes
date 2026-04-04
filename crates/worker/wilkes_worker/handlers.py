import time
import os
import sys
import numpy as np
from pathlib import Path
from typing import Any, Dict

from .ipc import emit
from .models import get_model, safe_encode
from .extraction import SUPPORTED_EXTENSIONS, extract_chunks
from .db import get_connection, init_schema, insert_meta, delete_existing_chunks

def build_index(request: Dict[str, Any]) -> None:
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

    supported = request.get("supported_extensions")
    if supported:
        # Rust provides extensions without dots, Python's Path.suffix includes the dot.
        exts = {f".{ext.lower()}" for ext in supported}
    else:
        exts = SUPPORTED_EXTENSIONS

    files = [p for p in candidates if p.suffix.lower() in exts]
    total_files = len(files)

    emit({"Progress": {"Build": {
        "files_processed": 0,
        "total_files": total_files,
        "message": "Extracting text...",
        "done": False
    }}})

    all_chunks = extract_chunks(files, splitter)

    if not all_chunks:
        emit({"Done": None})
        return

    emit({"Progress": {"Build": {
        "files_processed": 0,
        "total_files": total_files,
        "message": f"Embedding {len(all_chunks)} chunks...",
        "done": False
    }}})

    embeddings = safe_encode(model, [c[2] for c in all_chunks], show_progress_bar=True)

    conn = get_connection(actual_db_path)
    dimension = embeddings.shape[1] if len(embeddings) > 0 else 0

    init_schema(conn, dimension)

    built_at = int(build_start)
    build_duration_ms = int((time.time() - build_start) * 1000)
    insert_meta(conn, model_id, dimension, built_at, build_duration_ms, str(root))

    path_strs = {str(p) for p, *_ in all_chunks}
    delete_existing_chunks(conn, path_strs)

    import sqlite_vec
    cur = conn.cursor()
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
        for suffix in ["", "-wal", "-shm"]:
            try:
                p = str(db_path) + suffix
                if os.path.exists(p):
                    os.remove(p)
            except OSError:
                pass
        os.replace(str(actual_db_path), str(db_path))

    emit({"Progress": {"Build": {
        "files_processed": total_files,
        "total_files": total_files,
        "message": "Done.",
        "done": True
    }}})
    emit({"Done": None})


def embed_texts(request):
    model_id = request["model"]
    texts = request.get("texts") or []
    device = request.get("device", "auto")

    if not texts:
        emit({"Embeddings": []})
        emit({"Done": None})
        return

    model = get_model(model_id, device)
    embeddings = safe_encode(model, texts, show_progress_bar=True)
    emit({"Embeddings": embeddings.tolist()})
    emit({"Done": None})


def info(request):
    model_id = request["model"]
    device = request.get("device", "auto")
    model = get_model(model_id, device)
    
    # SentenceTransformer models have a 'get_sentence_embedding_dimension' method
    dim = model.get_sentence_embedding_dimension()
    seq_len = model.get_max_seq_length()

    # Fallback: If metadata is missing, perform a dummy probe to see the actual output shape
    if dim is None:
        dummy_emb = safe_encode(model, [""])
        dim = dummy_emb.shape[1]
    
    if dim is None or int(dim) == 0:
        emit({"Error": f"Unable to determine dimension for model '{model_id}' via probe."})
        return
    
    # Handle Infinity for JSON compatibility
    if seq_len == float('inf') or seq_len > 1_000_000:
        seq_len = 999999
    emit({"Info": {"dimension": int(dim), "max_seq_length": int(seq_len)}})
    emit({"Done": None})
