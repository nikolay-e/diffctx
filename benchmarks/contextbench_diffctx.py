#!/usr/bin/env python3
from __future__ import annotations

import json
import os
import random
import re
import subprocess
import sys
import tempfile
import time
from collections import defaultdict
from pathlib import Path

LINES_RE = re.compile(r"^(\d+)-(\d+)$")
_WORKSPACE_PREFIX_RE = re.compile(r"^/workspace/[^/]+/")


def normalize_gold_path(path: str) -> str:
    return _WORKSPACE_PREFIX_RE.sub("", path)


_repos_suffix = os.environ.get("CONTEXTBENCH_REPOS_SUFFIX", "")
_DEFAULT_REPOS = Path(os.environ.get("CB_REPOS_DIR", str(Path.home() / ".cache" / "contextbench_repos")))
REPOS_DIR = _DEFAULT_REPOS / _repos_suffix if _repos_suffix else _DEFAULT_REPOS
REPOS_DIR.mkdir(parents=True, exist_ok=True)


def parse_lines_field(lines_str: str) -> tuple[int, int] | None:
    m = LINES_RE.match(lines_str.strip())
    if not m:
        return None
    s, e = int(m.group(1)), int(m.group(2))
    if s < 1 or e < s:
        return None
    return (s, e)


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


def patch_files(patch: str) -> set[str]:
    added, deleted, modified = set(), set(), set()
    cur_a = cur_b = None
    for line in patch.splitlines():
        if line.startswith("diff --git "):
            cur_a = cur_b = None
        elif line.startswith("--- "):
            p = line[4:]
            cur_a = None if p == "/dev/null" else (p[2:] if p.startswith("a/") else p)
        elif line.startswith("+++ "):
            p = line[4:]
            cur_b = None if p == "/dev/null" else (p[2:] if p.startswith("b/") else p)
            if cur_a is None and cur_b is not None:
                added.add(cur_b)
            elif cur_a is not None and cur_b is None:
                deleted.add(cur_a)
            elif cur_a == cur_b and cur_a is not None:
                modified.add(cur_a)
            elif cur_a is not None and cur_b is not None:
                modified.add(cur_b)
                modified.add(cur_a)
    return added | deleted | modified


def is_nontrivial(gold: list[dict], patch: str) -> bool:
    gf = gold_files(gold)
    pf = patch_files(patch)
    return bool(gf - pf)


def ensure_repo(repo_url: str, repo_name: str, base_commit: str, repos_dir: Path = REPOS_DIR) -> Path | None:
    repo_dir = repos_dir / repo_name.replace("/", "__")
    if not repo_dir.exists():
        r = subprocess.run(
            ["git", "clone", "--quiet", repo_url, str(repo_dir)],
            capture_output=True,
            text=True,
            timeout=300,
        )
        if r.returncode != 0:
            print(f"  CLONE FAIL: {r.stderr[:200]}")
            return None

    r = subprocess.run(
        ["git", "-C", str(repo_dir), "checkout", "--force", base_commit],
        capture_output=True,
        text=True,
        timeout=60,
    )
    if r.returncode != 0:
        subprocess.run(
            ["git", "-C", str(repo_dir), "fetch", "--all", "--quiet"],
            capture_output=True,
            text=True,
            timeout=300,
        )
        r = subprocess.run(
            ["git", "-C", str(repo_dir), "checkout", "--force", base_commit],
            capture_output=True,
            text=True,
            timeout=60,
        )
        if r.returncode != 0:
            print(f"  CHECKOUT FAIL: {r.stderr[:200]}")
            return None
    return repo_dir


def apply_as_commit(repo_dir: Path, patch_text: str) -> bool:
    with tempfile.NamedTemporaryFile(mode="w", suffix=".patch", delete=False) as f:
        f.write(patch_text)
        patch_path = f.name
    try:
        r = subprocess.run(
            ["git", "-C", str(repo_dir), "apply", "--index", patch_path],
            capture_output=True,
            text=True,
            timeout=60,
        )
        if r.returncode != 0:
            r = subprocess.run(
                ["git", "-C", str(repo_dir), "apply", "--index", "--3way", patch_path],
                capture_output=True,
                text=True,
                timeout=60,
            )
            if r.returncode != 0:
                print(f"  APPLY FAIL: {r.stderr[:200]}")
                return False

        subprocess.run(
            ["git", "-C", str(repo_dir), "commit", "-m", "bench", "--allow-empty", "--no-verify"],
            capture_output=True,
            text=True,
            timeout=30,
        )
        return True
    finally:
        os.unlink(patch_path)


