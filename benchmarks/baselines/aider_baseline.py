"""Aider repo-map baseline.

Runs Aider's `RepoMap.get_repo_map` against the same repo+patch+budget
inputs as diffctx, via a subprocess in an isolated `uv tool` venv (Aider
hard-pins ~95 deps including litellm, numpy==1.26.4, fastapi — those would
break the main treemapper env if installed in-process).

Spawn-once, reuse-many: a single helper process is kept alive across all
instances in a run, so we pay aider's import cost (~1-2s) once per
benchmark file, not 1500 times.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import time
from pathlib import Path
from typing import Any

from benchmarks.adapters.base import BenchmarkInstance, EvalResult
from benchmarks.adapters.evaluator import SelectionOutput, UniversalEvaluator
from benchmarks.adapters.runner import RunParams
from benchmarks.baselines._idents import extract_idents_from_patch, is_skippable_path

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
        self._proc = subprocess.Popen(
            cmd,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
        )
        ready = self._proc.stdout.readline().strip()  # type: ignore[union-attr]
        if not ready or json.loads(ready).get("ready") is not True:
            err = self._proc.stderr.read() if self._proc.stderr else ""  # type: ignore[union-attr]
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


def make_aider_eval_fn(repos_dir: Path, request_timeout: float = 300.0):
    from benchmarks.common import apply_as_commit, ensure_repo, reset_to_parent

    evaluator = UniversalEvaluator()
    worktree_dir = repos_dir / "worktrees"
    worktree_dir.mkdir(parents=True, exist_ok=True)

    aider = _AiderProcess()

    def eval_fn(instance: BenchmarkInstance, params: RunParams) -> EvalResult:
        repo_url = str(instance.extra.get("repo_url") or f"https://github.com/{instance.repo}")
        repo_dir = ensure_repo(repo_url, instance.repo, instance.base_commit, worktree_dir)
        if repo_dir is None:
            r = EvalResult(
                instance_id=instance.instance_id,
                source_benchmark=instance.source_benchmark,
                file_recall=0.0,
                file_precision=0.0,
                budget=params.budget,
            )
            r.extra["status"] = "clone_fail"
            r.extra["language"] = instance.language
            return r

        try:
            apply_as_commit(repo_dir, instance.gold_patch, "aider-baseline-gold")
            t0 = time.perf_counter()
            other_files = _walk_other_files(repo_dir)
            mentioned_fnames = sorted(instance.gold_files)
            mentioned_idents = sorted(extract_idents_from_patch(instance.gold_patch))

            payload: dict[str, Any] = {
                "repo_root": str(repo_dir),
                "chat_files": [],
                "other_files": other_files,
                "mentioned_fnames": mentioned_fnames,
                "mentioned_idents": mentioned_idents,
                "map_tokens": params.budget,
            }
            try:
                resp = aider.request(payload, timeout=request_timeout)
            except (TimeoutError, RuntimeError) as e:
                r = EvalResult(
                    instance_id=instance.instance_id,
                    source_benchmark=instance.source_benchmark,
                    file_recall=0.0,
                    file_precision=0.0,
                    budget=params.budget,
                    elapsed_seconds=time.perf_counter() - t0,
                )
                r.extra["status"] = "aider_timeout" if isinstance(e, TimeoutError) else "aider_crash"
                r.extra["error"] = str(e)
                r.extra["language"] = instance.language
                # Restart helper for next instance.
                aider.shutdown()
                return r

            elapsed = time.perf_counter() - t0
            if not resp.get("ok"):
                r = EvalResult(
                    instance_id=instance.instance_id,
                    source_benchmark=instance.source_benchmark,
                    file_recall=0.0,
                    file_precision=0.0,
                    budget=params.budget,
                    elapsed_seconds=elapsed,
                )
                r.extra["status"] = "aider_error"
                r.extra["error"] = (resp.get("error") or "")[:500]
                r.extra["language"] = instance.language
                return r

            # Aider returns absolute paths or repo-relative depending on input;
            # normalize back to repo-relative.
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
            return result
        finally:
            try:
                reset_to_parent(repo_dir)
            except Exception:
                pass

    eval_fn.shutdown = aider.shutdown  # type: ignore[attr-defined]
    return eval_fn
