"""The external mode's cell list, as a GitHub Actions output.

One entry per (dataset, shard). A dataset is registered here and in
`external_run.sh` / `external_merge.sh`; the workflow itself never names one,
so adding a dataset touches those three files and `eval/cli.py`.

    python3 external_cells.py arb_edit2ripple,contextbench_nontrivial
"""

from __future__ import annotations

import json
import sys

# dataset -> number of shards. A shard keeps whole repositories together, so the
# count is chosen to keep one hosted runner's disk (about 14 GB) comfortable.
DATASETS: dict[str, int] = {
    "arb_edit2ripple": 1,
    "contextbench_nontrivial": 6,
    "swerebench_infra": 5,
    "cfgnet500": 1,
}


def cells(requested: list[str]) -> list[dict]:
    unknown = [d for d in requested if d not in DATASETS]
    if unknown or not requested:
        raise SystemExit(f"unknown dataset {unknown or requested!r}; registered: {', '.join(sorted(DATASETS))}")
    return [{"dataset": d, "shard": i, "shards": DATASETS[d]} for d in requested for i in range(DATASETS[d])]


def main() -> int:
    spec = sys.argv[1] if len(sys.argv) > 1 else ""
    requested = sorted(DATASETS) if spec == "all" else [d.strip() for d in spec.split(",") if d.strip()]
    selected = cells(requested)
    print(f"cells={json.dumps(selected, separators=(',', ':'))}")
    print(f"datasets={json.dumps(sorted({c['dataset'] for c in selected}), separators=(',', ':'))}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
