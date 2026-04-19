from __future__ import annotations

import logging
import os
import signal
import time
from pathlib import Path
from typing import Any

from . import git as _git
from ._pipeline_discovery import _empty_tree, run_discovery
from ._pipeline_scoring import (
    _UNLIMITED_BUDGET,
    assign_token_counts,
    log_full_mode,
    log_ppr_mode,
    score_and_select,
    select_full_mode,
)
from .core import _compute_seed_weights, _identify_core_fragments
from .git import CatFileBatch, GitError
from .postpass import _ensure_changed_files_represented
from .render import build_diff_context_output
from .scoring import EgoGraphScoring, PPRScoring
from .signatures import _generate_signature_variants

logger = logging.getLogger(__name__)


class DiffContextTimeoutError(Exception):
    pass


_PIPELINE_TIMEOUT = 300


def _validate_inputs(root_dir: Path, alpha: float, tau: float, budget_tokens: int | None) -> None:
    if not _git.is_git_repo(root_dir):
        raise GitError(f"'{root_dir}' is not a git repository")
    if alpha <= 0.0 or alpha >= 1.0:
        raise ValueError(f"alpha must be in (0, 1), got {alpha}")
    if tau < 0.0:
        raise ValueError(f"tau must be >= 0, got {tau}")
    if tau < 1e-15:
        logger.warning("tau≈0 disables adaptive stopping; budget will be fully consumed")
    if budget_tokens is not None and budget_tokens <= 0:
        raise ValueError(f"budget_tokens must be > 0, got {budget_tokens}")


def build_diff_context(
    root_dir: Path,
    diff_range: str,
    budget_tokens: int | None = None,
    alpha: float = 0.60,
    tau: float = 0.08,
    no_content: bool = False,
    ignore_file: Path | None = None,
    no_default_ignores: bool = False,
    full: bool = False,
    whitelist_file: Path | None = None,
    scoring_mode: str = "hybrid",
    timeout: int = _PIPELINE_TIMEOUT,
) -> dict[str, Any]:
    _validate_inputs(root_dir, alpha, tau, budget_tokens)
    root_dir = root_dir.resolve()

    import threading

    can_use_alarm = hasattr(signal, "SIGALRM") and threading.current_thread() is threading.main_thread()

    if can_use_alarm and timeout > 0:

        def _raise_timeout(_sig: int, _frame: object) -> None:
            raise DiffContextTimeoutError(f"diffctx timed out after {timeout}s")

        prev_handler = signal.signal(signal.SIGALRM, _raise_timeout)
        signal.alarm(timeout)
        try:
            return _build_diff_context_inner(
                root_dir,
                diff_range,
                budget_tokens,
                alpha,
                tau,
                no_content,
                ignore_file,
                no_default_ignores,
                full,
                whitelist_file,
                scoring_mode,
            )
        finally:
            signal.alarm(0)
            signal.signal(signal.SIGALRM, prev_handler)

    return _build_diff_context_inner(
        root_dir,
        diff_range,
        budget_tokens,
        alpha,
        tau,
        no_content,
        ignore_file,
        no_default_ignores,
        full,
        whitelist_file,
        scoring_mode,
    )


def _build_diff_context_inner(
    root_dir: Path,
    diff_range: str,
    budget_tokens: int | None,
    alpha: float,
    tau: float,
    no_content: bool,
    ignore_file: Path | None,
    no_default_ignores: bool,
    full: bool,
    whitelist_file: Path | None,
    scoring_mode: str,
) -> dict[str, Any]:

    dr = run_discovery(root_dir, diff_range, ignore_file, no_default_ignores, whitelist_file, scoring_mode)
    if dr is None:
        return _empty_tree(root_dir)

    hunks = dr.hunks
    diff_text = dr.diff_text
    all_fragments = dr.all_fragments
    changed_files = dr.changed_files
    discovered_files = dr.discovered_files
    preferred_revs = dr.preferred_revs
    config = dr.config
    t0, t1, t2 = dr.timing

    assign_token_counts(all_fragments)

    core_ids = _identify_core_fragments(hunks, all_fragments)

    if not os.environ.get("DIFFCTX_DISABLE_SIGNATURES"):
        signature_frags = _generate_signature_variants(all_fragments)
        assign_token_counts(signature_frags)
        all_fragments.extend(signature_frags)

    t4 = time.perf_counter()

    if full:
        selected = select_full_mode(all_fragments, changed_files)
        log_full_mode(selected)
    else:
        seed_weights = _compute_seed_weights(hunks, core_ids, all_fragments)
        selected, result = score_and_select(
            all_fragments,
            core_ids,
            diff_text,
            budget_tokens,
            tau,
            hunks=hunks,
            repo_root=root_dir,
            seed_weights=seed_weights,
            scoring_strategy=(
                EgoGraphScoring(max_depth=config.ego_depth)
                if config.scoring == "ego"
                else PPRScoring(alpha=config.ppr_alpha, low_relevance_filter=config.low_relevance)
            ),
            discovered_paths=set(discovered_files),
        )
        effective_budget = budget_tokens if budget_tokens is not None else _UNLIMITED_BUDGET
        remaining = effective_budget - result.used_tokens
        with CatFileBatch(root_dir) as batch_reader:
            selected = _ensure_changed_files_represented(
                selected, all_fragments, changed_files, remaining, root_dir, preferred_revs, batch_reader
            )
        log_ppr_mode(selected, core_ids, budget_tokens, result, alpha, tau)

    t5 = time.perf_counter()
    logger.debug("diffctx: timing — graph+select %.3fs", t5 - t4)

    _dump_dir = os.environ.get("DIFFCTX_DUMP_DIR")
    if _dump_dir:
        sel_paths = {str(f.path.relative_to(root_dir)) for f in selected}
        (Path(_dump_dir) / "selected.txt").write_text("\n".join(sorted(sel_paths)) + "\n")

    if no_content:
        for frag in selected:
            frag.content = ""

    output = build_diff_context_output(root_dir, selected)
    output["latency"] = {
        "fragmentation_ms": round((t1 - t0) * 1000, 1),
        "discovery_ms": round((t2 - t1) * 1000, 1),
        "tokenization_ms": round((t4 - t2) * 1000, 1),
        "scoring_selection_ms": round((t5 - t4) * 1000, 1),
        "total_ms": round((t5 - t0) * 1000, 1),
    }
    return output
