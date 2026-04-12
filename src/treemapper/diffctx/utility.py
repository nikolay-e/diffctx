# pylint: disable=duplicate-code
from __future__ import annotations

import math
import re
from dataclasses import dataclass, field, replace
from pathlib import Path
from typing import TYPE_CHECKING

from .config.limits import UTILITY
from .edges.structural.testing import _is_test_file
from .stopwords import _DOCS_STOPWORDS, CODE_STOPWORDS
from .tokenizer import extract_tokens
from .types import Fragment, FragmentId, extract_identifiers

_EXPANSION_STOPWORDS = CODE_STOPWORDS | _DOCS_STOPWORDS

if TYPE_CHECKING:
    from .graph import Graph

_CALL_RE = re.compile(r"(\w+)\s*\(")
_TYPE_REF_RE = re.compile(r"(?::|->)\s*([A-Z]\w+)")
_GENERIC_TYPE_RE = re.compile(r"[\[<,]\s*([A-Z]\w*)")

_TF_EXTENSIONS = frozenset({".tf", ".tfvars", ".hcl"})
_CONFIG_EXTENSIONS_FOR_DIFF = frozenset({".yaml", ".yml", ".json", ".toml", ".ini"})
_TF_VAR_NEED_RE = re.compile(r"var\.(\w+)")
_TF_RES_REF_NEED_RE = re.compile(r"(?<![.\w])([a-zA-Z]\w*)\.(\w+)(?:\[\*?\w*\])?\.[\w\[\]*]+")
_TF_SKIP_REF_TYPES = frozenset({"var", "local", "data", "module", "path", "terraform", "count", "each", "self"})

_LANGUAGE_BUILTINS: frozenset[str] = frozenset(
    {
        # Python builtins
        "range",
        "enumerate",
        "zip",
        "sorted",
        "reversed",
        "isinstance",
        "issubclass",
        "hasattr",
        "getattr",
        "setattr",
        "delattr",
        "callable",
        "iter",
        "next",
        "any",
        "all",
        "abs",
        "round",
        "pow",
        "divmod",
        "repr",
        "dir",
        "vars",
        "globals",
        "locals",
        "breakpoint",
        "property",
        "classmethod",
        "staticmethod",
        "dataclass",
        "object",
        # Python exceptions
        "exception",
        "baseexception",
        "valueerror",
        "typeerror",
        "keyerror",
        "indexerror",
        "attributeerror",
        "importerror",
        "runtimeerror",
        "stopiteration",
        "generatorexit",
        "oserror",
        "ioerror",
        "filenotfounderror",
        "permissionerror",
        "notimplementederror",
        "zerodivisionerror",
        "overflowerror",
        "memoryerror",
        "recursionerror",
        "unicodeerror",
        "assertionerror",
        "lookuperror",
        "arithmeticerror",
        # JavaScript / DOM APIs
        "array.from",
        "object.keys",
        "object.values",
        "object.entries",
        "array.isarray",
        "number.isnan",
        "number.isfinite",
        "parseint",
        "parsefloat",
        "isnan",
        "isfinite",
        "settimeout",
        "setinterval",
        "clearinterval",
        "cleartimeout",
        "requestanimationframe",
        "cancelanimationframe",
        "typeof",
        "void",
        # Go builtins
        "make",
        "append",
        "panic",
        "recover",
        "cap",
        "println",
        "printf",
        "sprintf",
        "fprintf",
        "errorf",
        # Rust — only distinctive, non-generic names
        "vec",
        "arc",
        "unwrap",
        # React hooks (distinctive use* naming convention)
        "usestate",
        "useeffect",
        "usecontext",
        "usereducer",
        "usecallback",
        "usememo",
        "useref",
        "uselayouteffect",
        "useimperativehandle",
        "usedebugvalue",
        "useid",
        "usetransition",
        "usedeferredvalue",
        "createcontext",
        "forwardref",
        "createref",
        "suspense",
        "strictmode",
        "profiler",
        # React Router / Redux / React Query hooks
        "usenavigate",
        "useparams",
        "uselocation",
        "usesearchparams",
        "useloaderdata",
        "useactiondata",
        "usefetcher",
        "useoutletcontext",
        "usedispatch",
        "useselector",
        "usestore",
        "usequery",
        "usemutation",
        "usesubscription",
        # Test framework globals
        "describe",
        "beforeeach",
        "aftereach",
        "beforeall",
        "afterall",
        "assert",
    }
)
_CLOSURE_MIN_EDGE_WEIGHT = 0.5
_INVARIANT_RE = re.compile(r"\b(?:assert|require|ensure|precondition|postcondition|invariant)\s*\(\s*(\w+)", re.IGNORECASE)

