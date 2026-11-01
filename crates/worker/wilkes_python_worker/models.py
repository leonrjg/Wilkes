import logging
import os
import threading
from typing import Any, List
from functools import lru_cache
from .ipc import emit
from .protocol import event_download_progress

logger = logging.getLogger(__name__)

#: Texts put through the model in one forward pass when the request names none.
#:
#: The host sends this on every embed request, read from the user's settings —
#: see `SemanticSettings::default_embed_batch_size`, which is where the number
#: and the measurements behind it live. This copy is only for a request that
#: arrived without one, which means a host older than this worker.
#:
#: It matters because the worker's peak memory is set by its largest single
#: batch and nothing else: not the size of the request, and not how many
#: batches it is split into. On the machine this was measured on, a batch of
#: the corpus's longest chunks cost 5.6 GB at 16 and over 9.5 GB at 32, on a
#: model that occupies 1.9 GB at rest.
DEFAULT_BATCH_SIZE = 16

#: How often a model load looks at what has reached the cache.
DOWNLOAD_POLL_SECONDS = 1.0

def cached_bytes(blobs_dir: str) -> int:
    if not os.path.isdir(blobs_dir):
        return 0
    total = 0
    with os.scandir(blobs_dir) as entries:
        for entry in entries:
            try:
                total += entry.stat().st_size
            except FileNotFoundError:
                # A blob finishing mid-walk is renamed from its `.incomplete`
                # name; the next look counts it under the new one.
                logger.debug("Blob %s moved while being measured", entry.name)
    return total

def is_loaded_by_pytorch(filename: str, has_safetensors: bool) -> bool:
    if filename.startswith(("onnx/", "openvino/")):
        return False
    if filename.endswith((".h5", ".ot", ".msgpack", ".onnx", ".gguf")):
        return False
    return not (has_safetensors and filename.endswith(".bin"))

def repo_size(model_id: str) -> int:
    """What a load will fetch from the repository, or 0 if that is unknown.

    An estimate for the progress bar. What a load fetches is decided inside the
    libraries doing it, but repositories commonly publish one model in many
    formats, and counting them all left all-MiniLM-L6-v2 finishing at 9%.
    """
    from huggingface_hub import HfApi
    try:
        info = HfApi().model_info(model_id, files_metadata=True)
    except Exception:
        logger.warning("Could not read the size of %s", model_id, exc_info=True)
        return 0
    siblings = info.siblings or []
    has_safetensors = any(s.rfilename.endswith(".safetensors") for s in siblings)
    return sum(
        s.size or 0
        for s in siblings
        if is_loaded_by_pytorch(s.rfilename, has_safetensors)
    )

def watch_download(model_id: str, stop: threading.Event) -> None:
    """Report what reaches the cache until `stop` is set.

    Watches the cache rather than hooking the download, because the download
    belongs to whichever library a model's load goes through, and the cache is
    the one thing they all share. Silent for a model that is already cached:
    nothing changes on disk, so a load is never reported as a download.
    """
    from huggingface_hub.constants import HF_HUB_CACHE
    blobs_dir = os.path.join(
        HF_HUB_CACHE, f"models--{model_id.replace('/', '--')}", "blobs"
    )
    reported = cached_bytes(blobs_dir)
    total = None
    while not stop.wait(DOWNLOAD_POLL_SECONDS):
        received = cached_bytes(blobs_dir)
        if received == reported:
            continue
        reported = received
        if total is None:
            total = repo_size(model_id)
        emit(event_download_progress(received, total))

@lru_cache(maxsize=2)
def get_model(model_id: str, device: str) -> Any:
    emit({"Progress": {"Build": {
        "files_processed": 0,
        "total_files": 0,
        "message": "Initializing embedding engine...",
        "done": False
    }}})
    from sentence_transformers import SentenceTransformer

    stop_watching = threading.Event()
    watcher = threading.Thread(
        target=watch_download, args=(model_id, stop_watching), daemon=True
    )
    watcher.start()
    try:
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
    finally:
        # Joined, not just signalled: an event written after this request's
        # terminal event would be read as part of the next request.
        stop_watching.set()
        watcher.join()
    return model

def safe_encode(model: Any, texts: List[str], **kwargs: Any) -> Any:
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
