import json
import threading
from typing import Any, Dict

# A model load reports its download from a second thread, and `print` writes
# the line and its newline separately: unlocked, two events can land on one
# line, and the host reads stdout one JSON object per line.
_stdout_lock = threading.Lock()

def emit(event: Dict[str, Any]) -> None:
    line = json.dumps(event)
    with _stdout_lock:
        print(line, flush=True)
