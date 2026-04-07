import sys
import json
import logging
import traceback

from .ipc import emit
from .handlers import embed_texts, info
from .protocol import WorkerRequest, event_error

def configure_logging():
    # Configure logging to stderr so the Rust side can capture and display it
    logging.basicConfig(
        level=logging.INFO,
        format="%(name)s - %(levelname)s - %(message)s",
        stream=sys.stderr
    )

    # Disable info logs from huggingface_hub and its underlying http client
    logging.getLogger("huggingface_hub").setLevel(logging.WARNING)
    logging.getLogger("httpx").setLevel(logging.WARNING)
    try:
        from huggingface_hub.utils import logging as hf_logging
        hf_logging.set_verbosity_warning()
    except ImportError:
        pass

    # Ensure transformers is also verbose enough
    try:
        from transformers.utils import logging as tf_logging
        tf_logging.set_verbosity_warning()
        tf_logging.enable_default_handler()
    except ImportError:
        pass

def main():
    configure_logging()
    
    while True:
        line = sys.stdin.readline()
        if not line:
            break

        try:
            request: WorkerRequest = json.loads(line)
            sys.stderr.write(f"Request: {json.dumps({k: v for k, v in request.items() if k != 'texts'})}\n")
            mode = request.get("mode", "embed")

            if mode == "embed":
                embed_texts(request)
            elif mode == "info":
                info(request)
            else:
                emit(event_error(f"Unknown mode: {mode}"))
        except Exception:
            emit(event_error(traceback.format_exc()))

if __name__ == "__main__":
    main()
