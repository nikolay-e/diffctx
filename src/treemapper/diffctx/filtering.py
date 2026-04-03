from __future__ import annotations

import logging
from collections import defaultdict
from pathlib import Path

from .config.extensions import CODE_EXTENSIONS
from .graph import Graph
from .types import Fragment, FragmentId

logger = logging.getLogger(__name__)

_SAME_FILE_FLOOR = 0.01
_HUB_REVERSE_THRESHOLD = 3
_MAX_CONTEXT_FRAGMENTS_PER_FILE = 10
_LOW_RELEVANCE_THRESHOLD = 0.005


def _apply_same_file_floor(
    rel: dict[FragmentId, float],
    core_ids: set[FragmentId],
    fragments: list[Fragment],
) -> None:
    core_paths = {fid.path for fid in core_ids}
    for frag in fragments:
        if frag.id not in core_ids and frag.path in core_paths:
            if rel.get(frag.id, 0.0) < _SAME_FILE_FLOOR:
                rel[frag.id] = _SAME_FILE_FLOOR


def _classify_semantic_edges(
    graph: Graph,
    changed_paths: set[Path],
) -> tuple[dict[Path, set[Path]], set[Path]]:
    reverse_deps: dict[Path, set[Path]] = defaultdict(set)
    direct_edge_paths: set[Path] = set()
    for (src, dst), category in graph.edge_categories.items():
        if category != "semantic":
            continue
        src_changed = src.path in changed_paths
        dst_changed = dst.path in changed_paths
        if not (src_changed ^ dst_changed):
            continue

        changed_frag = src if src_changed else dst
        other_frag = dst if src_changed else src

        fwd_w = graph.adjacency.get(changed_frag, {}).get(other_frag, 0.0)
        rev_w = graph.adjacency.get(other_frag, {}).get(changed_frag, 0.0)

        if rev_w > fwd_w:
            reverse_deps[changed_frag.path].add(other_frag.path)
        else:
            direct_edge_paths.add(other_frag.path)
    return reverse_deps, direct_edge_paths


def _find_hub_noise_paths(
    graph: Graph,
    changed_paths: set[Path],
) -> set[Path]:
    reverse_deps, direct_edge_paths = _classify_semantic_edges(graph, changed_paths)

    noise_counts: dict[Path, int] = {}
    for deps in reverse_deps.values():
        if len(deps) > _HUB_REVERSE_THRESHOLD:
            for dep in deps:
                noise_counts[dep] = noise_counts.get(dep, 0) + 1
    return {p for p, count in noise_counts.items() if count >= 3 and p not in direct_edge_paths}


def _find_config_generic_code_files(
    graph: Graph,
    changed_paths: set[Path],
) -> set[Path]:
    has_real_edge: set[Path] = set()
    has_generic_config: set[Path] = set()
    generic_edge_count: dict[Path, int] = {}
    config_stems: set[str] = {p.stem.lower() for p in changed_paths}
    for (src, dst), category in graph.edge_categories.items():
        src_changed = src.path in changed_paths
        dst_changed = dst.path in changed_paths
        if not (src_changed ^ dst_changed):
            continue
        other_path = (dst if src_changed else src).path
        if category == "config_generic":
            has_generic_config.add(other_path)
            generic_edge_count[other_path] = generic_edge_count.get(other_path, 0) + 1
        elif category in ("semantic", "config"):
            has_real_edge.add(other_path)

    generic_only = has_generic_config - has_real_edge
    return {
        p
        for p in generic_only
        if p.suffix.lower() in CODE_EXTENSIONS and generic_edge_count.get(p, 0) <= 1 and p.stem.lower() not in config_stems
    }


def _filter_unrelated_fragments(
    fragments: list[Fragment],
    core_ids: set[FragmentId],
    graph: Graph,
) -> list[Fragment]:
    changed_paths = {fid.path for fid in core_ids}

    paths_to_remove = _find_hub_noise_paths(graph, changed_paths)
    paths_to_remove |= _find_config_generic_code_files(graph, changed_paths)
    paths_to_remove -= changed_paths

    if not paths_to_remove:
        return fragments

    kept = [f for f in fragments if f.path not in paths_to_remove]
    removed_count = len(fragments) - len(kept)
    if removed_count:
        logger.debug(
            "diffctx: filtered %d fragments from %d unrelated files",
            removed_count,
            len(paths_to_remove),
        )
    return kept


def _cap_context_fragments(
    fragments: list[Fragment],
    core_ids: set[FragmentId],
    rel: dict[FragmentId, float],
) -> list[Fragment]:
    changed_paths = {fid.path for fid in core_ids}

    ctx_by_path: dict[Path, list[Fragment]] = defaultdict(list)
    result: list[Fragment] = []

    for f in fragments:
        if f.path in changed_paths:
            result.append(f)
        else:
            ctx_by_path[f.path].append(f)

    for path, file_frags in ctx_by_path.items():
        if len(file_frags) <= _MAX_CONTEXT_FRAGMENTS_PER_FILE:
            result.extend(file_frags)
        else:
            file_frags.sort(key=lambda f: rel.get(f.id, 0.0), reverse=True)
            result.extend(file_frags[:_MAX_CONTEXT_FRAGMENTS_PER_FILE])
            logger.debug(
                "diffctx: capped %s from %d to %d fragments",
                path,
                len(file_frags),
                _MAX_CONTEXT_FRAGMENTS_PER_FILE,
            )

    return result


def _filter_low_relevance_fragments(
    fragments: list[Fragment],
    core_ids: set[FragmentId],
    rel: dict[FragmentId, float],
) -> list[Fragment]:
    changed_paths = {fid.path for fid in core_ids}
    kept = [f for f in fragments if f.path in changed_paths or rel.get(f.id, 0.0) >= _LOW_RELEVANCE_THRESHOLD]
    removed = len(fragments) - len(kept)
    if removed:
        logger.debug("diffctx: filtered %d low-relevance fragments (threshold=%.4f)", removed, _LOW_RELEVANCE_THRESHOLD)
    return kept
