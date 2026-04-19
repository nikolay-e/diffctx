from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING

from ..edges.structural.testing import _is_test_file
from ..stopwords import CODE_STOPWORDS
from ..tokenizer import extract_tokens
from ..types import Fragment, FragmentId, extract_identifiers
from ._builtins import (
    _CALL_RE,
    _CLOSURE_EDGE_CATEGORIES,
    _CLOSURE_MIN_EDGE_WEIGHT,
    _COMMENT_PREFIXES,
    _CONFIG_EXTENSIONS_FOR_DIFF,
    _EXPANSION_STOPWORDS,
    _GENERIC_TYPE_RE,
    _INVARIANT_RE,
    _JS_IMPORT_RE,
    _JS_LOCAL_IMPORT_RE,
    _LANGUAGE_BUILTINS,
    _ONE_CLASS_PER_FILE_SUFFIXES,
    _PY_IMPORT_RE,
    _TF_EXTENSIONS,
    _TF_RES_REF_NEED_RE,
    _TF_SKIP_REF_TYPES,
    _TF_VAR_NEED_RE,
    _TYPE_REF_RE,
)

if TYPE_CHECKING:
    from ..graph import Graph

_MIN_SYMBOL_LENGTH = 3
_BACKGROUND_MIN_IDENT_LENGTH = 5

_DEFINES_SCOPE_MATCH = 1.0
_DEFINES_NO_SCOPE = 0.5
_DEFINES_OTHER_SCOPE = 0.3
_IMPACT_SCOPE_MATCH = 0.15
_IMPACT_MENTIONS = 0.8
_SIGNATURE_DEFINES = 0.7
_TEST_MENTIONS = 0.6
_MENTIONS_FALLBACK = 0.3

_DEFINITION_PRIORITY = 0.9
_CALL_DEFINITION_PRIORITY = 1.0
_SIGNATURE_PRIORITY = 0.7
_IMPACT_PRIORITY = 0.8
_INVARIANT_PRIORITY = 0.85
_TEST_PRIORITY = 0.6
_CLOSURE_PRIORITY = 0.5
_BACKGROUND_PRIORITY = 0.2
_CONCEPT_BACKGROUND_PRIORITY = 0.3
_FALLBACK_PRIORITY = 0.5


@dataclass(frozen=True)
class InformationNeed:
    need_type: str
    symbol: str
    scope: Path | None
    priority: float


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


def _is_local_import(source: str) -> bool:
    return source.startswith(".") or source.startswith("@/") or source.startswith("~/")


def _add_needs_for_syms(syms: set[str], needs: dict[tuple[str, str], InformationNeed]) -> None:
    for sym in syms:
        if len(sym) >= _MIN_SYMBOL_LENGTH and sym not in CODE_STOPWORDS:
            needs.setdefault(("definition", sym), InformationNeed("definition", sym, None, _DEFINITION_PRIORITY))


def _collect_js_import_needs(line: str, needs: dict[tuple[str, str], InformationNeed]) -> None:
    for m in _JS_LOCAL_IMPORT_RE.finditer(line):
        named, default, source = m.group(1), m.group(2), m.group(3)
        if not _is_local_import(source):
            continue
        syms: set[str] = set()
        if named:
            syms = _parse_import_names(named)
        elif default:
            syms = {default.lower()}
        _add_needs_for_syms(syms, needs)


def _collect_py_import_needs(line: str, needs: dict[tuple[str, str], InformationNeed]) -> None:
    for m in _PY_IMPORT_RE.finditer(line):
        module, names = m.group(1), m.group(2)
        if not module.startswith("."):
            continue
        _add_needs_for_syms(_parse_import_names(names), needs)


def _collect_import_needs(
    changed_lines: list[str],
    needs: dict[tuple[str, str], InformationNeed],
) -> None:
    for line in changed_lines:
        _collect_js_import_needs(line, needs)
        _collect_py_import_needs(line, needs)


def _is_comment_line(line: str) -> bool:
    stripped = line.lstrip()
    return any(stripped.startswith(p) for p in _COMMENT_PREFIXES)


