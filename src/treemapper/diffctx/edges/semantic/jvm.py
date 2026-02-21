from __future__ import annotations

import re
from collections import defaultdict
from pathlib import Path

from ...config.weights import EDGE_WEIGHTS
from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict

_JAVA_EXTS = {".java"}
_KOTLIN_EXTS = {".kt", ".kts"}
_SCALA_EXTS = {".scala", ".sc"}
_JVM_EXTS = _JAVA_EXTS | _KOTLIN_EXTS | _SCALA_EXTS

_JAVA_IMPORT_RE = re.compile(r"^\s*import\s+(?:static\s+)?([a-z][a-z0-9_]*(?:\.[a-z][a-z0-9_]*)*(?:\.[A-Z]\w*)?)", re.MULTILINE)
_JAVA_PACKAGE_RE = re.compile(r"^\s*package\s+([a-z][a-z0-9_]*(?:\.[a-z][a-z0-9_]*)*)", re.MULTILINE)
_JAVA_CLASS_RE = re.compile(
    r"^\s*(?:public |private |protected )?(?:abstract |final )?(?:class|interface|enum|record)\s+([A-Z]\w*)", re.MULTILINE
)
_JAVA_EXTENDS_RE = re.compile(r"\bextends\s+([A-Z]\w*(?:\s*,\s*[A-Z]\w*)*)")
_JAVA_IMPLEMENTS_RE = re.compile(r"\bimplements\s+([A-Z]\w*(?:\s*,\s*[A-Z]\w*)*)")

_KOTLIN_IMPORT_RE = re.compile(r"^\s*import\s+([a-z][a-z0-9_]*(?:\.[a-z][a-z0-9_]*)*(?:\.[A-Z]\w*)?)", re.MULTILINE)
_KOTLIN_CLASS_RE = re.compile(
    r"^\s*(?:\w+\s+)*(?:class|interface|object|enum)\s+([A-Z]\w*)",
    re.MULTILINE,
)
_KOTLIN_FUN_RE = re.compile(
    r"^\s*(?:\w+\s+)*fun\s+(?:<[^>]+>\s+)?([a-z]\w*)",
    re.MULTILINE,
)

_SCALA_IMPORT_RE = re.compile(r"^\s*import\s+([a-z][a-z0-9_]*(?:\.[a-z][a-z0-9_]*)*(?:\.[A-Z_]\w*)?)", re.MULTILINE)
_SCALA_CLASS_RE = re.compile(
    r"^\s*(?:(?:abstract|sealed|final|case|implicit|private|protected)\s+)*(?:class|trait|object)\s+([A-Z]\w*)",
    re.MULTILINE,
)
_SCALA_DEF_RE = re.compile(r"^\s*(?:private |protected )?def\s+([a-z]\w*)", re.MULTILINE)

_TYPE_REF_RE = re.compile(r"(?<![a-z_])([A-Z]\w*)\b")
_KOTLIN_DECL_RE = re.compile(r"(?:class|interface|object)\s+\w+")
_SCALA_WITH_RE = re.compile(r"\bwith\s+([A-Z]\w*)")

_ANNOTATION_RE = re.compile(r"@([A-Z]\w*)")


def _is_jvm_file(path: Path) -> bool:
    return path.suffix.lower() in _JVM_EXTS


def _is_java(path: Path) -> bool:
    return path.suffix.lower() in _JAVA_EXTS


def _is_kotlin(path: Path) -> bool:
    return path.suffix.lower() in _KOTLIN_EXTS


def _is_scala(path: Path) -> bool:
    return path.suffix.lower() in _SCALA_EXTS


def _extract_imports(content: str, path: Path) -> set[str]:
    if _is_java(path):
        return {m.group(1) for m in _JAVA_IMPORT_RE.finditer(content)}
    elif _is_kotlin(path):
        return {m.group(1) for m in _KOTLIN_IMPORT_RE.finditer(content)}
    elif _is_scala(path):
        return {m.group(1) for m in _SCALA_IMPORT_RE.finditer(content)}
    return set()


def _extract_package(content: str) -> str | None:
    match = _JAVA_PACKAGE_RE.search(content)
    return match.group(1) if match else None


def _extract_classes(content: str, path: Path) -> set[str]:
    classes: set[str] = set()
    if _is_java(path):
        classes.update(m.group(1) for m in _JAVA_CLASS_RE.finditer(content))
    elif _is_kotlin(path):
        classes.update(m.group(1) for m in _KOTLIN_CLASS_RE.finditer(content))
    elif _is_scala(path):
        classes.update(m.group(1) for m in _SCALA_CLASS_RE.finditer(content))
    return classes


def _split_class_list(regex: re.Pattern[str], content: str) -> set[str]:
    refs: set[str] = set()
    for m in regex.finditer(content):
        for cls in m.group(1).split(","):
            stripped = cls.strip()
            if stripped:
                refs.add(stripped)
    return refs


def _find_kotlin_colon(text: str) -> int | None:
    depth = 0
    for i, ch in enumerate(text):
        if ch in "<(":
            depth += 1
        elif ch in ">)":
            depth = max(0, depth - 1)
        elif ch == ":" and depth == 0:
            return i
        elif ch in "{\n":
            return None
    return None


