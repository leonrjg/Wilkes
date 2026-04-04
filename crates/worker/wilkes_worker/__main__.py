import sys
import json
import logging
import traceback

from .ipc import emit
from .handlers import build_index, embed_texts, info

def configure_logging():
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

def main():
    configure_logging()
    
    while True:
        line = sys.stdin.readline()
        if not line:
            break

        try:
            request = json.loads(line)
            sys.stderr.write(f"Request: {json.dumps({k: v for k, v in request.items() if k != "texts"})}\n")
            mode = request.get("mode", "build")

            if mode == "build":
                build_index(request)
            elif mode == "embed":
                embed_texts(request)
            elif mode == "info":
                info(request)
            else:
                emit({"Error": f"Unknown mode: {mode}"})
        except Exception as e:
            emit({"Error": traceback.format_exc()})

if __name__ == "__main__":
    main()
