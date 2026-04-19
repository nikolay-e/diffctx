from __future__ import annotations

import logging
import re
from collections import defaultdict
from pathlib import Path

import tree_sitter_java
import tree_sitter_scala
from tree_sitter import Language, Node, Parser, Tree

from ...config.weights import EDGE_WEIGHTS
from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict

logger = logging.getLogger(__name__)

_JAVA_LANG = Language(tree_sitter_java.language())
_SCALA_LANG = Language(tree_sitter_scala.language())

_DISCOVERY_MAX_DEPTH = 2

_JAVA_EXTS = {".java"}
_KOTLIN_EXTS = {".kt", ".kts"}
_SCALA_EXTS = {".scala", ".sc"}
_JVM_EXTS = _JAVA_EXTS | _KOTLIN_EXTS | _SCALA_EXTS

_KOTLIN_IMPORT_RE = re.compile(
    r"^\s*import\s+([a-z][a-z0-9_]*(?:\.[a-z][a-z0-9_]*)*(?:\.[A-Z]\w*|\.\*)?)",
    re.MULTILINE,
)
_KOTLIN_CLASS_RE = re.compile(
    r"^\s*(?:\w+\s+)*(?:class|interface|object|enum)\s+([A-Z]\w*)",
    re.MULTILINE,
)
_KOTLIN_FUN_RE = re.compile(
    r"^\s*(?:\w+\s+)*fun\s+(?:<[^>]+>\s+)?([a-zA-Z_]\w*)",
    re.MULTILINE,
)
_JAVA_PACKAGE_RE = re.compile(r"^\s*package\s+([a-z][a-z0-9_]*(?:\.[a-z][a-z0-9_]*)*)", re.MULTILINE)

_TYPE_REF_RE = re.compile(r"(?<![a-z_])([A-Z]\w*)\b")
_KOTLIN_DECL_RE = re.compile(r"(?:class|interface|object)\s+\w+")
_SCALA_WITH_RE = re.compile(r"\bwith\s+([A-Z]\w*)")

_ANNOTATION_RE = re.compile(r"@([A-Z]\w*)")

_JVM_STDLIB_TYPES: frozenset[str] = frozenset(
    {
        "String",
        "Integer",
        "Long",
        "Double",
        "Float",
        "Boolean",
        "Byte",
        "Short",
        "Character",
        "Object",
        "Class",
        "System",
        "Math",
        "Collections",
        "Arrays",
        "Optional",
        "HashMap",
        "ArrayList",
        "LinkedList",
        "Iterator",
        "Iterable",
        "Comparable",
        "Runnable",
        "Thread",
        "Exception",
        "RuntimeException",
        "IllegalArgumentException",
        "IllegalStateException",
        "NullPointerException",
        "IndexOutOfBoundsException",
        "IOException",
        "InputStream",
        "OutputStream",
        "StringBuilder",
        "StringBuffer",
        "Number",
        "Enum",
        "Void",
        "Override",
        "Unit",
        "Any",
        "AnyVal",
        "AnyRef",
        "Nothing",
        "Option",
        "Some",
        "Either",
        "Left",
        "Right",
        "Try",
        "Success",
        "Failure",
        "Future",
        "Promise",
        "Seq",
        "Vector",
        "Map",
        "Set",
        "Tuple",
        "Function",
        "Product",
        "Serializable",
        "Pair",
        "Triple",
        "Sequence",
    }
)


def _is_jvm_file(path: Path) -> bool:
    return path.suffix.lower() in _JVM_EXTS


def _is_java(path: Path) -> bool:
    return path.suffix.lower() in _JAVA_EXTS


def _is_kotlin(path: Path) -> bool:
    return path.suffix.lower() in _KOTLIN_EXTS


def _is_scala(path: Path) -> bool:
    return path.suffix.lower() in _SCALA_EXTS


def _parse_tree(content: str, path: Path) -> Tree | None:
    if _is_java(path):
        parser = Parser(_JAVA_LANG)
        return parser.parse(content.encode())
    if _is_scala(path):
        parser = Parser(_SCALA_LANG)
        return parser.parse(content.encode())
    return None


