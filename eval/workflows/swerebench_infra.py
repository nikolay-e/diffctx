#!/usr/bin/env python3
"""SWE-rebench V2 infra slice: build the manifest, verify it, run diffctx over it.

    python -m eval swerebench-infra build  --parquet P --out datasets/xlang/swerebench_infra.json
    python -m eval swerebench-infra verify --parquet P
    python -m eval swerebench-infra run    --out DIR [--parquet P] [--limit N] [--ids a,b] [--shard i/n]
    python -m eval swerebench-infra summarize --rows DIR/rows.json

One row per (instance, direction). The seed side's hunks are applied as one
commit; the other side's modified files are hidden gold. Recall is the share
of hidden files in the 8k selection (shipped defaults); ranked R@k ranks every
scored file by its best fragment relevance, seeds excluded. The gold is a
co-change record from one fix, a recall oracle and a weak precision one.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import tempfile
import time
from collections import defaultdict
from pathlib import Path
from typing import Any

from eval.harness.adapters import swerebench_infra as sl
from eval.harness.common import (
    WORKERS,
    apply_gold_patch,
    ensure_repo,
    repos_dir,
    reset_to_parent,
    run_parallel,
    worker_dir,
)

MANIFEST = Path(__file__).resolve().parents[2] / "datasets" / "xlang" / "swerebench_infra.json"
DIRECTIONS = {"code_to_infra": ("code", "infra_files"), "infra_to_code": ("infra", "code_files")}
RANK_KS = (5, 10, 20)
DEFAULT_BUDGET = 8000
_PROVENANCE_ENV = "DIFFCTX_PROVENANCE_DUMP"


def fetch_parquet(cache_dir: Path) -> Path:
    from huggingface_hub import hf_hub_download

    return Path(
        hf_hub_download(sl.HF_PATH, sl.PARQUET_PATH, repo_type="dataset", revision=sl.DATASET_REVISION, cache_dir=cache_dir)
    )


def read_patches(parquet: Path, ids: set[str]) -> dict[str, str]:
    import pyarrow.parquet as pq

    pf = pq.ParquetFile(parquet)
    found: dict[str, str] = {}
    for i in range(pf.num_row_groups):
        for row in pf.read_row_group(i, columns=["instance_id", "patch"]).to_pylist():
            if row["instance_id"] in ids:
                found[row["instance_id"]] = row["patch"]
        if len(found) == len(ids):
            break
    return found


def _ranked_files(dump: Path, repo_dir: Path, seeds: set[str]) -> list[str]:
    """Files by best fragment relevance, ties by path, seeds dropped."""
    root = str(repo_dir.resolve()) + os.sep
    best: dict[str, float] = {}
    for line in dump.read_text().splitlines():
        if not line.strip():
            continue
        row = json.loads(line)
        path = row["path"]
        path = path[len(root) :] if path.startswith(root) else path
        if path not in seeds:
            best[path] = max(best.get(path, 0.0), float(row.get("relevance", 0.0)))
    return [p for p, _ in sorted(best.items(), key=lambda kv: (-kv[1], kv[0]))]


def run_diffctx(repo_dir: Path, budget: int, seeds: set[str]) -> tuple[set[str], list[str]] | None:
    from diffctx._native.pipeline import build_diff_context

    fd, dump_name = tempfile.mkstemp(prefix="swe-prov-", suffix=".jsonl")
    os.close(fd)
    os.environ[_PROVENANCE_ENV] = dump_name
    try:
        output = build_diff_context(repo_dir, "HEAD~1..HEAD", budget_tokens=budget, tau=None)
        selected = {f["path"] for f in output.get("fragments", [])}
        return selected, _ranked_files(Path(dump_name), repo_dir, seeds)
    except Exception as e:
        print(f"  DIFFCTX FAIL: {type(e).__name__}: {e}", flush=True)
        return None
    finally:
        os.environ.pop(_PROVENANCE_ENV, None)
        Path(dump_name).unlink(missing_ok=True)


def evaluate_direction(
    row: dict[str, Any], patch: str, direction: str, repo_dir: Path, budget: int, coupled: dict[str, dict] | None
) -> dict[str, Any]:
    seed_side, hidden_key = DIRECTIONS[direction]
    hidden = sorted(row[hidden_key])
    seed_files = {b.path for b in sl.split_blocks(patch) if sl.classify_block(b) == seed_side}
    result: dict[str, Any] = {
        "instance_id": row["instance_id"],
        "direction": direction,
        "repo": row["repo"],
        "language": row["language"],
        "infra_kinds": row["infra_kinds"],
        "n_seed_files": len(seed_files),
        "hidden": hidden,
        "status": "ok",
    }
    outcome = apply_gold_patch(repo_dir, sl.seed_patch(patch, seed_side), message=f"seed-{direction}")
    if not outcome:
        result["status"] = "apply_fail"
        return result
    try:
        t0 = time.time()
        run = run_diffctx(repo_dir, budget, seed_files)
        result["elapsed_s"] = round(time.time() - t0, 2)
    finally:
        reset_to_parent(repo_dir)
    if run is None:
        result["status"] = "diffctx_fail"
        return result
    selected, ranked = run
    result["selected_hidden"] = sorted(set(hidden) & selected)
    result["recall"] = len(result["selected_hidden"]) / len(hidden)
    result["n_selected_files"] = len(selected)
    rank_of = {p: i for i, p in enumerate(ranked)}
    result["ranks"] = {h: rank_of.get(h) for h in hidden}
    for k in RANK_KS:
        result[f"r_at_{k}"] = sum(1 for h in hidden if rank_of.get(h, k) < k) / len(hidden)
    if coupled is not None:
        c_all = [h for h in hidden if coupled.get(h, {}).get("coupled")]
        c_direct = [h for h in c_all if not coupled[h].get("indirect")]
        result["coupled_hidden"] = c_all
        result["coupled_direct_hidden"] = c_direct
        result["coupled_selected"] = sorted(set(c_all) & selected)
        result["coupled_recall"] = len(result["coupled_selected"]) / len(c_all) if c_all else None
        result["coupled_direct_recall"] = len(set(c_direct) & selected) / len(c_direct) if c_direct else None
    return result


def _run_one(args: tuple) -> list[dict]:
    i, row, patch, budget, coupled, repos = args
    iid = row["instance_id"]
    print(f"[{i}] {iid}", flush=True)
    out: list[dict] = []
    try:
        repo_dir = ensure_repo(f"https://github.com/{row['repo']}.git", row["repo"], row["base_commit"], worker_dir(repos))
        for direction in DIRECTIONS:
            if repo_dir is None:
                out.append(
                    {
                        "instance_id": iid,
                        "direction": direction,
                        "repo": row["repo"],
                        "language": row["language"],
                        "status": "clone_fail",
                    }
                )
                continue
            out.append(evaluate_direction(row, patch, direction, repo_dir, budget, coupled))
    except Exception as e:
        print(f"  ERROR {iid}: {type(e).__name__}: {e}", flush=True)
        out.append(
            {
                "instance_id": iid,
                "direction": "both",
                "repo": row["repo"],
                "language": row["language"],
                "status": "error",
                "error": str(e),
            }
        )
    return out


def _mean_ci(values: list[float]) -> dict[str, Any]:
    from eval.harness.stats import bootstrap_ci

    mean, lo, hi = bootstrap_ci(values)
    return {"n": len(values), "mean": round(mean, 4), "ci95": [round(lo, 4), round(hi, 4)]}


def summarize(rows: list[dict], manifest: dict[str, Any]) -> dict[str, Any]:
    out: dict[str, Any] = {
        "dataset": manifest["dataset"],
        "revision": manifest["revision"],
        "rule_version": manifest["rule_version"],
        "oracle_caveat": "co-change in one fix: a recall oracle, weak for precision (version bumps and drive-by edits ride along)",
        "status": dict(sorted(_count(r["status"] for r in rows).items())),
        "directions": {},
    }
    labelled = set(manifest["labels"]["instances"])
    for direction in DIRECTIONS:
        ok = [r for r in rows if r["direction"] == direction and r["status"] == "ok"]
        block: dict[str, Any] = {"recall": _mean_ci([r["recall"] for r in ok])}
        hidden_total = sum(len(r["hidden"]) for r in ok)
        block["recall_micro"] = round(sum(len(r["selected_hidden"]) for r in ok) / hidden_total, 4) if hidden_total else None
        for k in RANK_KS:
            block[f"r_at_{k}"] = _mean_ci([r[f"r_at_{k}"] for r in ok])
        block["by_language"] = _group(ok, "language")
        block["by_infra_kind"] = _group_kinds(ok)
        lab = [r for r in ok if r["instance_id"] in labelled]
        block["labelled"] = {
            "instances": len(lab),
            "recall_all_hidden": _mean_ci([r["recall"] for r in lab]),
            "recall_coupled_only": _mean_ci([r["coupled_recall"] for r in lab if r.get("coupled_recall") is not None]),
            "recall_coupled_direct_only": _mean_ci(
                [r["coupled_direct_recall"] for r in lab if r.get("coupled_direct_recall") is not None]
            ),
            "instances_without_coupled_hidden": sum(1 for r in lab if r.get("coupled_recall") is None),
        }
        out["directions"][direction] = block
    return out


def _count(items) -> dict[str, int]:
    counts: dict[str, int] = defaultdict(int)
    for item in items:
        counts[item] += 1
    return counts


def _group(rows: list[dict], key: str) -> dict[str, dict[str, Any]]:
    groups: dict[str, list[float]] = defaultdict(list)
    for r in rows:
        groups[r[key]].append(r["recall"])
    return {k: {"n": len(v), "mean": round(sum(v) / len(v), 4)} for k, v in sorted(groups.items())}


def _group_kinds(rows: list[dict]) -> dict[str, dict[str, Any]]:
    groups: dict[str, list[float]] = defaultdict(list)
    for r in rows:
        for kind in r["infra_kinds"]:
            groups[kind].append(r["recall"])
    return {k: {"n": len(v), "mean": round(sum(v) / len(v), 4)} for k, v in sorted(groups.items())}


def load_manifest(path: Path = MANIFEST) -> dict[str, Any]:
    return json.loads(path.read_text())


def _labels_by_path(manifest: dict[str, Any]) -> dict[str, dict[str, dict]]:
    return {iid: {x["path"]: x for x in items} for iid, items in manifest["labels"]["instances"].items()}


def verify(manifest: dict[str, Any], parquet: Path) -> list[str]:
    """Rebuild everything the rule determines and name each difference; labels are ours, only checked for shape."""
    problems: list[str] = []
    fresh = sl.build_manifest(parquet, set(manifest["publication_exclusions"]["instance_ids"]))
    for key, value in fresh.items():
        if manifest.get(key) != value:
            problems.append(f"{key} differs from the rebuilt manifest")
    rows = {r["instance_id"]: r for r in manifest["instances"]}
    for iid, items in manifest["labels"]["instances"].items():
        if iid not in rows:
            problems.append(f"label for {iid} which is not in the sample")
            continue
        want = set(rows[iid]["code_files"]) | set(rows[iid]["infra_files"])
        got = {x["path"] for x in items}
        if want != got:
            problems.append(f"{iid}: labelled files differ from the manifest's modified files")
    if set(manifest["labels"]["instances"]) != set(manifest["label_sample_ids"]):
        problems.append("labelled ids differ from label_sample_ids")
    return problems


def _select(manifest: dict[str, Any], ids: str | None, limit: int | None, shard: str | None, labelled_only: bool) -> list[dict]:
    rows = list(manifest["instances"])
    if labelled_only:
        rows = [r for r in rows if r["instance_id"] in manifest["labels"]["instances"]]
    if ids:
        wanted = set(ids.split(","))
        rows = [r for r in rows if r["instance_id"] in wanted]
    if shard:
        index, count = (int(x) for x in shard.split("/"))
        rows = rows[index::count]
    return rows[:limit] if limit else rows


def cmd_run(args: argparse.Namespace) -> int:
    manifest = load_manifest(Path(args.manifest))
    rows = _select(manifest, args.ids, args.limit, args.shard, args.labelled_only)
    if not rows:
        print("no instances selected", file=sys.stderr)
        return 1
    parquet = Path(args.parquet) if args.parquet else fetch_parquet(Path(args.hf_cache))
    patches = read_patches(parquet, {r["instance_id"] for r in rows})
    labels = _labels_by_path(manifest)
    repos = repos_dir("SWE_REBENCH_REPOS_DIR")
    run_args = [
        (i, r, patches[r["instance_id"]], args.budget, labels.get(r["instance_id"]), repos)
        for i, r in enumerate(rows, 1)
        if r["instance_id"] in patches
    ]
    results = run_parallel(_run_one, run_args, args.workers, collect="extend")
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    (out / "rows.json").write_text(json.dumps(results, indent=1) + "\n")
    summary = summarize(results, manifest)
    summary["budget"] = args.budget
    summary["selected_instances"] = len(rows)
    (out / "summary.json").write_text(json.dumps(summary, indent=1) + "\n")
    print(json.dumps(summary, indent=1))
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(prog="eval swerebench-infra")
    sub = ap.add_subparsers(dest="cmd", required=True)

    b = sub.add_parser("build", help="rebuild the manifest from the pinned parquet, keeping our labels")
    b.add_argument("--parquet", required=True)
    b.add_argument("--out", default=str(MANIFEST))
    b.add_argument("--exclude", action="append", help="instance id to replace (kept in the manifest's exclusions)")

    v = sub.add_parser("verify", help="the rule reproduces the committed manifest")
    v.add_argument("--parquet", required=True)
    v.add_argument("--manifest", default=str(MANIFEST))

    r = sub.add_parser("run", help="run diffctx over the manifest")
    r.add_argument("--out", required=True)
    r.add_argument("--manifest", default=str(MANIFEST))
    r.add_argument("--parquet")
    r.add_argument("--hf-cache", default=str(Path.home() / ".cache" / "swe_rebench_v2"))
    r.add_argument("--budget", type=int, default=DEFAULT_BUDGET)
    r.add_argument("--limit", type=int)
    r.add_argument("--ids")
    r.add_argument("--shard", help="i/n: every n-th instance starting at i")
    r.add_argument("--labelled-only", action="store_true")
    r.add_argument("--workers", type=int, default=WORKERS)

    s = sub.add_parser("summarize", help="re-summarize a rows.json")
    s.add_argument("--rows", required=True, nargs="+")
    s.add_argument("--manifest", default=str(MANIFEST))

    args = ap.parse_args()
    if args.cmd == "build":
        previous = load_manifest(Path(args.out)) if Path(args.out).exists() else {}
        excluded = set(previous.get("publication_exclusions", {}).get("instance_ids", [])) | set(args.exclude or [])
        manifest = sl.build_manifest(Path(args.parquet), excluded)
        if "labels" in previous:
            manifest["labels"] = previous["labels"]
        Path(args.out).write_text(json.dumps(manifest, indent=1) + "\n")
        print(json.dumps({k: manifest[k] for k in ("qualifying", "sample")}, indent=1))
        return 0
    if args.cmd == "verify":
        problems = verify(load_manifest(Path(args.manifest)), Path(args.parquet))
        print("\n".join(problems) if problems else "manifest reproduces from the rule")
        return 1 if problems else 0
    if args.cmd == "run":
        return cmd_run(args)
    rows: list[dict] = []
    for path in args.rows:
        rows.extend(json.loads(Path(path).read_text()))
    print(json.dumps(summarize(rows, load_manifest(Path(args.manifest))), indent=1))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
