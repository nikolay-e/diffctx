#!/usr/bin/env python3
"""Bake bench repo cache into a Docker layer.

Clones all repos referenced by the three test-set datasets as bare mirrors
and fetches every required commit. Designed to run inside a Docker build
stage so that /cache/contextbench_repos becomes a baked-in image layer.

Datasets baked:
  - Contextbench/ContextBench   (default config, train split)
  - princeton-nlp/SWE-bench_Verified  (test split)
  - AmazonScience/SWE-PolyBench_500   (test split)

Idempotent: skips clones that already exist, skips commits already local.
"""

from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path

CLONE_TIMEOUT_SECS = 3600
FETCH_TIMEOUT_SECS = 900
CLONE_PARALLELISM = int(os.environ.get("BAKE_PARALLELISM", "4"))
NETWORK_RETRY_ATTEMPTS = 3
NETWORK_RETRY_BACKOFF_SECS = 5

DATASETS: list[tuple[str, str | None, str]] = [
    ("Contextbench/ContextBench", "default", "train"),
    ("Contextbench/ContextBench", "contextbench_verified", "train"),
    ("princeton-nlp/SWE-bench_Verified", None, "test"),
    ("AmazonScience/SWE-PolyBench_500", None, "test"),
]


def safe_name(repo: str) -> str:
    return repo.replace("/", "__")


def run(cmd: list[str], timeout: int) -> tuple[int, str]:
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout, check=False)
        return r.returncode, (r.stderr or "")[:500]
    except subprocess.TimeoutExpired:
        return 124, f"timeout after {timeout}s"


def _is_valid_bare(cache_dir: Path) -> bool:
    if not cache_dir.exists():
        return False
    r = subprocess.run(
        ["git", "-C", str(cache_dir), "rev-parse", "--verify", "HEAD"],
        capture_output=True,
        timeout=10,
    )
    return r.returncode == 0


def _purge(cache_dir: Path) -> None:
    shutil.rmtree(cache_dir, ignore_errors=True)


_PERF_CONFIG = (
    ("gc.auto", "0"),
    ("feature.manyFiles", "true"),
    ("index.version", "4"),
    ("core.fsmonitor", "false"),
)


def _apply_perf_config(cache_dir: Path) -> None:
    for key, value in _PERF_CONFIG:
        subprocess.run(
            ["git", "-C", str(cache_dir), "config", key, value],
            capture_output=True,
            timeout=10,
        )


def _clone_attempt(url: str, cache_dir: Path) -> str | None:
    try:
        r = subprocess.run(
            ["git", "clone", "--bare", url, str(cache_dir)],
            capture_output=True,
            timeout=CLONE_TIMEOUT_SECS,
        )
    except subprocess.TimeoutExpired:
        _purge(cache_dir)
        return f"timeout after {CLONE_TIMEOUT_SECS}s"
    if r.returncode != 0:
        _purge(cache_dir)
        return r.stderr.decode("utf-8", "replace")[:500]
    return None


def clone_one(repo: str, url: str, target: Path) -> tuple[str, bool, str]:
    cache_dir = target / safe_name(repo)
    if cache_dir.exists():
        if _is_valid_bare(cache_dir):
            return repo, True, "exists"
        print(f"  CORRUPT cache for {repo}, removing and recloning", flush=True)
        _purge(cache_dir)

    last_err = ""
    for attempt in range(1, NETWORK_RETRY_ATTEMPTS + 1):
        print(f"  CLONE {repo} <- {url} (attempt {attempt})", flush=True)
        last_err = _clone_attempt(url, cache_dir) or ""
        if last_err:
            if attempt < NETWORK_RETRY_ATTEMPTS:
                time.sleep(NETWORK_RETRY_BACKOFF_SECS * (2 ** (attempt - 1)))
            continue
        if not _is_valid_bare(cache_dir):
            _purge(cache_dir)
            return repo, False, "post-clone validation failed"
        _apply_perf_config(cache_dir)
        return repo, True, "cloned"

    return repo, False, last_err


def fetch_one(repo: str, commit: str, target: Path) -> tuple[str, str, bool, str]:
    cache_dir = target / safe_name(repo)
    if not cache_dir.exists():
        return repo, commit, False, "no cache dir"
    rc, _ = run(["git", "-C", str(cache_dir), "cat-file", "-t", commit], 30)
    if rc == 0:
        return repo, commit, True, "have"
    last_err = ""
    for attempt in range(1, NETWORK_RETRY_ATTEMPTS + 1):
        print(f"  FETCH {repo}@{commit[:12]} (attempt {attempt})", flush=True)
        rc, err = run(["git", "-C", str(cache_dir), "fetch", "--quiet", "origin", commit], FETCH_TIMEOUT_SECS)
        if rc == 0:
            return repo, commit, True, "fetched"
        last_err = err
        if "unadvertised object" in err or "not our ref" in err or "couldn't find remote ref" in err.lower():
            return repo, commit, False, f"unreachable_commit: {err}"
        if attempt < NETWORK_RETRY_ATTEMPTS:
            time.sleep(NETWORK_RETRY_BACKOFF_SECS * (2 ** (attempt - 1)))
    return repo, commit, False, last_err


