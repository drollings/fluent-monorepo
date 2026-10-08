#!/usr/bin/env bash
# Container smoke for the prototype server: compile the release binary,
# build the image, prove it has no Python, boot it, and exercise /health
# plus one paper preview.
#
# Exit status is 0 only when every probe passes. Requires podman, a Rust
# toolchain (`cargo build --release -p protopyne-server`), and a network
# path to the base-image registry on first build.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
IMAGE="${PROTOPYNE_IMAGE:-localhost/protopyne-server:smoke}"
PORT="${PROTOPYNE_PORT:-19060}"
NAME="protopyne-smoke-$$"

cd "$ROOT"
echo "--- building $IMAGE (compiles inside the builder stage)"
podman build -f src/protopyne-server/Containerfile -t "$IMAGE" .

echo "--- proving the image has no Python"
if podman run --rm --entrypoint /bin/sh "$IMAGE" -c 'command -v python3 || command -v python'; then
  echo "SMOKE FAIL: python interpreter present in image" >&2
  exit 1
fi
echo "no python interpreter: ok"

echo "--- booting on :$PORT"
podman run -d --rm --name "$NAME" -p "$PORT:9060" "$IMAGE" >/dev/null
cleanup() { podman rm -f "$NAME" >/dev/null 2>&1 || true; }
trap cleanup EXIT

echo "--- waiting for /health"
for _ in $(seq 1 60); do
  if curl -fsS "http://127.0.0.1:$PORT/health" >/dev/null 2>&1; then
    break
  fi
  sleep 1
done
HEALTH="$(curl -fsS "http://127.0.0.1:$PORT/health")"
echo "health: $HEALTH"
echo "$HEALTH" | grep -q '"status":"ok"' || { echo "SMOKE FAIL: unhealthy" >&2; exit 1; }

echo "--- running one paper preview"
PREVIEW="$(curl -fsS -X POST "http://127.0.0.1:$PORT/proposals/preview" \
  -H 'Content-Type: application/json' -d '{"seed":7}')"
echo "$PREVIEW" | grep -q '"fills"' || { echo "SMOKE FAIL: preview has no report" >&2; exit 1; }
echo "preview: ok"

echo "--- chart contract"
curl -fsS "http://127.0.0.1:$PORT/chart/bars" | grep -q '"close"' || { echo "SMOKE FAIL: no chart bars" >&2; exit 1; }
echo "chart: ok"

echo "SMOKE PASS"
