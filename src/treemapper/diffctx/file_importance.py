from __future__ import annotations

from pathlib import Path

from .config.limits import UTILITY
from .fragmentation import _GENERATED_PATH_SEGMENTS
from .types import Fragment

_PERIPHERAL_DIRS = frozenset(
    {
        "examples",
        "example",
        "demo",
        "demos",
        "samples",
        "sample",
        "showcase",
        "docs",
        "doc",
        "documentation",
        "tutorials",
        "tutorial",
        "guides",
        "benchmarks",
        "benchmark",
        "perf",
        "bench",
        "playground",
        "sandbox",
        "scratch",
        "vendor",
        "third_party",
        "third-party",
        "node_modules",
        "external",
        "fixtures",
        "testdata",
        "test_data",
        "test-data",
        "__fixtures__",
        "__mocks__",
        "stories",
        "__stories__",
    }
)

_PERIPHERAL_STEMS = ("example_", "demo_", "sample_")
_PERIPHERAL_SUFFIXES = ("_example", "_demo", "_sample")


def _is_peripheral_file(path: Path) -> bool:
    parts_lower = {p.lower() for p in path.parts}
    if parts_lower & _PERIPHERAL_DIRS:
        return True
    stem = path.stem.lower()
    for prefix in _PERIPHERAL_STEMS:
        if stem.startswith(prefix):
            return True
    for suffix in _PERIPHERAL_SUFFIXES:
        if stem.endswith(suffix):
            return True
    return False


def _is_generated_path(path: Path) -> bool:
    parts_lower = {p.lower() for p in path.parts}
    return bool(parts_lower & _GENERATED_PATH_SEGMENTS)


def compute_file_importance(
    fragments: list[Fragment],
) -> dict[Path, float]:
    all_paths = {f.path for f in fragments}
    importance: dict[Path, float] = {}
    peripheral_cap = UTILITY.peripheral_cap

    for path in all_paths:
        if _is_generated_path(path):
            importance[path] = 0.10
        elif _is_peripheral_file(path):
            importance[path] = peripheral_cap
        else:
            importance[path] = 1.0

    return importance
