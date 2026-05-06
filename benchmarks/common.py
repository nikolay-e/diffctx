from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import tempfile
import time
import uuid

try:
    import fcntl as _fcntl
except ImportError:
    _fcntl = None  # type: ignore[assignment]
from collections.abc import Generator
from contextlib import contextmanager
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
                # Pure rename: old path is gone after the patch is applied,
                # only the new path exists on the post-patch worktree.
                deleted.add(cur_a)
                modified.add(cur_b)
    return added, deleted, modified


def patch_files(patch: str) -> set[str]:
    added, deleted, modified = patch_files_detailed(patch)
    return added | deleted | modified


def patch_size_metrics(patch: str) -> dict[str, int]:
    """Lightweight per-instance descriptive numbers extracted from a unified diff.

    Used by the evaluator to stamp `n_changed_files`, `n_hunks`, and
    `diff_size_lines` into `EvalResult.extra` so cell aggregators can
    stratify recall by patch difficulty without re-parsing the diff.
    """
    added, deleted, modified = patch_files_detailed(patch)
    n_changed = len(added | deleted | modified)
    n_hunks = 0
    n_added_lines = 0
    n_removed_lines = 0
    for line in patch.splitlines():
        if line.startswith("@@ "):
            n_hunks += 1
        elif line.startswith("+") and not line.startswith("+++"):
            n_added_lines += 1
        elif line.startswith("-") and not line.startswith("---"):
            n_removed_lines += 1
    return {
        "n_changed_files": n_changed,
        "n_added_files": len(added),
        "n_deleted_files": len(deleted),
        "n_modified_files": len(modified),
        "n_hunks": n_hunks,
        "diff_added_lines": n_added_lines,
        "diff_removed_lines": n_removed_lines,
        "diff_size_lines": n_added_lines + n_removed_lines,
    }


def patch_files_at_head(patch: str) -> set[str]:
    """Files present on disk after the patch is applied — gold for file-recall metrics.

    Excludes pure deletions and old paths of pure renames, which no
    retrieval algorithm running against HEAD can return.
    """
    added, _, modified = patch_files_detailed(patch)
    return added | modified


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
        print(f"  CLONE FAIL {repo}: {r.stderr}", flush=True)
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


@contextmanager
def _cache_lock(cache_dir: Path) -> Generator[None, None, None]:
    lock_path = cache_dir.parent / f".{cache_dir.name}.lock"
    lock_path.parent.mkdir(parents=True, exist_ok=True)
    with open(lock_path, "w") as fd:
        if _fcntl is not None:
            _fcntl.flock(fd.fileno(), _fcntl.LOCK_EX)
        try:
            yield
        finally:
            if _fcntl is not None:
                _fcntl.flock(fd.fileno(), _fcntl.LOCK_UN)


def _purge_cache_dir(cache_dir: Path) -> None:
    shutil.rmtree(cache_dir, ignore_errors=True)


def _clone_bare(url: str, cache_dir: Path, attempts: int = 3, base_backoff: float = 5.0) -> bool:
    for attempt in range(1, attempts + 1):
        r = run_cmd(["git", "clone", "--quiet", "--bare", url, str(cache_dir)], check=False, timeout=900)
        if r.returncode == 0:
            return True
        _purge_cache_dir(cache_dir)
        if attempt < attempts:
            time.sleep(base_backoff * (2 ** (attempt - 1)))
        else:
            print(f"  CLONE FAIL {url}: {r.stderr}", flush=True)
    return False


def _is_bare_valid(cache_dir: Path) -> bool:
    r = subprocess.run(
        ["git", "-C", str(cache_dir), "rev-parse", "--verify", "HEAD"],
        capture_output=True,
        timeout=10,
    )
    return r.returncode == 0