_COMMENT_PREFIXES = ("#", "//", "* ", "/*", "--", '"""', "'''", "<!--")

_JS_IMPORT_RE = re.compile(r"""import\s+\{([^}]+)\}\s+from\s+['"]([^'"]+)['"]""")
_PY_IMPORT_RE = re.compile(r"""from\s+(\S+)\s+import\s+(.+)""")


def _parse_import_names(names_str: str) -> set[str]:
    result: set[str] = set()
    for name in names_str.split(","):
        name = name.strip().split(" as ")[0].strip()
        if name:
            result.add(name.lower())
    return result


def _collect_external_symbols_from_lines(changed_lines: list[str]) -> frozenset[str]:
    symbols: set[str] = set()
    for line in changed_lines:
        for m in _JS_IMPORT_RE.finditer(line):
            js_names, js_source = m.group(1), m.group(2)
            if not js_source.startswith("."):
                symbols.update(_parse_import_names(js_names))
        for m in _PY_IMPORT_RE.finditer(line):
            py_module, py_names = m.group(1), m.group(2)
            if not py_module.startswith("."):
                symbols.update(_parse_import_names(py_names))
    return frozenset(symbols)


_JS_LOCAL_IMPORT_RE = re.compile(r"""import\s+(?:\{([^}]+)\}|([A-Z]\w+))\s+from\s+['"]([^'"]+)['"]""")


def _is_local_import(source: str) -> bool:
    return source.startswith(".") or source.startswith("@/") or source.startswith("~/")


def _collect_import_needs(
    changed_lines: list[str],
    needs: dict[tuple[str, str], InformationNeed],
) -> None:
    for line in changed_lines:
        for m in _JS_LOCAL_IMPORT_RE.finditer(line):
            named, default, source = m.group(1), m.group(2), m.group(3)
            if not _is_local_import(source):
                continue
            syms: set[str] = set()
            if named:
                syms = _parse_import_names(named)
            elif default:
                syms = {default.lower()}
            for sym in syms:
                if len(sym) >= 3 and sym not in CODE_STOPWORDS:
                    needs.setdefault(("definition", sym), InformationNeed("definition", sym, None, 0.9))
        for m in _PY_IMPORT_RE.finditer(line):
            module, names = m.group(1), m.group(2)
            if module.startswith("."):
                for sym in _parse_import_names(names):
                    if len(sym) >= 3 and sym not in CODE_STOPWORDS:
                        needs.setdefault(("definition", sym), InformationNeed("definition", sym, None, 0.9))


def _is_comment_line(line: str) -> bool:
    stripped = line.lstrip()
    return any(stripped.startswith(p) for p in _COMMENT_PREFIXES)


@dataclass(frozen=True)
class InformationNeed:
    need_type: str
    symbol: str
    scope: Path | None
    priority: float


def _defines_strength(scope_match: bool, has_scope: bool) -> float:
    if scope_match:
        return 1.0
    return 0.5 if not has_scope else 0.3


def _is_test_fragment(frag: Fragment) -> bool:
    if _is_test_file(frag.path):
        return True
    return frag.symbol_name is not None and frag.symbol_name.lower().startswith("test_")


