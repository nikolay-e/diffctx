from __future__ import annotations

import logging
import subprocess
from collections import defaultdict
from pathlib import Path

import pathspec

from ..ignore import is_whitelisted, should_ignore
from .config import LIMITS
from .git import GitError
from .languages import get_language_for_file
from .types import DiffHunk, extract_identifiers

logger = logging.getLogger(__name__)

# Access git functions via module to support monkeypatching in tests
from . import git as _git  # noqa: E402

_RARE_THRESHOLD = LIMITS.rare_identifier_threshold
_MAX_EXPANSION_FILES = LIMITS.max_expansion_files
_FALLBACK_MAX_FILES = 10_000
_MAX_DISCOVERED_FILES = LIMITS.max_discovered_files
_MAX_FILE_SIZE = LIMITS.max_file_size
_MAX_CANDIDATE_FILES = LIMITS.max_candidate_files
_MIN_CONCEPT_LENGTH = 4

_BUILD_SYSTEM_NAMES = frozenset(
    {
        "cmakelists.txt",
        "makefile",
        "gnumakefile",
        "meson.build",
        "build.gradle",
        "build.gradle.kts",
        "pom.xml",
        "build.xml",
        "premake5.lua",
        "justfile",
        "taskfile.yml",
        "taskfile.yaml",
        "rakefile",
    }
)

_BUILD_SYSTEM_EXTENSIONS = frozenset(
    {
        ".mk",
        ".mak",
        ".make",
        ".cmake",
        ".bzl",
        ".ninja",
    }
)

_PACKAGE_MANIFEST_NAMES = frozenset(
    {
        "package.json",
        "package-lock.json",
        "yarn.lock",
        "pnpm-lock.yaml",
        "cargo.toml",
        "cargo.lock",
        "pyproject.toml",
        "setup.py",
        "setup.cfg",
        "go.mod",
        "go.sum",
        "gemfile",
        "gemfile.lock",
        "composer.json",
        "composer.lock",
        "pubspec.yaml",
        "pubspec.lock",
    }
)


def _normalize_path(path: Path, root_dir: Path) -> Path:
    if path.is_absolute():
        return path.resolve()
    return (root_dir / path).resolve()


def _is_allowed_file(path: Path) -> bool:
    return get_language_for_file(str(path)) is not None


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


def _prioritize_candidates(
    candidates: list[Path],
    changed_files: set[Path],
) -> list[Path]:
    changed_dirs: set[Path] = set()
    changed_extensions: set[str] = set()
    for f in changed_files:
        changed_dirs.add(f.parent)
        if f.parent.parent != f.parent:
            changed_dirs.add(f.parent.parent)
        if f.suffix:
            changed_extensions.add(f.suffix.lower())

    priority: list[Path] = []
    rest: list[Path] = []
    for c in candidates:
        if c.parent in changed_dirs or c.suffix.lower() in changed_extensions:
            priority.append(c)
        else:
            rest.append(c)

    budget = _MAX_CANDIDATE_FILES - len(priority)
    if budget > 0:
        priority.extend(rest[:budget])
    return priority[:_MAX_CANDIDATE_FILES]


def _collect_candidate_files(
    root_dir: Path, included_set: set[Path], combined_spec: pathspec.PathSpec
) -> tuple[list[Path], bool]:
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
            candidates = [f for f in files if _is_candidate_file(f, root_dir, included_set, combined_spec)]
            is_large_repo = len(candidates) > _MAX_CANDIDATE_FILES
            if is_large_repo:
                logger.debug(
                    "diffctx: %d candidates exceed cap %d, prioritizing by proximity",
                    len(candidates),
                    _MAX_CANDIDATE_FILES,
                )
                candidates = _prioritize_candidates(candidates, included_set)
            return candidates, is_large_repo
    except (subprocess.SubprocessError, OSError):
        pass
    logger.warning("diffctx: git ls-files failed, falling back to rglob (limit %d files)", _FALLBACK_MAX_FILES)
    fallback: list[Path] = []
    for f in root_dir.rglob("*"):
        if len(fallback) >= _FALLBACK_MAX_FILES:
            logger.warning("diffctx: fallback scan hit limit, results may be incomplete")
            break
        if _is_candidate_file(f, root_dir, included_set, combined_spec):
            fallback.append(f)
    return fallback, False


