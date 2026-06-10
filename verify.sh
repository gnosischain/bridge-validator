#!/usr/bin/env bash
#
# verify.sh — rebuild the bridge-validator image from source and compare the
# application payload (/app: the worker binary + migrations) byte-for-byte
# against the published image, for BOTH published platform slices
# (linux/amd64 and linux/arm64). See bridge_validator/VERIFICATION_DETAILS.md
# for the trust model and limitations.
#
# This stays small because the Dockerfile pins both base images by digest and
# `cargo build --release --locked` in that pinned builder is deterministic —
# no provenance lookups or FROM-line patching needed (tags older than the
# digest-pin change may not reproduce; see VERIFICATION_DETAILS.md).
#
# Each slice is an independent artifact, so both are verified. The slice
# matching your host builds natively; the other runs under emulation
# (Rosetta/QEMU — slow, often 30+ min for a Rust release build). To verify a
# single slice only, set PLATFORMS, e.g. PLATFORMS=linux/amd64.
#
# Usage: ./verify.sh <TAG> <EXPECTED_SOURCE_COMMIT>
#   EXPECTED_SOURCE_COMMIT is required and must come from a trusted,
#   out-of-band source: tags are mutable, and a re-pointed tag plus a matching
#   malicious image would otherwise pass (the rebuild only proves
#   image-matches-its-source, not that the source is the one you trust).
#
# Optional env: SOURCE_REPO, IMAGE_REPO, PLATFORMS (space-separated)
# Requires: docker (+buildx), git; emulation for the non-native slice.
#
# Exit codes: 0 pass · 1 payload differs (possible tampering) · 2 setup/tag check failed

set -euo pipefail

TAG="${1:?Usage: $0 <TAG> <EXPECTED_SOURCE_COMMIT>}"
EXPECTED_COMMIT="${2:?EXPECTED_SOURCE_COMMIT is required — obtain it out-of-band}"
SOURCE_REPO="${SOURCE_REPO:-https://github.com/gnosischain/bridge-validator.git}"
IMAGE_REPO="${IMAGE_REPO:-gnosischain/bridge-validator}"
PLATFORMS="${PLATFORMS:-linux/amd64 linux/arm64}"

WORKDIR=$(mktemp -d -t bv-verify-XXXXXX)
trap 'rm -rf "$WORKDIR"' EXIT

echo "=== Clone $SOURCE_REPO @ $TAG ==="
git clone --quiet --depth=1 --branch "$TAG" "$SOURCE_REPO" "$WORKDIR/src"
SOURCE_COMMIT=$(git -C "$WORKDIR/src" rev-parse HEAD)
if [[ "$(echo "$SOURCE_COMMIT" | tr '[:upper:]' '[:lower:]')" != "$(echo "$EXPECTED_COMMIT" | tr '[:upper:]' '[:lower:]')"* ]]; then
  echo "ERROR: tag '$TAG' resolves to $SOURCE_COMMIT, expected $EXPECTED_COMMIT." >&2
  echo "       The tag may have been re-pointed on the remote. Stop and investigate." >&2
  exit 2
fi
echo "Tag resolves to expected commit ($SOURCE_COMMIT)."

# Verify one platform slice: rebuild, pull, extract /app from both, diff.
# Returns 0 on byte-identical payloads, 1 on any difference.
verify_platform() {
  local platform="$1"
  local slug="${platform##*/}"                       # linux/amd64 -> amd64
  local local_tag="bridge-validator:verify-$TAG-$slug"
  local pub_cid loc_cid

  echo
  echo "=== [$platform] Rebuild from source with CI's flags ==="
  docker buildx build --platform "$platform" --no-cache --provenance=false --sbom=false --load \
    -f "$WORKDIR/src/bridge_validator/Dockerfile" -t "$local_tag" "$WORKDIR/src"

  echo
  echo "=== [$platform] Pull published slice and extract /app from both ==="
  docker pull --platform "$platform" "$IMAGE_REPO:$TAG"
  pub_cid=$(docker create --platform "$platform" "$IMAGE_REPO:$TAG")
  loc_cid=$(docker create --platform "$platform" "$local_tag")
  docker cp "$pub_cid:/app" "$WORKDIR/pub-$slug"
  docker cp "$loc_cid:/app" "$WORKDIR/loc-$slug"
  docker rm "$pub_cid" "$loc_cid" >/dev/null
  docker image rm "$local_tag" >/dev/null 2>&1 || true

  echo
  echo "=== [$platform] Compare payloads ==="
  if diff -r "$WORKDIR/pub-$slug" "$WORKDIR/loc-$slug"; then
    echo "[$platform] ✅ /app (worker + migrations) is byte-identical."
    return 0
  else
    echo "[$platform] ❌ /app differs from the source rebuild."
    return 1
  fi
}

FAILED=()
for platform in $PLATFORMS; do
  verify_platform "$platform" || FAILED+=("$platform")
done

echo
echo "================================================================"
if (( ${#FAILED[@]} == 0 )); then
  echo "  ✅  VERIFICATION PASSED for: $PLATFORMS"
  echo "  Tag: $TAG · commit: $SOURCE_COMMIT · image: $IMAGE_REPO:$TAG"
  echo
  echo "  Not verified by this run: that EXPECTED_SOURCE_COMMIT is the commit"
  echo "  you intend, and that the CI runner was uncompromised — pair with the"
  echo "  digest and cosign checks in bridge_validator/HOW_TO_VERIFY.md."
  echo "================================================================"
  exit 0
else
  echo "  ❌  VERIFICATION FAILED for: ${FAILED[*]}"
  echo "  This is the signature of a tampered image. DO NOT DEPLOY."
  echo "================================================================"
  exit 1
fi
