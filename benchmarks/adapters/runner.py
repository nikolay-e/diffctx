from __future__ import annotations

import json
import signal as _signal
import time as _time
from collections.abc import Callable, Iterable, Iterator
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Any

from benchmarks.adapters.base import BenchmarkAdapter, BenchmarkInstance, EvalResult

EvalFn = Callable[[BenchmarkInstance, "RunParams"], EvalResult]
EvalAllCellsFn = Callable[[BenchmarkInstance, list["RunParams"]], list[tuple["RunParams", EvalResult]]]


@dataclass(frozen=True)
class RunParams:
    """Parameters for one diffctx evaluation pass.

    `tau` and `core_budget_fraction` are the two calibrated knobs (validated
    by the sensitivity sweep — every other operational parameter showed
    near-zero effect). Anything else can be threaded through `extra_env`.
    """

    # Calibrated v1 (2119 instances, 4 benchmarks, pebble-fixed pool):
    # winner (tau, cbf) = (0.12, 0.5) at min(per_benchmark file_recall)
    # = 0.1092. Surface is flat (top-3 within 0.001) — robust default.
    tau: float = 0.12
    core_budget_fraction: float = 0.5
    budget: int = 8000
    scoring: str = "ego"
    extra_env: dict[str, str] = field(default_factory=dict)

    def to_env(self) -> dict[str, str]:
        env = dict(self.extra_env)
        env["DIFFCTX_OP_SELECTION_STOPPING_THRESHOLD"] = f"{self.tau}"
        env["DIFFCTX_OP_SELECTION_CORE_BUDGET_FRACTION"] = f"{self.core_budget_fraction}"
        return env

    def label(self) -> str:
        return f"tau={self.tau:.4f}_cbf={self.core_budget_fraction:.4f}_b={self.budget}_s={self.scoring}"


def read_manifest(path: Path) -> frozenset[str]:
    """Return the set of `instance_id`s listed in a v1 manifest file."""
    return frozenset(line.strip() for line in path.read_text().splitlines() if line.strip())


def filter_instances_by_manifest(
    adapters: Iterable[BenchmarkAdapter],
    manifest_ids: frozenset[str] | set[str],
) -> Iterator[BenchmarkInstance]:
    """Stream instances whose ID is in `manifest_ids`, across all adapters.

    Adapters are walked once each; missing IDs are silently skipped (the
    caller's report should compare counts and warn).
    """
    wanted = frozenset(manifest_ids)
    for adapter in adapters:
        for inst in adapter.load():
            if inst.instance_id in wanted:
                yield inst


def read_checkpoint(path: Path) -> set[str]:
    """Return instance_ids already recorded in a JSONL checkpoint file."""
    if not path.exists():
        return set()
    done: set[str] = set()
    for line in path.read_text().splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            done.add(json.loads(line)["instance_id"])
        except (KeyError, ValueError):
            continue
    return done


def append_checkpoint(path: Path, result: EvalResult) -> None:
    """Append one result as a JSONL row. fsync after each write so a
    `os._exit(137)` kill in a sibling worker cannot leave a half-written
    last line that breaks `jq` / line iteration on resume.
    """
    import os as _os

    path.parent.mkdir(parents=True, exist_ok=True)
    line = json.dumps(asdict(result), default=str) + "\n"
    with path.open("a") as f:
        f.write(line)
        f.flush()
        _os.fsync(f.fileno())


def _load_existing_results(path: Path, allowed_ids: set[str]) -> list[EvalResult]:
    """Replay a checkpoint into in-memory `EvalResult`s so a fully-resumed
    run still contributes to per-trial aggregation."""
    if not path.exists():
        return []
    out: list[EvalResult] = []
    for line in path.read_text().splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if row.get("instance_id") not in allowed_ids:
            continue
        out.append(
            EvalResult(
                instance_id=row["instance_id"],
                source_benchmark=row.get("source_benchmark", "unknown"),
                file_recall=float(row.get("file_recall", 0.0)),
                file_precision=float(row.get("file_precision", 0.0)),
                fragment_recall=row.get("fragment_recall"),
                fragment_precision=row.get("fragment_precision"),
                line_f1=row.get("line_f1"),
                used_tokens=int(row.get("used_tokens", 0)),
                budget=int(row.get("budget", 0)),
                elapsed_seconds=float(row.get("elapsed_seconds", 0.0)),
                extra=row.get("extra", {}) or {},
            )
        )
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


