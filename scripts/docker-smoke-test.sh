#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

IMAGE="${SMOKE_IMAGE:-inf-splitter:smoke-test}"
CONTAINER="inf-splitter-smoke-${RANDOM}"
TMPDIR="$(mktemp -d)"
FAKE_PID=""

cleanup() {
  if [[ -n "$FAKE_PID" ]]; then
    kill "$FAKE_PID" 2>/dev/null || true
    wait "$FAKE_PID" 2>/dev/null || true
  fi
  docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
  rm -rf "$TMPDIR"
}
trap cleanup EXIT

FAKE_PORT=$(python3 -c 'import socket; s=socket.socket(); s.bind(("", 0)); print(s.getsockname()[1]); s.close()')

python3 -c "
from http.server import HTTPServer, BaseHTTPRequestHandler

class H(BaseHTTPRequestHandler):
    def do_HEAD(self):
        self.send_response(200)
        self.end_headers()
    def log_message(self, *args):
        pass
HTTPServer(('0.0.0.0', $FAKE_PORT), H).serve_forever()
" &
FAKE_PID=$!

# Get host IP visible from the container (via slirp4netns gateway).
HOST_IP=$(ip route get 1 2>/dev/null | awk '{print $7; exit}')
if [[ -z "$HOST_IP" ]]; then
  HOST_IP=$(hostname -I | awk '{print $1}')
fi
echo "Fake upstream: http://${HOST_IP}:${FAKE_PORT}"

mkdir -p "$TMPDIR/config"
cat > "$TMPDIR/config/inf-splitter.toml" <<EOF

[local]
endpoint_openai = "http://${HOST_IP}:${FAKE_PORT}"
models = "smoke-model"
EOF

http_get() {
  local url="$1"
  curl -fsS "$url"
}

if [[ "${SKIP_BUILD:-0}" != "1" ]]; then
  echo "Building ${IMAGE}..."
  docker build -t "$IMAGE" .
fi

echo "Starting container ${CONTAINER}..."
docker run -d --name "$CONTAINER" \
  -p "127.0.0.1::3000" \
  -v "${TMPDIR}/config:/app/config:ro" \
  "$IMAGE" >/dev/null

HOST_PORT="$(docker port "$CONTAINER" 3000/tcp | head -n1 | cut -d: -f2)"
BASE_URL="http://127.0.0.1:${HOST_PORT}"

echo "Waiting for ${BASE_URL}/health ..."
response=""
for _ in $(seq 1 30); do
  if response="$(http_get "${BASE_URL}/health" 2>/dev/null)"; then
    echo "GET /health: ${response}"
    break
  fi
  sleep 1
done

if [[ "${response:-}" == "" ]]; then
  echo "Smoke test failed: /health did not become ready" >&2
  docker logs "$CONTAINER" >&2 || true
  exit 1
fi

models="$(http_get "${BASE_URL}/openai/v1/models")"
if [[ "$models" != *"smoke-model"* ]]; then
  echo "Smoke test failed: /openai/v1/models response unexpected: ${models}" >&2
  exit 1
fi
echo "GET /openai/v1/models OK"

echo "Docker smoke test passed."
