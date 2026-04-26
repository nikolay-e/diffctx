#!/bin/bash
set -euo pipefail

CPUS="${CPUS:-6}"
MEM="${MEM:-12g}"
WORKERS="${WORKERS:-3}"
LIMIT="${LIMIT:-9999}"
SEEDS="${SEEDS:-42}"
DATASET="${DATASET:-full}"
HF_HOME_HOST="${HF_HOME_HOST:-$HOME/.cache/huggingface}"
USE_BAKED_CACHE="${USE_BAKED_CACHE:-1}"
CACHE_HOST="${CACHE_HOST:-$HOME/.cache/contextbench_repos}"
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

mkdir -p "$HF_HOME_HOST" "$REPO_ROOT/results/logs"
[[ "$USE_BAKED_CACHE" = "1" ]] || mkdir -p "$CACHE_HOST"

IMAGE="${IMAGE:-treemapper-bench:latest}"

if [[ "${BUILD:-1}" = "1" ]]; then
  echo "[build] $IMAGE"
  docker build --platform linux/arm64 -f "$REPO_ROOT/Dockerfile.bench" -t "$IMAGE" "$REPO_ROOT"
fi

echo "[run] cpus=$CPUS mem=$MEM workers=$WORKERS limit=$LIMIT dataset=$DATASET"

LOG_DIR="$REPO_ROOT/results/logs"
SWEEP_LOG="$LOG_DIR/sweep_docker.log"
echo "[sweep start] $(date -u +%FT%TZ)" | tee "$SWEEP_LOG"

for mode in hybrid ppr ego bm25; do
  for budget in -1 0 16000; do
    TAG="cb_${mode}_b${budget}"
    LOG="$LOG_DIR/${TAG}_docker.log"
    T0=$(date -u +%s)
    echo "[run start] mode=$mode budget=$budget at $(date -u +%FT%TZ)" | tee -a "$SWEEP_LOG"
    CACHE_MOUNT_ARGS=()
    if [[ "$USE_BAKED_CACHE" != "1" ]]; then
      CACHE_MOUNT_ARGS=(-v "$CACHE_HOST:/cache/contextbench_repos")
    fi
    docker run --rm \
      --cpus="$CPUS" \
      --memory="$MEM" \
      --memory-swap="$MEM" \
      -e BENCH_WORKERS="$WORKERS" \
      -e BENCH_BATCH_SIZE="${BATCH_SIZE:-$WORKERS}" \
      -e PYTHONUNBUFFERED=1 \
      -e HF_HOME=/cache/huggingface \
      -e HF_DATASETS_CACHE=/cache/huggingface/datasets \
      "${CACHE_MOUNT_ARGS[@]}" \
      -v "$HF_HOME_HOST:/cache/huggingface" \
      -v "$REPO_ROOT/results:/app/results" \
      -v "$REPO_ROOT/benchmarks:/app/benchmarks:ro" \
      -v "$REPO_ROOT/src/treemapper:/app/src/treemapper:ro" \
      "$IMAGE" \
      cb --scoring "$mode" --budget "$budget" --limit "$LIMIT" --dataset "$DATASET" --seeds "$SEEDS" \
      >"$LOG" 2>&1
    RC=$?
    T1=$(date -u +%s)
    echo "[run done ] mode=$mode budget=$budget rc=$RC dur=$((T1 - T0))s" | tee -a "$SWEEP_LOG"
  done
done
echo "[sweep done ] $(date -u +%FT%TZ)" | tee -a "$SWEEP_LOG"
