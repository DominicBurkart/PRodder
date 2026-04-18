#!/usr/bin/env bash
#
# Smoke test for the nix-built PRodder OCI container.
#
# Usage:
#   ./scripts/smoke.sh                # builds and loads via `nix build .#container`
#   ./scripts/smoke.sh --stream       # uses `nix run .#containerStream | podman load`
#   PRODDER_IMAGE=prodder:latest ./scripts/smoke.sh --no-build   # skip the build
#
# Exit codes:
#   0  all smoke checks passed
#   1  one or more smoke checks failed
#   2  prerequisite missing (nix or podman)

set -euo pipefail

IMAGE="${PRODDER_IMAGE:-prodder:latest}"
MODE="build"
DO_BUILD=1

for arg in "$@"; do
  case "$arg" in
    --stream)    MODE="stream" ;;
    --no-build)  DO_BUILD=0 ;;
    -h|--help)
      sed -n '2,14p' "$0"
      exit 0
      ;;
    *) echo "unknown arg: $arg" >&2; exit 2 ;;
  esac
done

need() {
  command -v "$1" >/dev/null 2>&1 || { echo "missing prerequisite: $1" >&2; exit 2; }
}
need podman

if [ "$DO_BUILD" = "1" ]; then
  need nix
  case "$MODE" in
    build)
      echo "=== nix build .#container ==="
      nix build .#container
      echo "=== podman load < result ==="
      podman load <"$(readlink -f result)"
      ;;
    stream)
      echo "=== nix run .#containerStream | podman load ==="
      nix run .#containerStream | podman load
      ;;
  esac
fi

STATUS=0
pass() { printf '  PASS  %s\n' "$1"; }
fail() { printf '  FAIL  %s\n' "$1"; STATUS=1; }

echo "=== Smoke: image is present ==="
if podman image exists "$IMAGE"; then
  pass "image $IMAGE present"
else
  fail "image $IMAGE not present"
  exit $STATUS
fi

echo "=== Smoke: image is distroless (no /bin/sh) ==="
# A distroless image has no shell. `podman run ... /bin/sh -c true` must fail.
if podman run --rm --entrypoint /bin/sh "$IMAGE" -c true 2>/dev/null; then
  fail "image contains /bin/sh (not distroless)"
else
  pass "no /bin/sh in image (distroless)"
fi

echo "=== Smoke: binary runs and exits non-zero without GH_TOKEN ==="
OUTPUT=$(podman run --rm "$IMAGE" 2>&1 || true)
if printf '%s' "$OUTPUT" | grep -q "GH_TOKEN"; then
  pass "binary reports missing GH_TOKEN"
else
  fail "expected GH_TOKEN error, got: $OUTPUT"
fi

echo "=== Smoke: TLS trust store is wired up ==="
# SSL_CERT_FILE env must point at cacert bundle (even if we can't read it
# from the host, we can inspect it with `podman image inspect`).
CERT_ENV=$(podman image inspect "$IMAGE" \
  --format '{{range .Config.Env}}{{println .}}{{end}}' \
  | grep '^SSL_CERT_FILE=' || true)
if [ -n "$CERT_ENV" ]; then
  pass "$CERT_ENV"
else
  fail "SSL_CERT_FILE not set in image env"
fi

echo "=== Smoke: image labels present ==="
LABELS=$(podman image inspect "$IMAGE" \
  --format '{{range $k,$v := .Config.Labels}}{{$k}}={{$v}}{{println}}{{end}}' \
  || true)
if printf '%s' "$LABELS" | grep -q 'org.opencontainers.image.source'; then
  pass "OCI labels set"
else
  fail "OCI labels missing"
fi

if [ "$STATUS" -eq 0 ]; then
  echo ""
  echo "All smoke checks passed."
else
  echo ""
  echo "Smoke checks FAILED."
fi
exit $STATUS
