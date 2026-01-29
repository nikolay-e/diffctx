from __future__ import annotations

import re
from collections import defaultdict
from collections.abc import Callable
from pathlib import Path

from ...config import EDGE_WEIGHTS
from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict, path_to_module

_IMPORT_RE = re.compile(r"(?:from\s+([\w.]+)\s+import|import\s+([\w.]+))")

_JAVA_EXT = ".java"
_SCALA_EXT = ".scala"


def _is_python_test(name: str) -> bool:
    return name.startswith("test_") or name.endswith("_test.py")


def _is_js_test(name: str, path_str: str) -> bool:
    return ".test." in name or ".spec." in name or "__tests__" in path_str


def _is_rust_test(name: str, path_str: str) -> bool:
    return "/tests/" in path_str or name == "tests.rs"


def _is_jvm_test(name: str) -> bool:
    stem = name.rsplit(".", 1)[0] if "." in name else name
    return stem.endswith("test") or stem.startswith("test")


_TEST_DETECTORS: dict[str, Callable[[str, str], bool]] = {
    ".py": lambda name, _path_str: _is_python_test(name),
    ".js": _is_js_test,
    ".ts": _is_js_test,
    ".jsx": _is_js_test,
    ".tsx": _is_js_test,
    ".rs": _is_rust_test,
    _JAVA_EXT: lambda name, _path_str: _is_jvm_test(name),
    ".kt": lambda name, _path_str: _is_jvm_test(name),
    ".kts": lambda name, _path_str: _is_jvm_test(name),
    _SCALA_EXT: lambda name, _path_str: _is_jvm_test(name),
}


def _is_test_file(path: Path) -> bool:
    name = path.name.lower()
    path_str = str(path).lower()
    suffix = path.suffix.lower()

    detector = _TEST_DETECTORS.get(suffix)
    if detector is not None and detector(name, path_str):
        return True

    return "/tests/" in path_str or "/test/" in path_str


def _has_direct_import(test_frag: Fragment, src_frag: Fragment, repo_root: Path | None) -> bool:
    src_module = path_to_module(src_frag.path, repo_root=repo_root)
    if not src_module:
        return False
    for match in _IMPORT_RE.finditer(test_frag.content):
        imported = match.group(1) or match.group(2)
        if imported and (imported == src_module or imported.endswith(f".{src_module}")):
            return True
    return False


def _extract_target_name_from_test(test_name: str) -> str | None:
    if test_name.startswith("test_"):
        return test_name[5:]
    if test_name.endswith("_test"):
        return test_name[:-5]
    if ".test" in test_name:
        return test_name.split(".test")[0]
    if ".spec" in test_name:
        return test_name.split(".spec")[0]
    return None


class TestEdgeBuilder(EdgeBuilder):
    weight_direct = EDGE_WEIGHTS["test_direct"].forward
    weight_naming = EDGE_WEIGHTS["test_naming"].forward
    reverse_weight_factor = EDGE_WEIGHTS["test_reverse"].forward / EDGE_WEIGHTS["test_direct"].forward

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        edges: EdgeDict = {}

        by_base: dict[str, list[Fragment]] = defaultdict(list)
        test_frags: list[Fragment] = []

        for f in fragments:
            if _is_test_file(f.path):
                test_frags.append(f)
            else:
                by_base[f.path.stem.lower()].append(f)

        for test_frag in test_frags:
            target_name = _extract_target_name_from_test(test_frag.path.stem.lower())
            if not target_name:
                continue

            for src_frag in by_base.get(target_name, []):
                self._add_test_source_edge(edges, test_frag, src_frag, repo_root)

        return edges

    def _add_test_source_edge(self, edges: EdgeDict, test_frag: Fragment, src_frag: Fragment, repo_root: Path | None) -> None:
        weight = self.weight_direct if _has_direct_import(test_frag, src_frag, repo_root) else self.weight_naming
        edges[(test_frag.id, src_frag.id)] = weight
        edges[(src_frag.id, test_frag.id)] = EDGE_WEIGHTS["test_reverse"].forward


def _build_test_edges(fragments: list[Fragment]) -> dict[tuple[FragmentId, FragmentId], float]:
    return TestEdgeBuilder().build(fragments)
