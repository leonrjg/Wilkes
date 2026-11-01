import re
import traceback
from typing import Optional

import numpy as np

from .ipc import emit
from .models import DEFAULT_BATCH_SIZE, get_model, safe_encode
from .protocol import (
    WorkerRequest,
    event_done,
    event_embeddings,
    event_error,
    event_info,
    event_unmet,
)

#: How Transformers lists what a model's own code imports and is not installed.
MISSING_PACKAGES = re.compile(
    r"requires the following packages that were not found in your environment: ([^.]+)\."
)


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


def unmet_need(error: Exception) -> Optional[dict]:
    """The event for a load that failed for want of something, if this one did.

    Worded failures, because nothing structured reaches here: the message is the
    only thing Transformers says. A module of Transformers itself being missing
    is an incompatibility, not a package to install.
    """
    message = str(error)
    missing = MISSING_PACKAGES.search(message)
    if missing:
        packages = [name.strip() for name in missing.group(1).split(",")]
        return event_unmet(packages, False, message)
    if isinstance(error, ModuleNotFoundError) and error.name:
        package = error.name.split(".")[0]
        if not package.startswith("transformers"):
            return event_unmet([package], False, message)
    if "does not recognize this architecture" in message:
        return event_unmet([], True, message)
    return None


def info(request: WorkerRequest) -> None:
    model_id = request["model"]
    device = request.get("device", "auto")
    try:
        model = get_model(model_id, device)
    except Exception as error:
        need = unmet_need(error)
        emit(need if need else event_error(traceback.format_exc()))
        return
    seq_len = model.get_max_seq_length()

    # Through the model rather than from its metadata. A model whose custom code
    # does not run on this Transformers loads without complaint and only fails
    # here, and the width that comes out is the one the index has to hold.
    dim = safe_encode(model, ["Probe"]).shape[1]

    if int(dim) == 0:
        emit(event_error(f"Unable to determine dimension for model '{model_id}' via probe."))
        return

    # Handle Infinity for JSON compatibility
    if seq_len == float('inf') or seq_len > 1_000_000:
        seq_len = 999999
    emit(event_info(int(dim), int(seq_len)))
    emit(event_done())
