from __future__ import annotations

import json
import time
from collections.abc import Callable, Iterable, Iterator
from dataclasses import asdict, dataclass, field
from pathlib import Path

from benchmarks.adapters.base import BenchmarkAdapter, BenchmarkInstance, EvalResult

EvalFn = Callable[[BenchmarkInstance, "RunParams"], EvalResult]


@dataclass(frozen=True)
class RunParams:
    """Parameters for one diffctx evaluation pass.

    `tau` and `core_budget_fraction` are the two calibrated knobs (validated
    by the sensitivity sweep — every other operational parameter showed
    near-zero effect). Anything else can be threaded through `extra_env`.
    """

    tau: float = 0.08
    core_budget_fraction: float = 0.70
    budget: int = 8000
    scoring: str = "hybrid"
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
    """Append one result as a JSONL row. Atomic per-line on POSIX."""
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a") as f:
        f.write(json.dumps(asdict(result), default=str) + "\n")


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


def run_eval_set(
    instances: list[BenchmarkInstance],
    eval_fn: EvalFn,
    params: RunParams,
    workers: int = 1,
    timeout_per_instance: float = 300.0,
    resume_from: Path | None = None,
    checkpoint_path: Path | None = None,
) -> list[EvalResult]:
    """Run `eval_fn(instance, params)` for every instance.

    - `workers > 1` uses a thread pool; otherwise sequential.
    - `timeout_per_instance` records a `status="timeout"` failure for any
      future that does not return within the deadline. The hung worker
      thread is left running (Python cannot kill threads safely); pool
      shutdown does not wait for it.
    - `resume_from` (JSONL path): instance_ids already present in that file
      are skipped — re-running after a crash continues where it left off.
    - `checkpoint_path` (JSONL path): each completed result is appended
      immediately so a crash mid-sweep loses at most one in-flight result.
    """
    done_ids: set[str] = read_checkpoint(resume_from) if resume_from else set()
    pending = [i for i in instances if i.instance_id not in done_ids]
    results: list[EvalResult] = []

    def _record(r: EvalResult) -> None:
        results.append(r)
        if checkpoint_path is not None:
            append_checkpoint(checkpoint_path, r)

    if not pending:
        return results

    if workers <= 1 or len(pending) <= 1:
        for inst in pending:
            try:
                _record(eval_fn(inst, params))
            except Exception as e:
                _record(_failure_result(inst, params, "error", f"{type(e).__name__}: {e}"))
        return results

    from concurrent.futures import (
        ThreadPoolExecutor,
        as_completed,
    )
    from concurrent.futures import (
        TimeoutError as FuturesTimeoutError,
    )

    with ThreadPoolExecutor(max_workers=workers) as pool:
        futures = {pool.submit(eval_fn, inst, params): inst for inst in pending}
        # Generous outer deadline: timeout * ceil(len/workers) covers the
        # serialised case if workers all hang together.
        outer_deadline = time.monotonic() + timeout_per_instance * max(1, (len(pending) + workers - 1) // workers)
        completed: set[str] = set()
        try:
            for future in as_completed(futures, timeout=max(0.0, outer_deadline - time.monotonic())):
                inst = futures[future]
                try:
                    r = future.result(timeout=0)
                except FuturesTimeoutError:
                    r = _failure_result(inst, params, "timeout", f"after {timeout_per_instance}s")
                except Exception as e:
                    r = _failure_result(inst, params, "error", f"{type(e).__name__}: {e}")
                completed.add(inst.instance_id)
                _record(r)
        except FuturesTimeoutError:
            for inst in futures.values():
                if inst.instance_id not in completed:
                    _record(_failure_result(inst, params, "timeout", "exceeded global deadline"))
        # Cancel any pending futures; running threads are abandoned.
        pool.shutdown(wait=False, cancel_futures=True)

    return results