def _handle_process_expired(
    inst: BenchmarkInstance,
    params: RunParams,
    e: object,
) -> EvalResult:
    ec = getattr(e, "exitcode", None)
    try:
        sig_name = _signal.Signals(-ec).name if ec is not None and ec < 0 else str(ec)
    except (ValueError, TypeError):
        sig_name = str(ec)
    if ec == -9:
        status = "oom_kill"
    elif ec is not None and ec < 0:
        status = "signal_kill"
    else:
        status = "error"
    msg = f"ProcessExpired signal={sig_name} exitcode={ec} repo={inst.repo}"
    print(f"[WARN] {inst.instance_id} {msg}", flush=True)
    return _failure_result(inst, params, status, msg)


def _log_non_ok_result(r: EvalResult, status: str, err: str) -> None:
    lang = (r.extra or {}).get("language", "")
    t_clone = (r.extra or {}).get("t_clone_s", "")
    detail = f" lang={lang}" if lang else ""
    detail += f" t_clone={t_clone}s" if t_clone else ""
    print(f"[WARN] {r.instance_id} status={status}{detail} error={err}", flush=True)


def _maybe_checkpoint(path: Path | None, r: EvalResult, status: str, err: str) -> None:
    if path is None:
        return
    # Pool-level transient failures (BrokenProcessPool) must NOT be persisted:
    # on retry the orchestrator rebuilds the pool and these instances should be
    # re-evaluated, not skipped via the resume set.
    # EXCEPTION: status=="timeout" is deterministic per-instance (the same input
    # would hit the same deadline again) and MUST be checkpointed to prevent an
    # infinite retry loop on a pathological repository.
    if status != "timeout" and "BrokenProcessPool" in err:
        return
    append_checkpoint(path, r)


def run_eval_set(
    instances: list[BenchmarkInstance],
    eval_fn: EvalFn,
    params: RunParams,
    workers: int = 1,
    timeout_per_instance: float = 20.0,
    resume_from: Path | None = None,
    checkpoint_path: Path | None = None,
    pool: object | None = None,
) -> list[EvalResult]:
    """Run `eval_fn(instance, params)` for every instance.

    - `workers > 1` uses a process pool (spawn context) so workers do
      not share the GIL; otherwise sequential.
    - `timeout_per_instance` is the wall-clock budget for ONE diffctx
      call. The actual kill switch is armed inside the eval_fn (see
      `benchmarks/diffctx_eval_fn.py`) around `build_diff_context` /
      `compute_scored_state` only — git ops (clone, worktree add,
      apply_as_commit) run uninstrumented because they are benchmark
      scaffolding, not the algorithm under measurement. The orchestrator
      passes the deadline to workers via the `DIFFCTX_BENCH_TIMEOUT_SEC`
      environment variable.
    - `resume_from` (JSONL path): instance_ids already present in that file
      are skipped — re-running after a crash continues where it left off.
    - `checkpoint_path` (JSONL path): each completed result is appended
      immediately so a crash mid-sweep loses at most one in-flight result.
    """
    done_ids: set[str] = read_checkpoint(resume_from) if resume_from else set()
    pending = [i for i in instances if i.instance_id not in done_ids]
    results: list[EvalResult] = _load_existing_results(resume_from, done_ids) if resume_from else []

    def _record(r: EvalResult) -> None:
        results.append(r)
        status = str((r.extra or {}).get("status", ""))
        err = str((r.extra or {}).get("error", ""))
        if status not in ("ok", "empty_query", "empty_corpus") and (status or err):
            _log_non_ok_result(r, status, err)
        _maybe_checkpoint(checkpoint_path, r, status, err)

    if pending:
        if workers <= 1 or len(pending) <= 1:
            _run_serial(pending, eval_fn, params, _record)
        else:
            _run_parallel(pending, eval_fn, params, workers, timeout_per_instance, _record, pool=pool)

    return results


