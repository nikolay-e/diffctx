from __future__ import annotations

import contextlib
import fcntl
import json
import os
import re
import subprocess
import tempfile
import uuid
from pathlib import Path

WORKERS = int(os.environ.get("BENCH_WORKERS", "11"))
RESULTS_DIR = Path("results")

LINES_RE = re.compile(r"^(\d+)-(\d+)$")
_WORKSPACE_PREFIX_RE = re.compile(r"^/workspace/[^/]+/")

_LOCK_FILES = ("index.lock", "HEAD.lock", "refs/heads.lock")


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


def _parse_diff_path(raw: str, prefix: str) -> str | None:
    if raw == "/dev/null":
        return None
    return raw[len(prefix) :] if raw.startswith(prefix) else raw


def patch_files_detailed(patch: str) -> tuple[set[str], set[str], set[str]]:
    added: set[str] = set()
    deleted: set[str] = set()
    modified: set[str] = set()
    cur_a = cur_b = None
    for line in patch.splitlines():
        if line.startswith("diff --git "):
            cur_a = cur_b = None
        elif line.startswith("--- "):
            cur_a = _parse_diff_path(line[4:], "a/")
        elif line.startswith("+++ "):
            cur_b = _parse_diff_path(line[4:], "b/")
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


_PERF_CONFIG = (
    ("gc.auto", "0"),
    ("feature.manyFiles", "true"),
    ("index.version", "4"),
    ("core.untrackedCache", "true"),
    ("core.fsmonitor", "false"),
)


def _apply_perf_config(repo_dir: Path) -> None:
    for key, value in _PERF_CONFIG:
        run_cmd(["git", "-C", str(repo_dir), "config", key, value], check=False, timeout=10)


def _clone_one_repo(args: tuple[str, str]) -> None:
    repo, url = args
    safe_name = repo.replace("/", "__")
    cache_dir = _SHARED_CACHE / safe_name
    if cache_dir.exists():
        _apply_perf_config(cache_dir)
        run_cmd(["git", "-C", str(cache_dir), "worktree", "prune"], check=False, timeout=30)
        return
    print(f"  Cloning {repo}...", flush=True)
    r = run_cmd(["git", "clone", "--quiet", "--bare", url, str(cache_dir)], check=False, timeout=600)
    if r.returncode != 0:
        print(f"  CLONE FAIL {repo}: {r.stderr[:200]}")
        return
    _apply_perf_config(cache_dir)


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


def _ensure_bare_cache(repo_url: str, repo_name: str) -> Path | None:
    safe_name = repo_name.replace("/", "__")
    cache_dir = _SHARED_CACHE / safe_name
    if cache_dir.exists():
        return cache_dir
    url = repo_url or f"https://github.com/{repo_name}.git"
    r = run_cmd(["git", "clone", "--quiet", "--bare", url, str(cache_dir)], check=False, timeout=600)
    if r.returncode != 0:
        print(f"  CLONE FAIL: {r.stderr[:200]}")
        return None
    _apply_perf_config(cache_dir)
    return cache_dir


def _remove_stale_locks(git_dir: Path) -> None:
    for lock_name in _LOCK_FILES:
        lock = git_dir / lock_name
        if lock.exists():
            try:
                lock.unlink()
            except OSError:
                pass


def _git_dir_for_repo(repo_dir: Path) -> Path:
    git_path = repo_dir / ".git"
    if git_path.is_file():
        content = git_path.read_text().strip()
        if content.startswith("gitdir: "):
            return Path(content[8:])
    return git_path


@contextlib.contextmanager
def _bare_repo_lock(cache_dir: Path):
    lock_path = cache_dir / ".bench-worktree.lock"
    fd = os.open(str(lock_path), os.O_CREAT | os.O_RDWR, 0o644)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX)
        yield
    finally:
        fcntl.flock(fd, fcntl.LOCK_UN)
        os.close(fd)


def ensure_repo(
    repo_url: str,
    repo_name: str,
    base_commit: str,
    target_dir: Path,
    checkout_timeout: int = 120,
) -> Path | None:
    cache_dir = _ensure_bare_cache(repo_url, repo_name)
    if not cache_dir:
        return None
    repo_dir = target_dir / repo_name.replace("/", "__")
    if repo_dir.exists():
        _remove_stale_locks(_git_dir_for_repo(repo_dir))
        run_cmd(["git", "-C", str(repo_dir), "clean", "-fd"], check=False, timeout=30)
        r = run_cmd(
            ["git", "-C", str(repo_dir), "checkout", "--force", base_commit],
            check=False,
            timeout=checkout_timeout,
        )
    else:
        with _bare_repo_lock(cache_dir):
            run_cmd(["git", "-C", str(cache_dir), "worktree", "prune"], check=False, timeout=30)
            r = run_cmd(
                ["git", "-C", str(cache_dir), "worktree", "add", "--detach", "--force", str(repo_dir), base_commit],
                check=False,
                timeout=checkout_timeout,
            )
        if r.returncode == 0:
            _apply_perf_config(repo_dir)
    if r.returncode != 0:
        print(f"  WORKTREE/CHECKOUT FAIL {base_commit[:12]}: {r.stderr[:200]}")
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


