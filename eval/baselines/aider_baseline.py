"""Aider repo-map baseline.

Runs Aider's `RepoMap.get_repo_map` against the same repo+patch+budget
inputs as diffctx, via a subprocess in an isolated `uv tool` venv (Aider
hard-pins ~95 deps including litellm, numpy==1.26.4, fastapi — those would
break the main diffctx env if installed in-process).

Spawn-once, reuse-many: one helper process per worker process is kept alive
across all instances assigned to that worker, so we pay aider's import
cost (~1-2s) once per worker, not once per instance.
"""

from __future__ import annotations

import functools
import json
import os
import shutil
import subprocess
import tempfile
import time
from pathlib import Path
from typing import Any

from eval.baselines._idents import extract_idents_from_patch, is_skippable_path
from eval.harness.adapters.base import BenchmarkInstance, EvalResult
from eval.harness.adapters.evaluator import SelectionOutput, UniversalEvaluator
from eval.harness.adapters.runner import RunParams

_RUNNER = Path(__file__).with_name("aider_subprocess.py")
_AIDER_VERSION = "aider-chat==0.86.2"


def _walk_other_files(repo_dir: Path) -> list[str]:
    out: list[str] = []
    for root, dirs, files in os.walk(repo_dir):
        dirs[:] = [
            d for d in dirs if d not in {".git", "node_modules", ".venv", "venv", "__pycache__", "target", "dist", "build"}
        ]
        for name in files:
            full = Path(root) / name
            rel = full.relative_to(repo_dir).as_posix()
            if is_skippable_path(rel, full):
                continue
            out.append(str(full))
    return out


class _AiderProcess:
    """Long-lived subprocess wrapper with NDJSON IPC."""

    def __init__(self) -> None:
        if shutil.which("uv") is None:
            raise RuntimeError("`uv` not found on PATH; install uv to run the Aider baseline")
        self._proc: subprocess.Popen | None = None

    def start(self) -> None:
        cmd = [
            "uv",
            "tool",
            "run",
            "--from",
            _AIDER_VERSION,
            "--with",
            "tiktoken",
            "python",
            str(_RUNNER),
        ]
        # stderr goes to a file, never a PIPE: the request loop only drains
        # stdout, so a chatty helper (oracle mode floods warnings per gold
        # path) fills a 64KB stderr pipe and deadlocks on write().
        self._stderr_log = tempfile.NamedTemporaryFile(mode="w+", prefix="aider_helper_", suffix=".stderr", delete=False)
        self._proc = subprocess.Popen(
            cmd,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=self._stderr_log,
            text=True,
            bufsize=1,
        )
        ready = self._proc.stdout.readline().strip()  # type: ignore[union-attr]
        if not ready or json.loads(ready).get("ready") is not True:
            self._stderr_log.flush()
            self._stderr_log.seek(0)
            err = self._stderr_log.read()
            raise RuntimeError(f"Aider helper did not signal ready: {err[:500]}")

    def request(self, payload: dict, timeout: float) -> dict:
        if self._proc is None or self._proc.poll() is not None:
            self.start()
        assert self._proc and self._proc.stdin and self._proc.stdout
        self._proc.stdin.write(json.dumps(payload) + "\n")
        self._proc.stdin.flush()
        # NDJSON: read exactly one line. Timeout via select.
        import select

        ready, _, _ = select.select([self._proc.stdout], [], [], timeout)
        if not ready:
            raise TimeoutError(f"Aider helper did not respond within {timeout}s")
        line = self._proc.stdout.readline().strip()
        if not line:
            raise RuntimeError("Aider helper closed stdout (probably crashed)")
        return json.loads(line)

    def shutdown(self) -> None:
        if self._proc is not None:
            try:
                self._proc.stdin.write(json.dumps({"op": "shutdown"}) + "\n")  # type: ignore[union-attr]
                self._proc.stdin.flush()  # type: ignore[union-attr]
            except Exception:
                pass
            try:
                self._proc.terminate()
                self._proc.wait(timeout=5)
            except Exception:
                self._proc.kill()
            self._proc = None
        stderr_log = getattr(self, "_stderr_log", None)
        if stderr_log is not None:
            try:
                stderr_log.close()
            except Exception:
                pass
            self._stderr_log = None


_DIFF_PREFIXES = ("--- a/", "+++ b/", "diff --git a/", "rename from ", "rename to ")


def _parse_diff_line_paths(line: str, prefix: str) -> set[str]:
    tail = line[len(prefix) :].strip()
    if prefix == "diff --git a/":
        parts = tail.split(" b/", 1)
        return {parts[0].strip(), parts[1].strip()} if len(parts) == 2 else set()
    if prefix in ("--- a/", "+++ b/"):
        return {tail} if tail and tail not in {"/dev/null"} else set()
    return {tail} if tail else set()


def _patch_visible_paths(patch: str) -> set[str]:
    out: set[str] = set()
    for line in patch.splitlines():
        for prefix in _DIFF_PREFIXES:
            if line.startswith(prefix):
                out.update(_parse_diff_line_paths(line, prefix))
    return out


