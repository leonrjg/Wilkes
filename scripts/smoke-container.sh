#!/usr/bin/env bash
# Run a freshly built native image without downloading any models.
set -euo pipefail
image="${1:?usage: smoke-container.sh IMAGE}"
container=""
cleanup() {
  if [[ -n "$container" ]]; then
    docker logs "$container"
    docker rm --force "$container" > /dev/null
  fi
}
trap cleanup EXIT

# Exercise the bundled executable and its dynamic libraries; EOF shuts it down.
docker run --rm --entrypoint /bin/sh "$image" -c '/app/wilkes-rust-worker </dev/null && /app/uv --version'
token="wilkes-container-smoke-token"
container="$(docker run --detach --publish 127.0.0.1::2000 \
  --env WILKES_API_TOKEN="$token" \
  --tmpfs /data:uid=1001,gid=1001 "$image")"
address="$(docker port "$container" 2000/tcp)"
python3 - "$address" "$token" <<'PY'
import json
import sys
import time
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen

base = f"http://{sys.argv[1]}"
auth = {"Authorization": f"Bearer {sys.argv[2]}"}
deadline = time.monotonic() + 60
while True:
    try:
        with urlopen(Request(f"{base}/api/settings", headers=auth), timeout=3) as response:
            settings = json.load(response)
        assert isinstance(settings, dict) and "image_analysis" in settings, settings
        break
    except (URLError, TimeoutError):
        if time.monotonic() >= deadline:
            raise
        time.sleep(1)
# The container binds every interface, so the API refuses a caller without
# the token.
try:
    urlopen(f"{base}/api/settings", timeout=3)
    raise AssertionError("the API answered without its token")
except HTTPError as error:
    assert error.code == 401, error.code
with urlopen(base, timeout=3) as response:
    html = response.read().decode()
assert 'id="root"' in html, "Packaged frontend is missing"
needle = "wilkes-container-smoke-needle"
boundary = "wilkes-container-smoke"
body = (
    f"--{boundary}\r\n"
    'Content-Disposition: form-data; name="file"; filename="smoke.txt"\r\n'
    "Content-Type: text/plain\r\n\r\n"
    f"{needle}\r\n--{boundary}--\r\n"
).encode()
request = Request(f"{base}/api/upload", data=body, headers={
    "Content-Type": f"multipart/form-data; boundary={boundary}", **auth,
})
with urlopen(request, timeout=10) as response:
    upload = json.load(response)
assert upload["file_count"] == 1, upload
request = Request(f"{base}/api/search", data=json.dumps({
    "pattern": needle, "root": upload["root"], "is_regex": False,
    "case_sensitive": False, "max_results": 10,
}).encode(), headers={"Content-Type": "application/json", **auth})
with urlopen(request, timeout=10) as response:
    events = response.read().decode()
assert "event: error" not in events, events
assert "event: result" in events and needle in events, events
print("Packaged worker launches; fresh server serves its UI, requires its token, serves settings, uploads a document, and finds its text without models.")
PY
