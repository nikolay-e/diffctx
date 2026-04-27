#!/usr/bin/env python3
"""Bake bench repo cache into a Docker layer.

Reads ContextBench full dataset, enumerates unique (repo, base_commit) pairs,
clones each repo as a bare mirror, and fetches each commit explicitly. Designed
to run inside a Docker build stage so that the resulting /cache/contextbench_repos
directory becomes a baked-in image layer (no clone at runtime).

Idempotent: skips clones that already exist, skips fetches that are already local.
Runs clones in parallel for throughput.
"""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path

CLONE_TIMEOUT_SECS = 1800
FETCH_TIMEOUT_SECS = 600
CLONE_PARALLELISM = int(os.environ.get("BAKE_PARALLELISM", "4"))


def safe_name(repo: str) -> str:
    return repo.replace("/", "__")


def repo_url_for(inst: dict) -> str:
    url = inst.get("repo_url")
    if url:
        return url
    return f"https://github.com/{inst['repo']}.git"


def run(cmd: list[str], timeout: int) -> tuple[int, str]:
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout, check=False)
        return r.returncode, (r.stderr or "")[:500]
    except subprocess.TimeoutExpired:
        return 124, f"timeout after {timeout}s"


def clone_one(repo: str, url: str, target: Path) -> tuple[str, bool, str]:
    safe = safe_name(repo)
    cache_dir = target / safe
    if cache_dir.exists():
        return repo, True, "exists"
    print(f"  CLONE {repo} <- {url}", flush=True)
    rc, err = run(["git", "clone", "--quiet", "--bare", url, str(cache_dir)], CLONE_TIMEOUT_SECS)
    if rc != 0:
        return repo, False, err
    # Disable gc so packs stay deterministic
    run(["git", "-C", str(cache_dir), "config", "gc.auto", "0"], 30)
    return repo, True, "cloned"


def fetch_one(repo: str, commit: str, target: Path) -> tuple[str, str, bool, str]:
    cache_dir = target / safe_name(repo)
    if not cache_dir.exists():
        return repo, commit, False, "no cache dir"
    rc, _ = run(["git", "-C", str(cache_dir), "cat-file", "-t", commit], 30)
    if rc == 0:
        return repo, commit, True, "have"
    print(f"  FETCH {repo}@{commit[:12]}", flush=True)
    rc, err = run(["git", "-C", str(cache_dir), "fetch", "--quiet", "origin", commit], FETCH_TIMEOUT_SECS)
    return repo, commit, rc == 0, err if rc != 0 else "fetched"


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("target", type=Path, help="Target cache dir (e.g. /cache/contextbench_repos)")
    ap.add_argument("--dataset", default="full", choices=["full", "verified"])
    ap.add_argument("--limit", type=int, default=0, help="Optional cap on instances (0 = all)")
    args = ap.parse_args()

    args.target.mkdir(parents=True, exist_ok=True)

    print(f"Loading ContextBench/{args.dataset}...", flush=True)
    from datasets import load_dataset

    config = "contextbench_verified" if args.dataset == "verified" else "default"
    ds = load_dataset("Contextbench/ContextBench", config, split="train")
    instances = list(ds)
    if args.limit:
        instances = instances[: args.limit]
    print(f"Loaded {len(instances)} instances", flush=True)

    repos: dict[str, str] = {}
    commits: set[tuple[str, str]] = set()
    for raw in instances:
        inst: dict = dict(raw)  # type: ignore[arg-type]
        repo = inst["repo"]
        if repo not in repos:
            repos[repo] = repo_url_for(inst)
        commits.add((repo, inst["base_commit"]))

    print(f"Unique repos: {len(repos)}, unique commits: {len(commits)}", flush=True)
    print(f"Cloning with parallelism={CLONE_PARALLELISM}", flush=True)

    failed_clones: list[tuple[str, str]] = []
    with ThreadPoolExecutor(max_workers=CLONE_PARALLELISM) as pool:
        futs = {pool.submit(clone_one, r, u, args.target): r for r, u in repos.items()}
        for fut in as_completed(futs):
            repo, ok, msg = fut.result()
            if not ok:
                failed_clones.append((repo, msg))
                print(f"  CLONE FAIL {repo}: {msg}", flush=True)

    print(f"\nClones: {len(repos) - len(failed_clones)}/{len(repos)} successful", flush=True)
    if failed_clones:
        print(f"  Failed: {[r for r, _ in failed_clones]}", flush=True)

    failed_fetches: list[tuple[str, str, str]] = []
    with ThreadPoolExecutor(max_workers=CLONE_PARALLELISM * 2) as pool:
        futs = {pool.submit(fetch_one, r, c, args.target): (r, c) for r, c in commits}
        ok_count = 0
        for fut in as_completed(futs):
            repo, commit, ok, msg = fut.result()
            if ok:
                ok_count += 1
            else:
                failed_fetches.append((repo, commit, msg))

    print(f"\nCommits: {ok_count}/{len(commits)} present", flush=True)
    if failed_fetches:
        print(f"  Failed fetches: {len(failed_fetches)}", flush=True)
        for r, c, m in failed_fetches[:10]:
            print(f"    {r}@{c[:12]}: {m}", flush=True)

    print("\nCache size estimate:", flush=True)
    subprocess.run(["du", "-sh", str(args.target)], check=False)

    # Don't fail the build for partial cache — bench gracefully handles missing repos
    return 0


if __name__ == "__main__":
    sys.exit(main())