def _collect_nodes(root: Node, target_types: set[str]) -> list[Node]:
    result: list[Node] = []
    stack = [root]
    while stack:
        node = stack.pop()
        if node.type in target_types:
            result.append(node)
        else:
            stack.extend(node.children)
    return result


def _node_text(node: Node) -> str:
    return node.text.decode()  # type: ignore[no-any-return]


def _extract_java_imports(root: Node) -> set[str]:
    imports: set[str] = set()
    for imp_node in _collect_nodes(root, {"import_declaration"}):
        has_asterisk = False
        scoped_text = None
        for child in imp_node.children:
            if child.type == "scoped_identifier":
                scoped_text = _node_text(child)
            elif child.type == "asterisk":
                has_asterisk = True
        if scoped_text:
            if has_asterisk:
                imports.add(f"{scoped_text}.*")
            else:
                imports.add(scoped_text)
    return imports


def _extract_scala_imports(root: Node) -> set[str]:
    imports: set[str] = set()
    for imp_node in _collect_nodes(root, {"import_declaration"}):
        parts: list[str] = []
        has_wildcard = False
        has_selectors = False
        selector_names: list[str] = []
        for child in imp_node.children:
            if child.type == "identifier":
                parts.append(_node_text(child))
            elif child.type == "namespace_wildcard":
                has_wildcard = True
            elif child.type == "namespace_selectors":
                has_selectors = True
                for sel_child in child.children:
                    if sel_child.type == "identifier":
                        selector_names.append(_node_text(sel_child))
        if not parts:
            continue
        base = ".".join(parts)
        if has_wildcard:
            imports.add(f"{base}.*")
        elif has_selectors:
            for name in selector_names:
                imports.add(f"{base}.{name}")
        else:
            imports.add(base)
    return imports


def _extract_imports(content: str, path: Path) -> set[str]:
    tree = _parse_tree(content, path)
    if tree is not None:
        if _is_java(path):
            return _extract_java_imports(tree.root_node)
        return _extract_scala_imports(tree.root_node)
    if _is_kotlin(path):
        return {m.group(1) for m in _KOTLIN_IMPORT_RE.finditer(content)}
    return set()


def _extract_package(content: str, path: Path | None = None) -> str | None:
    if path is not None:
        tree = _parse_tree(content, path)
        if tree is not None:
            if _is_java(path):
                for node in _collect_nodes(tree.root_node, {"package_declaration"}):
                    for child in node.children:
                        if child.type == "scoped_identifier":
                            return _node_text(child)
            elif _is_scala(path):
                for node in _collect_nodes(tree.root_node, {"package_clause"}):
                    for child in node.children:
                        if child.type == "package_identifier":
                            return _node_text(child)
    match = _JAVA_PACKAGE_RE.search(content)
    return match.group(1) if match else None


def _identifier_child(node: Node) -> str | None:
    for child in node.children:
        if child.type == "identifier":
            return _node_text(child)
    return None


def _extract_java_classes(root: Node) -> set[str]:
    target_types = {"class_declaration", "interface_declaration", "enum_declaration", "record_declaration"}
    classes: set[str] = set()
    for node in _collect_nodes(root, target_types):
        name = _identifier_child(node)
        if name:
            classes.add(name)
    return classes


def _extract_scala_classes(root: Node) -> set[str]:
    target_types = {"class_definition", "trait_definition", "object_definition"}
    classes: set[str] = set()
    for node in _collect_nodes(root, target_types):
        name = _identifier_child(node)
        if name:
            classes.add(name)
    return classes


def _extract_classes(content: str, path: Path) -> set[str]:
    tree = _parse_tree(content, path)
    if tree is not None:
        if _is_java(path):
            return _extract_java_classes(tree.root_node)
        return _extract_scala_classes(tree.root_node)
    if _is_kotlin(path):
        return {m.group(1) for m in _KOTLIN_CLASS_RE.finditer(content)}
    return set()


def _type_identifier_from_node(node: Node) -> str | None:
    if node.type == "type_identifier":
        return _node_text(node)
    if node.type == "generic_type":
        for child in node.children:
            if child.type == "type_identifier":
                return _node_text(child)
    return None


