from __future__ import annotations

import logging
import subprocess
from collections import defaultdict
from pathlib import Path
from typing import Any

import pathspec

from ..ignore import get_ignore_specs, should_ignore
from ..tokens import count_tokens
from .config import LIMITS
from .config.extensions import CODE_EXTENSIONS, CONFIG_EXTENSIONS, DOC_EXTENSIONS
from .edges import discover_all_related_files
from .fragments import enclosing_fragment, fragment_file  # type: ignore[attr-defined]
from .git import (
    GitError,
    get_changed_files,
    get_diff_text,
    get_untracked_files,
    is_git_repo,
    parse_diff,
    show_file_at_revision,
    split_diff_range,
)
from .graph import Graph, build_graph
from .languages import FILENAME_TO_LANGUAGE
from .ppr import personalized_pagerank
from .render import build_partial_tree
from .select import SelectionResult, lazy_greedy_select
from .types import DiffHunk, Fragment, FragmentId, extract_identifiers
from .utility import concepts_from_diff_text, needs_from_diff

__all__ = ["GitError", "build_diff_context"]

_RARE_THRESHOLD = LIMITS.rare_identifier_threshold
_MAX_EXPANSION_FILES = LIMITS.max_expansion_files
_OVERHEAD_PER_FRAGMENT = LIMITS.overhead_per_fragment
_FALLBACK_MAX_FILES = 10_000

_SEMANTIC_KINDS = frozenset(
    {
        "function",
        "class",
        "struct",
        "impl",
        "interface",
        "enum",
        "module",
        "type",
        "variable",
        "record",
        "property",
        "declaration",
        "definition",
        "section",
    }
)


def _kind_priority(kind: str) -> int:
    return 0 if kind in _SEMANTIC_KINDS else 1


def _read_file_content(
    file_path: Path,
    root_dir: Path,
    preferred_revs: list[str],
) -> str | None:
    abs_path = _normalize_path(file_path, root_dir)
    try:
        rel = abs_path.relative_to(root_dir.resolve())
    except ValueError:
        logging.debug("diffctx: path %s not under root %s", abs_path, root_dir)
        return None

    for rev in preferred_revs:
        try:
            return show_file_at_revision(root_dir, rev, rel)
        except GitError:
            continue

    if abs_path.exists() and abs_path.is_file():
        try:
            return abs_path.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            pass

    return None


_UNLIMITED_BUDGET = 10_000_000  # Large budget to let tau-based stopping work


def _build_preferred_revs(base_rev: str | None, head_rev: str | None) -> list[str]:
    revs: list[str] = []
    if head_rev:
        revs.append(head_rev)
    if base_rev and base_rev != head_rev:
        revs.append(base_rev)
    return revs


def _process_files_for_fragments(
    files: list[Path],
    root_dir: Path,
    preferred_revs: list[str],
    seen_frag_ids: set[FragmentId],
) -> list[Fragment]:
    fragments: list[Fragment] = []
    for file_path in files:
        content = _read_file_content(file_path, root_dir, preferred_revs)
        if content is None:
            continue
        for frag in fragment_file(file_path, content):
            if frag.id not in seen_frag_ids:
                fragments.append(frag)
                seen_frag_ids.add(frag.id)
    return fragments