def _match_strength_typed(frag: Fragment, need: InformationNeed) -> float:
    sym = need.symbol
    frag_sym = frag.symbol_name.lower() if frag.symbol_name else ""
    defines = frag_sym == sym and frag_sym != ""
    mentions = sym in frag.identifiers
    scope_match = need.scope is not None and need.scope == frag.path
    nt = need.need_type

    if nt == "impact" and scope_match:
        return 0.15
    if defines and "_signature" not in frag.kind:
        return _defines_strength(scope_match, need.scope is not None)
    if nt == "impact" and mentions and not defines:
        return 0.8
    if defines and ("_signature" in frag.kind or nt == "signature"):
        return 0.7
    if nt == "test" and mentions and _is_test_fragment(frag):
        return 0.6
    return 0.3 if mentions else 0.0


def _extract_changed_lines(diff_text: str) -> list[str]:
    result: list[str] = []
    for line in diff_text.splitlines():
        is_added = line.startswith("+") and not line.startswith("+++")
        is_removed = line.startswith("-") and not line.startswith("---")
        if is_added or is_removed:
            result.append(line[1:])
    return result


def concepts_from_diff_text(
    diff_text: str, profile: str = "code", *, use_nlp: bool = False, changed_lines: list[str] | None = None
) -> frozenset[str]:
    if changed_lines is None:
        changed_lines = _extract_changed_lines(diff_text)
    text = "\n".join(changed_lines)

    if use_nlp and profile != "code":
        return extract_tokens(text, profile=profile, use_nlp=True)

    return extract_identifiers(
        text,
        profile=profile,
        skip_stopwords=True,
        extra_stopwords=_EXPANSION_STOPWORDS | _LANGUAGE_BUILTINS,
        min_length=3,
    )


_CLOSURE_EDGE_CATEGORIES = frozenset({"structural", "semantic"})


def _eligible_neighbor_symbols(
    frag: Fragment,
    graph: Graph,
    frag_by_id: dict[FragmentId, Fragment],
    closure: set[str],
) -> set[str]:
    result: set[str] = set()
    for nbr_id, weight in graph.neighbors(frag.id).items():
        if weight < _CLOSURE_MIN_EDGE_WEIGHT:
            continue
        cat = graph.edge_categories.get((frag.id, nbr_id), "")
        if cat and cat not in _CLOSURE_EDGE_CATEGORIES:
            continue
        nbr = frag_by_id.get(nbr_id)
        if nbr and nbr.symbol_name and nbr.symbol_name.lower() not in closure:
            result.add(nbr.symbol_name.lower())
    return result


def _closure_expand_step(
    closure: set[str],
    frag_by_symbol: dict[str, list[Fragment]],
    frag_by_id: dict[FragmentId, Fragment],
    graph: Graph,
) -> set[str]:
    new_symbols: set[str] = set()
    for sym in closure:
        for frag in frag_by_symbol.get(sym, []):
            new_symbols.update(_eligible_neighbor_symbols(frag, graph, frag_by_id, closure))
    return new_symbols


def _apply_closure(
    sigma: set[str],
    all_fragments: list[Fragment],
    graph: Graph,
    closure_depth: int,
) -> set[str]:
    frag_by_symbol: dict[str, list[Fragment]] = {}
    for f in all_fragments:
        if f.symbol_name:
            frag_by_symbol.setdefault(f.symbol_name.lower(), []).append(f)

    frag_by_id: dict[FragmentId, Fragment] = {f.id: f for f in all_fragments}

    closure: set[str] = set(sigma)
    for _ in range(closure_depth):
        new_symbols = _closure_expand_step(closure, frag_by_symbol, frag_by_id, graph)
        if not new_symbols:
            break
        closure |= new_symbols

    return closure


def _collect_core_needs(
    all_fragments: list[Fragment],
    core_ids: set[FragmentId],
    needs: dict[tuple[str, str], InformationNeed],
) -> set[str]:
    core_symbol_names: set[str] = set()
    for frag in all_fragments:
        if frag.id not in core_ids or not frag.symbol_name:
            continue
        sym = frag.symbol_name.lower()
        core_symbol_names.add(sym)
        key = ("impact", sym)
        if key not in needs:
            needs[key] = InformationNeed("impact", sym, frag.path, 0.8)
    return core_symbol_names


