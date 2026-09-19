"""The full sweep's cell list, filtered by method, as a GitHub Actions output.

Excluded combinations and why:
- ppr and bm25 do not consume depth: only their depth=-1 cell runs.
- ego always has a radius: no depth=-1 cell.
- internal-bm25 maps to ScoringMode::Bm25, whose depth is the hardcoded
  `ego_depth_default` that DIFFCTX_OP_GRAPH_DEPTH does not reach
  (config/mode.rs), so its depth cells were byte-identical copies of -1.
- rrf and pit map to `ego_depth_extended`, which the override does reach:
  their depth cells are five distinct configurations and all run.
- aider has no shareable state across map_tokens values: one budget per
  cell, and B=-1 / B=0 do not apply to a repo-map subprocess.
"""

from __future__ import annotations

import json
import sys

METHODS = ["ppr", "ego", "bm25", "internal-bm25", "rrf", "pit", "aider"]
TEST_SETS = ["contextbench_verified", "polybench500", "swebench_verified"]
DEPTHS = [-1, 0, 1, 2, 3, 4]
AIDER_BUDGETS = [8000, 16000, 32000, 64000, 128000]
DEPTHLESS = {"ppr", "bm25", "internal-bm25"}


def cells(wanted: set[str]) -> list[dict]:
    out: list[dict] = []
    for method in METHODS[:-1]:
        if method not in wanted:
            continue
        for depth in DEPTHS:
            if method in DEPTHLESS and depth != -1:
                continue
            if method == "ego" and depth == -1:
                continue
            for test_set in TEST_SETS:
                out.append({"method": method, "depth": depth, "test_set": test_set})
    if "aider" in wanted:
        for test_set in TEST_SETS:
            for budget in AIDER_BUDGETS:
                out.append({"method": "aider", "depth": -1, "test_set": test_set, "budget": budget})
    return out


def main() -> int:
    arg = (sys.argv[1] if len(sys.argv) > 1 else "all").strip()
    wanted = set(METHODS) if arg in ("", "all") else {m.strip() for m in arg.split(",") if m.strip()}
    unknown = wanted - set(METHODS)
    if unknown:
        sys.stderr.write(f"unknown sweep methods: {sorted(unknown)}\n")
        return 1
    selected = cells(wanted)
    if not selected:
        sys.stderr.write("no cells selected\n")
        return 1
    sys.stderr.write(f"{len(selected)} cells\n")
    print("cells=" + json.dumps(selected, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
