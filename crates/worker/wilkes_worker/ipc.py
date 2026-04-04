import json
from typing import Any, Dict

def emit(event: Dict[str, Any]) -> None:
    print(json.dumps(event), flush=True)
