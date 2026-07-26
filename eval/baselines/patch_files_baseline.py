"""Changed-files-only floor baseline.

Returns exactly the files touched by the gold patch (post-apply paths),
packed smallest-first under the token budget. Zero retrieval, zero graph:
this is the recall floor any method that keeps the diff's own files must
clear, and the |gold ∩ changed| lower bound for context-beyond-diff claims.
"""

from __future__ import annotations

import functools
import os
import time
from pathlib import Path

import tiktoken

from eval.harness.adapters.base import BenchmarkInstance, EvalResult
from eval.harness.adapters.evaluator import SelectionOutput, UniversalEvaluator
from eval.harness.adapters.runner import RunParams
from eval.harness.common import apply_as_commit, ensure_repo, patch_files_at_head, reset_to_parent


def _fail_result(instance: BenchmarkInstance, budget: int, status: str, error: str | None = None) -> EvalResult:
    r = EvalResult(
        instance_id=instance.instance_id,
        source_benchmark=instance.source_benchmark,
        file_recall=0.0,
        file_precision=0.0,
        budget=budget,
    )
    r.extra["status"] = status
    if error:
        r.extra["error"] = error
    r.extra["language"] = instance.language
    return r


def _patch_files_eval(
    instance: BenchmarkInstance,
    params: RunParams,
    evaluator: UniversalEvaluator,
    worktree_dir: Path,
    encoder: tiktoken.Encoding,
) -> EvalResult:
    repo_url = str(instance.extra.get("repo_url") or f"https://github.com/{instance.repo}")
    repo_dir = ensure_repo(repo_url, instance.repo, instance.base_commit, worktree_dir)
    if repo_dir is None:
        return _fail_result(instance, params.budget, "clone_fail")

    applied = False
    try:
        applied = apply_as_commit(repo_dir, instance.gold_patch, "patch-files-baseline-gold")
        if not applied:
            return _fail_result(instance, params.budget, "apply_fail", "gold patch did not apply as commit")
        t0 = time.perf_counter()

        candidates: list[tuple[int, str]] = []
        for rel in sorted(patch_files_at_head(instance.gold_patch)):
            full = repo_dir / rel
            try:
                text = full.read_text(encoding="utf-8", errors="replace")
            except OSError:
                continue
            candidates.append((len(encoder.encode(text, disallowed_special=())), rel))

        candidates.sort()
        selected: list[str] = []
        used = 0
        for cost, rel in candidates:
            if cost <= 0 or used + cost > params.budget:
                continue
            selected.append(rel)
            used += cost

        elapsed = time.perf_counter() - t0
        selection = SelectionOutput(
            selected_files=frozenset(selected),
            selected_fragments=None,
            used_tokens=used,
            elapsed_seconds=elapsed,
        )
        result = evaluator.evaluate(instance, selection, budget=params.budget)
        result.used_tokens = used
        result.elapsed_seconds = elapsed
        result.extra["status"] = "ok"
        result.extra["language"] = instance.language
        result.extra["baseline"] = "patch_files"
        result.extra["n_changed_candidates"] = len(candidates)
        return result
    finally:
        if applied:
            try:
                reset_to_parent(repo_dir)
            except Exception:
                pass


def _pool_eval_patch_files(repos_dir_str: str, instance: BenchmarkInstance, params: RunParams) -> EvalResult:
    repos_dir = Path(repos_dir_str)
    worktree_dir = repos_dir / "worktrees" / f"w{os.getpid()}"
    worktree_dir.mkdir(parents=True, exist_ok=True)
    evaluator = UniversalEvaluator()
    encoder = tiktoken.get_encoding("o200k_base")
    return _patch_files_eval(instance, params, evaluator, worktree_dir, encoder)


def make_patch_files_eval_fn(repos_dir: Path):
    return functools.partial(_pool_eval_patch_files, str(repos_dir))