def run_diffctx(repo_dir: Path, budget: int = 8000, scoring_mode: str = "auto") -> dict | None:
    from treemapper.diffctx.pipeline import build_diff_context

    try:
        return build_diff_context(repo_dir, "HEAD~1..HEAD", budget_tokens=budget, scoring_mode=scoring_mode)
    except Exception as e:
        print(f"  DIFFCTX FAIL: {type(e).__name__}: {e}")
        return None


def run_baseline_patch_files(repo_dir: Path, budget: int = 8000) -> dict | None:
    import tiktoken

    enc = tiktoken.get_encoding("o200k_base")
    r = subprocess.run(
        ["git", "-C", str(repo_dir), "diff", "HEAD~1..HEAD", "--name-only"],
        capture_output=True,
        text=True,
        timeout=30,
    )
    fragments = []
    used = 0
    for rel_path in r.stdout.strip().splitlines():
        if used >= budget:
            break
        full = repo_dir / rel_path
        if not full.is_file():
            continue
        try:
            content = full.read_text(encoding="utf-8")
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


def evaluate_instance(
    inst: dict,
    budget: int = 8000,
    repos_dir: Path = REPOS_DIR,
    scoring_mode: str = "auto",
    baseline: str = "treemapper",
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

    t0 = time.time()
    if baseline == "patch_files":
        output = run_baseline_patch_files(repo_dir, budget)
    else:
        output = run_diffctx(repo_dir, budget, scoring_mode)
    elapsed = time.time() - t0

    subprocess.run(
        ["git", "-C", str(repo_dir), "reset", "--hard", "HEAD~1"],
        capture_output=True,
        text=True,
        timeout=30,
    )

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

    from metrics.def_coverage import def_coverage

    ext_def_cov = def_coverage(inst["patch"], output.get("fragments", []))

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
        "def_coverage": round(ext_def_cov, 3),
        "latency": output.get("latency"),
    }

    diagnostics = []

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

    result["diagnostics"] = diagnostics

    print(f"Fragments: {frag_count} | Time: {elapsed:.1f}s")
    print(f"File recall: {file_recall:.3f} | Precision: {file_precision:.3f}")
    print(f"Nontrivial file recall: {nontrivial_recall:.3f}")
    print(f"Line recall (all): {lo_all['line_recall']:.3f} | Line recall (nontrivial only): {lo_nontrivial['line_recall']:.3f}")
    for d in diagnostics:
        print(f"  {d}")

    return result


def aggregate(results: list[dict]) -> None:
    ok = [r for r in results if r["status"] == "ok"]
    if not ok:
        print("\nNo successful evaluations.")
        return

    print(f"\n{'='*60}")
    print(f"AGGREGATE ({len(ok)} instances)")
    print(f"{'='*60}")

    for metric in ["file_recall", "nontrivial_file_recall", "line_recall", "line_recall_nontrivial", "def_coverage"]:
        vals = [r[metric] for r in ok if metric in r]
        if not vals:
            continue
        avg = sum(vals) / len(vals)
        print(f"  {metric:30s}: {avg:.3f} (min={min(vals):.3f}, max={max(vals):.3f})")

    by_lang: dict[str, list[dict]] = defaultdict(list)
    for r in ok:
        by_lang[r["language"]].append(r)

    if len(by_lang) > 1:
        print("\nPer-language breakdown:")
        for lang in sorted(by_lang):
            lr = by_lang[lang]
            avg_fr = sum(r["file_recall"] for r in lr) / len(lr)
            avg_ntr = sum(r["nontrivial_file_recall"] for r in lr) / len(lr)
            avg_lr = sum(r["line_recall"] for r in lr) / len(lr)
            print(f"  {lang:12s} (n={len(lr):3d}): file_recall={avg_fr:.3f}  nontrivial={avg_ntr:.3f}  line_recall={avg_lr:.3f}")

    by_repo: dict[str, list[dict]] = defaultdict(list)
    for r in ok:
        by_repo[r["repo"]].append(r)

    if len(by_repo) > 1:
        print("\nPer-repo breakdown:")
        for repo in sorted(by_repo, key=lambda r: -len(by_repo[r])):
            rr = by_repo[repo]
            avg_ntr = sum(r["nontrivial_file_recall"] for r in rr) / len(rr)
            print(f"  {repo:30s} (n={len(rr):3d}): nontrivial_recall={avg_ntr:.3f}")

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


