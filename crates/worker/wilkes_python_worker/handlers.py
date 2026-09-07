import numpy as np

from .ipc import emit
from .models import DEFAULT_BATCH_SIZE, get_model, safe_encode
from .protocol import WorkerRequest, event_done, event_embeddings, event_error, event_info


def embed_texts(request: WorkerRequest) -> None:
    model_id = request["model"]
    texts = request.get("texts") or []
    device = request.get("device", "auto")

    if not texts:
        emit(event_embeddings([]))
        emit(event_done())
        return

    # The host's setting, not a constant here: it is the user's resource
    # control, and the host is what holds the settings. Absent only when a
    # host older than this worker sent the request, which is the one case the
    # worker's own default is the right answer for.
    batch_size = request.get("batch_size", DEFAULT_BATCH_SIZE)
    if not isinstance(batch_size, int) or batch_size < 1:
        raise ValueError(
            f"batch_size must be a positive integer, got {batch_size!r}"
        )

    model = get_model(model_id, device)
    embeddings = safe_encode(
        model, texts, batch_size=batch_size, show_progress_bar=True
    )
    emit(event_embeddings(embeddings.tolist()))
    emit(event_done())


def info(request: WorkerRequest) -> None:
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
        emit(event_error(f"Unable to determine dimension for model '{model_id}' via probe."))
        return

    # Handle Infinity for JSON compatibility
    if seq_len == float('inf') or seq_len > 1_000_000:
        seq_len = 999999
    emit(event_info(int(dim), int(seq_len)))
    emit(event_done())