def _ensure_bare_cache(repo_url: str, repo_name: str) -> Path | None:
    safe_name = repo_name.replace("/", "__")
    cache_dir = _SHARED_CACHE / safe_name

    if cache_dir.exists() and _is_bare_valid(cache_dir):
        return cache_dir

    with _cache_lock(cache_dir):
        if cache_dir.exists():
            if _is_bare_valid(cache_dir):
                return cache_dir
            print(f"[ensure_repo] corrupt bare {cache_dir}, recloning", flush=True)
            _purge_cache_dir(cache_dir)
        url = repo_url or f"https://github.com/{repo_name}.git"
        if not _clone_bare(url, cache_dir):
            return None
        _apply_perf_config(cache_dir)
        return cache_dir


def _ensure_commit_present(cache_dir: Path, commit: str) -> bool:
    tree_ref = f"{commit}^{{tree}}"
    r = run_cmd(["git", "-C", str(cache_dir), "cat-file", "-e", tree_ref], check=False, timeout=30)
    if r.returncode == 0:
        return True
    with _cache_lock(cache_dir):
        r = run_cmd(["git", "-C", str(cache_dir), "cat-file", "-e", tree_ref], check=False, timeout=30)
        if r.returncode == 0:
            return True
        run_cmd(
            ["git", "-C", str(cache_dir), "fetch", "--quiet", "--depth=1", "origin", commit],
            check=False,
            timeout=600,
        )
        r = run_cmd(["git", "-C", str(cache_dir), "cat-file", "-e", tree_ref], check=False, timeout=30)
        return r.returncode == 0


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


def _try_worktree_add(
    cache_dir: Path, repo_dir: Path, base_commit: str, checkout_timeout: int
) -> subprocess.CompletedProcess[str]:
    run_cmd(["git", "-C", str(cache_dir), "worktree", "prune", "--expire=now"], check=False, timeout=30)
    return run_cmd(
        ["git", "-C", str(cache_dir), "worktree", "add", "--detach", "--force", str(repo_dir), base_commit],
        check=False,
        timeout=checkout_timeout,
    )


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

    # Unconditional cleanup of orphan worktrees from previously killed processes.
    # No-op on a clean cache; rescues worktree/<name>/locked after SIGKILL/OOM.
    subprocess.run(
        ["git", "-C", str(cache_dir), "worktree", "prune", "--expire=now"],
        capture_output=True,
        timeout=30,
    )

    if not _ensure_commit_present(cache_dir, base_commit):
        print(f"  WORKTREE/CHECKOUT FAIL {base_commit[:12]}: unreachable_commit (not in cache and fetch failed)", flush=True)
        return None

    # Per-worker isolated worktree path. `target_dir` is already per-worker
    # (UUID-keyed via `worker_dir`), so reusing the deterministic path inside
    # it across instances of the same repo is safe and saves a worktree add.
    # If reuse fails, fall through to a fresh UUID-suffixed path so we never
    # collide with a stale `worktrees/` registration in the shared bare cache.
    # `git worktree add` against that bare cache is serialized by git's own
    # `.git/worktrees.lock`; no userspace mutex needed.
    repo_dir = target_dir / repo_name.replace("/", "__")
    if repo_dir.exists():
        _remove_stale_locks(_git_dir_for_repo(repo_dir))
        run_cmd(["git", "-C", str(repo_dir), "clean", "-fd"], check=False, timeout=30)
        r = run_cmd(
            ["git", "-C", str(repo_dir), "checkout", "--force", base_commit],
            check=False,
            timeout=checkout_timeout,
        )
        if r.returncode == 0:
            return repo_dir
        print(f"  RESET worktree {repo_name} ({r.stderr})", flush=True)
        _purge_cache_dir(repo_dir)

    r = _try_worktree_add(cache_dir, repo_dir, base_commit, checkout_timeout)
    if r.returncode != 0:
        time.sleep(1)
        _purge_cache_dir(repo_dir)
        repo_dir = target_dir / f"{repo_name.replace('/', '__')}__{uuid.uuid4().hex[:8]}"
        r = _try_worktree_add(cache_dir, repo_dir, base_commit, checkout_timeout)
    if r.returncode != 0:
        print(f"  worktree add failed for {repo_name}, retrying via _ensure_bare_cache: {r.stderr}", flush=True)
        cache_dir = _ensure_bare_cache(repo_url, repo_name)
        if not cache_dir:
            return None
        if not _ensure_commit_present(cache_dir, base_commit):
            print(f"  WORKTREE/CHECKOUT FAIL {base_commit[:12]}: unreachable_commit after retry", flush=True)
            return None
        _purge_cache_dir(repo_dir)
        r = _try_worktree_add(cache_dir, repo_dir, base_commit, checkout_timeout)
        if r.returncode != 0:
            print(f"  WORKTREE/CHECKOUT FAIL {base_commit[:12]}: {r.stderr}", flush=True)
            return None
    _apply_perf_config(repo_dir)
    return repo_dir


