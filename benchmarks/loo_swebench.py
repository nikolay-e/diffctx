#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import os
import random
import subprocess
import tempfile
import time
from collections import defaultdict
from pathlib import Path

REPOS_DIR = Path(os.environ.get("LOO_REPOS_DIR", str(Path.home() / ".cache" / "contextbench_repos")))
REPOS_DIR.mkdir(parents=True, exist_ok=True)


def run_cmd(cmd, cwd=None, check=True, timeout=120):
    return subprocess.run(cmd, cwd=cwd, text=True, capture_output=True, check=check, timeout=timeout)


def patch_files(patch: str) -> set[str]:
    files: set[str] = set()
    for line in patch.splitlines():
        if line.startswith("+++ "):
            p = line[4:]
            if p != "/dev/null":
                files.add(p[2:] if p.startswith("b/") else p)
        elif line.startswith("--- "):
            p = line[4:]
            if p != "/dev/null":
                files.add(p[2:] if p.startswith("a/") else p)
    return files


def strip_file_from_patch(patch_text: str, file_to_hide: str) -> str:
    lines = patch_text.split("\n")
    result = []
    skip = False
    hidden_markers = {f"a/{file_to_hide}", f"b/{file_to_hide}"}
    for line in lines:
        if line.startswith("diff --git "):
            parts = line.split()
            skip = any(p.strip('"') in hidden_markers or p.lstrip('"') in hidden_markers for p in parts[2:])
        if not skip:
            result.append(line)
    return "\n".join(result)


def ensure_repo(repo_url: str, repo_name: str, base_commit: str, repos_dir: Path = REPOS_DIR) -> Path | None:
    repo_dir = repos_dir / repo_name.replace("/", "__")
    if not repo_dir.exists():
        r = run_cmd(["git", "clone", "--quiet", repo_url, str(repo_dir)], check=False, timeout=600)
        if r.returncode != 0:
            print(f"  CLONE FAIL: {r.stderr[:200]}")
            return None
    r = run_cmd(["git", "-C", str(repo_dir), "checkout", "--force", base_commit], check=False)
    if r.returncode != 0:
        run_cmd(["git", "-C", str(repo_dir), "fetch", "--all", "--quiet"], check=False, timeout=600)
        r = run_cmd(["git", "-C", str(repo_dir), "checkout", "--force", base_commit], check=False)
        if r.returncode != 0:
            print(f"  CHECKOUT FAIL: {r.stderr[:200]}")
            return None
    return repo_dir


def apply_partial_patch(repo_dir: Path, partial_patch: str) -> bool:
    with tempfile.NamedTemporaryFile(mode="w", suffix=".patch", delete=False) as f:
        f.write(partial_patch)
        patch_path = f.name
    try:
        r = run_cmd(["git", "-C", str(repo_dir), "apply", "--index", patch_path], check=False)
        if r.returncode != 0:
            r = run_cmd(["git", "-C", str(repo_dir), "apply", "--index", "--3way", patch_path], check=False)
            if r.returncode != 0:
                return False
        run_cmd(["git", "-C", str(repo_dir), "commit", "-m", "partial", "--allow-empty", "--no-verify"], check=False)
        return True
    finally:
        os.unlink(patch_path)


def run_diffctx(repo_dir: Path, budget: int, scoring_mode: str = "auto") -> set[str]:
    from treemapper.diffctx.pipeline import build_diff_context

    try:
        output = build_diff_context(repo_dir, "HEAD~1..HEAD", budget_tokens=budget, scoring_mode=scoring_mode)
        return {f["path"] for f in output.get("fragments", [])}
    except Exception:
        return set()


_VENDOR_SEGMENTS = frozenset({"vendor/", "node_modules/", "third_party/", "generated/", "__generated__/", ".pb.go", "_pb2.py"})


