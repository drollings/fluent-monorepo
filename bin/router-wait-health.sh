#!/usr/bin/env bash
# Poll a service health URL until it answers 2xx or a timeout elapses.
# On timeout, tail the service's log and fail loudly (make aborts the target).
#
# Usage: router-wait-health.sh <health-url> [timeout_s] [logfile] [service-name]
#   timeout_s     default 30
#   logfile       default /tmp/coral-router.out (tailed on failure)
#   service-name  default coral-router (used in status messages only)
set -euo pipefail

URL="${1:?usage: router-wait-health.sh <health-url> [timeout_s] [logfile] [service-name]}"
TIMEOUT="${2:-30}"
LOG="${3:-/tmp/coral-router.out}"
SERVICE="${4:-coral-router}"

if ! command -v curl > /dev/null 2>&1; then
    echo "ERROR: curl not found" >&2
    exit 1
fi

for ((i = 1; i <= TIMEOUT; i++)); do
    if curl -sf -m 1 "$URL" > /dev/null 2>&1; then
        echo "${SERVICE} healthy at ${URL} (attempt ${i}/${TIMEOUT})"
        exit 0
    fi
    sleep 1
done

echo "ERROR: ${SERVICE} did not become healthy at ${URL} within ${TIMEOUT}s" >&2
if [ -n "$LOG" ] && [ -f "$LOG" ]; then
    echo "--- tail of ${LOG} ---" >&2
    tail -40 "$LOG" >&2
fi
exit 1
