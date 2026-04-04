from typing import Any, List
from functools import lru_cache
from .ipc import emit

@lru_cache(maxsize=2)
def get_model(model_id: str, device: str) -> Any:
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