def _find_core_for_hunk(
    frags: list[Fragment],
    h_start: int,
    h_end: int,
) -> set[FragmentId]:
    core: set[FragmentId] = set()

    covering = [f for f in frags if f.start_line <= h_start and h_end <= f.end_line]
    if covering:
        best = min(covering, key=lambda f: (_kind_priority(f.kind), f.line_count))
        core.add(best.id)
        return core

    overlapping = [f for f in frags if f.start_line <= h_end and f.end_line >= h_start]
    if overlapping:
        for f in overlapping:
            core.add(f.id)
        return core

    enc = enclosing_fragment(frags, h_start)
    if enc is not None:
        core.add(enc.id)
        return core

    before = [f for f in frags if f.end_line < h_start]
    after = [f for f in frags if f.start_line > h_end]
    if before:
        core.add(max(before, key=lambda f: f.end_line).id)
    if after:
        core.add(min(after, key=lambda f: f.start_line).id)

    return core


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

    needs = needs_from_diff(all_fragments, core_ids, graph, diff_text)

    effective_budget = budget_tokens if budget_tokens is not None else _UNLIMITED_BUDGET

    result = lazy_greedy_select(
        fragments=all_fragments,
        core_ids=core_ids,
        rel=rel_scores,
        needs=needs,
        budget_tokens=effective_budget,
        tau=tau,
    )

    selected = _coherence_post_pass(result, all_fragments, graph, effective_budget)
    return selected.selected, selected


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
) -> dict[str, Any]:
    _validate_inputs(root_dir, alpha, tau, budget_tokens)
    root_dir = root_dir.resolve()

    hunks = parse_diff(root_dir, diff_range)

    base_rev, head_rev = split_diff_range(diff_range)
    is_working_tree_diff = base_rev is None and head_rev is None
    combined_spec = get_ignore_specs(root_dir, ignore_file, no_default_ignores, None)

    untracked: list[Path] = []
    if is_working_tree_diff:
        untracked = _discover_untracked_files(root_dir, combined_spec)
        hunks.extend(_synthetic_hunks(untracked))

    if not hunks:
        return _empty_tree(root_dir)

    diff_text = get_diff_text(root_dir, diff_range)
    expansion_concepts = concepts_from_diff_text(diff_text)
    if untracked:
        expansion_concepts = _enrich_concepts(expansion_concepts, untracked)

    changed_files = get_changed_files(root_dir, diff_range)
    changed_files = [_normalize_path(p, root_dir) for p in changed_files]
    changed_files.extend(untracked)
    changed_files = _filter_ignored(changed_files, root_dir, combined_spec)

    preferred_revs = _build_preferred_revs(base_rev, head_rev)

    seen_frag_ids: set[FragmentId] = set()
    all_fragments = _process_files_for_fragments(changed_files, root_dir, preferred_revs, seen_frag_ids)

    all_candidate_files = _collect_candidate_files(root_dir, set(changed_files), combined_spec)

    edge_discovered = discover_all_related_files(changed_files, all_candidate_files, root_dir)
    edge_discovered = [_normalize_path(p, root_dir) for p in edge_discovered]
    all_fragments.extend(_process_files_for_fragments(edge_discovered, root_dir, preferred_revs, seen_frag_ids))

    expanded_files = _expand_universe_by_rare_identifiers(
        root_dir, expansion_concepts, changed_files + edge_discovered, combined_spec
    )
    expanded_files = [_normalize_path(p, root_dir) for p in expanded_files]
    all_fragments.extend(_process_files_for_fragments(expanded_files, root_dir, preferred_revs, seen_frag_ids))

    for frag in all_fragments:
        frag.token_count = count_tokens(frag.content).count + _OVERHEAD_PER_FRAGMENT

    core_ids = _identify_core_fragments(hunks, all_fragments)

    signature_frags = _generate_signature_variants(all_fragments, core_ids)
    for frag in signature_frags:
        frag.token_count = count_tokens(frag.content).count + _OVERHEAD_PER_FRAGMENT
    all_fragments.extend(signature_frags)

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
        _log_ppr_mode(selected, core_ids, budget_tokens, result, alpha, tau)

    if no_content:
        for frag in selected:
            frag.content = ""

    return build_partial_tree(root_dir, selected)


def _validate_inputs(root_dir: Path, alpha: float, tau: float, budget_tokens: int | None) -> None:
    if not is_git_repo(root_dir):
        raise GitError(f"'{root_dir}' is not a git repository")
    if not (0.0 < alpha < 1.0):
        raise ValueError(f"alpha must be in (0, 1), got {alpha}")
    if tau < 0.0:
        raise ValueError(f"tau must be >= 0, got {tau}")
    if tau == 0.0:
        logging.warning("tau=0 disables adaptive stopping; budget will be fully consumed")
    if budget_tokens is not None and budget_tokens <= 0:
        raise ValueError(f"budget_tokens must be > 0, got {budget_tokens}")


