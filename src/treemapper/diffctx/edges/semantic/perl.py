from __future__ import annotations

import re
from collections import defaultdict
from pathlib import Path

from ...config.weights import EDGE_WEIGHTS
from ...types import Fragment, FragmentId
from ..base import EdgeBuilder, EdgeDict, FragmentIndex, discover_files_by_refs

_PERL_EXTS = {".pl", ".pm", ".t"}

_USE_RE = re.compile(r"^\s*use\s+([\w:]{2,200})", re.MULTILINE)
_REQUIRE_RE = re.compile(r"""^\s*require\s+['"]([^'"]{1,300})['"]""", re.MULTILINE)

_SUB_RE = re.compile(r"^\s*sub\s+([a-zA-Z_]\w*)", re.MULTILINE)
_PACKAGE_RE = re.compile(r"^\s*package\s+([\w:]+)", re.MULTILINE)
_ISA_RE = re.compile(r"our\s+@ISA\s*=\s*(?:qw\s*)?[\(\[]([^)\]]+)", re.MULTILINE)
_USE_BASE_RE = re.compile(r"^\s*use\s+(?:base|parent)\s+(?:qw\s*)?[\('\[]([^)\]']+)", re.MULTILINE)
_EXTENDS_RE = re.compile(r"^\s*extends\s+['\"]?([\w:]+)", re.MULTILINE)

_METHOD_CALL_RE = re.compile(r"(\w+)->(\w+)\s*\(")
_FUNC_CALL_RE = re.compile(r"(?<!\w)([a-z_]\w{1,100})\s*\(")
_QUALIFIED_CALL_RE = re.compile(r"([\w:]+)::(\w+)\s*\(")

_PERL_KEYWORDS = frozenset(
    {
        "if",
        "elsif",
        "else",
        "unless",
        "while",
        "until",
        "for",
        "foreach",
        "do",
        "sub",
        "my",
        "our",
        "local",
        "return",
        "next",
        "last",
        "redo",
        "die",
        "warn",
        "exit",
        "print",
        "say",
        "chomp",
        "chop",
        "push",
        "pop",
        "shift",
        "unshift",
        "splice",
        "split",
        "join",
        "map",
        "grep",
        "sort",
        "reverse",
        "keys",
        "values",
        "each",
        "exists",
        "delete",
        "defined",
        "open",
        "close",
        "read",
        "write",
        "seek",
        "tell",
        "ref",
        "bless",
        "new",
        "eval",
        "require",
        "use",
        "package",
        "BEGIN",
        "END",
        "AUTOLOAD",
        "DESTROY",
        "length",
        "substr",
        "index",
        "rindex",
        "sprintf",
        "chr",
        "ord",
        "hex",
        "oct",
        "int",
        "abs",
    }
)

_PERL_BUILTIN_MODULES = frozenset(
    {
        "strict",
        "warnings",
        "utf8",
        "constant",
        "vars",
        "lib",
        "base",
        "parent",
        "Exporter",
        "Carp",
        "Data::Dumper",
        "File::Basename",
        "File::Path",
        "File::Spec",
        "File::Find",
        "Getopt::Long",
        "POSIX",
        "Scalar::Util",
        "List::Util",
    }
)

_DIFF_USE_RE = re.compile(r"^\+\s*use\s+([\w:]{2,200})", re.MULTILINE)
_DIFF_REQUIRE_RE = re.compile(r"""^\+\s*require\s+['"]([^'"]{1,300})['"]""", re.MULTILINE)


def _is_perl_file(path: Path) -> bool:
    return path.suffix.lower() in _PERL_EXTS


def _extract_refs(content: str) -> tuple[set[str], set[str]]:
    uses: set[str] = set()
    requires: set[str] = set()

    for m in _USE_RE.finditer(content):
        module = m.group(1)
        if module not in _PERL_BUILTIN_MODULES and module.lower() not in {
            "strict",
            "warnings",
            "utf8",
            "constant",
            "vars",
            "lib",
            "base",
            "parent",
        }:
            uses.add(module)

    for m in _REQUIRE_RE.finditer(content):
        requires.add(m.group(1))

    return uses, requires