def is_mechanical_change(patch_text: str) -> bool:
    if "similarity index 100%" in patch_text:
        return True
    if patch_text.count("diff --git") > 30:
        return True
    changes = [ln for ln in patch_text.splitlines() if ln.startswith(("+", "-")) and not ln.startswith(("+++", "---"))]
    if not changes:
        return True
    whitespace_only = sum(1 for ln in changes if not ln[1:].strip())
    return whitespace_only / len(changes) > 0.8


def is_vendor_or_generated(file_path: str) -> bool:
    return any(seg in file_path for seg in _VENDOR_SEGMENTS)


def _pick_distractor(repo_dir: Path, hidden: str) -> str | None:
    suffix = Path(hidden).suffix
    if not suffix:
        return None
    r = run_cmd(["find", str(repo_dir), "-name", f"*{suffix}", "-type", "f"], check=False, timeout=10)
    candidates = [line for line in r.stdout.splitlines() if line.strip() and hidden not in line]
    if not candidates:
        return None
    import hashlib

    seed = int(hashlib.md5(hidden.encode()).hexdigest()[:8], 16)  # NOSONAR — deterministic, not crypto
    rng = random.Random(seed)
    pick = rng.choice(candidates[:50])
    try:
        return str(Path(pick).relative_to(repo_dir))
    except ValueError:
        return None


def evaluate_loo(inst: dict, budget: int, scoring_mode: str = "auto", repos_dir: Path = REPOS_DIR) -> list[dict]:
    iid = inst["instance_id"]
    all_patch_files = patch_files(inst["patch"])

    if len(all_patch_files) < 2:
        return []

    repo_url = inst.get("repo_url") or f"https://github.com/{inst['repo']}.git"
    repo_dir = ensure_repo(repo_url, inst["repo"], inst["base_commit"], repos_dir)
    if not repo_dir:
        return []

    results = []
    for hidden in sorted(all_patch_files):
        partial = strip_file_from_patch(inst["patch"], hidden)
        remaining_files = patch_files(partial)
        if not remaining_files:
            continue

        run_cmd(["git", "-C", str(repo_dir), "checkout", "--force", inst["base_commit"]], check=False)
        run_cmd(["git", "-C", str(repo_dir), "clean", "-fd"], check=False)

        if not apply_partial_patch(repo_dir, partial):
            continue

        selected = run_diffctx(repo_dir, budget, scoring_mode)
        found = hidden in selected

        distractor = _pick_distractor(repo_dir, hidden)
        found_distractor = distractor in selected if distractor else False

        results.append(
            {
                "instance_id": iid,
                "hidden_file": hidden,
                "found": found,
                "distractor": distractor,
                "found_distractor": found_distractor,
                "n_patch_files": len(all_patch_files),
                "n_remaining": len(remaining_files),
                "n_selected": len(selected),
                "language": inst.get("language", "unknown"),
                "repo": inst["repo"],
            }
        )

    run_cmd(["git", "-C", str(repo_dir), "checkout", "--force", inst["base_commit"]], check=False)
    run_cmd(["git", "-C", str(repo_dir), "clean", "-fd"], check=False)
    return results