def _coherence_post_pass(
    result: SelectionResult,
    all_fragments: list[Fragment],
    graph: Graph,
    budget: int,
) -> SelectionResult:
    selected_ids = {f.id for f in result.selected}
    remaining = budget - result.used_tokens

    name_to_frags: dict[str, list[Fragment]] = {}
    for f in all_fragments:
        if f.symbol_name:
            name_to_frags.setdefault(f.symbol_name.lower(), []).append(f)

    frag_by_id: dict[FragmentId, Fragment] = {f.id: f for f in all_fragments}

    dangling_names: set[str] = set()
    for frag in result.selected:
        for nbr_id in graph.neighbors(frag.id):
            if nbr_id in selected_ids:
                continue
            cat = graph.edge_categories.get((frag.id, nbr_id), "")
            if cat == "semantic":
                nbr_frag = frag_by_id.get(nbr_id)
                if nbr_frag and nbr_frag.symbol_name:
                    dangling_names.add(nbr_frag.symbol_name.lower())

    added: list[Fragment] = []
    for name in dangling_names:
        candidates = name_to_frags.get(name, [])
        for c in candidates:
            if c.id in selected_ids:
                break
        else:
            sig_candidates = [f for f in candidates if "_signature" in f.kind]
            full_candidates = [f for f in candidates if "_signature" not in f.kind]
            pick = sig_candidates[0] if sig_candidates else (full_candidates[0] if full_candidates else None)
            if pick and pick.token_count <= remaining and pick.id not in selected_ids:
                added.append(pick)
                selected_ids.add(pick.id)
                remaining -= pick.token_count

    if not added:
        return result

    return SelectionResult(
        selected=result.selected + added,
        reason=result.reason,
        used_tokens=result.used_tokens + sum(f.token_count for f in added),
        utility=result.utility,
    )


def _compute_seed_weights(
    hunks: list[DiffHunk],
    core_ids: set[FragmentId],
    all_fragments: list[Fragment],
) -> dict[FragmentId, float]:
    frag_hunk_lines: dict[FragmentId, float] = {}
    for h in hunks:
        h_start, h_end = h.core_selection_range
        hunk_size = max(1, h_end - h_start + 1)
        for frag in all_fragments:
            if frag.id not in core_ids or frag.path != h.path:
                continue
            if frag.start_line <= h_end and frag.end_line >= h_start:
                frag_hunk_lines[frag.id] = frag_hunk_lines.get(frag.id, 0) + hunk_size
    if not frag_hunk_lines:
        return {}
    return frag_hunk_lines


_CONTAINER_FRAGMENT_KINDS = frozenset({"class", "interface", "struct"})
_SIGNATURE_ELIGIBLE_KINDS = frozenset({"function", "class", "method", "struct", "interface", "enum"})
_MIN_LINES_FOR_SIGNATURE = 5


def _generate_signature_variants(fragments: list[Fragment], core_ids: set[FragmentId]) -> list[Fragment]:
    signatures: list[Fragment] = []
    seen: set[FragmentId] = set()
    for frag in fragments:
        if frag.id in core_ids:
            continue
        if frag.kind not in _SIGNATURE_ELIGIBLE_KINDS:
            continue
        if frag.line_count < _MIN_LINES_FOR_SIGNATURE:
            continue
        lines = frag.content.splitlines()
        sig_end = min(2, len(lines))
        sig_content = "\n".join(lines[:sig_end])
        sig_id = FragmentId(frag.path, frag.start_line, frag.start_line + sig_end - 1)
        if sig_id in seen:
            continue
        seen.add(sig_id)
        signatures.append(
            Fragment(
                id=sig_id,
                kind=f"{frag.kind}_signature",
                content=sig_content,
                identifiers=frag.identifiers,
                symbol_name=frag.symbol_name,
            )
        )
    return signatures


