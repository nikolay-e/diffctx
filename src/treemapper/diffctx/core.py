from __future__ import annotations

import logging
from collections import defaultdict
from pathlib import Path

from .types import DiffHunk, Fragment, FragmentId

logger = logging.getLogger(__name__)

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

_CONTAINER_FRAGMENT_KINDS = frozenset({"class", "interface", "struct"})


def _kind_priority(kind: str) -> int:
    return 0 if kind in _SEMANTIC_KINDS else 1


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

    before = [f for f in frags if f.end_line < h_start]
    after = [f for f in frags if f.start_line > h_end]
    if before:
        core.add(max(before, key=lambda f: f.end_line).id)
    if after:
        core.add(min(after, key=lambda f: f.start_line).id)

    return core


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
                if core_id.path == path and frag.start_line <= core_id.start_line and core_id.end_line <= frag.end_line:
                    headers_to_add.append(frag.id)
                    break
    core_ids.update(headers_to_add)


def _map_hunks_to_fragments(
    hunks: list[DiffHunk],
    core_ids: set[FragmentId],
    all_fragments: list[Fragment],
) -> dict[FragmentId, float]:
    result: dict[FragmentId, float] = {}
    for h in hunks:
        h_start, h_end = h.core_selection_range
        hunk_size = max(1, h_end - h_start + 1)
        for frag in all_fragments:
            if frag.id not in core_ids or frag.path != h.path:
                continue
            if frag.start_line <= h_end and frag.end_line >= h_start:
                result[frag.id] = result.get(frag.id, 0) + hunk_size
    return result


def _add_container_weights(
    frag_hunk_lines: dict[FragmentId, float],
    core_ids: set[FragmentId],
    all_fragments: list[Fragment],
) -> None:
    for frag in all_fragments:
        if frag.id not in core_ids or frag.id in frag_hunk_lines:
            continue
        if frag.kind not in _CONTAINER_FRAGMENT_KINDS:
            continue
        contained_weight = sum(
            w
            for fid, w in frag_hunk_lines.items()
            if fid.path == frag.path and frag.start_line <= fid.start_line and fid.end_line <= frag.end_line
        )
        if contained_weight > 0:
            frag_hunk_lines[frag.id] = contained_weight


def _best_hunk_size_for_path(hunks: list[DiffHunk], path: Path) -> int:
    best = 0
    for h in hunks:
        if h.path == path:
            h_start, h_end = h.core_selection_range
            best = max(best, h_end - h_start + 1)
    return best


def _fill_missing_core_weights(
    frag_hunk_lines: dict[FragmentId, float],
    core_ids: set[FragmentId],
    hunks: list[DiffHunk],
) -> None:
    for fid in core_ids:
        if fid in frag_hunk_lines:
            continue
        best = _best_hunk_size_for_path(hunks, fid.path)
        if best > 0:
            frag_hunk_lines[fid] = best


def _compute_seed_weights(
    hunks: list[DiffHunk],
    core_ids: set[FragmentId],
    all_fragments: list[Fragment],
) -> dict[FragmentId, float]:
    frag_hunk_lines = _map_hunks_to_fragments(hunks, core_ids, all_fragments)
    if not frag_hunk_lines:
        return {}

    _add_container_weights(frag_hunk_lines, core_ids, all_fragments)
    _fill_missing_core_weights(frag_hunk_lines, core_ids, hunks)

    return frag_hunk_lines
