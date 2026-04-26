#!/usr/bin/env python3
from __future__ import annotations

import json
import random
import statistics
import subprocess
import sys
import time
from collections import defaultdict
from pathlib import Path

from benchmarks.common import (
    WORKERS,
    apply_as_commit,
    ensure_repo,
    normalize_gold_path,
    parse_lines_field,
    patch_files,
    repos_dir,
    reset_to_parent,
    run_parallel,
    save_results,
    warm_cache,
    worker_dir,
)
from benchmarks.stats import bootstrap_ci

_DEFAULT_DIFF_RANGE = "HEAD~1..HEAD"

REPOS_DIR = repos_dir("CB_REPOS_DIR", suffix_var="CONTEXTBENCH_REPOS_SUFFIX")


def parse_gold_context(raw: str) -> list[dict]:
    items = json.loads(raw)
    out = []
    for g in items:
        if not g.get("file") or g.get("start_line") is None:
            continue
        g["file"] = normalize_gold_path(g["file"])
        out.append(g)
    return out


def gold_files(gold: list[dict]) -> set[str]:
    return {g["file"] for g in gold}


def is_nontrivial(gold: list[dict], patch: str) -> bool:
    return bool(gold_files(gold) - patch_files(patch))


def run_diffctx(repo_dir: Path, budget: int = 8000, scoring_mode: str = "hybrid", tau: float = 0.08) -> dict | None:
    from treemapper.diffctx.pipeline import build_diff_context

    try:
        return build_diff_context(repo_dir, _DEFAULT_DIFF_RANGE, budget_tokens=budget, scoring_mode=scoring_mode, tau=tau)
    except Exception as e:
        print(f"  DIFFCTX FAIL: {type(e).__name__}: {e}")
        return None


def _pack_files_to_fragments(repo_dir: Path, ranked_files: list[str], budget: int) -> dict:
    import tiktoken

    enc = tiktoken.get_encoding("o200k_base")
    fragments = []
    used = 0
    for rel_path in ranked_files:
        if used >= budget:
            break
        full = repo_dir / rel_path
        if not full.is_file():
            continue
        try:
            content = full.read_text(encoding="utf-8", errors="replace")
        except (OSError, UnicodeDecodeError):
            continue
        tokens = enc.encode(content)
        available = budget - used
        truncated = enc.decode(tokens[:available])
        lines = truncated.splitlines()
        fragments.append(
            {
                "path": rel_path,
                "lines": f"1-{len(lines)}",
                "kind": "file",
                "content": truncated,
            }
        )
        used += min(len(tokens), available)
    return {
        "name": repo_dir.name,
        "type": "diff_context",
        "fragment_count": len(fragments),
        "fragments": fragments,
    }


def run_baseline_patch_files(repo_dir: Path, budget: int = 8000) -> dict | None:
    r = subprocess.run(
        ["git", "-C", str(repo_dir), "diff", _DEFAULT_DIFF_RANGE, "--name-only"],
        capture_output=True,
        text=True,
        timeout=30,
    )
    return _pack_files_to_fragments(repo_dir, r.stdout.strip().splitlines(), budget)