def _identify_core_fragments(hunks: list[DiffHunk], all_fragments: list[Fragment]) -> set[FragmentId]:
    frags_by_path: dict[Path, list[Fragment]] = defaultdict(list)
    for frag in all_fragments:
        frags_by_path[frag.path].append(frag)

    core_ids: set[FragmentId] = set()
    for h in hunks:
        frags = frags_by_path.get(h.path, [])
        if frags:
            h_start, h_end = h.core_selection_range
            core_ids.update(_find_core_for_hunk(frags, h_start, h_end))

    _add_container_headers(core_ids, frags_by_path)
    return core_ids


def _add_container_headers(core_ids: set[FragmentId], frags_by_path: dict[Path, list[Fragment]]) -> None:
    core_paths = {fid.path for fid in core_ids}
    headers_to_add: list[FragmentId] = []
    for path in core_paths:
        for frag in frags_by_path.get(path, []):
            if frag.kind not in _CONTAINER_FRAGMENT_KINDS or frag.id in core_ids:
                continue
            for core_id in core_ids:
                if core_id.path == path and core_id.start_line > frag.end_line:
                    headers_to_add.append(frag.id)
                    break
    core_ids.update(headers_to_add)


def _log_full_mode(selected: list[Fragment]) -> None:
    try:
        used = sum(f.token_count for f in selected)
        logging.info(
            "diffctx: full mode selected=%d from changed files used=%d tokens",
            len(selected),
            used,
        )
    except (TypeError, AttributeError) as e:
        # nosemgrep: python-logger-credential-disclosure
        logging.debug("diffctx: failed to compute token count: %s", e)