def apply_as_commit(repo_dir: Path, patch_text: str, message: str = "bench") -> bool:
    """Apply ``patch_text`` as a real commit on top of HEAD.

    Two failure modes were observed in production and are now defended:

    1. **`git commit` silently fails when no `user.email` / `user.name`
       is configured.** Subprocess returncode is non-zero but the
       previous code passed `check=False`, so HEAD never advanced. The
       worktree's existing HEAD is then a `base_commit` -- often a
       merge commit from SWE-bench data -- and any downstream
       ``HEAD~1..HEAD`` query returns the merge's own diff (against an
       arbitrary first-parent), which has nothing to do with the gold
       patch. This corrupted ~53% of swebench_verified measurements.
       Fix: pass `-c user.name=... -c user.email=...` inline, and
       check the commit's exit code explicitly.

    2. **`git apply --3way` fuzzy-merges and silently produces
       wrong-content commits.** Removed -- only strict
       ``git apply --index`` is used. If the strict apply fails the
       instance is reported as unrecoverable.

    Returns False on apply error, commit error, or empty commit
    (HEAD didn't advance). Caller should flag these as
    ``status="apply_fail"`` and exclude from metrics.
    """
    with tempfile.NamedTemporaryFile(mode="w", suffix=".patch", delete=False) as f:
        f.write(patch_text)
        patch_path = f.name
    try:
        r = run_cmd(["git", "-C", str(repo_dir), "apply", "--index", patch_path], check=False)
        if r.returncode != 0:
            print(f"  APPLY FAIL: {r.stderr[:300]}")
            return False

        before = run_cmd(
            ["git", "-C", str(repo_dir), "rev-parse", "HEAD"],
            check=False,
        ).stdout.strip()

        commit_r = run_cmd(
            [
                "git",
                "-c",
                "user.name=diffctx-bench",
                "-c",
                "user.email=bench@diffctx.local",
                "-C",
                str(repo_dir),
                "commit",
                "-m",
                message,
                "--no-verify",
            ],
            check=False,
        )
        if commit_r.returncode != 0:
            print(f"  COMMIT FAIL: {commit_r.stderr[:300]}")
            return False

        after = run_cmd(
            ["git", "-C", str(repo_dir), "rev-parse", "HEAD"],
            check=False,
        ).stdout.strip()
        if before == after:
            # HEAD didn't move -- commit silently no-op'd. Treat as failure.
            print(f"  COMMIT NOOP: HEAD still at {before[:12]}")
            return False
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

    # Explicit assignment, not setdefault: parent may have inherited a
    # different value (e.g. ambient RAYON_NUM_THREADS=24 from a shell
    # profile) which would defeat the cap. With N workers each running
    # Rayon at full core count, we get N x cores threads contending for
    # the same cores — the calibration timeout breaks before the
    # algorithm finishes.
    os.environ["RAYON_NUM_THREADS"] = os.environ.get("BENCH_RAYON_THREADS", "1")
    os.environ["OMP_NUM_THREADS"] = "1"
    os.environ["MKL_NUM_THREADS"] = "1"
    os.environ["OPENBLAS_NUM_THREADS"] = "1"
    os.environ["TOKENIZERS_PARALLELISM"] = "false"

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
