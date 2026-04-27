#!/usr/bin/env bash
# Thin wrapper around scripts/sensitivity_check.py.
#
# Usage:
#   scripts/sensitivity_check.sh [--diff RANGE] [--budget N] [--repo PATH] [--params LIST]
#
# Defaults: --diff HEAD~5..HEAD --budget 4096 --repo .
# Perturbs each Group-C operational parameter by ±25% and ±50%, prints a
# table of token-count delta and Jaccard similarity of the selected
# fragment set vs baseline. Run a small subset with --params first to
# verify env-overrides take effect:
#
#   scripts/sensitivity_check.sh --params DIFFCTX_OP_PPR_ALPHA,DIFFCTX_OP_UTILITY_ETA

set -euo pipefail
SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
exec python3 "$SCRIPT_DIR/sensitivity_check.py" "$@"