def run_baseline_bm25(repo_dir: Path, budget: int = 8000) -> dict | None:
    import re

    try:
        from rank_bm25 import BM25Okapi
    except ImportError:
        print("ERROR: rank_bm25 not installed. Run: pip install rank-bm25", file=sys.stderr)
        sys.exit(1)

    ls_result = subprocess.run(
        ["git", "-C", str(repo_dir), "ls-files"],
        capture_output=True,
        text=True,
        timeout=30,
    )
    all_files = [f for f in ls_result.stdout.strip().splitlines() if f]
    if not all_files:
        return _pack_files_to_fragments(repo_dir, [], budget)

    diff_result = subprocess.run(
        ["git", "-C", str(repo_dir), "diff", _DEFAULT_DIFF_RANGE],
        capture_output=True,
        text=True,
        timeout=30,
    )
    added_lines = []
    for line in diff_result.stdout.splitlines():
        if line.startswith("+") and not line.startswith("+++"):
            added_lines.append(line[1:])

    query_tokens = re.findall(r"[A-Za-z_]\w+", " ".join(added_lines).lower())
    if not query_tokens:
        return _pack_files_to_fragments(repo_dir, [], budget)

    corpus_tokens: list[list[str]] = []
    valid_files: list[str] = []
    for rel_path in all_files:
        full = repo_dir / rel_path
        if not full.is_file():
            continue
        try:
            text = full.read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        tokens = re.findall(r"[A-Za-z_]\w+", text.lower())
        if not tokens:
            continue
        corpus_tokens.append(tokens)
        valid_files.append(rel_path)

    if not corpus_tokens:
        return _pack_files_to_fragments(repo_dir, [], budget)

    bm25 = BM25Okapi(corpus_tokens)
    scores = bm25.get_scores(query_tokens)
    ranked_indices = sorted(range(len(valid_files)), key=lambda i: scores[i], reverse=True)
    ranked_files = [valid_files[i] for i in ranked_indices if scores[i] > 0]

    return _pack_files_to_fragments(repo_dir, ranked_files, budget)


def _get_diffctx_config() -> dict:
    return {
        "low_relevance_threshold": 0.015,
        "proximity_decay": 0.30,
        "peripheral_cap": 0.15,
        "max_context_frags_per_file": 30,
    }


def extract_selected_files(output: dict) -> set[str]:
    return {f["path"] for f in output.get("fragments", [])}


def extract_selected_ranges(output: dict) -> dict[str, list[tuple[int, int]]]:
    ranges: dict[str, list[tuple[int, int]]] = defaultdict(list)
    for f in output.get("fragments", []):
        parsed = parse_lines_field(f.get("lines", ""))
        if parsed:
            ranges[f["path"]].append(parsed)
    return ranges


def line_overlap(
    gold: list[dict], selected_ranges: dict[str, list[tuple[int, int]]], exclude_files: set[str] | None = None
) -> dict:
    exclude = exclude_files or set()
    gold_lines = 0
    covered_lines = 0
    for g in gold:
        if g["file"] in exclude:
            continue
        gs, ge = g["start_line"], g["end_line"]
        gold_set = set(range(gs, ge + 1))
        gold_lines += len(gold_set)
        sel_set = set()
        for s, e in selected_ranges.get(g["file"], []):
            sel_set.update(range(s, e + 1))
        covered_lines += len(gold_set & sel_set)
    return {
        "gold_lines": gold_lines,
        "covered_lines": covered_lines,
        "line_recall": covered_lines / gold_lines if gold_lines else 0.0,
    }


def _collect_instance_diagnostics(
    frag_count: int,
    lo_all: dict,
    file_recall: float,
    gf: set,
    sel_files: set,
    nontrivial_recall: float,
    output: dict,
) -> list[str]:
    diagnostics: list[str] = []
    if frag_count == 0:
        diagnostics.append("WARN: diffctx returned 0 fragments")
    if lo_all["line_recall"] < 1e-9 and frag_count > 0:
        diagnostics.append("DIAG: line_recall=0 with fragments>0 — possible line parse bug or no file overlap")
    if file_recall < 1e-9 and frag_count > 0:
        diagnostics.append("DIAG: file_recall=0 with fragments>0 — selected files don't overlap gold at all")
        diagnostics.append(f"  gold_files: {sorted(gf)[:5]}")
        diagnostics.append(f"  selected:   {sorted(sel_files)[:5]}")
    if nontrivial_recall < 1e-9 and frag_count > 5:
        diagnostics.append("DIAG: nontrivial_recall=0 — diffctx may only be selecting patch-adjacent files")
    unparsed = sum(1 for f in output.get("fragments", []) if parse_lines_field(f.get("lines", "")) is None)
    if unparsed:
        diagnostics.append(f"DIAG: {unparsed}/{frag_count} fragments have unparseable 'lines' field")
    return diagnostics