def _process_line_for_needs(
    line: str,
    external_syms: frozenset[str],
    needs: dict[tuple[str, str], InformationNeed],
) -> None:
    for m in _CALL_RE.finditer(line):
        name = m.group(1)
        low = name.lower()
        if len(name) < 3 or low in CODE_STOPWORDS or low in _LANGUAGE_BUILTINS:
            continue
        if low in external_syms:
            continue
        needs.setdefault(("definition", low), InformationNeed("definition", low, None, 1.0))
    for m in _TYPE_REF_RE.finditer(line):
        sym = m.group(1).lower()
        needs.setdefault(("signature", sym), InformationNeed("signature", sym, None, 0.7))
    for m in _GENERIC_TYPE_RE.finditer(line):
        sym = m.group(1).lower()
        needs.setdefault(("signature", sym), InformationNeed("signature", sym, None, 0.7))


def _collect_diff_line_needs(
    diff_text: str,
    needs: dict[tuple[str, str], InformationNeed],
    changed_lines: list[str] | None = None,
) -> None:
    if changed_lines is None:
        changed_lines = _extract_changed_lines(diff_text)
    external_syms = _collect_external_symbols_from_lines(changed_lines)
    for line in changed_lines:
        if not _is_comment_line(line):
            _process_line_for_needs(line, external_syms, needs)


def _collect_test_needs(
    all_fragments: list[Fragment],
    core_symbol_names: set[str],
    needs: dict[tuple[str, str], InformationNeed],
) -> None:
    for frag in all_fragments:
        if not _is_test_fragment(frag):
            continue
        tested = frag.symbol_name.lower().removeprefix("test_") if frag.symbol_name else None
        if tested and (tested in core_symbol_names or ("definition", tested) in needs):
            key = ("test", tested)
            if key not in needs:
                needs[key] = InformationNeed("test", tested, None, 0.6)


def _collect_invariant_needs(
    diff_text: str,
    needs: dict[tuple[str, str], InformationNeed],
    changed_lines: list[str] | None = None,
) -> None:
    if changed_lines is None:
        changed_lines = _extract_changed_lines(diff_text)
    for line in changed_lines:
        for m in _INVARIANT_RE.finditer(line):
            sym = m.group(1).lower()
            if len(sym) >= 3 and sym not in CODE_STOPWORDS:
                needs.setdefault(("invariant", sym), InformationNeed("invariant", sym, None, 0.85))


def _is_terraform_diff(all_fragments: list[Fragment], core_ids: set[FragmentId]) -> bool:
    return any(f.path.suffix.lower() in _TF_EXTENSIONS for f in all_fragments if f.id in core_ids)


def _is_config_only_diff(all_fragments: list[Fragment], core_ids: set[FragmentId]) -> bool:
    core_frags = [f for f in all_fragments if f.id in core_ids]
    return bool(core_frags) and all(f.path.suffix.lower() in _CONFIG_EXTENSIONS_FOR_DIFF for f in core_frags)


def _collect_config_context_needs(
    all_fragments: list[Fragment],
    core_ids: set[FragmentId],
    needs: dict[tuple[str, str], InformationNeed],
) -> None:
    covered = {n.symbol for n in needs.values()}
    for frag in all_fragments:
        if frag.id not in core_ids:
            continue
        for ident in frag.identifiers:
            if len(ident) >= 5 and ident not in covered:
                key = ("background", ident)
                if key not in needs:
                    needs[key] = InformationNeed("background", ident, None, 0.2)


