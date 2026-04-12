from __future__ import annotations

import re
from collections import defaultdict
from pathlib import Path

from ...config.weights import EDGE_WEIGHTS
from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict, FragmentIndex, discover_files_by_refs

_CLOJURE_EXTS = {".clj", ".cljs", ".cljc", ".edn"}

_CLOJURE_NS_REQUIRE_RE = re.compile(
    r"\(:?require\s[^)]{0,2000}?\[([a-z][\w.-]{0,200})",
)
_CLOJURE_REQUIRE_RE = re.compile(
    r"\(require\s{1,10}['\"]?([a-z][\w.-]{0,200})",
)

_DEFN_RE = re.compile(r"^\s*\(defn-?\s+([a-z_][\w*+!?<>=-]{0,100})", re.MULTILINE)
_DEF_RE = re.compile(r"^\s*\(def\s+([a-z_][\w*+!?<>=-]{0,100})", re.MULTILINE)
_DEFPROTOCOL_RE = re.compile(r"^\s*\(defprotocol\s+([A-Z]\w{0,100})", re.MULTILINE)
_DEFRECORD_RE = re.compile(r"^\s*\(defrecord\s+([A-Z]\w{0,100})", re.MULTILINE)
_DEFTYPE_RE = re.compile(r"^\s*\(deftype\s+([A-Z]\w{0,100})", re.MULTILINE)
_DEFMULTI_RE = re.compile(r"^\s*\(defmulti\s+([a-z_][\w*+!?<>=-]{0,100})", re.MULTILINE)
_EXTEND_PROTOCOL_RE = re.compile(r"\(extend-(?:protocol|type)\s+([A-Z]\w{0,100})", re.MULTILINE)
_IMPLEMENTS_RE = re.compile(r"\((?:reify|proxy)\s+\[?([A-Z]\w{0,100})", re.MULTILINE)

_FUNC_CALL_RE = re.compile(r"\(([a-z_][\w*+!?<>=-]{1,100})\b")
_TYPE_REF_RE = re.compile(r"(?<![a-z_])([A-Z]\w{1,100})\b")
_QUALIFIED_CALL_RE = re.compile(r"\(([a-z][\w.-]{0,100})/([a-z_][\w*+!?<>=-]{1,100})")

_CLOJURE_KEYWORDS = frozenset(
    {
        "if",
        "do",
        "let",
        "fn",
        "def",
        "defn",
        "defn-",
        "defmacro",
        "loop",
        "recur",
        "when",
        "cond",
        "case",
        "and",
        "or",
        "not",
        "throw",
        "try",
        "catch",
        "finally",
        "new",
        "set!",
        "quote",
        "var",
        "import",
        "require",
        "use",
        "ns",
        "in-ns",
        "refer",
        "println",
        "print",
        "str",
        "prn",
        "pr",
        "pr-str",
        "first",
        "rest",
        "cons",
        "conj",
        "assoc",
        "dissoc",
        "get",
        "nth",
        "count",
        "map",
        "filter",
        "reduce",
        "apply",
        "comp",
        "partial",
        "into",
        "seq",
        "vec",
        "list",
        "hash-map",
        "sorted-map",
        "atom",
        "deref",
        "swap!",
        "reset!",
        "compare-and-set!",
        "nil",
        "true",
        "false",
    }
)

_DIFF_REQUIRE_RE = re.compile(r"^\+.*\[:?require\s[^]]{0,2000}?\[([a-z][\w.-]{0,200})", re.MULTILINE)
_DIFF_REQUIRE_SIMPLE_RE = re.compile(r"^\+\s*\(require\s{1,10}['\"]?([a-z][\w.-]{0,200})", re.MULTILINE)


def _is_clojure_file(path: Path) -> bool:
    return path.suffix.lower() in _CLOJURE_EXTS


def _extract_requires(content: str) -> set[str]:
    refs: set[str] = set()
    for m in _CLOJURE_NS_REQUIRE_RE.finditer(content):
        refs.add(m.group(1))
    for m in _CLOJURE_REQUIRE_RE.finditer(content):
        refs.add(m.group(1))
    return refs


def _extract_definitions(content: str) -> tuple[set[str], set[str]]:
    funcs: set[str] = set()
    for m in _DEFN_RE.finditer(content):
        funcs.add(m.group(1))
    for m in _DEF_RE.finditer(content):
        funcs.add(m.group(1))
    for m in _DEFMULTI_RE.finditer(content):
        funcs.add(m.group(1))

    types: set[str] = set()
    types.update(m.group(1) for m in _DEFPROTOCOL_RE.finditer(content))
    types.update(m.group(1) for m in _DEFRECORD_RE.finditer(content))
    types.update(m.group(1) for m in _DEFTYPE_RE.finditer(content))

    return funcs, types


def _extract_protocol_refs(content: str) -> set[str]:
    refs: set[str] = set()
    for m in _EXTEND_PROTOCOL_RE.finditer(content):
        refs.add(m.group(1))
    for m in _IMPLEMENTS_RE.finditer(content):
        refs.add(m.group(1))
    return refs


def _extract_references(content: str) -> tuple[set[str], set[str], set[str]]:
    func_calls = {m.group(1) for m in _FUNC_CALL_RE.finditer(content) if m.group(1) not in _CLOJURE_KEYWORDS}

    type_refs = {m.group(1) for m in _TYPE_REF_RE.finditer(content)}

    qualified_ns: set[str] = set()
    for m in _QUALIFIED_CALL_RE.finditer(content):
        qualified_ns.add(m.group(1))

    return func_calls, type_refs, qualified_ns