def evaluate_instance(
    inst: dict,
    budget: int = 8000,
    repos_dir: Path = REPOS_DIR,
    scoring_mode: str = "hybrid",
    baseline: str = "treemapper",
    tau: float = 0.08,
) -> dict | None:
    iid = inst["instance_id"]
    gold = parse_gold_context(inst["gold_context"])
    gf = gold_files(gold)
    pf = patch_files(inst["patch"])
    nontrivial_gold = gf - pf
    print(f"\n{'='*60}")
    print(f"Instance: {iid}")
    print(f"Language: {inst['language']} | Repo: {inst['repo']}")
    print(f"Gold files: {len(gf)} | Patch files: {len(pf)} | Nontrivial gold: {len(nontrivial_gold)}")

    if not nontrivial_gold:
        print("SKIP: all gold files are in the patch (trivial)")
        return None

    repo_dir = ensure_repo(inst["repo_url"], inst["repo"], inst["base_commit"], repos_dir)
    if not repo_dir:
        return {"id": iid, "status": "clone_fail"}

    if not apply_as_commit(repo_dir, inst["patch"]):
        subprocess.run(
            ["git", "-C", str(repo_dir), "checkout", "--force", inst["base_commit"]],
            capture_output=True,
            text=True,
            timeout=30,
        )
        return {"id": iid, "status": "apply_fail"}

    try:
        t0 = time.time()
        if baseline == "patch_files":
            output = run_baseline_patch_files(repo_dir, budget)
        elif baseline == "bm25":
            output = run_baseline_bm25(repo_dir, budget)
        else:
            output = run_diffctx(repo_dir, budget, scoring_mode, tau)
        elapsed = time.time() - t0
    finally:
        reset_to_parent(repo_dir)

    if not output:
        return {"id": iid, "status": "diffctx_fail"}

    sel_files = extract_selected_files(output)
    sel_ranges = extract_selected_ranges(output)
    frag_count = output.get("fragment_count", 0)

    file_recall = len(gf & sel_files) / len(gf) if gf else 0
    file_precision = len(gf & sel_files) / len(sel_files) if sel_files else 0
    nontrivial_recall = len(nontrivial_gold & sel_files) / len(nontrivial_gold) if nontrivial_gold else 0

    lo_all = line_overlap(gold, sel_ranges)
    lo_nontrivial = line_overlap(gold, sel_ranges, exclude_files=pf)

    result = {
        "id": iid,
        "status": "ok",
        "language": inst["language"],
        "repo": inst["repo"],
        "elapsed_s": round(elapsed, 1),
        "fragments": frag_count,
        "gold_files": len(gf),
        "selected_files": len(sel_files),
        "nontrivial_gold_files": len(nontrivial_gold),
        "file_recall": round(file_recall, 3),
        "file_precision": round(file_precision, 3),
        "nontrivial_file_recall": round(nontrivial_recall, 3),
        "line_recall": round(lo_all["line_recall"], 3),
        "line_recall_nontrivial": round(lo_nontrivial["line_recall"], 3),
        "gold_lines": lo_all["gold_lines"],
        "covered_lines": lo_all["covered_lines"],
        "latency": output.get("latency"),
        "config": {
            "scoring_mode": scoring_mode,
            "budget": budget,
            **_get_diffctx_config(),
        },
    }

    diagnostics = _collect_instance_diagnostics(frag_count, lo_all, file_recall, gf, sel_files, nontrivial_recall, output)
    result["diagnostics"] = diagnostics

    print(f"Fragments: {frag_count} | Time: {elapsed:.1f}s")
    print(f"File recall: {file_recall:.3f} | Precision: {file_precision:.3f}")
    print(f"Nontrivial file recall: {nontrivial_recall:.3f}")
    print(f"Line recall (all): {lo_all['line_recall']:.3f} | Line recall (nontrivial only): {lo_nontrivial['line_recall']:.3f}")
    for d in diagnostics:
        print(f"  {d}")

    return result


def _print_per_language_breakdown(by_lang: dict) -> None:
    if len(by_lang) <= 1:
        return
    print("\nPer-language breakdown:")
    for lang in sorted(by_lang):
        lr = by_lang[lang]
        avg_fr = sum(r["file_recall"] for r in lr) / len(lr)
        avg_ntr = sum(r["nontrivial_file_recall"] for r in lr) / len(lr)
        avg_lr = sum(r["line_recall"] for r in lr) / len(lr)
        print(f"  {lang:12s} (n={len(lr):3d}): file_recall={avg_fr:.3f}  nontrivial={avg_ntr:.3f}  line_recall={avg_lr:.3f}")


