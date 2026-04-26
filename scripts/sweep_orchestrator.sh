#!/bin/bash
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT" || exit 1

LOG_DIR="$REPO_ROOT/results/logs"
SWEEP_LOG="$LOG_DIR/sweep_orchestrator.log"
mkdir -p "$LOG_DIR"

CACHE_HOST="$HOME/.cache/contextbench_repos"
HF_HOME_HOST="$HOME/.cache/huggingface"
IMAGE="${IMAGE:-treemapper-bench:latest}"
USE_BAKED_CACHE="${USE_BAKED_CACHE:-1}"

MIN_OK_THRESHOLD=550
LIMIT=9999
DATASET=full
SEEDS=42
CPUS=10
MEM=38g

declare -a TIERS=(
  "7 20"
  "4 12"
)

declare -a CONFIGS=(
  "hybrid -1"
  "hybrid 0"
  "hybrid 16000"
  "ppr -1"
  "ppr 0"
  "ppr 16000"
  "ego -1"
  "ego 0"
  "ego 16000"
  "bm25 -1"
  "bm25 0"
  "bm25 16000"
)

log() {
  printf '[%s] %s\n' "$(date -u +%FT%TZ)" "$*" | tee -a "$SWEEP_LOG"
  return 0
}

run_one_config() {
  local mode="$1"
  local budget="$2"
  local workers="$3"
  local batch="$4"
  local tag="cb_${mode}_b${budget}_w${workers}"
  local log="$LOG_DIR/${tag}_docker.log"
  local t0
  t0=$(date -u +%s)
  log "RUN start mode=$mode budget=$budget workers=$workers batch=$batch"
  local cache_mount=()
  if [[ "$USE_BAKED_CACHE" != "1" ]]; then
    cache_mount=(-v "$CACHE_HOST:/cache/contextbench_repos")
  fi
  docker run --rm \
    --cpus="$CPUS" \
    --memory="$MEM" \
    --memory-swap="$MEM" \
    -e BENCH_WORKERS="$workers" \
    -e BENCH_BATCH_SIZE="$batch" \
    -e PYTHONUNBUFFERED=1 \
    -e HF_HOME=/cache/huggingface \
    -e HF_DATASETS_CACHE=/cache/huggingface/datasets \
    "${cache_mount[@]}" \
    -v "$HF_HOME_HOST:/cache/huggingface" \
    -v "$REPO_ROOT/results:/app/results" \
    -v "$REPO_ROOT/benchmarks:/app/benchmarks:ro" \
    -v "$REPO_ROOT/src/treemapper:/app/src/treemapper:ro" \
    "$IMAGE" \
    cb --scoring "$mode" --budget "$budget" --limit "$LIMIT" --dataset "$DATASET" --seeds "$SEEDS" \
    >"$log" 2>&1
  local rc=$?
  local t1
  t1=$(date -u +%s)
  log "RUN done  mode=$mode budget=$budget workers=$workers rc=$rc dur=$((t1 - t0))s"
  return $rc
}

count_ok() {
  local mode="$1"
  local budget="$2"
  local f="$REPO_ROOT/results/cb_${mode}_n${LIMIT}_b${budget}.json"
  if [[ ! -f "$f" ]]; then
    echo 0
    return 0
  fi
  python3 -c "
import json,sys
try:
    d=json.load(open('$f'))
    r=d.get('results', d)
    print(sum(1 for x in r if x.get('status')=='ok'))
except Exception:
    print(0)
"
}

log "=== ORCHESTRATOR START ==="
log "tiers: WORKERS/BATCH = ${TIERS[*]}"
log "configs: 12 (4 modes x 3 budgets), full nontrivial dataset"

declare -a FAILED=()

for cfg in "${CONFIGS[@]}"; do
  mode=$(echo "$cfg" | awk '{print $1}')
  budget=$(echo "$cfg" | awk '{print $2}')
  existing_ok=$(count_ok "$mode" "$budget")
  if [[ "$existing_ok" -ge "$MIN_OK_THRESHOLD" ]]; then
    log "CFG $mode b=$budget SKIP (existing ok=$existing_ok >= $MIN_OK_THRESHOLD)"
    continue
  fi
  ok_count=0
  attempt=0
  for tier in "${TIERS[@]}"; do
    workers=$(echo "$tier" | awk '{print $1}')
    batch=$(echo "$tier" | awk '{print $2}')
    attempt=$((attempt + 1))
    log "CFG $mode b=$budget attempt=$attempt (w=$workers, batch=$batch)"
    run_one_config "$mode" "$budget" "$workers" "$batch"
    ok_count=$(count_ok "$mode" "$budget")
    log "CFG $mode b=$budget attempt=$attempt RESULT ok=$ok_count threshold=$MIN_OK_THRESHOLD"
    if [[ "$ok_count" -ge "$MIN_OK_THRESHOLD" ]]; then
      log "CFG $mode b=$budget OK (ok=$ok_count, attempt=$attempt)"
      break
    fi
    log "CFG $mode b=$budget BELOW THRESHOLD (ok=$ok_count) — escalating to next tier"
  done
  if [[ "$ok_count" -lt "$MIN_OK_THRESHOLD" ]]; then
    FAILED+=("$mode b=$budget ok=$ok_count")
    log "CFG $mode b=$budget FAILED ALL TIERS (ok=$ok_count)"
  fi
done

log "=== ORCHESTRATOR DONE ==="
log "Failed configs: ${#FAILED[@]}"
for f in "${FAILED[@]}"; do
  log "  FAIL: $f"
done

# Generate aggregation report
log "=== Generating SWEEP_REPORT.md ==="
python3 "$REPO_ROOT/scripts/aggregate_sweep_report.py" 2>&1 | tee -a "$SWEEP_LOG"

log "=== ALL DONE ==="
