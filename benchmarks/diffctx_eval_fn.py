"""Production `EvalFn` that wires diffctx into the unified runner.

This is the only file that bridges the adapter layer to the existing
benchmark machinery (repo cloning, patch application, diffctx invocation).
Tests use stub `EvalFn`s — they never reach this module.
"""

from __future__ import annotations

import functools
import os
import time
from pathlib import Path

import tiktoken

from benchmarks.adapters import BenchmarkInstance, GoldenFragment
from benchmarks.adapters.base import EvalResult
from benchmarks.adapters.evaluator import SelectionOutput, UniversalEvaluator
from benchmarks.adapters.runner import RunParams

_TOKEN_ENC = tiktoken.get_encoding("o200k_base")

_WORKER_STATE: dict = {}


def _compute_used_tokens(output: dict) -> int:
    aggregate = output.get("token_count")
    if isinstance(aggregate, int) and aggregate > 0:
        return aggregate
    total = 0
    for f in output.get("fragments", []) or []:
        per_frag = f.get("token_count")
        if isinstance(per_frag, int) and per_frag > 0:
            total += per_frag
            continue
        content = f.get("content")
        if isinstance(content, str) and content:
            total += len(_TOKEN_ENC.encode(content, disallowed_special=()))
    return total


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


def _read_diffctx_timeout_sec() -> float:
    """Read the wall-clock budget for a single diffctx invocation from
    the worker's environment. Set by the CLI orchestrators before pool
    spawn; absent or <=0 disables the timer (git/clone/setup operations
    around the diffctx call are intentionally NOT covered).
    """
    raw = os.environ.get("DIFFCTX_BENCH_TIMEOUT_SEC", "")
    try:
        return float(raw) if raw else 0.0
    except ValueError:
        return 0.0


def _arm_diffctx_kill_switch(timeout_s: float):
    """Arm a daemon timer that calls `os._exit(137)` after `timeout_s`
    elapse. Used to bound the wall-clock cost of ONE diffctx call (heavy
    scoring + selection); the surrounding `ensure_repo` / `apply_as_commit`
    / `reset_to_parent` git operations are deliberately uninstrumented
    because they are benchmark scaffolding, not part of the algorithm
    under measurement.
    """
    import threading

    if timeout_s <= 0:
        # Trace once per worker so we know whether the env propagated.
        if not getattr(_arm_diffctx_kill_switch, "_warned_no_timeout", False):
            print(
                f"[worker pid={os.getpid()}] WARN: DIFFCTX_BENCH_TIMEOUT_SEC unset "
                f"or <=0, timer disabled (raw value: {os.environ.get('DIFFCTX_BENCH_TIMEOUT_SEC', 'MISSING')!r})",
                flush=True,
            )
            _arm_diffctx_kill_switch._warned_no_timeout = True  # type: ignore[attr-defined]
        return None
    timer = threading.Timer(timeout_s, lambda: os._exit(137))
    timer.daemon = True
    timer.start()
    return timer


def _ensure_worker_state(repos_dir_str: str) -> tuple[Path, UniversalEvaluator]:
    state = _WORKER_STATE.get(repos_dir_str)
    if state is None:
        runner = os.environ.get("RUNNER_NAME", "").replace("-", "_").replace(" ", "_")
        uid = f"{runner}_{os.getpid()}" if runner else str(os.getpid())
        worktree_dir = Path(repos_dir_str) / "worktrees" / f"w{uid}"
        worktree_dir.mkdir(parents=True, exist_ok=True)
        evaluator = UniversalEvaluator()
        state = (worktree_dir, evaluator)
        _WORKER_STATE[repos_dir_str] = state
    return state


def _pool_eval(repos_dir_str: str, instance: BenchmarkInstance, params: RunParams) -> EvalResult:
    from benchmarks.common import apply_as_commit, ensure_repo, reset_to_parent

    worktree_dir, evaluator = _ensure_worker_state(repos_dir_str)

    repo_url = str(instance.extra.get("repo_url") or f"https://github.com/{instance.repo}")
    repo_dir = ensure_repo(repo_url, instance.repo, instance.base_commit, worktree_dir)
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

        # Kill switch covers ONLY the diffctx call. Git ops above
        # (ensure_repo, apply_as_commit) and reset_to_parent below run
        # uninstrumented — `git worktree add` on huge repos (vscode,
        # mui) takes 30-60s of pure filesystem I/O which is benchmark
        # scaffolding, not the algorithm under measurement.
        timer = _arm_diffctx_kill_switch(_read_diffctx_timeout_sec())
        try:
            output = build_diff_context(
                repo_dir,
                "HEAD~1..HEAD",
                budget_tokens=params.budget,
                scoring_mode=params.scoring,
                tau=params.tau,
            )
        finally:
            if timer is not None:
                timer.cancel()
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
        used_tokens = _compute_used_tokens(output)
        selection = SelectionOutput(
            selected_files=_selected_files(fragments),
            selected_fragments=fragments,
            used_tokens=used_tokens,
            elapsed_seconds=elapsed,
        )
        result = evaluator.evaluate(instance, selection, budget=params.budget)
        result.used_tokens = used_tokens
        result.extra["status"] = "ok"
        result.extra["language"] = instance.language
        result.extra["fragment_count"] = len(fragments)
        latency = output.get("latency") or {}
        if latency:
            result.extra["latency_total_ms"] = latency.get("total_ms")
            result.extra["latency_breakdown"] = {k: v for k, v in latency.items() if k != "total_ms"}
        return result
    finally:
        for k, v in prior_env.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v
        try:
            reset_to_parent(repo_dir)
        except Exception:
            pass