def run_eval_set_multi_budget(
    instances: list[BenchmarkInstance],
    eval_all_cells_fn: EvalAllCellsFn,
    params_list: list[RunParams],
    workers: int = 1,
    timeout_per_instance: float = 20.0,
    resume_dir: Path | None = None,
    checkpoint_dir: Path | None = None,
) -> dict[int, list[EvalResult]]:
    """Multi-budget driver that reuses `compute_scored_state` across budgets.

    For diffctx, the heavy phase (fragment extraction, edge collection,
    graph build, scoring) is independent of the (`budget`, `tau`) cell;
    `pool_eval_all_cells` runs it once per instance and re-runs only the
    cheap selection stage for each `RunParams` in `params_list`. This
    typically converts a 7-budget sweep from 7x heavy work to 1x heavy +
    7x cheap, a 5-7x wall-clock reduction for the headline grid.

    Returns `{budget: list[EvalResult]}` keyed by `params.budget`. Per-
    budget checkpoint files live at `<checkpoint_dir>/b<budget>.jsonl`,
    matching the resume logic in the single-budget driver.
    """
    if not params_list:
        return {}
    budgets_in_order = [p.budget for p in params_list]
    ckpts: dict[int, Path | None] = {
        p.budget: ((checkpoint_dir / f"b{p.budget}.checkpoint.jsonl") if checkpoint_dir else None) for p in params_list
    }
    resume_paths: dict[int, Path | None] = {
        p.budget: ((resume_dir / f"b{p.budget}.checkpoint.jsonl") if resume_dir else None) for p in params_list
    }
    done_ids: dict[int, set[str]] = {b: (read_checkpoint(p) if p else set()) for b, p in resume_paths.items()}
    results_by_budget: dict[int, list[EvalResult]] = {
        b: (_load_existing_results(p, done_ids[b]) if p else []) for b, p in resume_paths.items()
    }

    # An instance is "pending" if any of its budgets isn't already on disk.
    pending: list[tuple[BenchmarkInstance, list[RunParams]]] = []
    for inst in instances:
        needed = [p for p in params_list if inst.instance_id not in done_ids[p.budget]]
        if needed:
            pending.append((inst, needed))

    def _record_per_cell(per_cell: list[tuple[RunParams, EvalResult]]) -> None:
        for params, r in per_cell:
            results_by_budget[params.budget].append(r)
            status = str((r.extra or {}).get("status", ""))
            err = str((r.extra or {}).get("error", ""))
            if status not in ("ok", "empty_query", "empty_corpus") and (status or err):
                _log_non_ok_result(r, status, err)
            _maybe_checkpoint(ckpts[params.budget], r, status, err)

    if not pending:
        return results_by_budget

    if workers <= 1 or len(pending) <= 1:
        _run_multi_budget_serial(pending, eval_all_cells_fn, _record_per_cell)
    else:
        _run_multi_budget_parallel(
            pending,
            eval_all_cells_fn,
            workers,
            timeout_per_instance,
            len(params_list),
            _record_per_cell,
        )

    # Re-order to match params_list submission order; defensive against
    # internal restructuring of dict insertion semantics.
    return {b: results_by_budget[b] for b in budgets_in_order}


def _run_multi_budget_serial(
    pending: list[tuple[BenchmarkInstance, list[RunParams]]],
    eval_all_cells_fn: EvalAllCellsFn,
    record: Callable[[list[tuple[RunParams, EvalResult]]], None],
) -> None:
    for inst, needed in pending:
        try:
            per_cell = eval_all_cells_fn(inst, needed)
        except Exception as e:
            per_cell = [(p, _failure_result(inst, p, "error", f"{type(e).__name__}: {e}")) for p in needed]
        record(per_cell)


def _resolve_multi_budget_future(
    future: Any,
    inst: BenchmarkInstance,
    needed: list[RunParams],
    pebble_timeout: float,
    timeout_per_instance: float,
) -> list[tuple[RunParams, EvalResult]]:
    from concurrent.futures import TimeoutError as FuturesTimeoutError

    from pebble import ProcessExpired

    try:
        return future.result()
    except FuturesTimeoutError:
        return [(p, _failure_result(inst, p, "timeout", f"pebble killed after {pebble_timeout:.0f}s")) for p in needed]
    except ProcessExpired as e:
        if e.exitcode == 137:
            return [
                (p, _failure_result(inst, p, "timeout", f"diffctx exceeded {timeout_per_instance:.0f}s budget")) for p in needed
            ]
        msg = f"ProcessExpired exitcode={getattr(e, 'exitcode', '?')}"
        return [(p, _failure_result(inst, p, "error", msg)) for p in needed]
    except Exception as e:
        tb = getattr(e, "traceback", None)
        err_msg = f"{type(e).__name__}: {e}"
        if tb:
            print(f"[worker-traceback] {inst.instance_id}:\n{tb}", flush=True)
            err_msg = f"{err_msg} | traceback: {tb[-500:]}"
        return [(p, _failure_result(inst, p, "error", err_msg)) for p in needed]


def _run_multi_budget_parallel(
    pending: list[tuple[BenchmarkInstance, list[RunParams]]],
    eval_all_cells_fn: EvalAllCellsFn,
    workers: int,
    timeout_per_instance: float,
    n_budgets: int,
    record: Callable[[list[tuple[RunParams, EvalResult]]], None],
) -> None:
    from pebble import ProcessPool

    from benchmarks.common import _init_worker

    pebble_timeout = timeout_per_instance * n_budgets + 30.0
    with ProcessPool(max_workers=workers, max_tasks=40, initializer=_init_worker) as pool:
        futures = [
            (pool.schedule(eval_all_cells_fn, args=[inst, needed], timeout=pebble_timeout), inst, needed)
            for inst, needed in pending
        ]
        for future, inst, needed in futures:
            per_cell = _resolve_multi_budget_future(future, inst, needed, pebble_timeout, timeout_per_instance)
            record(per_cell)


