from __future__ import annotations

import logging
import os
import signal
import time
from pathlib import Path
from typing import Any

from ..ignore import get_ignore_specs, get_whitelist_spec
from ..tokens import count_tokens

# Access git functions via module to support monkeypatching in tests
from . import git as _git
from .config import LIMITS
from .core import _compute_seed_weights, _identify_core_fragments
from .file_importance import compute_file_importance
from .fragmentation import _process_files_for_fragments
from .git import CatFileBatch, GitError, split_diff_range
from .mode import PipelineConfig, ScoringMode
from .postpass import _coherence_post_pass, _ensure_changed_files_represented, _rescue_nontrivial_context
from .render import build_diff_context_output
from .scoring import (
    BM25Discovery,
    DefaultDiscovery,
    DiscoveryContext,
    DiscoveryStrategy,
    EgoGraphScoring,
    EnsembleDiscovery,
    PPRScoring,
    ScoringStrategy,
)
from .select import lazy_greedy_select
from .signatures import _generate_signature_variants
from .types import Fragment, FragmentId
from .universe import (
    _collect_candidate_files,
    _discover_untracked_files,
    _enrich_concepts,
    _filter_whitelist,
    _normalize_path,
    _resolve_changed_files,
    _synthetic_hunks,
)
from .utility import concepts_from_diff_text, needs_from_diff

logger = logging.getLogger(__name__)


class DiffContextTimeoutError(Exception):
    pass


_PIPELINE_TIMEOUT = 300
_OVERHEAD_PER_FRAGMENT = LIMITS.overhead_per_fragment
_UNLIMITED_BUDGET = 10_000_000
_MAX_CACHE_BYTES = 200 * 1024 * 1024


def _build_preferred_revs(base_rev: str | None, head_rev: str | None) -> list[str]:
    revs: list[str] = []
    if head_rev:
        revs.append(head_rev)
    if base_rev and base_rev != head_rev:
        revs.append(base_rev)
    return revs


def _assign_token_counts(fragments: list[Fragment]) -> None:
    for frag in fragments:
        frag.token_count = count_tokens(frag.content).count + _OVERHEAD_PER_FRAGMENT


def _select_full_mode(
    all_fragments: list[Fragment],
    changed_files: list[Path],
) -> list[Fragment]:
    changed_paths = set(changed_files)
    selected = [f for f in all_fragments if f.path in changed_paths]
    selected.sort(key=lambda f: (f.path, f.start_line))
    return selected


def _score_and_select(
    all_fragments: list[Fragment],
    core_ids: set[FragmentId],
    diff_text: str,
    budget_tokens: int | None,
    tau: float,
    hunks: list[Any],
    repo_root: Path | None = None,
    seed_weights: dict[FragmentId, float] | None = None,
    scoring_strategy: ScoringStrategy | None = None,
    discovered_paths: set[Path] | None = None,
) -> tuple[list[Fragment], Any]:
    strategy = scoring_strategy or PPRScoring()

    dump_scores = os.environ.get("DIFFCTX_DUMP_SCORES")
    scoring_result = strategy.score_and_filter(
        all_fragments,
        core_ids,
        hunks,
        repo_root=repo_root,
        seed_weights=seed_weights,
        dump_scores_file=dump_scores,
        discovered_paths=discovered_paths,
    )

    needs = needs_from_diff(scoring_result.filtered_fragments, core_ids, scoring_result.graph, diff_text)
    file_importance = compute_file_importance(scoring_result.filtered_fragments)
    effective_budget = budget_tokens if budget_tokens is not None else _UNLIMITED_BUDGET

    if os.environ.get("DIFFCTX_NO_SUBMODULAR"):
        from .select import _topk_select

        result = _topk_select(scoring_result.filtered_fragments, core_ids, scoring_result.rel_scores, effective_budget)
    else:
        result = lazy_greedy_select(
            fragments=scoring_result.filtered_fragments,
            core_ids=core_ids,
            rel=scoring_result.rel_scores,
            needs=needs,
            budget_tokens=effective_budget,
            tau=tau,
            file_importance=file_importance,
        )

    selected = _coherence_post_pass(result, scoring_result.filtered_fragments, scoring_result.graph, effective_budget)
    selected = _rescue_nontrivial_context(selected, all_fragments, scoring_result.rel_scores, core_ids, effective_budget)
    return selected.selected, selected


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


def _log_full_mode(selected: list[Fragment]) -> None:
    used = sum(f.token_count for f in selected)
    logger.info(
        "diffctx: full mode selected=%d from changed files used=%d tokens",
        len(selected),
        used,
    )


def _log_ppr_mode(
    selected: list[Fragment],
    core_ids: set[FragmentId],
    budget_tokens: int | None,
    result: Any,
    alpha: float,
    tau: float,
) -> None:
    used = sum(f.token_count for f in selected)
    budget_str = str(budget_tokens) if budget_tokens is not None else "unlimited"
    logger.info(
        "diffctx: selected=%d core=%d used=%d/%s reason=%s utility=%.4f alpha=%.3f tau=%.3f",
        len(selected),
        len(core_ids),
        used,
        budget_str,
        result.reason,
        result.utility,
        alpha,
        tau,
    )


