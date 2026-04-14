from __future__ import annotations

import json
import os
import re
import subprocess
import tempfile
from pathlib import Path

WORKERS = 11
RESULTS_DIR = Path("results")

LINES_RE = re.compile(r"^(\d+)-(\d+)$")
_WORKSPACE_PREFIX_RE = re.compile(r"^/workspace/[^/]+/")


def normalize_gold_path(path: str) -> str:
    return _WORKSPACE_PREFIX_RE.sub("", path)


def repos_dir(env_var: str = "CB_REPOS_DIR", suffix_var: str | None = None) -> Path:
    base = Path(os.environ.get(env_var, str(Path.home() / ".cache" / "contextbench_repos")))
    if suffix_var:
        sfx = os.environ.get(suffix_var, "")
        if sfx:
            base = base / sfx
    base.mkdir(parents=True, exist_ok=True)
    return base


def run_cmd(
    cmd: list[str], cwd: str | Path | None = None, check: bool = True, timeout: int = 120
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(cmd, cwd=cwd, text=True, capture_output=True, check=check, timeout=timeout)


def patch_files_detailed(patch: str) -> tuple[set[str], set[str], set[str]]:
    added: set[str] = set()
    deleted: set[str] = set()
    modified: set[str] = set()
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
    return added, deleted, modified


def patch_files(patch: str) -> set[str]:
    added, deleted, modified = patch_files_detailed(patch)
    return added | deleted | modified


_SHARED_CACHE = Path(os.environ.get("CB_REPOS_DIR", str(Path.home() / ".cache" / "contextbench_repos")))
_SHARED_CACHE.mkdir(parents=True, exist_ok=True)


def _clone_one_repo(args: tuple[str, str]) -> None:
    repo, url = args
    safe_name = repo.replace("/", "__")
    cache_dir = _SHARED_CACHE / safe_name
    if cache_dir.exists():
        return
    print(f"  Cloning {repo}...", flush=True)
    r = run_cmd(["git", "clone", "--quiet", "--bare", url, str(cache_dir)], check=False, timeout=600)
    if r.returncode != 0:
        print(f"  CLONE FAIL {repo}: {r.stderr[:200]}")


def _fetch_one_commit(args: tuple[str, str]) -> None:
    repo, commit = args
    cache_dir = _SHARED_CACHE / repo.replace("/", "__")
    if not cache_dir.exists():
        return
    r = run_cmd(["git", "-C", str(cache_dir), "cat-file", "-t", commit], check=False, timeout=30)
    if r.returncode != 0:
        print(f"  Fetching {commit[:12]} for {repo}...", flush=True)
        run_cmd(["git", "-C", str(cache_dir), "fetch", "--quiet", "origin", commit], check=False, timeout=600)


def warm_cache(instances: list[dict]) -> None:
    from concurrent.futures import ThreadPoolExecutor

    repos_to_clone: dict[str, str] = {}
    for inst in instances:
        repo = inst["repo"]
        if repo not in repos_to_clone:
            repos_to_clone[repo] = inst.get("repo_url") or f"https://github.com/{repo}.git"

    with ThreadPoolExecutor(max_workers=WORKERS) as pool:
        pool.map(_clone_one_repo, repos_to_clone.items())

    commits_to_fetch: dict[tuple[str, str], None] = {}
    for inst in instances:
        commits_to_fetch[(inst["repo"], inst["base_commit"])] = None

    with ThreadPoolExecutor(max_workers=WORKERS) as pool:
        pool.map(_fetch_one_commit, commits_to_fetch.keys())

    print(f"  Cache warm: {len(repos_to_clone)} repos", flush=True)


def ensure_repo(
    repo_url: str,
    repo_name: str,
    base_commit: str,
    target_dir: Path,
    checkout_timeout: int = 120,
) -> Path | None:
    safe_name = repo_name.replace("/", "__")
    cache_dir = _SHARED_CACHE / safe_name
    if not cache_dir.exists():
        url = repo_url or f"https://github.com/{repo_name}.git"
        r = run_cmd(["git", "clone", "--quiet", "--bare", url, str(cache_dir)], check=False, timeout=600)
        if r.returncode != 0:
            print(f"  CLONE FAIL: {r.stderr[:200]}")
            return None
    repo_dir = target_dir / repo_name.replace("/", "__")
    if not repo_dir.exists():
        r = run_cmd(
            ["git", "clone", "--quiet", "--local", str(cache_dir), str(repo_dir)],
            check=False,
            timeout=120,
        )
        if r.returncode != 0:
            print(f"  LOCAL CLONE FAIL: {r.stderr[:200]}")
            return None
    run_cmd(["git", "-C", str(repo_dir), "clean", "-fd"], check=False, timeout=30)
    r = run_cmd(["git", "-C", str(repo_dir), "checkout", "--force", base_commit], check=False, timeout=checkout_timeout)
    if r.returncode != 0:
        print(f"  CHECKOUT FAIL {base_commit[:12]}: {r.stderr[:200]}")
        return None
    return repo_dir


def apply_as_commit(repo_dir: Path, patch_text: str, message: str = "bench") -> bool:
    with tempfile.NamedTemporaryFile(mode="w", suffix=".patch", delete=False) as f:
        f.write(patch_text)
        patch_path = f.name
    try:
        r = run_cmd(["git", "-C", str(repo_dir), "apply", "--index", patch_path], check=False)
        if r.returncode != 0:
            r = run_cmd(["git", "-C", str(repo_dir), "apply", "--index", "--3way", patch_path], check=False)
            if r.returncode != 0:
                print(f"  APPLY FAIL: {r.stderr[:300]}")
                return False
        run_cmd(["git", "-C", str(repo_dir), "commit", "-m", message, "--allow-empty", "--no-verify"], check=False)
        return True
    finally:
        os.unlink(patch_path)


def reset_to_parent(repo_dir: Path) -> None:
    run_cmd(["git", "-C", str(repo_dir), "reset", "--hard", "HEAD~1"], check=False, timeout=30)


def reset_to_commit(repo_dir: Path, commit: str) -> None:
    run_cmd(["git", "-C", str(repo_dir), "checkout", "--force", commit], check=False, timeout=30)


def parse_lines_field(lines_str: str) -> tuple[int, int] | None:
    m = LINES_RE.match(lines_str.strip())
    if not m:
        return None
    s, e = int(m.group(1)), int(m.group(2))
    if s < 1 or e < s:
        return None
    return (s, e)


def save_results(results: list, tag: str, output_dir: Path = RESULTS_DIR) -> Path:
    output_dir.mkdir(parents=True, exist_ok=True)
    path = output_dir / f"{tag}.json"
    path.write_text(json.dumps(results, indent=2))
    print(f"\nResults saved to {path}")
    return path


def worker_dir(base: Path) -> Path:
    d = base / f"w{os.getpid()}"
    d.mkdir(exist_ok=True)
    return d


def _suppress_spawn_warnings() -> None:
    import warnings

    warnings.filterwarnings("ignore", category=SyntaxWarning)


def run_parallel(worker_fn, run_args: list, workers: int, collect: str = "append") -> list:
    results: list = []
    if workers > 1:
        from concurrent.futures import ProcessPoolExecutor, as_completed

        with ProcessPoolExecutor(max_workers=workers, initializer=_suppress_spawn_warnings) as pool:
            futures = {pool.submit(worker_fn, a): a[0] for a in run_args}
            for future in as_completed(futures):
                r = future.result()
                if collect == "extend":
                    results.extend(r)
                elif r:
                    results.append(r)
    else:
        for a in run_args:
            r = worker_fn(a)
            if collect == "extend":
                results.extend(r)
            elif r:
                results.append(r)
    return results