def _collect_terraform_needs(
    diff_text: str,
    needs: dict[tuple[str, str], InformationNeed],
    changed_lines: list[str] | None = None,
) -> None:
    if changed_lines is None:
        changed_lines = _extract_changed_lines(diff_text)
    for line in changed_lines:
        for m in _TF_VAR_NEED_RE.finditer(line):
            sym = m.group(1).lower()
            if len(sym) >= 3 and sym not in CODE_STOPWORDS:
                needs.setdefault(("definition", sym), InformationNeed("definition", sym, None, 1.0))
        for m in _TF_RES_REF_NEED_RE.finditer(line):
            ref_type, ref_name = m.group(1).lower(), m.group(2).lower()
            if ref_type in _TF_SKIP_REF_TYPES:
                continue
            full_ref = f"{ref_type}.{ref_name}"
            needs.setdefault(("definition", full_ref), InformationNeed("definition", full_ref, None, 0.9))


def needs_from_diff(
    all_fragments: list[Fragment],
    core_ids: set[FragmentId],
    graph: Graph,
    diff_text: str,
    closure_depth: int = 1,
) -> tuple[InformationNeed, ...]:
    needs: dict[tuple[str, str], InformationNeed] = {}
    changed_lines = _extract_changed_lines(diff_text)

    core_symbol_names = _collect_core_needs(all_fragments, core_ids, needs)
    _collect_diff_line_needs(diff_text, needs, changed_lines)
    _collect_import_needs(changed_lines, needs)
    _collect_invariant_needs(diff_text, needs, changed_lines)
    _collect_test_needs(all_fragments, core_symbol_names, needs)
    if _is_terraform_diff(all_fragments, core_ids):
        _collect_terraform_needs(diff_text, needs, changed_lines)
    if _is_config_only_diff(all_fragments, core_ids):
        _collect_config_context_needs(all_fragments, core_ids, needs)

    base_symbols = {n.symbol for n in needs.values()}
    closure = _apply_closure(base_symbols, all_fragments, graph, closure_depth)
    for sym in closure - base_symbols:
        key = ("definition", sym)
        if key not in needs:
            needs[key] = InformationNeed("definition", sym, None, 0.5)

    if not needs:
        fallback = concepts_from_diff_text(diff_text, changed_lines=changed_lines)
        return tuple(InformationNeed("definition", c, None, 0.5) for c in fallback)

    covered_symbols = {n.symbol for n in needs.values()}
    for c in concepts_from_diff_text(diff_text, changed_lines=changed_lines):
        if c not in covered_symbols:
            key = ("background", c)
            if key not in needs:
                needs[key] = InformationNeed("background", c, None, 0.3)

    return tuple(needs.values())


@dataclass
class UtilityState:
    max_rel: dict[tuple[str, str], float] = field(default_factory=dict)
    priorities: dict[tuple[str, str], float] = field(default_factory=dict)
    structural_sum: float = 0.0
    eta: float = UTILITY.eta
    gamma: float = UTILITY.gamma
    r_cap: float = 1.0
    changed_dirs: frozenset[Path] = field(default_factory=frozenset)
    proximity_decay: float = UTILITY.proximity_decay
    file_importance: dict[Path, float] = field(default_factory=dict)

    def copy(self) -> UtilityState:
        return replace(
            self,
            max_rel=dict(self.max_rel),
            priorities=dict(self.priorities),
            file_importance=dict(self.file_importance),
        )


def _phi(x: float) -> float:
    return math.sqrt(x) if x > 0 else 0.0


_MIN_REL_FOR_BONUS = 0.03
_RELATEDNESS_BONUS = 0.25


def _augmented_score(m: float, rel_score: float, state: UtilityState) -> float:
    # Paper: a(f,n) = m(f,n) + η·R(f). We normalize R by R_cap
    # to keep η interpretable across repos of different scale.
    r_norm = min(rel_score / state.r_cap, 1.0) if state.r_cap > 0 else 0.0
    return m + state.eta * r_norm


def _needs_from_identifiers(frag: Fragment) -> tuple[InformationNeed, ...]:
    return tuple(InformationNeed("definition", c, None, 0.5) for c in frag.identifiers)


@dataclass
class _GainResult:
    gain: float = 0.0
    has_match: bool = False
    need_updates: list[tuple[tuple[str, str], float, float]] = field(default_factory=list)
    diversity_bonus: float = 0.0
    structural_bonus: float = 0.0