def _run_one(args: tuple[int, dict, int, str, str]) -> dict | None:
    _i, inst, budget, scoring, baseline = args
    worker_dir = REPOS_DIR / f"w{os.getpid()}"
    worker_dir.mkdir(exist_ok=True)
    try:
        return evaluate_instance(inst, budget, repos_dir=worker_dir, scoring_mode=scoring, baseline=baseline)
    except Exception as e:
        print(f"  ERROR: {type(e).__name__}: {e}", flush=True)
        return None


def main():
    import argparse

    from treemapper.diffctx.filtering import _LOW_RELEVANCE_THRESHOLD

    print(f"diffctx _LOW_RELEVANCE_THRESHOLD = {_LOW_RELEVANCE_THRESHOLD}", file=sys.stderr)

    parser = argparse.ArgumentParser()
    parser.add_argument("--limit", type=int, default=3)
    parser.add_argument("--budget", type=int, default=8000)
    parser.add_argument("--lang", type=str, default=None)
    parser.add_argument("--nontrivial-only", action="store_true", default=True)
    parser.add_argument("--seed", type=int, default=42)
    parser.add_argument("--no-shuffle", action="store_true")
    parser.add_argument("--output", type=str, default=None)
    parser.add_argument("--workers", type=int, default=11)
    parser.add_argument("--scoring", type=str, default="auto", choices=["auto", "precise", "discover"])
    parser.add_argument("--baseline", type=str, default="treemapper", choices=["treemapper", "patch_files"])
    args = parser.parse_args()

    from datasets import load_dataset

    print("Loading ContextBench verified subset...")
    ds = load_dataset("Contextbench/ContextBench", "contextbench_verified", split="train")
    print(f"Loaded {len(ds)} instances")

    instances = list(ds)
    if args.lang:
        instances = [i for i in instances if i["language"] == args.lang]
        print(f"Filtered to {len(instances)} {args.lang} instances")

    if args.nontrivial_only:
        instances = [i for i in instances if is_nontrivial(parse_gold_context(i["gold_context"]), i["patch"])]
        print(f"Nontrivial instances: {len(instances)}")

    if not args.no_shuffle:
        rng = random.Random(args.seed)  # NOSONAR — deterministic shuffle for benchmark reproducibility, not crypto
        rng.shuffle(instances)
        print(f"Shuffled with seed={args.seed}")

    instances = instances[: args.limit]
    print(f"Evaluating {len(instances)} instances (budget={args.budget}, workers={args.workers}, scoring={args.scoring})")

    results = []
    t0 = time.time()

    run_args = [(i, inst, args.budget, args.scoring, args.baseline) for i, inst in enumerate(instances, 1)]

    if args.workers > 1:
        from concurrent.futures import ProcessPoolExecutor, as_completed

        with ProcessPoolExecutor(max_workers=args.workers) as pool:
            futures = {pool.submit(_run_one, a): a[0] for a in run_args}
            for future in as_completed(futures):
                r = future.result()
                if r:
                    results.append(r)
    else:
        for a in run_args:
            r = _run_one(a)
            if r:
                results.append(r)

    elapsed = time.time() - t0
    print(f"\nTotal wall time: {elapsed:.0f}s")

    aggregate(results)

    if args.output:
        with open(args.output, "w") as f:
            json.dump(results, f, indent=2)
        print(f"\nResults saved to {args.output}")


if __name__ == "__main__":
    main()