def _print_per_repo_breakdown(by_repo: dict) -> None:
    if len(by_repo) <= 1:
        return
    print("\nPer-repo breakdown:")
    for repo in sorted(by_repo, key=lambda r: -len(by_repo[r])):
        rr = by_repo[repo]
        avg_ntr = sum(r["nontrivial_file_recall"] for r in rr) / len(rr)
        print(f"  {repo:30s} (n={len(rr):3d}): nontrivial_recall={avg_ntr:.3f}")


def aggregate(results: list[dict]) -> None:
    ok = [r for r in results if r["status"] == "ok"]
    if not ok:
        print("\nNo successful evaluations.")
        return

    print(f"\n{'='*60}")
    print(f"AGGREGATE ({len(ok)} instances)")
    print(f"{'='*60}")

    all_metrics = sorted(
        {
            k
            for r in ok
            for k, v in r.items()
            if isinstance(v, (int, float))
            and k
            not in (
                "elapsed_s",
                "gold_lines",
                "covered_lines",
                "fragments",
                "gold_files",
                "selected_files",
                "nontrivial_gold_files",
            )
        }
    )
    for metric in all_metrics:
        vals = [r[metric] for r in ok if metric in r]
        if not vals:
            continue
        mean, lo, hi = bootstrap_ci(vals)
        print(f"  {metric:30s}: {mean:.3f} [{lo:.3f}, {hi:.3f}] (min={min(vals):.3f}, max={max(vals):.3f})")

    latencies = [r["elapsed_s"] for r in ok]
    p95_idx = int(0.95 * len(latencies))
    print(
        f"  {'latency':30s}: median={statistics.median(latencies):.1f}s "
        f"p95={sorted(latencies)[p95_idx]:.1f}s max={max(latencies):.1f}s"
    )

    by_lang: dict[str, list[dict]] = defaultdict(list)
    for r in ok:
        by_lang[r["language"]].append(r)
    _print_per_language_breakdown(by_lang)

    by_repo: dict[str, list[dict]] = defaultdict(list)
    for r in ok:
        by_repo[r["repo"]].append(r)
    _print_per_repo_breakdown(by_repo)

    zero_frag = sum(1 for r in ok if r["fragments"] == 0)
    zero_line = sum(1 for r in ok if r["line_recall"] < 1e-9 and r["fragments"] > 0)

    print("\nDiagnostic summary:")
    print(f"  Instances with 0 fragments: {zero_frag}/{len(ok)}")
    print(f"  Instances with line_recall=0 but fragments>0: {zero_line}/{len(ok)}")
    if zero_line > len(ok) * 0.5:
        print("  *** ALERT: >50% have zero line recall with nonzero fragments — likely a systemic bug ***")

    failed = [r for r in results if r["status"] != "ok"]
    if failed:
        by_status: dict[str, int] = defaultdict(int)
        for r in failed:
            by_status[r["status"]] += 1
        print(f"\nFailures: {dict(by_status)}")


def _run_one(args: tuple[int, dict, int, str, str, float]) -> dict | None:
    _i, inst, budget, scoring, baseline, tau = args
    wdir = worker_dir(REPOS_DIR)
    try:
        return evaluate_instance(inst, budget, repos_dir=wdir, scoring_mode=scoring, baseline=baseline, tau=tau)
    except Exception as e:
        print(f"  ERROR: {type(e).__name__}: {e}", flush=True)
        return {"id": inst["instance_id"], "status": "error"}