def _extract_definitions(content: str) -> tuple[set[str], set[str]]:
    funcs: set[str] = set()
    for m in _SUB_RE.finditer(content):
        name = m.group(1)
        if name not in _PERL_KEYWORDS and len(name) >= 2:
            funcs.add(name)

    packages: set[str] = set()
    packages.update(m.group(1) for m in _PACKAGE_RE.finditer(content))

    return funcs, packages


def _extract_inheritance(content: str) -> set[str]:
    parents: set[str] = set()
    for m in _ISA_RE.finditer(content):
        for name in re.split(r"[\s,]+", m.group(1).strip()):
            name = name.strip("'\"")
            if name and name not in _PERL_BUILTIN_MODULES:
                parents.add(name)
    for m in _USE_BASE_RE.finditer(content):
        for name in re.split(r"[\s,]+", m.group(1).strip()):
            name = name.strip("'\"")
            if name and name not in _PERL_BUILTIN_MODULES:
                parents.add(name)
    for m in _EXTENDS_RE.finditer(content):
        parents.add(m.group(1))
    return parents


def _extract_references(content: str) -> tuple[set[str], set[tuple[str, str]], set[tuple[str, str]]]:
    func_calls = {m.group(1) for m in _FUNC_CALL_RE.finditer(content) if m.group(1) not in _PERL_KEYWORDS}

    method_calls: set[tuple[str, str]] = set()
    for m in _METHOD_CALL_RE.finditer(content):
        method_calls.add((m.group(1), m.group(2)))

    qualified_calls: set[tuple[str, str]] = set()
    for m in _QUALIFIED_CALL_RE.finditer(content):
        qualified_calls.add((m.group(1), m.group(2)))

    return func_calls, method_calls, qualified_calls


def _module_to_filename(module: str) -> str:
    return module.split("::")[-1].lower()


def _module_to_path(module: str) -> str:
    return module.replace("::", "/").lower()