def _namespace_to_path(ns: str) -> str:
    return ns.replace(".", "/").replace("-", "_")


def _namespace_leaf(ns: str) -> str:
    return ns.rsplit(".", maxsplit=1)[-1].lower().replace("-", "_")


def _collect_clojure_refs(clojure_files: list[Path]) -> set[str]:
    refs: set[str] = set()
    for cf in clojure_files:
        try:
            content = cf.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
        for ns in _extract_requires(content):
            refs.add(_namespace_to_path(ns))
            refs.add(_namespace_leaf(ns))
    return refs


class ClojureEdgeBuilder(EdgeBuilder):
    weight = 0.60
    require_weight = EDGE_WEIGHTS["clojure_require"].forward
    fn_weight = EDGE_WEIGHTS["clojure_fn"].forward
    protocol_weight = EDGE_WEIGHTS["clojure_protocol"].forward
    reverse_weight_factor = EDGE_WEIGHTS["clojure_require"].reverse_factor

    def needs_from_diff(self, diff_content: str) -> list[str]:
        refs: list[str] = []
        for m in _DIFF_REQUIRE_RE.finditer(diff_content):
            refs.append(_namespace_to_path(m.group(1)))
            refs.append(_namespace_leaf(m.group(1)))
        for m in _DIFF_REQUIRE_SIMPLE_RE.finditer(diff_content):
            refs.append(_namespace_to_path(m.group(1)))
            refs.append(_namespace_leaf(m.group(1)))
        return refs

    def discover_related_files(
        self,
        changed_files: list[Path],
        all_candidate_files: list[Path],
        repo_root: Path | None = None,
        **kwargs: object,
    ) -> list[Path]:
        clojure_files = [f for f in changed_files if _is_clojure_file(f)]
        if not clojure_files:
            return []

        refs = _collect_clojure_refs(clojure_files)
        return discover_files_by_refs(refs, changed_files, all_candidate_files, repo_root)

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        clojure_frags = [f for f in fragments if _is_clojure_file(f.path)]
        if not clojure_frags:
            return {}

        edges: EdgeDict = {}
        idx = FragmentIndex(fragments, repo_root)
        fn_defs, type_defs = self._build_indices(clojure_frags)

        for cf in clojure_frags:
            self._add_fragment_edges(cf, idx, fn_defs, type_defs, edges)

        return edges

    def _build_indices(self, clojure_frags: list[Fragment]) -> tuple[dict[str, list[FragmentId]], dict[str, list[FragmentId]]]:
        fn_defs: dict[str, list[FragmentId]] = defaultdict(list)
        type_defs: dict[str, list[FragmentId]] = defaultdict(list)

        for f in clojure_frags:
            funcs, types = _extract_definitions(f.content)
            for fn in funcs:
                fn_defs[fn.lower()].append(f.id)
            for t in types:
                type_defs[t.lower()].append(f.id)

        return fn_defs, type_defs

    def _link_requires(self, cf: Fragment, idx: FragmentIndex, edges: EdgeDict) -> None:
        for ns in _extract_requires(cf.content):
            self.link_by_stem(cf.id, _namespace_leaf(ns), idx, edges, self.require_weight)
            self.link_by_path_match(cf.id, _namespace_to_path(ns), idx, edges, self.require_weight)

    def _link_protocol_refs(
        self,
        cf: Fragment,
        type_defs: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        for proto in _extract_protocol_refs(cf.content):
            for fid in type_defs.get(proto.lower(), []):
                if fid != cf.id:
                    self.add_edge(edges, cf.id, fid, self.protocol_weight)

    def _link_fn_calls(
        self,
        cf: Fragment,
        fn_defs: dict[str, list[FragmentId]],
        self_fn_lower: set[str],
        edges: EdgeDict,
    ) -> None:
        func_calls, _, _ = _extract_references(cf.content)
        for func_call in func_calls:
            if func_call.lower() in self_fn_lower:
                continue
            for fid in fn_defs.get(func_call.lower(), []):
                if fid != cf.id:
                    self.add_edge(edges, cf.id, fid, self.fn_weight)

    def _link_type_refs(
        self,
        cf: Fragment,
        type_defs: dict[str, list[FragmentId]],
        self_type_lower: set[str],
        edges: EdgeDict,
    ) -> None:
        _, type_refs, _ = _extract_references(cf.content)
        for type_ref in type_refs:
            if type_ref.lower() in self_type_lower:
                continue
            for fid in type_defs.get(type_ref.lower(), []):
                if fid != cf.id:
                    self.add_edge(edges, cf.id, fid, self.protocol_weight)

    def _link_qualified_ns(self, cf: Fragment, idx: FragmentIndex, edges: EdgeDict) -> None:
        _, _, qualified_ns = _extract_references(cf.content)
        for ns in qualified_ns:
            self.link_by_stem(cf.id, _namespace_leaf(ns), idx, edges, self.require_weight)

    def _add_fragment_edges(
        self,
        cf: Fragment,
        idx: FragmentIndex,
        fn_defs: dict[str, list[FragmentId]],
        type_defs: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        self._link_requires(cf, idx, edges)
        self._link_protocol_refs(cf, type_defs, edges)

        self_funcs, self_types = _extract_definitions(cf.content)
        self_fn_lower = {fn.lower() for fn in self_funcs}
        self_type_lower = {t.lower() for t in self_types}

        self._link_fn_calls(cf, fn_defs, self_fn_lower, edges)
        self._link_type_refs(cf, type_defs, self_type_lower, edges)
        self._link_qualified_ns(cf, idx, edges)