def _aider_failure(
    instance: BenchmarkInstance,
    params: RunParams,
    status: str,
    error: str | None = None,
    elapsed_seconds: float = 0.0,
) -> EvalResult:
    r = EvalResult(
        instance_id=instance.instance_id,
        source_benchmark=instance.source_benchmark,
        file_recall=0.0,
        file_precision=0.0,
        budget=params.budget,
        elapsed_seconds=elapsed_seconds,
    )
    r.extra["status"] = status
    if error is not None:
        r.extra["error"] = error
    r.extra["language"] = instance.language
    return r


def _build_aider_payload(instance: BenchmarkInstance, params: RunParams, repo_dir: Path, aider_mode: str) -> dict[str, Any]:
    if aider_mode == "oracle":
        mentioned_fnames = sorted(instance.gold_files)
    else:
        mentioned_fnames = sorted(_patch_visible_paths(instance.gold_patch))
    return {
        "repo_root": str(repo_dir),
        "chat_files": [],
        "other_files": _walk_other_files(repo_dir),
        "mentioned_fnames": mentioned_fnames,
        "mentioned_idents": sorted(extract_idents_from_patch(instance.gold_patch)),
        "map_tokens": params.budget,
    }


def _aider_eval(
    instance: BenchmarkInstance,
    params: RunParams,
    evaluator: UniversalEvaluator,
    worktree_dir: Path,
    aider: _AiderProcess,
    request_timeout: float,
    aider_mode: str,
) -> EvalResult:
    from eval.harness.common import apply_gold_patch, ensure_repo, reset_to_parent

    repo_url = str(instance.extra.get("repo_url") or f"https://github.com/{instance.repo}")
    repo_dir = ensure_repo(repo_url, instance.repo, instance.base_commit, worktree_dir)
    if repo_dir is None:
        return _aider_failure(instance, params, "clone_fail")

    applied = False
    try:
        apply_outcome = apply_gold_patch(repo_dir, instance.gold_patch, "aider-baseline-gold")
        applied = apply_outcome.applied
        if not applied:
            return _aider_failure(instance, params, "apply_fail", "gold patch did not apply as commit")
        t0 = time.perf_counter()
        payload = _build_aider_payload(instance, params, repo_dir, aider_mode)
        try:
            resp = aider.request(payload, timeout=request_timeout)
        except (TimeoutError, RuntimeError) as e:
            status = "aider_timeout" if isinstance(e, TimeoutError) else "aider_crash"
            aider.shutdown()
            return _aider_failure(instance, params, status, str(e), time.perf_counter() - t0)

        elapsed = time.perf_counter() - t0
        if not resp.get("ok"):
            return _aider_failure(instance, params, "aider_error", (resp.get("error") or "")[:500], elapsed)

        abs_root = str(repo_dir) + os.sep
        selected: list[str] = []
        for f in resp.get("files", []):
            if f.startswith(abs_root):
                selected.append(f[len(abs_root) :])
            else:
                selected.append(f)

        selection = SelectionOutput(
            selected_files=frozenset(selected),
            selected_fragments=None,
            used_tokens=0,
            elapsed_seconds=elapsed,
        )
        result = evaluator.evaluate(instance, selection, budget=params.budget)
        result.elapsed_seconds = elapsed
        result.extra["status"] = "ok"
        result.extra["language"] = instance.language
        result.extra["baseline"] = "aider"
        result.extra["map_chars"] = len(resp.get("map_text", ""))
        result.extra["apply_mode"] = apply_outcome.mode
        return result
    finally:
        if applied:
            try:
                reset_to_parent(repo_dir)
            except Exception:
                pass


_AIDER_PROC: _AiderProcess | None = None


def _noop_shutdown() -> None:
    # Satisfies the eval_fn.shutdown() contract; this baseline holds no
    # per-run resources to release (worker pool state is process-global).
    pass


def _pool_eval_aider(
    repos_dir_str: str,
    request_timeout: float,
    aider_mode: str,
    instance: BenchmarkInstance,
    params: RunParams,
) -> EvalResult:
    global _AIDER_PROC
    if _AIDER_PROC is None:
        _AIDER_PROC = _AiderProcess()
    evaluator = UniversalEvaluator()
    worktree_dir = Path(repos_dir_str) / "worktrees" / f"w{os.getpid()}"
    worktree_dir.mkdir(parents=True, exist_ok=True)
    return _aider_eval(instance, params, evaluator, worktree_dir, _AIDER_PROC, request_timeout, aider_mode)


def make_aider_eval_fn(
    repos_dir: Path,
    request_timeout: float = 300.0,
    aider_mode: str = "fair",
):
    if aider_mode not in {"fair", "oracle"}:
        raise ValueError(f"aider_mode must be 'fair' or 'oracle', got {aider_mode!r}")
    fn = functools.partial(_pool_eval_aider, str(repos_dir), request_timeout, aider_mode)
    fn.shutdown = _noop_shutdown  # type: ignore[attr-defined]
    return fn