class PerlEdgeBuilder(EdgeBuilder):
    weight = 0.60
    use_weight = EDGE_WEIGHTS["perl_use"].forward
    require_weight = EDGE_WEIGHTS["perl_require"].forward
    fn_weight = EDGE_WEIGHTS["perl_fn"].forward
    method_weight = EDGE_WEIGHTS["perl_method"].forward
    inheritance_weight = EDGE_WEIGHTS["perl_inheritance"].forward
    reverse_weight_factor = EDGE_WEIGHTS["perl_use"].reverse_factor

    def needs_from_diff(self, diff_content: str) -> list[str]:
        refs: list[str] = []
        for m in _DIFF_USE_RE.finditer(diff_content):
            module = m.group(1)
            if module.lower() not in {"strict", "warnings", "utf8", "constant", "vars", "lib", "base", "parent"}:
                refs.append(_module_to_filename(module))
        for m in _DIFF_REQUIRE_RE.finditer(diff_content):
            refs.append(m.group(1).split("/")[-1].lower())
        return refs

    def discover_related_files(
        self,
        changed_files: list[Path],
        all_candidate_files: list[Path],
        repo_root: Path | None = None,
    ) -> list[Path]:
        perl_changed = [f for f in changed_files if _is_perl_file(f)]
        if not perl_changed:
            return []

        refs: set[str] = set()
        for f in perl_changed:
            try:
                content = f.read_text(encoding="utf-8")
                uses, requires = _extract_refs(content)
                for module in uses:
                    refs.add(_module_to_filename(module))
                for req in requires:
                    refs.add(req.split("/")[-1].lower())
            except (OSError, UnicodeDecodeError):
                continue

        return discover_files_by_refs(refs, changed_files, all_candidate_files, repo_root)

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        perl_frags = [f for f in fragments if _is_perl_file(f.path)]
        if not perl_frags:
            return {}

        edges: EdgeDict = {}
        idx = FragmentIndex(fragments, repo_root)
        fn_defs, pkg_defs = self._build_indices(perl_frags)

        for pf in perl_frags:
            self._add_fragment_edges(pf, idx, fn_defs, pkg_defs, edges)

        return edges

    def _build_indices(self, perl_frags: list[Fragment]) -> tuple[dict[str, list[FragmentId]], dict[str, list[FragmentId]]]:
        fn_defs: dict[str, list[FragmentId]] = defaultdict(list)
        pkg_defs: dict[str, list[FragmentId]] = defaultdict(list)

        for f in perl_frags:
            funcs, packages = _extract_definitions(f.content)
            for fn in funcs:
                fn_defs[fn.lower()].append(f.id)
            for pkg in packages:
                pkg_defs[_module_to_filename(pkg)].append(f.id)
                pkg_defs[pkg.lower()].append(f.id)

        return fn_defs, pkg_defs

    def _add_fragment_edges(
        self,
        pf: Fragment,
        idx: FragmentIndex,
        fn_defs: dict[str, list[FragmentId]],
        pkg_defs: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        self._add_import_edges(pf, idx, edges)
        self._add_inheritance_edges(pf, pkg_defs, edges)
        self._add_call_edges(pf, fn_defs, pkg_defs, edges)

    def _add_import_edges(self, pf: Fragment, idx: FragmentIndex, edges: EdgeDict) -> None:
        uses, requires = _extract_refs(pf.content)
        for module in uses:
            self._link_module(pf.id, module, idx, edges, self.use_weight)
        for req in requires:
            self._link_ref(pf.id, req, idx, edges, self.require_weight)

    def _add_inheritance_edges(
        self,
        pf: Fragment,
        pkg_defs: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        for parent in _extract_inheritance(pf.content):
            for fid in pkg_defs.get(_module_to_filename(parent), []):
                if fid != pf.id:
                    self.add_edge(edges, pf.id, fid, self.inheritance_weight)

    def _add_call_edges(
        self,
        pf: Fragment,
        fn_defs: dict[str, list[FragmentId]],
        pkg_defs: dict[str, list[FragmentId]],
        edges: EdgeDict,
    ) -> None:
        func_calls, method_calls, qualified_calls = _extract_references(pf.content)
        self_funcs, _ = _extract_definitions(pf.content)
        self_fn_lower = {fn.lower() for fn in self_funcs}

        for func_call in func_calls:
            if func_call.lower() not in self_fn_lower:
                for fid in fn_defs.get(func_call.lower(), []):
                    if fid != pf.id:
                        self.add_edge(edges, pf.id, fid, self.fn_weight)

        self._add_method_edges(pf, fn_defs, pkg_defs, method_calls, edges)
        self._add_qualified_edges(pf, pkg_defs, qualified_calls, edges)

    def _add_method_edges(
        self,
        pf: Fragment,
        fn_defs: dict[str, list[FragmentId]],
        pkg_defs: dict[str, list[FragmentId]],
        method_calls: set[tuple[str, str]],
        edges: EdgeDict,
    ) -> None:
        for obj, method in method_calls:
            for fid in fn_defs.get(method.lower(), []):
                if fid != pf.id:
                    self.add_edge(edges, pf.id, fid, self.method_weight)
            for fid in pkg_defs.get(obj.lower(), []):
                if fid != pf.id:
                    self.add_edge(edges, pf.id, fid, self.method_weight)

    def _add_qualified_edges(
        self,
        pf: Fragment,
        pkg_defs: dict[str, list[FragmentId]],
        qualified_calls: set[tuple[str, str]],
        edges: EdgeDict,
    ) -> None:
        for pkg, _func in qualified_calls:
            for fid in pkg_defs.get(_module_to_filename(pkg), []):
                if fid != pf.id:
                    self.add_edge(edges, pf.id, fid, self.use_weight)

    def _link_module(
        self,
        src_id: FragmentId,
        module: str,
        idx: FragmentIndex,
        edges: EdgeDict,
        weight: float,
    ) -> None:
        filename = _module_to_filename(module)
        for name, frag_ids in idx.by_name.items():
            stem = name.replace(".pm", "").replace(".pl", "")
            if stem == filename:
                for fid in frag_ids:
                    if fid != src_id:
                        self.add_edge(edges, src_id, fid, weight)
                        return

        path_hint = _module_to_path(module)
        self.link_by_path_match(src_id, path_hint, idx, edges, weight)

    def _link_ref(
        self,
        src_id: FragmentId,
        ref: str,
        idx: FragmentIndex,
        edges: EdgeDict,
        weight: float,
    ) -> None:
        ref_name = ref.split("/")[-1].lower()
        for name, frag_ids in idx.by_name.items():
            if name == ref_name:
                for fid in frag_ids:
                    if fid != src_id:
                        self.add_edge(edges, src_id, fid, weight)
                        return

        self.link_by_path_match(src_id, ref, idx, edges, weight)
