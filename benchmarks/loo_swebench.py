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

REPOS_DIR = Path(tempfile.gettempdir()) / "contextbench_repos"
REPOS_DIR.mkdir(exist_ok=True)


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
    for line in lines:
        if line.startswith("diff --git "):
            skip = f"a/{file_to_hide}" in line or f"b/{file_to_hide}" in line
        if not skip:
            result.append(line)
    return "\n".join(result)


def ensure_repo(repo_url: str, repo_name: str, base_commit: str) -> Path | None:
    repo_dir = REPOS_DIR / repo_name.replace("/", "__")
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


def run_diffctx(repo_dir: Path, budget: int) -> set[str]:
    from treemapper.diffctx.pipeline import build_diff_context

    try:
        output = build_diff_context(repo_dir, "HEAD~1..HEAD", budget_tokens=budget)
        return {f["path"] for f in output.get("fragments", [])}
    except Exception:
        return set()


def evaluate_loo(inst: dict, budget: int) -> list[dict]:
    iid = inst["instance_id"]
    all_patch_files = patch_files(inst["patch"])

    if len(all_patch_files) < 2:
        return []

    repo_url = inst.get("repo_url") or f"https://github.com/{inst['repo']}.git"
    repo_dir = ensure_repo(repo_url, inst["repo"], inst["base_commit"])
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

        selected = run_diffctx(repo_dir, budget)
        found = hidden in selected

        results.append(
            {
                "instance_id": iid,
                "hidden_file": hidden,
                "found": found,
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


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--limit", type=int, default=50)
    ap.add_argument("--budget", type=int, default=8000)
    ap.add_argument("--seed", type=int, default=42)
    ap.add_argument("--dataset", default="Contextbench/ContextBench")
    ap.add_argument("--split", default="contextbench_verified")
    ap.add_argument("--output", type=str, default=None)
    args = ap.parse_args()

    from datasets import load_dataset

    ds = load_dataset(args.dataset, args.split, split="train")
    insts = list(ds)

    multi_file = [i for i in insts if len(patch_files(i["patch"])) >= 2]
    print(f"Total instances: {len(insts)}, multi-file: {len(multi_file)}")

    rng = random.Random(args.seed)
    rng.shuffle(multi_file)
    multi_file = multi_file[: args.limit]

    print(f"Evaluating LOO on {len(multi_file)} instances (budget={args.budget})")
    print()

    all_results: list[dict] = []
    t0 = time.time()

    for i, inst in enumerate(multi_file, 1):
        iid = inst["instance_id"]
        n_files = len(patch_files(inst["patch"]))
        print(f"[{i}/{len(multi_file)}] {iid} ({n_files} files)")

        try:
            results = evaluate_loo(inst, args.budget)
            hits = sum(1 for r in results if r["found"])
            total = len(results)
            print(f"  LOO: {hits}/{total} found ({100 * hits / max(1, total):.0f}%)")
            all_results.extend(results)
        except Exception as e:
            print(f"  ERROR: {type(e).__name__}: {e}")

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
    print(f"Total LOO trials: {total}")
    print(f"Found hidden file: {found}/{total} ({100 * found / total:.1f}%)")
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