def _run_one(run_args: tuple[int, dict, int, str, int]) -> list[dict]:
    i, inst, budget, scoring, timeout = run_args
    iid = inst["instance_id"]
    n_files = len(patch_files(inst["patch"]))
    worker_dir = REPOS_DIR / f"w{os.getpid()}"
    worker_dir.mkdir(exist_ok=True)
    print(f"[{i}] {iid} ({n_files} files)", flush=True)
    try:
        import signal

        def _timeout_handler(_sig: int, _frame: object) -> None:
            raise TimeoutError

        old_handler = signal.signal(signal.SIGALRM, _timeout_handler)
        signal.alarm(timeout)
        try:
            results = evaluate_loo(inst, budget, scoring, repos_dir=worker_dir)
        finally:
            signal.alarm(0)
            signal.signal(signal.SIGALRM, old_handler)
        hits = sum(1 for r in results if r["found"])
        total = len(results)
        print(f"  LOO: {hits}/{total} found ({100 * hits / max(1, total):.0f}%)", flush=True)
        return results
    except TimeoutError:
        print(f"  TIMEOUT ({timeout}s): {iid}", flush=True)
        return []
    except Exception as e:
        print(f"  ERROR: {type(e).__name__}: {e}", flush=True)
        return []


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--limit", type=int, default=50)
    ap.add_argument("--budget", type=int, default=8000)
    ap.add_argument("--seed", type=int, default=42)
    ap.add_argument("--dataset", default="Contextbench/ContextBench")
    ap.add_argument("--split", default="contextbench_verified")
    ap.add_argument("--output", type=str, default=None)
    ap.add_argument("--workers", type=int, default=11)
    ap.add_argument("--scoring", type=str, default="auto", choices=["auto", "precise", "discover"])
    ap.add_argument("--timeout", type=int, default=300)
    args = ap.parse_args()

    from datasets import load_dataset

    ds = load_dataset(args.dataset, args.split, split="train")
    insts = list(ds)

    multi_file = [
        i
        for i in insts
        if len(patch_files(i["patch"])) >= 2
        and not is_mechanical_change(i["patch"])
        and not any(is_vendor_or_generated(f) for f in patch_files(i["patch"]))
    ]
    print(f"Total instances: {len(insts)}, multi-file (filtered): {len(multi_file)}")

    rng = random.Random(args.seed)  # NOSONAR — deterministic PRNG for reproducible benchmarks
    rng.shuffle(multi_file)
    multi_file = multi_file[: args.limit]

    print(f"Evaluating LOO on {len(multi_file)} instances (budget={args.budget})")
    print()

    all_results: list[dict] = []
    t0 = time.time()

    run_args = [(i, inst, args.budget, args.scoring, args.timeout) for i, inst in enumerate(multi_file, 1)]

    if args.workers > 1:
        from concurrent.futures import ProcessPoolExecutor, as_completed

        with ProcessPoolExecutor(max_workers=args.workers) as pool:
            futures = {pool.submit(_run_one, a): a[0] for a in run_args}
            for future in as_completed(futures):
                all_results.extend(future.result())
    else:
        for a in run_args:
            all_results.extend(_run_one(a))

    elapsed = time.time() - t0
    print()
    print("=" * 70)
    print(f"LOO RESULTS ({elapsed:.0f}s)")
    print("=" * 70)

    if not all_results:
        print("No results.")
        return

    total = len(all_results)
    found = sum(1 for r in all_results if r["found"])
    distractor_found = sum(1 for r in all_results if r.get("found_distractor"))
    distractor_total = sum(1 for r in all_results if r.get("distractor"))
    print(f"Total LOO trials: {total}")
    print(f"Found hidden file: {found}/{total} ({100 * found / total:.1f}%)")
    if distractor_total:
        print(f"Found distractor:  {distractor_found}/{distractor_total} ({100 * distractor_found / distractor_total:.1f}%)")
    print()

    by_repo: dict[str, list[dict]] = defaultdict(list)
    for r in all_results:
        by_repo[r["repo"]].append(r)

    print("Per-repo breakdown:")
    for repo in sorted(by_repo, key=lambda r: len(by_repo[r]), reverse=True):
        trials = by_repo[repo]
        h = sum(1 for t in trials if t["found"])
        print(f"  {repo:40s} {h}/{len(trials):3d} ({100 * h / len(trials):.0f}%)")

    by_lang: dict[str, list[dict]] = defaultdict(list)
    for r in all_results:
        by_lang[r["language"]].append(r)

    print("\nPer-language breakdown:")
    for lang in sorted(by_lang, key=lambda la: len(by_lang[la]), reverse=True):
        trials = by_lang[lang]
        h = sum(1 for t in trials if t["found"])
        print(f"  {lang:20s} {h}/{len(trials):3d} ({100 * h / len(trials):.0f}%)")

    if args.output:
        Path(args.output).write_text(json.dumps(all_results, indent=2))
        print(f"\nResults saved to {args.output}")


if __name__ == "__main__":
    main()
