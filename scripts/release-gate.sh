#!/usr/bin/env bash
# Judge every release gate on one commit and write EVIDENCE_<sha>.md; exit 0 only when all are green.
#
#   scripts/release-gate.sh --sha <commit> [--evidence DIR] [--binary PATH] [--out DIR]
#
# The thresholds are eval/oracle/release.toml (the oracle's own stay in eval/oracle/gates.toml); the judging is
# scripts/release_gate.py. RELEASE_GATE_PYTHON names the interpreter (default python3, 3.11 or newer).
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
exec "${RELEASE_GATE_PYTHON:-python3}" "$root/scripts/release_gate.py" "$@"
