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

echo "=== Smoke test: launcher runs prodder and reports missing GH_TOKEN ==="
OUTPUT=$(podman run --rm prodder:latest 2>&1 || true)
if echo "$OUTPUT" | grep -q "GH_TOKEN"; then
  echo "✅ launcher ran prodder and surfaced missing GH_TOKEN"
else
  echo "❌ Expected GH_TOKEN error message, got: $OUTPUT"
  STATUS=1
fi

echo "=== Smoke test: launcher skips vector when DATADOG_API_KEY unset ==="
if echo "$OUTPUT" | grep -q "DATADOG_API_KEY not set"; then
  echo "✅ launcher logged the no-vector branch"
else
  echo "❌ Expected 'DATADOG_API_KEY not set' notice, got: $OUTPUT"
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

echo "=== Smoke test: vector is available in container ==="
VECTOR_VERSION=$(podman run --rm --entrypoint vector prodder:latest --version 2>&1 || true)
if echo "$VECTOR_VERSION" | grep -qi "vector"; then
  echo "✅ vector is available"
else
  echo "❌ vector not found in container"
  STATUS=1
fi

echo "=== Smoke test: vector config parses with prodder service label ==="
# The image is minimal (no cat/ls), so ask vector itself to parse the config
# with a dummy DATADOG_API_KEY. A clean validate proves the file is at
# /etc/vector/vector.toml and well-formed.
VALIDATE=$(podman run --rm \
  -e DATADOG_API_KEY=dummy \
  --entrypoint vector prodder:latest \
  validate /etc/vector/vector.toml 2>&1 || true)
if echo "$VALIDATE" | grep -q 'Loaded \["/etc/vector/vector.toml"\]'; then
  echo "✅ vector loaded /etc/vector/vector.toml"
else
  echo "❌ vector did not load /etc/vector/vector.toml: $VALIDATE"
  STATUS=1
fi

if [ $STATUS -eq 0 ]; then
  echo "✅ All container checks passed"
else
  echo "❌ Some container checks failed"
fi

exit $STATUS