def make_diffctx_eval_fn(repos_dir: Path):
    return functools.partial(_pool_eval, str(repos_dir))


def _build_eval_result_from_output(
    output: dict,
    instance: BenchmarkInstance,
    params: RunParams,
    elapsed: float,
    evaluator: UniversalEvaluator,
) -> EvalResult:
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
    used_tokens = _compute_used_tokens(output)
    selection = SelectionOutput(
        selected_files=_selected_files(fragments),
        selected_fragments=fragments,
        used_tokens=used_tokens,
        elapsed_seconds=elapsed,
    )
    result = evaluator.evaluate(instance, selection, budget=params.budget)
    result.used_tokens = used_tokens
    result.extra["status"] = "ok"
    result.extra["language"] = instance.language
    result.extra["fragment_count"] = len(fragments)
    latency = output.get("latency") or {}
    if latency:
        result.extra["latency_total_ms"] = latency.get("total_ms")
        result.extra["latency_breakdown"] = {k: v for k, v in latency.items() if k != "total_ms"}
    return result


def _apply_params_env(params: RunParams) -> dict[str, str | None]:
    prior = {k: os.environ.get(k) for k in params.to_env()}
    for k, v in params.to_env().items():
        os.environ[k] = v
    return prior


def _restore_params_env(prior: dict[str, str | None]) -> None:
    for k, v in prior.items():
        if v is None:
            os.environ.pop(k, None)
        else:
            os.environ[k] = v


def pool_eval_all_cells(
    repos_dir_str: str,
    instance: BenchmarkInstance,
    params_list: list[RunParams],
) -> list[tuple[RunParams, EvalResult]]:
    from benchmarks.common import apply_as_commit, ensure_repo, reset_to_parent
    from treemapper.diffctx.pipeline import compute_scored_state, select_with_params

    if not params_list:
        return []

    worktree_dir, evaluator = _ensure_worker_state(repos_dir_str)

    repo_url = str(instance.extra.get("repo_url") or f"https://github.com/{instance.repo}")
    repo_dir = ensure_repo(repo_url, instance.repo, instance.base_commit, worktree_dir)
    if repo_dir is None:
        return [(p, _failure_result(instance, p, "clone_fail", "ensure_repo returned None")) for p in params_list]

    scoring_mode = params_list[0].scoring
    out: list[tuple[RunParams, EvalResult]] = []
    try:
        apply_as_commit(repo_dir, instance.gold_patch, "diffctx-eval-gold")

        t_heavy_start = time.perf_counter()
        bench_timeout = _read_diffctx_timeout_sec()
        timer = _arm_diffctx_kill_switch(bench_timeout)
        try:
            state = compute_scored_state(repo_dir, "HEAD~1..HEAD", scoring_mode=scoring_mode)
        except Exception as e:
            err = f"{type(e).__name__}: {e}"
            return [(p, _failure_result(instance, p, "diffctx_fail", err)) for p in params_list]
        finally:
            if timer is not None:
                timer.cancel()
        heavy_elapsed = time.perf_counter() - t_heavy_start

        for params in params_list:
            prior_env = _apply_params_env(params)
            try:
                t_select_start = time.perf_counter()
                cell_timer = _arm_diffctx_kill_switch(bench_timeout)
                try:
                    output = select_with_params(state, budget_tokens=params.budget, tau=params.tau)
                finally:
                    if cell_timer is not None:
                        cell_timer.cancel()
                select_elapsed = time.perf_counter() - t_select_start
                charged = heavy_elapsed + select_elapsed if not out else select_elapsed
                result = _build_eval_result_from_output(output, instance, params, charged, evaluator)
                out.append((params, result))
            finally:
                _restore_params_env(prior_env)
    finally:
        try:
            reset_to_parent(repo_dir)
        except Exception:
            pass

    return out


def _failure_result(
    instance: BenchmarkInstance,
    params: RunParams,
    status: str,
    error: str,
) -> EvalResult:
    r = EvalResult(
        instance_id=instance.instance_id,
        source_benchmark=instance.source_benchmark,
        file_recall=0.0,
        file_precision=0.0,
        budget=params.budget,
    )
    r.extra["status"] = status
    r.extra["error"] = error
    r.extra["language"] = instance.language
    return r


def make_diffctx_eval_all_cells_fn(repos_dir: Path):
    """Sibling of `make_diffctx_eval_fn` for the inverted orchestrator
    (one task = one instance times N cells)."""
    return functools.partial(pool_eval_all_cells, str(repos_dir))
