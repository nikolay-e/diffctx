#!/bin/bash
# Build the multi-arch bench image locally with docker buildx.
#
# Usage:
#   scripts/build_bench_image.sh                       # build linux/amd64 + linux/arm64, load native to local docker
#   PLATFORMS=linux/arm64 scripts/build_bench_image.sh # single arch
#   PUSH=1 IMAGE=ghcr.io/me/treemapper-bench:dev scripts/build_bench_image.sh  # push to remote
#   BAKE_LIMIT=20 scripts/build_bench_image.sh         # tiny cache (faster build for testing)
#
# Requirements: docker buildx (Docker Desktop / OrbStack / Linux docker-buildx).

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT" || exit 1

IMAGE="${IMAGE:-treemapper-bench:latest}"
PLATFORMS="${PLATFORMS:-linux/amd64,linux/arm64}"
PUSH="${PUSH:-0}"
BAKE_DATASET="${BAKE_DATASET:-full}"
BAKE_LIMIT="${BAKE_LIMIT:-0}"
BAKE_PARALLELISM="${BAKE_PARALLELISM:-4}"
CACHE_REF="${CACHE_REF:-}"

BUILDER_NAME="treemapper-bench-builder"

# Ensure a buildx builder with multi-arch support exists
if ! docker buildx inspect "$BUILDER_NAME" >/dev/null 2>&1; then
  echo "[setup] creating buildx builder '$BUILDER_NAME' with docker-container driver"
  docker buildx create --name "$BUILDER_NAME" --driver docker-container --use
fi
docker buildx use "$BUILDER_NAME"
docker buildx inspect --bootstrap >/dev/null

CACHE_FROM_ARGS=()
CACHE_TO_ARGS=()
if [ -n "$CACHE_REF" ]; then
  CACHE_FROM_ARGS+=(--cache-from "type=registry,ref=$CACHE_REF")
  CACHE_TO_ARGS+=(--cache-to "type=registry,ref=$CACHE_REF,mode=max,image-manifest=true,oci-mediatypes=true")
fi

OUTPUT_ARGS=()
if [ "$PUSH" = "1" ]; then
  OUTPUT_ARGS+=(--push)
elif [ "$(echo "$PLATFORMS" | tr ',' '\n' | wc -l)" = "1" ]; then
  # Single platform — load to local docker daemon
  OUTPUT_ARGS+=(--load)
else
  echo "[note] multi-platform build without --push will only stay in buildx cache (not loadable into local docker)"
fi

echo "[build] image=$IMAGE platforms=$PLATFORMS dataset=$BAKE_DATASET limit=$BAKE_LIMIT push=$PUSH"
docker buildx build \
  --platform "$PLATFORMS" \
  --file Dockerfile.bench \
  --tag "$IMAGE" \
  --build-arg "BAKE_DATASET=$BAKE_DATASET" \
  --build-arg "BAKE_LIMIT=$BAKE_LIMIT" \
  --build-arg "BAKE_PARALLELISM=$BAKE_PARALLELISM" \
  ${CACHE_FROM_ARGS[@]+"${CACHE_FROM_ARGS[@]}"} \
  ${CACHE_TO_ARGS[@]+"${CACHE_TO_ARGS[@]}"} \
  ${OUTPUT_ARGS[@]+"${OUTPUT_ARGS[@]}"} \
  .

echo "[done] $IMAGE built for $PLATFORMS"
