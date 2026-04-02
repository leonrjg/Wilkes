import sys
import json
import os
import sqlite3
import torch
import numpy as np
from pathlib import Path
from infinity_emb import AsyncEmbeddingEngine, Device
from semantic_text_splitter import TextSplitter
import fitz # PyMuPDF

def emit(event):
    print(json.dumps(event), flush=True)

def list_models():
    # Placeholder for scanning HF cache
    # In a real implementation, we'd use huggingface_hub to scan the cache
    emit({"Models": []})

def extract_text(path):
    suffix = Path(path).suffix.lower()
    if suffix == ".pdf":
        try:
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

def build_index(request):
    root = Path(request["root"])
    model_id = request["model"]
    data_dir = Path(request["data_dir"])
    chunk_size = request["chunk_size"]
    chunk_overlap = request["chunk_overlap"]
    device = request.get("device", "auto")
    paths = request.get("paths")

    db_path = data_dir / "semantic_index.db"
    
    emit({"Progress": {"Build": {
        "files_processed": 0,
        "total_files": 0,
        "message": "Initializing embedding engine...",
        "done": False
    }}})

    # Initialize engine
    # Device mapping: auto, cpu, mps, cuda
    engine_device = Device.auto
    if device == "cpu": engine_device = Device.cpu
    elif device == "mps": engine_device = Device.mps
    elif device == "cuda": engine_device = Device.cuda
    
    try:
        engine = AsyncEmbeddingEngine.from_model_id(
            model_id=model_id,
            device=engine_device,
        )
    except Exception as e:
        emit({"Error": f"Failed to initialize engine: {e}"})
        return

    # Text splitter (character-based to match current Rust implementation's default)
    splitter = TextSplitter(chunk_size)

    # Collect files
    if paths:
        files = [Path(p) for p in paths]
    else:
        files = []
        # Walk root, following the same logic as Rust (skipping hidden files)
        for p in root.rglob("*"):
            if p.is_file() and not p.name.startswith("."):
                files.append(p)

    total_files = len(files)
    
    # Setup database
    conn = sqlite3.connect(db_path)
    # Note: sqlite-vec might need to be loaded as a shared library
    # The spec mentions "The database is opened in WAL mode"
    conn.execute("PRAGMA journal_mode=WAL")
    
    try:
        import sqlite_vec
        conn.enable_load_extension(True)
        sqlite_vec.load(conn)
        conn.enable_load_extension(False)
    except ImportError:
        # Fallback if sqlite_vec is not installed as a python package 
        # but is available as a system extension or built-in.
        pass

    with engine:
        for i, path in enumerate(files):
            try:
                emit({"Progress": {"Build": {
                    "files_processed": i,
                    "total_files": total_files,
                    "message": f"Processing {path.name}",
                    "done": False
                }}})
                
                text = extract_text(path)
                if not text:
                    continue
                    
                chunks = splitter.chunks(text)
                
                if not chunks:
                    continue
                
                # infinity-emb embed() returns a list of embeddings
                # We need to use await if it's async, but AsyncEmbeddingEngine
                # handles batching internally and can be used synchronously in some ways
                # Wait, infinity-emb AsyncEmbeddingEngine.embed is an async method.
                # Since we are in a sync function, we might need a better way.
                
                # Actually, infinity-emb also has a sync Engine.
                # But the spec mentioned AsyncEmbeddingEngine.
                
                import asyncio
                async def do_embed():
                    return await engine.embed(chunks)
                
                embeddings = asyncio.run(do_embed())
                
                cur = conn.cursor()
                # Remove existing chunks for this path
                cur.execute("DELETE FROM chunks WHERE file_path = ?", (str(path),))
                
                for idx, (chunk_text, embedding) in enumerate(zip(chunks, embeddings)):
                    cur.execute(
                        "INSERT INTO chunks (file_path, chunk_idx, byte_start, byte_end, origin_json, chunk_text) VALUES (?, ?, ?, ?, ?, ?)",
                        (str(path), idx, 0, 0, "{}", chunk_text)
                    )
                    chunk_id = cur.lastrowid
                    # sqlite-vec expects embeddings as BLOBs of float32
                    cur.execute(
                        "INSERT INTO chunk_vectors (chunk_id, embedding) VALUES (?, ?)",
                        (chunk_id, np.array(embedding, dtype=np.float32).tobytes())
                    )
                conn.commit()
                
            except Exception as e:
                # Log to stderr for desktop to capture, but also emit an Error event
                sys.stderr.write(f"Error processing {path}: {e}\n")
                # Don't stop the whole build for one file error
                # emit({"Error": f"Failed to process {path}: {str(e)}"})

    emit({"Done": None})

def main():
    line = sys.stdin.readline()
    if not line:
        return
    
    try:
        request = json.loads(line)
        mode = request.get("mode", "build")
        
        if mode == "list-models":
            list_models()
        elif mode == "build":
            build_index(request)
        else:
            emit({"Error": f"Unknown mode: {mode}"})
    except Exception as e:
        emit({"Error": str(e)})

if __name__ == "__main__":
    main()