def _defines_strength(scope_match: bool, has_scope: bool) -> float:
    if scope_match:
        return _DEFINES_SCOPE_MATCH
    return _DEFINES_NO_SCOPE if not has_scope else _DEFINES_OTHER_SCOPE


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
        return _IMPACT_SCOPE_MATCH
    if defines and "_signature" not in frag.kind:
        return _defines_strength(scope_match, need.scope is not None)
    if nt == "impact" and mentions and not defines:
        return _IMPACT_MENTIONS
    if defines and ("_signature" in frag.kind or nt == "signature"):
        return _SIGNATURE_DEFINES
    if nt == "test" and mentions and _is_test_fragment(frag):
        return _TEST_MENTIONS
    return _MENTIONS_FALLBACK if mentions else 0.0


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
        min_length=_MIN_SYMBOL_LENGTH,
    )


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


def _infer_core_symbol(frag: Fragment) -> str | None:
    if frag.symbol_name:
        return frag.symbol_name.lower()
    if frag.path.suffix.lower() in _ONE_CLASS_PER_FILE_SUFFIXES:
        stem = frag.path.stem
        if len(stem) >= _MIN_SYMBOL_LENGTH:
            return stem.lower()
    return None


def _collect_core_needs(
    all_fragments: list[Fragment],
    core_ids: set[FragmentId],
    needs: dict[tuple[str, str], InformationNeed],
) -> set[str]:
    core_symbol_names: set[str] = set()
    seen_paths: set[Path] = set()
    for frag in all_fragments:
        if frag.id not in core_ids:
            continue
        sym = _infer_core_symbol(frag)
        if not sym:
            continue
        if frag.path in seen_paths and not frag.symbol_name:
            continue
        if not frag.symbol_name:
            seen_paths.add(frag.path)
        core_symbol_names.add(sym)
        key = ("impact", sym)
        if key not in needs:
            needs[key] = InformationNeed("impact", sym, frag.path, _IMPACT_PRIORITY)
    return core_symbol_names


def _process_line_for_needs(
    line: str,
    external_syms: frozenset[str],
    needs: dict[tuple[str, str], InformationNeed],
) -> None:
    for m in _CALL_RE.finditer(line):
        name = m.group(1)
        low = name.lower()
        if len(name) < _MIN_SYMBOL_LENGTH or low in CODE_STOPWORDS or low in _LANGUAGE_BUILTINS:
            continue
        if low in external_syms:
            continue
        needs.setdefault(("definition", low), InformationNeed("definition", low, None, _CALL_DEFINITION_PRIORITY))
    for m in _TYPE_REF_RE.finditer(line):
        sym = m.group(1).lower()
        needs.setdefault(("signature", sym), InformationNeed("signature", sym, None, _SIGNATURE_PRIORITY))
    for m in _GENERIC_TYPE_RE.finditer(line):
        sym = m.group(1).lower()
        needs.setdefault(("signature", sym), InformationNeed("signature", sym, None, _SIGNATURE_PRIORITY))


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
                needs[key] = InformationNeed("test", tested, None, _TEST_PRIORITY)


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
            if len(sym) >= _MIN_SYMBOL_LENGTH and sym not in CODE_STOPWORDS:
                needs.setdefault(("invariant", sym), InformationNeed("invariant", sym, None, _INVARIANT_PRIORITY))


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
            if len(ident) >= _BACKGROUND_MIN_IDENT_LENGTH and ident not in covered:
                key = ("background", ident)
                if key not in needs:
                    needs[key] = InformationNeed("background", ident, None, _BACKGROUND_PRIORITY)


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
            if len(sym) >= _MIN_SYMBOL_LENGTH and sym not in CODE_STOPWORDS:
                needs.setdefault(("definition", sym), InformationNeed("definition", sym, None, _CALL_DEFINITION_PRIORITY))
        for m in _TF_RES_REF_NEED_RE.finditer(line):
            ref_type, ref_name = m.group(1).lower(), m.group(2).lower()
            if ref_type in _TF_SKIP_REF_TYPES:
                continue
            full_ref = f"{ref_type}.{ref_name}"
            needs.setdefault(("definition", full_ref), InformationNeed("definition", full_ref, None, _DEFINITION_PRIORITY))


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
            needs[key] = InformationNeed("definition", sym, None, _CLOSURE_PRIORITY)

    if not needs:
        fallback = concepts_from_diff_text(diff_text, changed_lines=changed_lines)
        return tuple(InformationNeed("definition", c, None, _FALLBACK_PRIORITY) for c in fallback)

    covered_symbols = {n.symbol for n in needs.values()}
    for c in concepts_from_diff_text(diff_text, changed_lines=changed_lines):
        if c not in covered_symbols:
            key = ("background", c)
            if key not in needs:
                needs[key] = InformationNeed("background", c, None, _CONCEPT_BACKGROUND_PRIORITY)

    return tuple(needs.values())