def _collect_type_refs_from_list(type_list_node: Node) -> set[str]:
    refs: set[str] = set()
    for child in type_list_node.children:
        name = _type_identifier_from_node(child)
        if name:
            refs.add(name)
    return refs


def _extract_java_inheritance(root: Node) -> set[str]:
    refs: set[str] = set()
    class_types = {"class_declaration", "interface_declaration", "enum_declaration", "record_declaration"}
    for node in _collect_nodes(root, class_types):
        for child in node.children:
            if child.type == "superclass":
                for sc_child in child.children:
                    name = _type_identifier_from_node(sc_child)
                    if name:
                        refs.add(name)
            elif child.type in ("super_interfaces", "extends_interfaces"):
                for tl_child in child.children:
                    if tl_child.type == "type_list":
                        refs.update(_collect_type_refs_from_list(tl_child))
    return refs


def _extract_scala_inheritance(root: Node) -> set[str]:
    refs: set[str] = set()
    class_types = {"class_definition", "trait_definition", "object_definition"}
    for node in _collect_nodes(root, class_types):
        for child in node.children:
            if child.type == "extends_clause":
                for ec_child in child.children:
                    name = _type_identifier_from_node(ec_child)
                    if name:
                        refs.add(name)
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
    tree = _parse_tree(content, path)
    if tree is not None:
        if _is_java(path):
            return _extract_java_inheritance(tree.root_node)
        return _extract_scala_inheritance(tree.root_node)
    return set()


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

    def discover_related_files(
        self,
        changed_files: list[Path],
        all_candidate_files: list[Path],
        repo_root: Path | None = None,
        **kwargs: object,
    ) -> list[Path]:
        jvm_changed = [f for f in changed_files if _is_jvm_file(f)]
        if not jvm_changed:
            return []

        changed_set = set(changed_files)
        jvm_candidates = [f for f in all_candidate_files if _is_jvm_file(f) and f not in changed_set]
        discovered_set: set[Path] = set()
        frontier: list[Path] = list(jvm_changed)

        for _depth in range(_DISCOVERY_MAX_DEPTH):
            hop_result = self._discover_single_hop(frontier, jvm_candidates, repo_root)
            new_files = [f for f in hop_result if f not in discovered_set]
            if not new_files:
                break
            discovered_set.update(new_files)
            frontier = new_files

        return list(discovered_set)

    @staticmethod
    def _collect_source_refs(source_files: list[Path]) -> tuple[set[str], set[str]]:
        type_refs: set[str] = set()
        import_packages: set[str] = set()
        for f in source_files:
            try:
                content = f.read_text(encoding="utf-8")
                type_refs.update(_extract_type_refs(content))
                for imp in _extract_imports(content, f):
                    if imp.endswith(".*"):
                        import_packages.add(imp[:-2])
                    else:
                        parts = imp.rsplit(".", 1)
                        if len(parts) == 2:
                            import_packages.add(parts[0])
            except (OSError, UnicodeDecodeError):
                logger.debug("skipping unreadable file: %s", f)
        return type_refs, import_packages

    @staticmethod
    def _compute_import_dirs(repo_root: Path | None, import_packages: set[str]) -> set[Path]:
        import_dirs: set[Path] = set()
        if repo_root and import_packages:
            for pkg in import_packages:
                pkg_path = repo_root / Path(*pkg.split("."))
                import_dirs.add(pkg_path)
                for src_prefix in ("src/main/java", "src/main/kotlin", "src/main/scala"):
                    import_dirs.add(repo_root / src_prefix / Path(*pkg.split(".")))
        return import_dirs

    @staticmethod
    def _collect_frontier_classes(source_files: list[Path]) -> set[str]:
        frontier_classes: set[str] = set()
        for f in source_files:
            try:
                content = f.read_text(encoding="utf-8")
                frontier_classes.update(_extract_classes(content, f))
            except (OSError, UnicodeDecodeError):
                logger.debug("skipping unreadable file: %s", f)
        return frontier_classes

    @staticmethod
    def _candidate_matches_frontier(
        candidate: Path,
        eligible_dirs: set[Path],
        type_refs: set[str],
        frontier_classes: set[str],
    ) -> bool:
        try:
            content = candidate.read_text(encoding="utf-8")
            cand_classes = _extract_classes(content, candidate)
            if candidate.parent in eligible_dirs and cand_classes & type_refs:
                return True
            if _extract_type_refs(content) & frontier_classes:
                return True
            return any(imp.rsplit(".", 1)[-1] in frontier_classes for imp in _extract_imports(content, candidate))
        except (OSError, UnicodeDecodeError):
            logger.debug("skipping unreadable file: %s", candidate)
            return False

    def _discover_single_hop(
        self,
        source_files: list[Path],
        candidates: list[Path],
        repo_root: Path | None,
    ) -> list[Path]:
        type_refs, import_packages = self._collect_source_refs(source_files)
        eligible_dirs = {f.parent for f in source_files} | self._compute_import_dirs(repo_root, import_packages)
        source_set = set(source_files)
        frontier_classes = self._collect_frontier_classes(source_files)

        return [
            c
            for c in candidates
            if c not in source_set and self._candidate_matches_frontier(c, eligible_dirs, type_refs, frontier_classes)
        ]

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
            pkg = _extract_package(f.content, f.path)
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

        self._link_imports(jf, fqn_to_frags, class_to_frags, edges, package_to_frags)
        self._link_refs(jf, class_to_frags, edges)
        self._link_same_package(jf, package_to_frags, edges)

    def _link_imports(
        self,
        jf: Fragment,
        fqn_to_frags: dict[str, list[FragmentId]],
        class_to_frags: dict[str, list[FragmentId]],
        edges: EdgeDict,
        package_to_frags: dict[str, list[FragmentId]] | None = None,
    ) -> None:
        for imp in _extract_imports(jf.content, jf.path):
            if imp.endswith(".*"):
                self._link_wildcard_import(jf, imp, package_to_frags, edges)
            else:
                self._link_explicit_import(jf, imp, fqn_to_frags, class_to_frags, edges)

    def _link_wildcard_import(
        self,
        jf: Fragment,
        imp: str,
        package_to_frags: dict[str, list[FragmentId]] | None,
        edges: EdgeDict,
    ) -> None:
        if package_to_frags is None:
            return
        pkg_prefix = imp[:-2]
        for fid in package_to_frags.get(pkg_prefix, []):
            if fid != jf.id:
                self.add_edge(edges, jf.id, fid, self.import_weight)

    def _link_explicit_import(
        self,
        jf: Fragment,
        imp: str,
        fqn_to_frags: dict[str, list[FragmentId]],
        class_to_frags: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        for fid in fqn_to_frags.get(imp.lower(), []):
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
        self._link_inheritance_refs(jf, class_to_frags, edges)
        self._link_type_refs(jf, class_to_frags, edges)
        self._link_annotation_refs(jf, class_to_frags, edges)

    def _link_inheritance_refs(self, jf: Fragment, class_to_frags: dict[str, list[FragmentId]], edges: EdgeDict) -> None:
        for inh_ref in _extract_inheritance(jf.content, jf.path):
            for fid in class_to_frags.get(inh_ref.lower(), []):
                if fid != jf.id:
                    self.add_edge(edges, jf.id, fid, self.inheritance_weight)

    def _link_type_refs(self, jf: Fragment, class_to_frags: dict[str, list[FragmentId]], edges: EdgeDict) -> None:
        for type_ref in _extract_type_refs(jf.content):
            if type_ref not in _JVM_STDLIB_TYPES:
                for fid in class_to_frags.get(type_ref.lower(), []):
                    if fid != jf.id:
                        self.add_edge(edges, jf.id, fid, self.type_weight)

    def _link_annotation_refs(self, jf: Fragment, class_to_frags: dict[str, list[FragmentId]], edges: EdgeDict) -> None:
        for ann_ref in _extract_annotations(jf.content):
            for fid in class_to_frags.get(ann_ref.lower(), []):
                if fid != jf.id:
                    self.add_edge(edges, jf.id, fid, self.annotation_weight)

    def _link_same_package(
        self,
        jf: Fragment,
        package_to_frags: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        current_pkg = _extract_package(jf.content, jf.path)
        if not current_pkg:
            return
        for fid in package_to_frags.get(current_pkg, []):
            if fid != jf.id:
                self.add_edge(edges, jf.id, fid, self.same_package_weight)