def _run_serial(
    pending: list[BenchmarkInstance],
    eval_fn: EvalFn,
    params: RunParams,
    record: Callable[[EvalResult], None],
) -> None:
    for inst in pending:
        try:
            record(eval_fn(inst, params))
        except Exception as e:
            record(_failure_result(inst, params, "error", f"{type(e).__name__}: {e}"))


def _resolve_future(
    future: Any,
    inst: BenchmarkInstance,
    params: RunParams,
    timeout_per_instance: float,
    pebble_timeout: float,
) -> EvalResult:
    from concurrent.futures import TimeoutError as FuturesTimeoutError

    from pebble import ProcessExpired

    try:
        return future.result()
    except FuturesTimeoutError:
        return _failure_result(inst, params, "timeout", f"pebble killed after {pebble_timeout:.0f}s")
    except ProcessExpired as e:
        # exitcode 137 == os._exit(137) from the narrow algorithm kill switch.
        # Persist as timeout so the checkpoint records it.
        if e.exitcode == 137:
            return _failure_result(inst, params, "timeout", f"diffctx exceeded {timeout_per_instance:.0f}s budget")
        return _handle_process_expired(inst, params, e)
    except Exception as e:
        tb = getattr(e, "traceback", None)
        err_msg = f"{type(e).__name__}: {e}"
        if tb:
            print(f"[worker-traceback] {inst.instance_id}:\n{tb}", flush=True)
            err_msg = f"{err_msg} | traceback: {tb[-500:]}"
        return _failure_result(inst, params, "error", err_msg)


def _run_parallel(
    pending: list[BenchmarkInstance],
    eval_fn: EvalFn,
    params: RunParams,
    workers: int,
    timeout_per_instance: float,
    record: Callable[[EvalResult], None],
    pool: object | None = None,
) -> None:
    """Pebble-based parallel drain.

    Why pebble instead of `concurrent.futures.ProcessPoolExecutor`:
    our kill switch (in `benchmarks/diffctx_eval_fn.py`) uses
    `os._exit(137)` to bound the diffctx call. `ProcessPoolExecutor`
    permanently brick's its pool when a worker dies via os._exit
    (documented Python behavior — `BrokenProcessPool` is terminal).
    pebble's `ProcessPool` instead respawns the dead worker
    transparently, so a single timeout no longer cascades into
    pool-wide failure. The `pool` arg (a foreign pool from a long-
    running calibrator) is ignored by this code path; it is kept in
    the signature for API stability with `run_eval_set`.
    """
    from pebble import ProcessPool

    from benchmarks.common import _init_worker

    # `pool` is the legacy ProcessPoolExecutor foreign-pool path.
    # Calibration's evaluate_grid_cached owns its own pebble pool now;
    # this branch should not be reachable from updated callers but is
    # preserved to surface a clear error if a stale caller passes a
    # ProcessPoolExecutor-shaped pool.
    if pool is not None:
        raise NotImplementedError(
            "run_eval_set received a foreign `pool` arg; pebble migration "
            "expects callers to pass `pool=None` and let _run_parallel own "
            "the pebble.ProcessPool."
        )

    # Per-task wall-clock deadline. Generous safety net: covers
    # ensure_repo + apply_as_commit + diffctx + N selections. The
    # narrow 20s budget on the algorithm itself is enforced inside
    # eval_fn via threading.Timer + os._exit(137); this outer pebble
    # timeout is the upper bound for git ops on huge repos.
    pebble_timeout = max(timeout_per_instance + 30.0, 60.0)

    _t_start = _time.monotonic()
    _t_last_progress = _t_start
    _completed = 0
    _total = len(pending)

    with ProcessPool(
        max_workers=workers,
        max_tasks=50,
        initializer=_init_worker,
    ) as pp:
        futures: dict = {}
        for inst in pending:
            future = pp.schedule(eval_fn, args=[inst, params], timeout=pebble_timeout)
            futures[future] = inst

        for future, inst in futures.items():
            record(_resolve_future(future, inst, params, timeout_per_instance, pebble_timeout))
            _completed += 1
            _now = _time.monotonic()
            if _now - _t_last_progress >= 30.0:
                _elapsed = _now - _t_start
                print(
                    f"[progress] {_completed}/{_total} ({100.0 * _completed / _total:.0f}%) elapsed={_elapsed:.0f}s",
                    flush=True,
                )
                _t_last_progress = _now

    _elapsed_total = _time.monotonic() - _t_start
    print(
        f"[progress] {_completed}/{_total} (100%) elapsed={_elapsed_total:.0f}s done",
        flush=True,
    )
