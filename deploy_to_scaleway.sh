#!/usr/bin/env bash
#
# Deploy PRodder as a Scaleway Serverless Job.
#
# Recurring path (after initial bootstrap, see below):
#   - builds the container image via Nix (./result is a docker-archive tarball)
#   - pushes it to rg.fr-par.scw.cloud/prodder/prodder:latest
#   - the next scheduled run of the `prodder` job definition picks it up
#
# One-time bootstrap (done manually; kept here for reproducibility).
# Specific values (schedule, resource limits, timeout) are intentionally
# omitted — query the live job definition with `scw jobs definition list`
# rather than mirroring them here, since comments inevitably drift.
#
#   scw registry namespace create name=prodder region=fr-par
#
#   scw jobs definition create \
#     name=prodder \
#     image-uri=rg.fr-par.scw.cloud/prodder/prodder:latest \
#     <cpu-limit / memory-limit / job-timeout / cron-schedule.* per current spec>
#
#   JOB_ID=$(scw jobs definition list -o json | jq -r '.[] | select(.name=="prodder") | .id')
#   GH_TOKEN_SM_ID=$(scw secret secret list name=GH_TOKEN -o json | jq -r '.[0].id')
#   DATADOG_SM_ID=$(scw secret secret list name=personal-datadog-account-api-key \
#     -o json | jq -r '.[0].id')
#
#   scw jobs secret create job-definition-id=$JOB_ID \
#     secret-manager-id=$GH_TOKEN_SM_ID \
#     secret-manager-version=latest env-var.name=GH_TOKEN
#   scw jobs secret create job-definition-id=$JOB_ID \
#     secret-manager-id=$DATADOG_SM_ID \
#     secret-manager-version=latest env-var.name=DATADOG_API_KEY

set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" &>/dev/null && pwd)
cd "$SCRIPT_DIR"

BRANCH="$(git rev-parse --abbrev-ref HEAD | awk '{$1=$1};1')"
if [[ "$BRANCH" != "main" ]]; then
  echo "refusing to deploy: not on main (current: $BRANCH)"
  exit 1
fi

if ! git diff-index --quiet HEAD --; then
  echo "refusing to deploy: uncommitted changes"
  exit 1
fi

git remote update
GIT_STATUS="$(git status -uno)"
if [[ "$GIT_STATUS" != *"Your branch is up to date"* \
   && "$GIT_STATUS" != *"Votre branche est à jour"* ]]; then
  echo "refusing to deploy: local branch is not in sync with remote"
  exit 1
fi

REGISTRY="rg.fr-par.scw.cloud"
NAMESPACE="prodder"
IMAGE="prodder"
TAG="latest"
DEST="${REGISTRY}/${NAMESPACE}/${IMAGE}:${TAG}"

echo "=> building container image via Nix"
nix build .#container

ARCHIVE="$(readlink -f ./result)"

echo "=> pushing ${ARCHIVE} to ${DEST}"
# Auth: skopeo reads credentials from ~/.config/containers/auth.json or the
# REGISTRY_AUTH_FILE env var. Log in first with:
#   echo "$SCW_SECRET_KEY" | skopeo login --username nologin --password-stdin rg.fr-par.scw.cloud
# See https://www.scaleway.com/en/docs/containers/container-registry/how-to/push-images/
skopeo copy --dest-tls-verify=true \
  "docker-archive:${ARCHIVE}" \
  "docker://${DEST}"

echo "=> pushed ${DEST}"
echo "   next scheduled run of the prodder job will pull the new image."
echo "   to trigger immediately:"
echo "     JOB_ID=\$(scw jobs definition list -o json | jq -r '.[] | select(.name==\"prodder\") | .id')"
echo "     scw jobs definition start \$JOB_ID"