def _create_discovery(config: PipelineConfig) -> DiscoveryStrategy:
    if config.discovery == "ensemble":
        return EnsembleDiscovery([DefaultDiscovery(), BM25Discovery(top_k=config.bm25_top_k)])
    return DefaultDiscovery()


def _empty_tree(root_dir: Path) -> dict[str, Any]:
    return {
        "name": root_dir.name,
        "type": "diff_context",
        "fragment_count": 0,
        "fragments": [],
    }


def _build_file_cache(candidate_files: list[Path]) -> dict[Path, str]:
    cache: dict[Path, str] = {}
    cache_bytes = 0
    for f in candidate_files:
        if cache_bytes > _MAX_CACHE_BYTES:
            break
        try:
            if f.stat().st_size <= 100_000:
                content = f.read_text(encoding="utf-8")
                cache_bytes += len(content.encode("utf-8", errors="replace"))
                cache[f] = content
        except (OSError, UnicodeDecodeError):
            continue
    return cache


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

    can_use_alarm = threading.current_thread() is threading.main_thread()

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

    hunks = _git.parse_diff(root_dir, diff_range)

    base_rev, head_rev = split_diff_range(diff_range)
    is_working_tree_diff = base_rev is None and head_rev is None
    combined_spec = get_ignore_specs(root_dir, ignore_file, no_default_ignores, None)
    wl_spec = get_whitelist_spec(whitelist_file, root_dir)

    untracked = _discover_untracked_files(root_dir, combined_spec) if is_working_tree_diff else []
    if untracked:
        hunks.extend(_synthetic_hunks(untracked))

    if not hunks:
        logger.warning("no diff hunks found — empty diff or parse failure")
        return _empty_tree(root_dir)

    diff_text = _git.get_diff_text(root_dir, diff_range)
    expansion_concepts = concepts_from_diff_text(diff_text)
    if untracked:
        expansion_concepts = _enrich_concepts(expansion_concepts, untracked)

    changed_files = _resolve_changed_files(root_dir, diff_range, untracked, combined_spec, wl_spec)

    preferred_revs = _build_preferred_revs(base_rev, head_rev)

    t0 = time.perf_counter()

    with CatFileBatch(root_dir) as batch_reader:
        seen_frag_ids: set[FragmentId] = set()
        all_fragments = _process_files_for_fragments(changed_files, root_dir, preferred_revs, seen_frag_ids, batch_reader)

        all_candidate_files = _collect_candidate_files(root_dir, set(changed_files), combined_spec)
        all_candidate_files = _filter_whitelist(all_candidate_files, root_dir, wl_spec)

        t1 = time.perf_counter()

        file_cache = _build_file_cache(all_candidate_files)

        mode = ScoringMode(os.environ.get("DIFFCTX_SCORING", scoring_mode))
        config = PipelineConfig.from_mode(mode, n_candidate_files=len(all_candidate_files))

        discovery_ctx = DiscoveryContext(
            root_dir=root_dir,
            changed_files=changed_files,
            all_candidate_files=all_candidate_files,
            diff_text=diff_text,
            expansion_concepts=frozenset(expansion_concepts),
            file_cache=file_cache,
            combined_spec=combined_spec,
        )
        discovered_files = _create_discovery(config).discover(discovery_ctx)
        discovered_files = [_normalize_path(p, root_dir) for p in discovered_files]
        all_fragments.extend(
            _process_files_for_fragments(discovered_files, root_dir, preferred_revs, seen_frag_ids, batch_reader)
        )

        t2 = time.perf_counter()

    logger.debug(
        "diffctx: timing — changed_files %.3fs, discovery %.3fs, total_io %.3fs",
        t1 - t0,
        t2 - t1,
        t2 - t0,
    )

    dump_dir = os.environ.get("DIFFCTX_DUMP_DIR")
    if dump_dir:
        _dump = Path(dump_dir)
        _dump.mkdir(parents=True, exist_ok=True)
        universe = set(changed_files) | set(discovered_files)
        (_dump / "universe.txt").write_text("\n".join(sorted(str(p.relative_to(root_dir)) for p in universe)) + "\n")
        fragmented = {str(f.path.relative_to(root_dir)) for f in all_fragments}
        (_dump / "fragmented.txt").write_text("\n".join(sorted(fragmented)) + "\n")
        (_dump / "candidates.txt").write_text(f"candidates={len(all_candidate_files)} discovered={len(discovered_files)}\n")

    _assign_token_counts(all_fragments)

    core_ids = _identify_core_fragments(hunks, all_fragments)

    if not os.environ.get("DIFFCTX_DISABLE_SIGNATURES"):
        signature_frags = _generate_signature_variants(all_fragments)
        _assign_token_counts(signature_frags)
        all_fragments.extend(signature_frags)

    t4 = time.perf_counter()

    if full:
        selected = _select_full_mode(all_fragments, changed_files)
        _log_full_mode(selected)
    else:
        seed_weights = _compute_seed_weights(hunks, core_ids, all_fragments)
        selected, result = _score_and_select(
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
        _log_ppr_mode(selected, core_ids, budget_tokens, result, alpha, tau)

    t5 = time.perf_counter()
    logger.debug("diffctx: timing — graph+select %.3fs", t5 - t4)

    if dump_dir:
        sel_paths = {str(f.path.relative_to(root_dir)) for f in selected}
        (Path(dump_dir) / "selected.txt").write_text("\n".join(sorted(sel_paths)) + "\n")

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