def _diversity_bonus(
    needs: tuple[InformationNeed, ...],
    rel_score: float,
    gain: float,
    state: UtilityState,
) -> float:
    if not needs or rel_score < _MIN_REL_FOR_BONUS:
        return 0.0
    if gain <= 0:
        return 0.0
    total_covered = sum(min(state.max_rel.get((n.need_type, n.symbol), 0.0), 1.0) for n in needs)
    unsatisfied = max(0.0, 1.0 - total_covered / max(1, len(needs)))
    return rel_score * _RELATEDNESS_BONUS * unsatisfied


def _compute_gain_core(
    frag: Fragment,
    rel_score: float,
    needs: tuple[InformationNeed, ...],
    state: UtilityState,
    use_state_priorities: bool = False,
) -> _GainResult:
    effective = needs if needs else _needs_from_identifiers(frag)
    result = _GainResult()
    if not effective:
        return result

    for need in effective:
        m = _match_strength_typed(frag, need)
        if m <= 0.0:
            continue
        if need.need_type == "impact" and state.file_importance:
            m *= state.file_importance.get(frag.path, 1.0)
        result.has_match = True
        a_fz = _augmented_score(m, rel_score, state)
        nkey = (need.need_type, need.symbol)
        old_max = state.max_rel.get(nkey, 0.0)
        new_max = max(old_max, a_fz)
        priority = state.priorities.get(nkey, need.priority) if use_state_priorities else need.priority
        result.gain += priority * (_phi(new_max) - _phi(old_max))
        result.need_updates.append((nkey, new_max, need.priority))

    result.diversity_bonus = _diversity_bonus(needs, rel_score, result.gain, state)

    # Structural proximity layer (U2): gamma * min(R/R_cap, 1).
    if result.has_match:
        r_norm = min(rel_score / state.r_cap, 1.0) if state.r_cap > 0 else 0.0
        result.structural_bonus = state.gamma * r_norm

    return result


def marginal_gain(
    frag: Fragment,
    rel_score: float,
    needs: tuple[InformationNeed, ...],
    state: UtilityState,
) -> float:
    result = _compute_gain_core(frag, rel_score, needs, state)
    return result.gain + result.diversity_bonus + result.structural_bonus


def apply_fragment(
    frag: Fragment,
    rel_score: float,
    needs: tuple[InformationNeed, ...],
    state: UtilityState,
) -> None:
    result = _compute_gain_core(frag, rel_score, needs, state, use_state_priorities=True)
    for nkey, new_max, priority in result.need_updates:
        state.max_rel[nkey] = new_max
        state.priorities[nkey] = max(state.priorities.get(nkey, 0.0), priority)
    state.structural_sum += result.diversity_bonus + result.structural_bonus


def _dir_distance(d1: Path, d2: Path) -> int:
    p1 = d1.parts
    p2 = d2.parts
    common = 0
    for a, b in zip(p1, p2):
        if a == b:
            common += 1
        else:
            break
    return (len(p1) - common) + (len(p2) - common)


def _proximity_factor(frag_path: Path, changed_dirs: frozenset[Path], alpha: float) -> float:
    if not changed_dirs:
        return 1.0
    frag_dir = frag_path.parent
    min_dist = min(_dir_distance(frag_dir, d) for d in changed_dirs)
    if min_dist <= 0:
        return 1.0
    return 1.0 / (1.0 + alpha * min_dist)


def compute_density(frag: Fragment, rel_score: float, needs: tuple[InformationNeed, ...], state: UtilityState) -> float:
    if frag.token_count <= 0:
        return 0.0
    gain = marginal_gain(frag, rel_score, needs, state)
    pf = _proximity_factor(frag.path, state.changed_dirs, state.proximity_decay)
    return gain * pf / frag.token_count


def utility_value(state: UtilityState) -> float:
    u1 = sum(state.priorities.get(sym, 1.0) * _phi(v) for sym, v in state.max_rel.items())
    return u1 + state.structural_sum
