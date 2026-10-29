"""ContextBench Verified, nontrivial instances only, as shardable manifests.

The nontrivial subset is the Verified instances whose gold names a file the
patch does not touch (the only ones where retrieval, rather than keeping the
diff's own files, is measured). It runs through the sweep's own machinery
(`python -m eval run-final`) so its numbers are the sweep's numbers; this module
only builds the shard manifests and merges the shards' results.

    python -m eval cb-nontrivial prepare --shards N --out DIR
    python -m eval cb-nontrivial summarize --shard-dirs D0 D1 ... --out FILE

The manifest id format is the committed manifests' (`contextbench_verified::...`),
so `run-final --manifests-dir DIR/shard-I` takes a shard directly.
"""

from __future__ import annotations

import argparse
import json
from collections import Counter
from pathlib import Path

from eval.external.sharding import shard_items

MANIFEST_NAME = "test_contextbench_verified.txt"
RESULTS_NAME = "contextbench_verified.json"


def nontrivial_instances() -> list:
    from eval.harness.adapters.contextbench import ContextBenchAdapter
    from eval.harness.common import patch_files_at_head

    adapter = ContextBenchAdapter(config="contextbench_verified")
    kept = [i for i in adapter.load() if i.gold_files - patch_files_at_head(i.gold_patch)]
    return sorted(kept, key=lambda i: (i.repo, i.base_commit, i.instance_id))


def prepare(shards: int, out: Path) -> dict:
    instances = nontrivial_instances()
    out.mkdir(parents=True, exist_ok=True)
    plan = {"dataset": "contextbench_nontrivial", "nontrivial": len(instances), "shards": []}
    for index in range(shards):
        mine = shard_items(instances, lambda i: i.repo, shards, index)
        shard_dir = out / f"shard-{index}"
        shard_dir.mkdir(exist_ok=True)
        (shard_dir / MANIFEST_NAME).write_text("".join(f"{i.instance_id}\n" for i in mine))
        plan["shards"].append({"index": index, "instances": len(mine), "repos": len({i.repo for i in mine})})
    (out / "plan.json").write_text(json.dumps(plan, indent=2) + "\n")
    return plan


def _mean_ci(values: list[float]) -> dict:
    from eval.harness.stats import bootstrap_ci

    mean, lo, hi = bootstrap_ci(values)
    return {"n": len(values), "mean": mean, "ci95": [lo, hi]}


def _extra_values(rows: list[dict], key: str) -> list[float]:
    return [float(r["extra"][key]) for r in rows if isinstance((r.get("extra") or {}).get(key), (int, float))]


def merge(shard_dirs: list[Path], engine_ref: str) -> dict:
    from eval.analysis.cell_metrics import compute_cell_summary

    # The checkpoint skips clone and error rows so a rerun retries them; the
    # results file keeps every instance, which is what the denominator needs.
    rows: list[dict] = []
    for shard_dir in shard_dirs:
        results = shard_dir / RESULTS_NAME
        if not results.exists():
            raise SystemExit(f"no results in {shard_dir}: the shard produced nothing")
        rows.extend(json.loads(results.read_text()))
    ok = [r for r in rows if (r.get("extra") or {}).get("status") == "ok"]
    statuses = Counter(str((r.get("extra") or {}).get("status", "unknown")) for r in rows)
    return {
        "dataset": "contextbench_nontrivial",
        "engine_ref": engine_ref,
        "engine_versions": sorted(
            {str(r["extra"]["engine_version"]) for r in rows if (r.get("extra") or {}).get("engine_version")}
        ),
        "effective_config_hashes": sorted(
            {str(r["extra"]["effective_config_hash"]) for r in rows if (r.get("extra") or {}).get("effective_config_hash")}
        ),
        "budget": sorted({int(r["budget"]) for r in rows if r.get("budget")}),
        "rows": len(rows),
        "ok": len(ok),
        "statuses": dict(sorted(statuses.items())),
        "file_recall_ok": _mean_ci([float(r.get("file_recall") or 0.0) for r in ok]),
        "file_recall_failures_zero": _mean_ci([float(r.get("file_recall") or 0.0) for r in rows]),
        "nontrivial_file_recall_ok": _mean_ci(_extra_values(ok, "nontrivial_file_recall")),
        "changed_file_retention_ok": _mean_ci(_extra_values(ok, "changed_file_retention")),
        "line_recall_ok": _mean_ci([float(r["line_recall"]) for r in ok if r.get("line_recall") is not None]),
        "cell_summary": compute_cell_summary(rows),
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    prep = sub.add_parser("prepare", help="write one manifest directory per shard")
    prep.add_argument("--shards", type=int, required=True)
    prep.add_argument("--out", type=Path, required=True)
    summ = sub.add_parser("summarize", help="merge the shards' results into one summary")
    summ.add_argument("--shard-dirs", type=Path, nargs="+", required=True)
    summ.add_argument("--out", type=Path, required=True)
    summ.add_argument("--engine-ref", default="")
    args = parser.parse_args()

    if args.command == "prepare":
        print(json.dumps(prepare(args.shards, args.out)))
        return 0
    summary = merge(args.shard_dirs, args.engine_ref)
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(summary, indent=2, sort_keys=True, default=str) + "\n")
    print(json.dumps({k: summary[k] for k in ("rows", "ok", "statuses", "file_recall_ok", "nontrivial_file_recall_ok")}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
