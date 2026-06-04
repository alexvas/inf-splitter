#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

IMAGE="${SMOKE_IMAGE:-inf-splitter:smoke-test}"
CONTAINER="inf-splitter-smoke-${RANDOM}"
TMPDIR="$(mktemp -d)"

cleanup() {
  docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
  rm -rf "$TMPDIR"
}
trap cleanup EXIT

mkdir -p "$TMPDIR/config"
cat > "$TMPDIR/config/inf-splitter.toml" <<'EOF'
port = 3383

[local]
endpoint = "http://127.0.0.1:9"
protocol = "OPENAI"
models = "smoke-model"
EOF

http_get() {
  local url="$1"
  curl -fsS "$url"
}

echo "Building ${IMAGE}..."
docker build -t "$IMAGE" .

echo "Starting container ${CONTAINER}..."
docker run -d --name "$CONTAINER" \
  -p "127.0.0.1::3383" \
  -v "${TMPDIR}/config:/app/config:ro" \
  "$IMAGE" >/dev/null

HOST_PORT="$(docker port "$CONTAINER" 3383/tcp | head -n1 | cut -d: -f2)"
BASE_URL="http://127.0.0.1:${HOST_PORT}"

echo "Waiting for ${BASE_URL}/health ..."
for _ in $(seq 1 30); do
  if response="$(http_get "${BASE_URL}/health" 2>/dev/null)"; then
    if [[ "$response" == *'"status":"ok"'* ]]; then
      echo "GET /health OK: ${response}"
      break
    fi
  fi
  sleep 1
done

if [[ "${response:-}" != *'"status":"ok"'* ]]; then
  echo "Smoke test failed: /health did not become ready" >&2
  docker logs "$CONTAINER" >&2 || true
  exit 1
fi

models="$(http_get "${BASE_URL}/v1/models")"
if [[ "$models" != *"smoke-model"* ]]; then
  echo "Smoke test failed: /v1/models response unexpected: ${models}" >&2
  exit 1
fi
echo "GET /v1/models OK"

health_status="$(docker inspect --format='{{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}' "$CONTAINER")"
echo "Docker HEALTHCHECK status: ${health_status}"

echo "Docker smoke test passed."