def _extract_kotlin_supertypes(content: str) -> set[str]:
    refs: set[str] = set()
    for m in _KOTLIN_DECL_RE.finditer(content):
        rest = content[m.end() :]
        colon_pos = _find_kotlin_colon(rest)
        if colon_pos is None:
            continue
        after = rest[colon_pos + 1 :]
        for i, ch in enumerate(after):
            if ch in "{\n":
                after = after[:i]
                break
        for tm in _TYPE_REF_RE.finditer(after):
            refs.add(tm.group(1))
    return refs


def _extract_inheritance(content: str, path: Path) -> set[str]:
    if _is_kotlin(path):
        return _extract_kotlin_supertypes(content)
    if _is_scala(path):
        refs = _split_class_list(_JAVA_EXTENDS_RE, content)
        refs.update(m.group(1) for m in _SCALA_WITH_RE.finditer(content))
        return refs
    refs = _split_class_list(_JAVA_EXTENDS_RE, content)
    refs.update(_split_class_list(_JAVA_IMPLEMENTS_RE, content))
    return refs


def _extract_type_refs(content: str) -> set[str]:
    return {m.group(1) for m in _TYPE_REF_RE.finditer(content)}


def _extract_annotations(content: str) -> set[str]:
    return {m.group(1) for m in _ANNOTATION_RE.finditer(content)}


class JVMEdgeBuilder(EdgeBuilder):
    weight = 0.70
    import_weight = EDGE_WEIGHTS["jvm_import"].forward
    inheritance_weight = EDGE_WEIGHTS["jvm_inheritance"].forward
    type_weight = EDGE_WEIGHTS["jvm_type"].forward
    same_package_weight = EDGE_WEIGHTS["jvm_same_package"].forward
    annotation_weight = EDGE_WEIGHTS["jvm_annotation"].forward
    reverse_weight_factor = EDGE_WEIGHTS["jvm_import"].reverse_factor

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        jvm_frags = [f for f in fragments if _is_jvm_file(f.path)]
        if not jvm_frags:
            return {}

        edges: EdgeDict = {}
        indices = self._build_indices(jvm_frags)

        for jf in jvm_frags:
            self._link_fragment(jf, indices, edges)

        return edges

    def _build_indices(
        self, jvm_frags: list[Fragment]
    ) -> tuple[dict[str, list[FragmentId]], dict[str, list[FragmentId]], dict[str, list[FragmentId]]]:
        package_to_frags: dict[str, list[FragmentId]] = defaultdict(list)
        class_to_frags: dict[str, list[FragmentId]] = defaultdict(list)
        fqn_to_frags: dict[str, list[FragmentId]] = defaultdict(list)

        for f in jvm_frags:
            pkg = _extract_package(f.content)
            if pkg:
                package_to_frags[pkg].append(f.id)

            for cls in _extract_classes(f.content, f.path):
                class_to_frags[cls.lower()].append(f.id)
                if pkg:
                    fqn_to_frags[f"{pkg}.{cls}".lower()].append(f.id)

        return package_to_frags, class_to_frags, fqn_to_frags

    def _link_fragment(
        self,
        jf: Fragment,
        indices: tuple[dict[str, list[FragmentId]], dict[str, list[FragmentId]], dict[str, list[FragmentId]]],
        edges: EdgeDict,
    ) -> None:
        package_to_frags, class_to_frags, fqn_to_frags = indices

        self._link_imports(jf, fqn_to_frags, class_to_frags, edges)
        self._link_refs(jf, class_to_frags, edges)
        self._link_same_package(jf, package_to_frags, edges)

    def _link_imports(
        self,
        jf: Fragment,
        fqn_to_frags: dict[str, list[FragmentId]],
        class_to_frags: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        for imp in _extract_imports(jf.content, jf.path):
            imp_lower = imp.lower()
            for fid in fqn_to_frags.get(imp_lower, []):
                if fid != jf.id:
                    self.add_edge(edges, jf.id, fid, self.import_weight)

            parts = imp.split(".")
            if parts:
                for fid in class_to_frags.get(parts[-1].lower(), []):
                    if fid != jf.id:
                        self.add_edge(edges, jf.id, fid, self.import_weight)

    def _link_refs(
        self,
        jf: Fragment,
        class_to_frags: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        ref_weights = [
            (_extract_inheritance(jf.content, jf.path), self.inheritance_weight),
            (_extract_type_refs(jf.content), self.type_weight),
            (_extract_annotations(jf.content), self.annotation_weight),
        ]

        for refs, weight in ref_weights:
            for ref in refs:
                for fid in class_to_frags.get(ref.lower(), []):
                    if fid != jf.id:
                        self.add_edge(edges, jf.id, fid, weight)

    def _link_same_package(
        self,
        jf: Fragment,
        package_to_frags: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        current_pkg = _extract_package(jf.content)
        if not current_pkg:
            return
        for fid in package_to_frags.get(current_pkg, []):
            if fid != jf.id:
                self.add_edge(edges, jf.id, fid, self.same_package_weight)
