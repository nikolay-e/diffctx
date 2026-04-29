"""Production `EvalFn` that wires diffctx into the unified runner.

This is the only file that bridges the adapter layer to the existing
benchmark machinery (repo cloning, patch application, diffctx invocation).
Tests use stub `EvalFn`s — they never reach this module.
"""

from __future__ import annotations

import os
import time
from pathlib import Path

from benchmarks.adapters import BenchmarkInstance, GoldenFragment
from benchmarks.adapters.base import EvalResult
from benchmarks.adapters.evaluator import SelectionOutput, UniversalEvaluator
from benchmarks.adapters.runner import RunParams


def _output_fragments(output: dict) -> tuple[GoldenFragment, ...]:
    from benchmarks.common import parse_lines_field

    out: list[GoldenFragment] = []
    for f in output.get("fragments", []) or []:
        path = f.get("path") or f.get("file")
        lines = f.get("lines", "")
        rng = parse_lines_field(lines) if isinstance(lines, str) else None
        if path is None:
            continue
        if rng is None:
            out.append(GoldenFragment(path=str(path), kind="file"))
        else:
            out.append(GoldenFragment(path=str(path), start_line=rng[0], end_line=rng[1], kind="hunk"))
    return tuple(out)


def _selected_files(fragments: tuple[GoldenFragment, ...]) -> frozenset[str]:
    return frozenset(f.path for f in fragments)


def make_diffctx_eval_fn(repos_dir: Path):
    """Return an `EvalFn` that clones the repo, applies the gold patch as a
    commit, runs diffctx with `params.to_env()` set, evaluates against the
    instance's gold context, and reverts.

    The repo cache is shared across calls; long-running processes should
    reuse the same `eval_fn` to amortize clone cost.
    """
    from benchmarks.common import apply_as_commit, ensure_repo, reset_to_parent

    evaluator = UniversalEvaluator()

    def eval_fn(instance: BenchmarkInstance, params: RunParams) -> EvalResult:
        repo_url = str(instance.extra.get("repo_url") or f"https://github.com/{instance.repo}")
        repo_dir = ensure_repo(repo_url, instance.repo, instance.base_commit, repos_dir)
        if repo_dir is None:
            result = EvalResult(
                instance_id=instance.instance_id,
                source_benchmark=instance.source_benchmark,
                file_recall=0.0,
                file_precision=0.0,
                budget=params.budget,
            )
            result.extra["status"] = "clone_fail"
            return result

        prior_env = {k: os.environ.get(k) for k in params.to_env()}
        try:
            for k, v in params.to_env().items():
                os.environ[k] = v
            apply_as_commit(repo_dir, instance.gold_patch, "diffctx-eval-gold")
            t0 = time.perf_counter()
            from treemapper.diffctx.pipeline import build_diff_context

            output = build_diff_context(
                repo_dir,
                "HEAD~1..HEAD",
                budget_tokens=params.budget,
                scoring_mode=params.scoring,
                tau=params.tau,
            )
            elapsed = time.perf_counter() - t0
            if output is None:
                result = EvalResult(
                    instance_id=instance.instance_id,
                    source_benchmark=instance.source_benchmark,
                    file_recall=0.0,
                    file_precision=0.0,
                    budget=params.budget,
                    elapsed_seconds=elapsed,
                )
                result.extra["status"] = "diffctx_fail"
                return result
            fragments = _output_fragments(output)
            selection = SelectionOutput(
                selected_files=_selected_files(fragments),
                selected_fragments=fragments,
                used_tokens=int(output.get("token_count", 0) or 0),
                elapsed_seconds=elapsed,
            )
            result = evaluator.evaluate(instance, selection, budget=params.budget)
            result.extra["status"] = "ok"
            result.extra["language"] = instance.language
            return result
        finally:
            for k, v in prior_env.items():
                if v is None:
                    os.environ.pop(k, None)
                else:
                    os.environ[k] = v
            try:
                reset_to_parent(repo_dir)
            except Exception:  # pragma: no cover — repo state restoration best-effort
                pass

    return eval_fn
