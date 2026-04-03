from __future__ import annotations

import logging
import time
from pathlib import Path
from typing import Any

from ..ignore import get_ignore_specs, get_whitelist_spec
from ..tokens import count_tokens

# Access git functions via module to support monkeypatching in tests
from . import git as _git
from .config import LIMITS
from .core import _compute_seed_weights, _identify_core_fragments
from .edges import discover_all_related_files
from .file_importance import compute_file_importance
from .filtering import (
    _apply_same_file_floor,
    _cap_context_fragments,
    _filter_low_relevance_fragments,
    _filter_unrelated_fragments,
)
from .fragmentation import _process_files_for_fragments
from .git import CatFileBatch, GitError, split_diff_range
from .graph import build_graph
from .postpass import _coherence_post_pass, _ensure_changed_files_represented
from .ppr import personalized_pagerank
from .render import build_diff_context_output
from .select import lazy_greedy_select
from .signatures import _generate_signature_variants
from .types import Fragment, FragmentId
from .universe import (
    _collect_candidate_files,
    _discover_untracked_files,
    _enrich_concepts,
    _expand_universe_by_rare_identifiers,
    _filter_whitelist,
    _normalize_path,
    _resolve_changed_files,
    _synthetic_hunks,
)
from .utility import concepts_from_diff_text, needs_from_diff

logger = logging.getLogger(__name__)

_OVERHEAD_PER_FRAGMENT = LIMITS.overhead_per_fragment
_MAX_DISCOVERED_FILES = LIMITS.max_discovered_files
_UNLIMITED_BUDGET = 10_000_000


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


def _select_with_ppr(
    all_fragments: list[Fragment],
    core_ids: set[FragmentId],
    diff_text: str,
    budget_tokens: int | None,
    alpha: float,
    tau: float,
    repo_root: Path | None = None,
    seed_weights: dict[FragmentId, float] | None = None,
) -> tuple[list[Fragment], Any]:
    graph = build_graph(all_fragments, repo_root=repo_root)
    rel_scores = personalized_pagerank(graph, core_ids, alpha=alpha, seed_weights=seed_weights)
    _apply_same_file_floor(rel_scores, core_ids, all_fragments)

    filtered_fragments = _filter_unrelated_fragments(all_fragments, core_ids, graph)
    filtered_fragments = _filter_low_relevance_fragments(filtered_fragments, core_ids, rel_scores)
    filtered_fragments = _cap_context_fragments(filtered_fragments, core_ids, rel_scores)

    needs = needs_from_diff(filtered_fragments, core_ids, graph, diff_text)

    file_importance = compute_file_importance(filtered_fragments)

    effective_budget = budget_tokens if budget_tokens is not None else _UNLIMITED_BUDGET

    result = lazy_greedy_select(
        fragments=filtered_fragments,
        core_ids=core_ids,
        rel=rel_scores,
        needs=needs,
        budget_tokens=effective_budget,
        tau=tau,
        file_importance=file_importance,
    )

    selected = _coherence_post_pass(result, filtered_fragments, graph, effective_budget)
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


def _empty_tree(root_dir: Path) -> dict[str, Any]:
    return {
        "name": root_dir.name,
        "type": "diff_context",
        "fragment_count": 0,
        "fragments": [],
    }


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
) -> dict[str, Any]:
    _validate_inputs(root_dir, alpha, tau, budget_tokens)
    root_dir = root_dir.resolve()

    hunks = _git.parse_diff(root_dir, diff_range)

    base_rev, head_rev = split_diff_range(diff_range)
    is_working_tree_diff = base_rev is None and head_rev is None
    combined_spec = get_ignore_specs(root_dir, ignore_file, no_default_ignores, None)
    wl_spec = get_whitelist_spec(whitelist_file, root_dir)

    untracked = _discover_untracked_files(root_dir, combined_spec) if is_working_tree_diff else []
    if untracked:
        hunks.extend(_synthetic_hunks(untracked))

    if not hunks:
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

        all_candidate_files, is_large_repo = _collect_candidate_files(root_dir, set(changed_files), combined_spec)
        all_candidate_files = _filter_whitelist(all_candidate_files, root_dir, wl_spec)

        t1 = time.perf_counter()

        edge_discovered = discover_all_related_files(changed_files, all_candidate_files, root_dir)
        if len(edge_discovered) > _MAX_DISCOVERED_FILES:
            logger.debug(
                "diffctx: capping edge-discovered files from %d to %d",
                len(edge_discovered),
                _MAX_DISCOVERED_FILES,
            )
            edge_discovered = edge_discovered[:_MAX_DISCOVERED_FILES]
        edge_discovered = [_normalize_path(p, root_dir) for p in edge_discovered]
        all_fragments.extend(_process_files_for_fragments(edge_discovered, root_dir, preferred_revs, seen_frag_ids, batch_reader))

        t2 = time.perf_counter()

        if not is_large_repo:
            expanded_files = _expand_universe_by_rare_identifiers(
                root_dir,
                expansion_concepts,
                changed_files + edge_discovered,
                combined_spec,
                candidate_files=all_candidate_files,
                changed_files=changed_files,
            )
            expanded_files = [_normalize_path(p, root_dir) for p in expanded_files]
            all_fragments.extend(
                _process_files_for_fragments(expanded_files, root_dir, preferred_revs, seen_frag_ids, batch_reader)
            )
        else:
            logger.debug("diffctx: skipping rare-identifier expansion for large repo")

        t3 = time.perf_counter()

    logger.debug(
        "diffctx: timing — changed_files %.3fs, edge_discovery %.3fs, expansion %.3fs, total_io %.3fs",
        t1 - t0,
        t2 - t1,
        t3 - t2,
        t3 - t0,
    )

    _assign_token_counts(all_fragments)

    core_ids = _identify_core_fragments(hunks, all_fragments)

    signature_frags = _generate_signature_variants(all_fragments)
    _assign_token_counts(signature_frags)
    all_fragments.extend(signature_frags)

    t4 = time.perf_counter()

    if full:
        selected = _select_full_mode(all_fragments, changed_files)
        _log_full_mode(selected)
    else:
        seed_weights = _compute_seed_weights(hunks, core_ids, all_fragments)
        selected, result = _select_with_ppr(
            all_fragments,
            core_ids,
            diff_text,
            budget_tokens,
            alpha,
            tau,
            repo_root=root_dir,
            seed_weights=seed_weights,
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

    if no_content:
        for frag in selected:
            frag.content = ""

    return build_diff_context_output(root_dir, selected)
