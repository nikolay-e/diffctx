from __future__ import annotations

import logging
import os
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from ..ignore import get_ignore_specs, get_whitelist_spec
from . import git as _git
from .fragmentation import _process_files_for_fragments
from .git import CatFileBatch, split_diff_range
from .mode import PipelineConfig, ScoringMode
from .scoring import BM25Discovery, DefaultDiscovery, DiscoveryContext, DiscoveryStrategy, EnsembleDiscovery
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
from .utility import concepts_from_diff_text

logger = logging.getLogger(__name__)

_MAX_CACHE_BYTES = 200 * 1024 * 1024


def _build_preferred_revs(base_rev: str | None, head_rev: str | None) -> list[str]:
    revs: list[str] = []
    if head_rev:
        revs.append(head_rev)
    if base_rev and base_rev != head_rev:
        revs.append(base_rev)
    return revs


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


def _read_one_cached(f: Path) -> tuple[Path, str, int] | None:
    try:
        if f.stat().st_size <= 100_000:
            content = f.read_text(encoding="utf-8")
            return f, content, len(content.encode("utf-8", errors="replace"))
    except (OSError, UnicodeDecodeError):
        pass
    return None


def _build_file_cache(candidate_files: list[Path]) -> dict[Path, str]:
    from concurrent.futures import ThreadPoolExecutor

    cache: dict[Path, str] = {}
    cache_bytes = 0

    with ThreadPoolExecutor(max_workers=2) as pool:
        for result in pool.map(_read_one_cached, candidate_files):
            if result is None:
                continue
            if cache_bytes > _MAX_CACHE_BYTES:
                break
            f, content, size = result
            cache[f] = content
            cache_bytes += size

    return cache


@dataclass
class DiscoveryResult:
    hunks: list[Any]
    diff_text: str
    changed_files: list[Path]
    discovered_files: list[Path]
    all_fragments: list[Fragment]
    preferred_revs: list[str]
    config: PipelineConfig
    timing: tuple[float, float, float]


def run_discovery(
    root_dir: Path,
    diff_range: str,
    ignore_file: Path | None,
    no_default_ignores: bool,
    whitelist_file: Path | None,
    scoring_mode: str,
) -> DiscoveryResult | None:
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
        return None

    diff_text = _git.get_diff_text(root_dir, diff_range)
    expansion_concepts = concepts_from_diff_text(diff_text)
    if untracked:
        expansion_concepts = _enrich_concepts(expansion_concepts, untracked)

    if head_rev and not os.environ.get("DIFFCTX_NO_COMMIT_SIGNAL"):
        commit_msg = _git.get_commit_message(root_dir, head_rev)
        if commit_msg:
            msg_concepts = concepts_from_diff_text(commit_msg)
            expansion_concepts = frozenset(expansion_concepts | msg_concepts)

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

    return DiscoveryResult(
        hunks=hunks,
        diff_text=diff_text,
        changed_files=changed_files,
        discovered_files=discovered_files,
        all_fragments=all_fragments,
        preferred_revs=preferred_revs,
        config=config,
        timing=(t0, t1, t2),
    )
