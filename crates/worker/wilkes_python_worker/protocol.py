"""
IPC contract between the Rust host and this Python worker.

Inbound shape mirrors WorkerRequest in crates/core/src/embed/worker_ipc.rs.
Outbound shapes mirror WorkerEvent in crates/core/src/embed/worker_ipc.rs.

When either Rust struct changes, update this file to match.
"""

from typing import List, Optional, TypedDict


# ── Inbound (Rust → Python, deserialized from stdin JSON) ────────────────────

class WorkerRequest(TypedDict):
    mode: str                       # "embed" | "info"
    model: str                      # HuggingFace model ID
    device: str                     # "auto" | "cpu" | "mps" | "cuda"
    texts: Optional[List[str]]      # present in "embed" mode


# ── Outbound (Python → Rust, emitted as JSON lines on stdout) ─────────────────
# Each function produces a dict that serde will deserialize as the named
# WorkerEvent variant.  Keep the key names in sync with the Rust enum.

def event_embeddings(vectors: List[List[float]]) -> dict:
    return {"Embeddings": vectors}

def event_info(dimension: int, max_seq_length: int) -> dict:
    return {"Info": {"dimension": dimension, "max_seq_length": max_seq_length}}

def event_done() -> dict:
    return {"Done": None}

def event_error(message: str) -> dict:
    return {"Error": message}
