"""BM25 file-level baseline.

Strawman that mirrors the SWE-bench paper's BM25 retrieval (Jimenez et al.
2024, Table 4): tokenize all repo files, query with identifiers extracted
from the gold patch, return top-ranked files until the token budget is
exhausted.

This is paper-blocking — without it, "diffctx beats BM25 by Δ at the same
budget" has no number to compare against.
"""

from __future__ import annotations

import os
import time
from pathlib import Path

import tiktoken

from benchmarks.adapters.base import BenchmarkInstance, EvalResult
from benchmarks.adapters.evaluator import SelectionOutput, UniversalEvaluator
from benchmarks.adapters.runner import RunParams
from benchmarks.baselines._idents import code_tokenize, extract_idents_from_patch, is_skippable_path


def _walk_repo_files(repo_dir: Path) -> list[Path]:
    out: list[Path] = []
    for root, dirs, files in os.walk(repo_dir):
        dirs[:] = [
            d for d in dirs if d not in {".git", "node_modules", ".venv", "venv", "__pycache__", "target", "dist", "build"}
        ]
        for name in files:
            full = Path(root) / name
            rel = full.relative_to(repo_dir).as_posix()
            if is_skippable_path(rel, full):
                continue
            out.append(full)
    return out


def make_bm25_eval_fn(repos_dir: Path):
    """Return an `EvalFn` that runs BM25 file-level retrieval against the
    repo at `instance.base_commit + gold_patch`.
    """
    from benchmarks.common import apply_as_commit, ensure_repo, reset_to_parent

    try:
        from rank_bm25 import BM25Okapi
    except ImportError as e:
        raise RuntimeError(
            "rank-bm25 not installed; expected to be in requirements-bench.lock. " "Run: pip install rank-bm25"
        ) from e

    evaluator = UniversalEvaluator()
    worktree_dir = repos_dir / "worktrees"
    worktree_dir.mkdir(parents=True, exist_ok=True)
    encoder = tiktoken.get_encoding("o200k_base")

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
            apply_as_commit(repo_dir, instance.gold_patch, "bm25-baseline-gold")
            t0 = time.perf_counter()

            query_tokens = sorted(extract_idents_from_patch(instance.gold_patch))
            if not query_tokens:
                # Empty query → degenerate; emit empty selection.
                selection = SelectionOutput(
                    selected_files=frozenset(),
                    selected_fragments=None,
                    used_tokens=0,
                    elapsed_seconds=time.perf_counter() - t0,
                )
                result = evaluator.evaluate(instance, selection, budget=params.budget)
                result.extra["status"] = "empty_query"
                result.extra["language"] = instance.language
                return result

            file_paths = _walk_repo_files(repo_dir)
            corpus_tokens: list[list[str]] = []
            file_token_counts: list[int] = []
            valid_files: list[str] = []
            for full in file_paths:
                try:
                    text = full.read_text(encoding="utf-8", errors="replace")
                except OSError:
                    continue
                toks = code_tokenize(text)
                if not toks:
                    continue
                corpus_tokens.append(toks)
                file_token_counts.append(len(encoder.encode(text, disallowed_special=())))
                valid_files.append(full.relative_to(repo_dir).as_posix())

            if not corpus_tokens:
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

            bm25 = BM25Okapi(corpus_tokens)
            scores = bm25.get_scores(query_tokens)
            ranked = sorted(range(len(valid_files)), key=lambda i: scores[i], reverse=True)

            # Strict greedy budget pack: walk rank order, skip files that
            # would push past `budget`. Matches the SWE-bench paper's BM25
            # protocol — recall at fixed budget is the headline metric, so
            # exceeding budget is forbidden.
            selected: list[str] = []
            used = 0
            for i in ranked:
                if scores[i] <= 0:
                    break
                cost = file_token_counts[i]
                if cost <= 0:
                    continue
                if used + cost > params.budget:
                    continue
                selected.append(valid_files[i])
                used += cost
                if used >= params.budget:
                    break

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
            result.extra["baseline"] = "bm25"
            result.extra["query_terms"] = len(query_tokens)
            return result
        finally:
            try:
                reset_to_parent(repo_dir)
            except Exception:
                pass

    return eval_fn