def _log_ppr_mode(
    selected: list[Fragment],
    core_ids: set[FragmentId],
    budget_tokens: int | None,
    result: Any,
    alpha: float,
    tau: float,
) -> None:
    try:
        used = sum(f.token_count for f in selected)
        budget_str = str(budget_tokens) if budget_tokens is not None else "unlimited"
        logging.info(
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
    except (TypeError, AttributeError) as e:
        # nosemgrep: python-logger-credential-disclosure
        logging.debug("diffctx: failed to compute token count: %s", e)


_MAX_FILE_SIZE = LIMITS.max_file_size

_ALLOWED_SUFFIXES = CODE_EXTENSIONS | CONFIG_EXTENSIONS | DOC_EXTENSIONS | frozenset({".env"})
_ALLOWED_FILENAMES = frozenset(k.lower() for k in FILENAME_TO_LANGUAGE.keys())


def _is_allowed_file(path: Path) -> bool:
    suffix = path.suffix.lower()
    if suffix in _ALLOWED_SUFFIXES:
        return True
    return path.name.lower() in _ALLOWED_FILENAMES


def _normalize_path(path: Path, root_dir: Path) -> Path:
    if path.is_absolute():
        return path.resolve()
    return (root_dir / path).resolve()


def _is_candidate_file(file_path: Path, root_dir: Path, included_set: set[Path], combined_spec: pathspec.PathSpec) -> bool:
    if not file_path.is_file():
        return False
    if not _is_allowed_file(file_path):
        return False
    if file_path in included_set:
        return False
    try:
        rel_path = file_path.relative_to(root_dir).as_posix()
        if should_ignore(rel_path, combined_spec):
            return False
        if file_path.stat().st_size > _MAX_FILE_SIZE:
            return False
    except (ValueError, OSError):
        return False
    return True


def _collect_candidate_files(root_dir: Path, included_set: set[Path], combined_spec: pathspec.PathSpec) -> list[Path]:
    try:
        result = subprocess.run(
            ["git", "ls-files", "-z"],
            cwd=root_dir,
            capture_output=True,
            text=False,
            timeout=30,
        )
        if result.returncode == 0 and result.stdout:
            out = result.stdout.decode("utf-8", errors="surrogateescape")
            files = [root_dir / f for f in out.split("\0") if f]
            return [f for f in files if _is_candidate_file(f, root_dir, included_set, combined_spec)]
    except (subprocess.SubprocessError, OSError):
        pass
    logging.warning("diffctx: git ls-files failed, falling back to rglob (limit %d files)", _FALLBACK_MAX_FILES)
    candidates: list[Path] = []
    for f in root_dir.rglob("*"):
        if len(candidates) >= _FALLBACK_MAX_FILES:
            logging.warning("diffctx: fallback scan hit limit, results may be incomplete")
            break
        if _is_candidate_file(f, root_dir, included_set, combined_spec):
            candidates.append(f)
    return candidates


def _build_ident_index(files: list[Path], concepts: frozenset[str]) -> dict[str, list[Path]]:
    inverted_index: dict[str, list[Path]] = defaultdict(list)
    for file_path in sorted(files)[:2000]:
        try:
            content = file_path.read_text(encoding="utf-8")
            file_idents = extract_identifiers(content, skip_stopwords=False)
            for ident in file_idents:
                if ident in concepts:
                    inverted_index[ident].append(file_path)
        except (OSError, UnicodeDecodeError):
            continue
    return inverted_index


_MIN_CONCEPT_LENGTH = 4


def _collect_expansion_files(
    inverted_index: dict[str, list[Path]], concepts: frozenset[str], included_set: set[Path]
) -> list[Path]:
    rare_concepts = [
        c for c in concepts if len(c) >= _MIN_CONCEPT_LENGTH and 0 < len(inverted_index.get(c, [])) <= _RARE_THRESHOLD
    ]
    expansion_files: set[Path] = set()

    for concept in rare_concepts:
        for file_path in inverted_index.get(concept, []):
            if file_path not in included_set:
                expansion_files.add(file_path)
                if len(expansion_files) >= _MAX_EXPANSION_FILES:
                    return list(expansion_files)

    return list(expansion_files)


def _filter_ignored(
    files: list[Path],
    root_dir: Path,
    combined_spec: pathspec.PathSpec,
) -> list[Path]:
    result: list[Path] = []
    for file_path in files:
        try:
            rel_path = file_path.relative_to(root_dir).as_posix()
            if not should_ignore(rel_path, combined_spec):
                result.append(file_path)
        except ValueError:
            pass
    return result


def _expand_universe_by_rare_identifiers(
    root_dir: Path,
    concepts: frozenset[str],
    already_included: list[Path],
    combined_spec: pathspec.PathSpec,
) -> list[Path]:
    if not concepts:
        return []

    included_set = set(already_included)
    files = _collect_candidate_files(root_dir, included_set, combined_spec)
    inverted_index = _build_ident_index(files, concepts)
    return _collect_expansion_files(inverted_index, concepts, included_set)


def _discover_untracked_files(root_dir: Path, combined_spec: pathspec.PathSpec) -> list[Path]:
    try:
        raw = get_untracked_files(root_dir)
    except GitError:
        return []
    result: list[Path] = []
    for f in raw:
        normalized = _normalize_path(f, root_dir)
        if not normalized.is_file() or not _is_allowed_file(normalized):
            continue
        try:
            if normalized.stat().st_size > _MAX_FILE_SIZE:
                continue
            rel = normalized.relative_to(root_dir).as_posix()
            if should_ignore(rel, combined_spec):
                continue
        except (ValueError, OSError):
            continue
        result.append(normalized)
    return result


def _synthetic_hunks(files: list[Path]) -> list[DiffHunk]:
    hunks: list[DiffHunk] = []
    for f in files:
        try:
            content = f.read_text(encoding="utf-8")
            line_count = len(content.splitlines())
            if line_count > 0:
                hunks.append(DiffHunk(path=f, new_start=1, new_len=line_count))
        except (OSError, UnicodeDecodeError):
            continue
    return hunks


def _enrich_concepts(concepts: frozenset[str], files: list[Path]) -> frozenset[str]:
    extra: set[str] = set()
    for f in files:
        try:
            content = f.read_text(encoding="utf-8")
            extra.update(extract_identifiers(content))
        except (OSError, UnicodeDecodeError):
            continue
    return concepts | frozenset(extra)


def _empty_tree(root_dir: Path) -> dict[str, Any]:
    return {
        "name": root_dir.name,
        "type": "diff_context",
        "fragment_count": 0,
        "fragments": [],
    }
