"""Seeded-random file-packing floor baseline.

Identical protocol to the BM25 baseline — same candidate universe
(`_walk_repo_files`), same o200k per-file token accounting, same
greedy-with-skip budget packing — with the score vector replaced by a
deterministic per-instance random permutation. The comparison against BM25
therefore isolates ranking quality: everything except the scores is shared
code.
"""

from __future__ import annotations

import functools
import os
import random
import time
import zlib
from pathlib import Path

import tiktoken

from eval.baselines.bm25_baseline import _build_bm25_corpus, _greedy_budget_pack, _walk_repo_files
from eval.harness.adapters.base import BenchmarkInstance, EvalResult
from eval.harness.adapters.evaluator import SelectionOutput, UniversalEvaluator
from eval.harness.adapters.runner import RunParams
from eval.harness.common import apply_as_commit, ensure_repo, reset_to_parent

_BASE_SEED = int(os.environ.get("DIFFCTX_RANDOM_BASELINE_SEED", "42"))


def _instance_rng(instance_id: str) -> random.Random:
    return random.Random(_BASE_SEED ^ zlib.crc32(instance_id.encode("utf-8")))


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


def _random_eval(
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
        applied = apply_as_commit(repo_dir, instance.gold_patch, "random-baseline-gold")
        if not applied:
            return _fail_result(instance, params.budget, "apply_fail", "gold patch did not apply as commit")
        t0 = time.perf_counter()

        _, file_token_counts, valid_files = _build_bm25_corpus(_walk_repo_files(repo_dir), repo_dir, encoder)
        if not valid_files:
            selection = SelectionOutput(
                selected_files=frozenset(),
                selected_fragments=None,
                used_tokens=0,
                elapsed_seconds=time.perf_counter() - t0,
            )
            result = evaluator.evaluate(instance, selection, budget=params.budget)
            result.extra["status"] = "empty_corpus"
            result.extra["language"] = instance.language
            return result

        rng = _instance_rng(instance.instance_id)
        scores = [rng.random() for _ in valid_files]
        ranked = sorted(range(len(valid_files)), key=lambda i: scores[i], reverse=True)
        selected, used = _greedy_budget_pack(ranked, scores, file_token_counts, valid_files, params.budget)

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
        result.extra["baseline"] = "random"
        result.extra["seed"] = _BASE_SEED
        return result
    finally:
        if applied:
            try:
                reset_to_parent(repo_dir)
            except Exception:
                pass


def _pool_eval_random(repos_dir_str: str, instance: BenchmarkInstance, params: RunParams) -> EvalResult:
    repos_dir = Path(repos_dir_str)
    worktree_dir = repos_dir / "worktrees" / f"w{os.getpid()}"
    worktree_dir.mkdir(parents=True, exist_ok=True)
    evaluator = UniversalEvaluator()
    encoder = tiktoken.get_encoding("o200k_base")
    return _random_eval(instance, params, evaluator, worktree_dir, encoder)


def make_random_eval_fn(repos_dir: Path):
    return functools.partial(_pool_eval_random, str(repos_dir))