def _print_cross_seed_summary(all_seed_results: dict[int, list[dict]]) -> None:
    metrics = ["file_recall", "nontrivial_file_recall", "line_recall", "line_recall_nontrivial"]

    print(f"\n{'='*60}")
    print(f"CROSS-SEED SUMMARY ({len(all_seed_results)} seeds: {sorted(all_seed_results)})")
    print(f"{'='*60}")

    seed_avgs: dict[str, list[float]] = defaultdict(list)
    for _seed, results in sorted(all_seed_results.items()):
        ok = [r for r in results if r.get("status") == "ok"]
        if not ok:
            continue
        for m in metrics:
            vals = [r[m] for r in ok if m in r]
            if vals:
                seed_avgs[m].append(sum(vals) / len(vals))

    for m in metrics:
        vals = seed_avgs.get(m, [])
        if not vals:
            continue
        mean = statistics.mean(vals)
        std = statistics.stdev(vals) if len(vals) > 1 else 0.0
        print(f"  {m:30s}: {mean:.3f} \u00b1 {std:.3f}")


def main():
    import argparse

    print("diffctx backend: Rust (_diffctx)", file=sys.stderr)

    parser = argparse.ArgumentParser()
    parser.add_argument("--limit", type=int, default=3)
    parser.add_argument("--budget", type=int, default=16000)
    parser.add_argument("--lang", type=str, default=None)
    parser.add_argument("--nontrivial-only", action="store_true", default=True)
    parser.add_argument("--seeds", type=str, default="42")
    parser.add_argument("--no-shuffle", action="store_true")
    parser.add_argument("--scoring", type=str, default="hybrid", choices=["hybrid", "ppr", "ego", "bm25"])
    parser.add_argument("--baseline", type=str, default="treemapper", choices=["treemapper", "patch_files", "bm25"])
    parser.add_argument("--dataset", type=str, default="full", choices=["verified", "full"])
    parser.add_argument("--tau", type=float, default=0.08)
    args = parser.parse_args()

    seeds = [int(s) for s in args.seeds.split(",")]

    from datasets import load_dataset

    config = "contextbench_verified" if args.dataset == "verified" else "default"
    print(f"Loading ContextBench ({args.dataset})...")
    ds = load_dataset("Contextbench/ContextBench", config, split="train")
    print(f"Loaded {len(ds)} instances")

    all_instances = list(ds)
    if args.lang:
        all_instances = [i for i in all_instances if i["language"] == args.lang]
        print(f"Filtered to {len(all_instances)} {args.lang} instances")

    if args.nontrivial_only:
        all_instances = [i for i in all_instances if is_nontrivial(parse_gold_context(i["gold_context"]), i["patch"])]
        print(f"Nontrivial instances: {len(all_instances)}")

    warm_cache(all_instances)

    all_seed_results: dict[int, list[dict]] = {}

    for seed in seeds:
        print(f"\n{'#'*60}")
        print(f"SEED {seed}")
        print(f"{'#'*60}")

        instances = list(all_instances)
        if not args.no_shuffle:
            rng = random.Random(seed)  # NOSONAR — deterministic shuffle for benchmark reproducibility, not crypto
            rng.shuffle(instances)
            print(f"Shuffled with seed={seed}")

        instances = instances[: args.limit]
        print(f"Evaluating {len(instances)} instances (budget={args.budget}, workers={WORKERS}, scoring={args.scoring})")

        t0 = time.time()
        run_args = [(i, inst, args.budget, args.scoring, args.baseline, args.tau) for i, inst in enumerate(instances, 1)]
        results = run_parallel(_run_one, run_args, WORKERS)
        elapsed = time.time() - t0
        print(f"\nTotal wall time: {elapsed:.0f}s")

        aggregate(results)

        if len(seeds) == 1:
            tag = f"cb_{args.scoring}_n{args.limit}_b{args.budget}"
        else:
            tag = f"cb_{args.scoring}_n{args.limit}_b{args.budget}_s{seed}"
        if args.baseline != "treemapper":
            tag = (
                f"cb_{args.baseline}_n{args.limit}_b{args.budget}"
                if len(seeds) == 1
                else f"cb_{args.baseline}_n{args.limit}_b{args.budget}_s{seed}"
            )
        save_results(results, tag, seed=seed, budget=args.budget, scoring=args.scoring, baseline=args.baseline)

        all_seed_results[seed] = results

    if len(seeds) > 1:
        _print_cross_seed_summary(all_seed_results)


if __name__ == "__main__":
    main()