def load_results(path: Path) -> list[dict]:
    data = json.loads(path.read_text())
    if isinstance(data, dict) and "results" in data:
        return list(data["results"])
    if isinstance(data, list):
        return data
    raise ValueError(f"unexpected results shape in {path}: {type(data).__name__}")


def _git_commit_sha() -> str:
    try:
        r = subprocess.run(
            ["git", "rev-parse", "--short=7", "HEAD"],
            capture_output=True,
            text=True,
            timeout=5,
        )
        return r.stdout.strip() or "unknown"
    except Exception:
        return "unknown"


def save_results(results: list, tag: str, output_dir: Path = RESULTS_DIR, **meta) -> Path:
    import platform
    import sys
    from datetime import datetime, timezone

    envelope = {
        "meta": {
            "timestamp": datetime.now(timezone.utc).isoformat(),
            "command": " ".join(sys.argv),
            "python_version": sys.version.split()[0],
            "platform": platform.platform(),
            "git_commit": _git_commit_sha(),
            **meta,
        },
        "results": results,
    }
    output_dir.mkdir(parents=True, exist_ok=True)
    path = output_dir / f"{tag}.json"
    path.write_text(json.dumps(envelope, indent=2))
    print(f"\nResults saved to {path}")
    return path


_worker_id: str = ""


def _init_worker() -> None:
    global _worker_id
    import importlib
    import warnings

    warnings.filterwarnings("ignore", category=SyntaxWarning)

    os.environ.setdefault("RAYON_NUM_THREADS", os.environ.get("BENCH_RAYON_THREADS", "1"))
    os.environ.setdefault("OMP_NUM_THREADS", "1")
    os.environ.setdefault("MKL_NUM_THREADS", "1")
    os.environ.setdefault("OPENBLAS_NUM_THREADS", "1")
    os.environ.setdefault("TOKENIZERS_PARALLELISM", "false")

    for mod in (
        "_diffctx",
        "treemapper.diffctx.pipeline",
        "tiktoken",
    ):
        try:
            importlib.import_module(mod)
        except ImportError:
            pass

    _worker_id = uuid.uuid4().hex[:12]


def worker_dir(base: Path) -> Path:
    wid = _worker_id or uuid.uuid4().hex[:12]
    d = base / f"w{wid}"
    d.mkdir(exist_ok=True)
    return d


def _collect_result(results: list, r, collect: str) -> None:
    if collect == "extend":
        results.extend(r)
    elif r:
        results.append(r)


def _run_serial(worker_fn, run_args: list, collect: str) -> list:
    results: list = []
    for a in run_args:
        _collect_result(results, worker_fn(a), collect)
    return results


def _run_pool(worker_fn, run_args: list, workers: int, collect: str) -> list:
    from concurrent.futures import ProcessPoolExecutor, as_completed
    from concurrent.futures.process import BrokenProcessPool

    batch_size = int(os.environ.get("BENCH_BATCH_SIZE", str(max(workers * 4, 20))))
    results: list = []
    for batch_start in range(0, len(run_args), batch_size):
        batch = run_args[batch_start : batch_start + batch_size]
        try:
            with ProcessPoolExecutor(max_workers=workers, initializer=_init_worker) as pool:
                futures = {pool.submit(worker_fn, a): a[0] for a in batch}
                for future in as_completed(futures):
                    try:
                        _collect_result(results, future.result(), collect)
                    except BrokenProcessPool as e:
                        print(f"  WORKER CRASH [{futures[future]}]: {type(e).__name__}", flush=True)
                    except Exception as e:
                        print(f"  WORKER CRASH [{futures[future]}]: {type(e).__name__}: {e}", flush=True)
        except BrokenProcessPool as e:
            print(f"  POOL CRASH batch {batch_start}-{batch_start+len(batch)}: {e}", flush=True)
    return results


def run_parallel(worker_fn, run_args: list, workers: int, collect: str = "append") -> list:
    if workers > 1:
        return _run_pool(worker_fn, run_args, workers, collect)
    return _run_serial(worker_fn, run_args, collect)
