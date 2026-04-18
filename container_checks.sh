#!/usr/bin/env bash
set -euo pipefail

STATUS=0
CONTAINER_ID=""

cleanup() {
  if [ -n "$CONTAINER_ID" ]; then
    podman stop "$CONTAINER_ID" &>/dev/null || true
    podman rm "$CONTAINER_ID" &>/dev/null || true
  fi
}
trap cleanup EXIT

echo "=== Smoke test: missing GH_TOKEN exits with error ==="
OUTPUT=$(podman run --rm prodder:latest 2>&1 || true)
if echo "$OUTPUT" | grep -q "GH_TOKEN"; then
  echo "✅ Binary ran and reported missing GH_TOKEN"
else
  echo "❌ Expected GH_TOKEN error message, got: $OUTPUT"
  STATUS=1
fi

echo "=== Smoke test: curl is available in container ==="
CURL_VERSION=$(podman run --rm --entrypoint curl prodder:latest --version 2>&1 || true)
if echo "$CURL_VERSION" | grep -qi "curl"; then
  echo "✅ curl is available"
else
  echo "❌ curl not found in container"
  STATUS=1
fi

if [ $STATUS -eq 0 ]; then
  echo "✅ All container checks passed"
else
  echo "❌ Some container checks failed"
fi

exit $STATUS