def _path_distance(a: Path, b: Path) -> int:
    a_parts = a.parent.parts
    b_parts = b.parent.parts
    common = 0
    for x, y in zip(a_parts, b_parts):
        if x != y:
            break
        common += 1
    return (len(a_parts) - common) + (len(b_parts) - common)


def _build_ident_index(
    files: list[Path],
    concepts: frozenset[str],
    changed_files: list[Path] | None = None,
) -> dict[str, list[Path]]:
    if changed_files:
        changed_dirs = {f.parent for f in changed_files}

        def sort_key(p: Path) -> tuple[int, int, str]:
            in_same_dir = 0 if p.parent in changed_dirs else 1
            min_dist = min((_path_distance(p, cf) for cf in changed_files), default=0)
            return (in_same_dir, min_dist, str(p))

        prioritized = sorted(files, key=sort_key)[:2000]
    else:
        prioritized = sorted(files)[:2000]

    inverted_index: dict[str, list[Path]] = defaultdict(list)
    for file_path in prioritized:
        try:
            content = file_path.read_text(encoding="utf-8")
            file_idents = extract_identifiers(content, skip_stopwords=False)
            for ident in file_idents:
                if ident in concepts:
                    inverted_index[ident].append(file_path)
        except (OSError, UnicodeDecodeError):
            continue
    return inverted_index


def _is_build_or_manifest(path: Path) -> bool:
    name_lower = path.name.lower()
    return (
        name_lower in _BUILD_SYSTEM_NAMES
        or name_lower in _PACKAGE_MANIFEST_NAMES
        or path.suffix.lower() in _BUILD_SYSTEM_EXTENSIONS
    )


def _collect_expansion_files(
    inverted_index: dict[str, list[Path]], concepts: frozenset[str], included_set: set[Path]
) -> list[Path]:
    rare_concepts = [
        c for c in concepts if len(c) >= _MIN_CONCEPT_LENGTH and 0 < len(inverted_index.get(c, [])) <= _RARE_THRESHOLD
    ]
    expansion_files: set[Path] = set()

    for concept in rare_concepts:
        for file_path in inverted_index.get(concept, []):
            if file_path not in included_set and not _is_build_or_manifest(file_path):
                expansion_files.add(file_path)
                if len(expansion_files) >= _MAX_EXPANSION_FILES:
                    return list(expansion_files)

    return list(expansion_files)


def _filter_whitelist(
    files: list[Path],
    root_dir: Path,
    wl_spec: pathspec.PathSpec | None,
) -> list[Path]:
    if wl_spec is None:
        return files
    result: list[Path] = []
    for file_path in files:
        try:
            rel_path = file_path.relative_to(root_dir).as_posix()
            if is_whitelisted(rel_path, wl_spec):
                result.append(file_path)
        except ValueError:
            pass
    return result


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
    candidate_files: list[Path] | None = None,
    changed_files: list[Path] | None = None,
) -> list[Path]:
    if not concepts:
        return []

    included_set = set(already_included)
    if candidate_files is not None:
        files = [f for f in candidate_files if f not in included_set]
    else:
        files, _ = _collect_candidate_files(root_dir, included_set, combined_spec)
    inverted_index = _build_ident_index(files, concepts, changed_files=changed_files)
    return _collect_expansion_files(inverted_index, concepts, included_set)


def _resolve_changed_files(
    root_dir: Path,
    diff_range: str,
    untracked: list[Path],
    combined_spec: pathspec.PathSpec,
    wl_spec: pathspec.PathSpec | None,
) -> list[Path]:
    changed_files = _git.get_changed_files(root_dir, diff_range)
    changed_files = [_normalize_path(p, root_dir) for p in changed_files]
    changed_files.extend(untracked)
    changed_files = _filter_ignored(changed_files, root_dir, combined_spec)
    changed_files = _filter_whitelist(changed_files, root_dir, wl_spec)

    renamed_old, pure_rename_new = _git.get_renamed_paths(root_dir, diff_range)
    excluded_paths = _git.get_deleted_files(root_dir, diff_range) | renamed_old | pure_rename_new
    if excluded_paths:
        changed_files = [f for f in changed_files if f.resolve() not in excluded_paths]
    return changed_files


def _discover_untracked_files(root_dir: Path, combined_spec: pathspec.PathSpec) -> list[Path]:
    try:
        raw = _git.get_untracked_files(root_dir)
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
