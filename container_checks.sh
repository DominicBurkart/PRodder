#!/usr/bin/env bash
#
# Backwards-compat shim: the canonical smoke test now lives at
# scripts/smoke.sh and is invoked against the nix-built OCI image.
# This script continues to work for the Dockerfile-based flow used by
# PR #5 / the existing CI container job, which pre-builds the image
# with `podman build -t prodder:latest .` and expects this entrypoint.
#
# Contract (unchanged): caller has already loaded prodder:latest into
# podman. This shim just forwards to scripts/smoke.sh with --no-build.
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "$SCRIPT_DIR/scripts/smoke.sh" --no-build "$@"