def collect_repos_commits(
    hf_path: str,
    config: str | None,
    split: str,
    limit: int,
) -> tuple[dict[str, str], set[tuple[str, str]]]:
    from datasets import load_dataset

    print(f"  Loading {hf_path} (config={config or 'default'}, split={split}) ...", flush=True)
    ds = load_dataset(hf_path, name=config, split=split) if config else load_dataset(hf_path, split=split)
    instances = list(ds)
    if limit:
        instances = instances[:limit]
    repos: dict[str, str] = {}
    commits: set[tuple[str, str]] = set()
    for raw in instances:
        inst = dict(raw)
        repo = inst.get("repo") or inst.get("repo_name") or ""
        if not repo:
            continue
        commit = inst.get("base_commit") or inst.get("commit") or ""
        if not commit:
            continue
        if repo not in repos:
            url = inst.get("repo_url") or f"https://github.com/{repo}.git"
            repos[repo] = url
        commits.add((repo, commit))
    print(f"  → {len(repos)} unique repos, {len(commits)} unique commits", flush=True)
    return repos, commits


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("target", type=Path, help="Target cache dir (e.g. /cache/contextbench_repos)")
    ap.add_argument("--limit", type=int, default=0, help="Cap instances per dataset (0 = all)")
    args = ap.parse_args()

    args.target.mkdir(parents=True, exist_ok=True)

    all_repos: dict[str, str] = {}
    all_commits: set[tuple[str, str]] = set()

    for hf_path, config, split in DATASETS:
        repos, commits = collect_repos_commits(hf_path, config, split, args.limit)
        all_repos.update(repos)
        all_commits.update(commits)

    print(
        f"\nTotal: {len(all_repos)} unique repos, {len(all_commits)} unique commits",
        flush=True,
    )
    print(f"Cloning with parallelism={CLONE_PARALLELISM}", flush=True)

    failed_clones: list[tuple[str, str]] = []
    with ThreadPoolExecutor(max_workers=CLONE_PARALLELISM) as clone_pool:
        clone_futs = {clone_pool.submit(clone_one, r, u, args.target): r for r, u in all_repos.items()}
        for clone_fut in as_completed(clone_futs):
            repo, ok, msg = clone_fut.result()
            if not ok:
                failed_clones.append((repo, msg))
                print(f"  CLONE FAIL {repo}: {msg}", flush=True)

    print(f"\nClones: {len(all_repos) - len(failed_clones)}/{len(all_repos)} successful", flush=True)

    failed_fetches: list[tuple[str, str, str]] = []
    ok_count = 0
    with ThreadPoolExecutor(max_workers=CLONE_PARALLELISM * 2) as fetch_pool:
        fetch_futs = {fetch_pool.submit(fetch_one, r, c, args.target): (r, c) for r, c in all_commits}
        for fetch_fut in as_completed(fetch_futs):
            repo, commit, ok, msg = fetch_fut.result()
            if ok:
                ok_count += 1
            else:
                failed_fetches.append((repo, commit, msg))

    print(f"\nCommits: {ok_count}/{len(all_commits)} present", flush=True)
    if failed_fetches:
        print(f"  Failed fetches: {len(failed_fetches)}", flush=True)
        for r, c, m in failed_fetches[:10]:
            print(f"    {r}@{c[:12]}: {m}", flush=True)

    print("\nCache size:", flush=True)
    subprocess.run(["du", "-sh", str(args.target)], check=False)

    import json

    manifest_path = args.target / ".bake_manifest.json"
    manifest_path.write_text(
        json.dumps(
            {
                "failed_clones": [{"repo": r, "error": m} for r, m in failed_clones],
                "failed_fetches": [{"repo": r, "commit": c, "error": m} for r, c, m in failed_fetches],
                "ok_repos": sorted(set(all_repos) - {r for r, _ in failed_clones}),
            },
            indent=2,
        )
    )

    if failed_clones:
        print(
            f"BAKE FAILED: {len(failed_clones)} clone failures (fetch failures={len(failed_fetches)})",
            flush=True,
        )
        return 1
    if failed_fetches:
        print(
            f"BAKE WARNING: {len(failed_fetches)} unreachable commits (force-pushed/deleted upstream) — sweep will skip these instances",
            flush=True,
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())
