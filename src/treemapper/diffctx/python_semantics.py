from __future__ import annotations

import ast
import textwrap

from treemapper.diffctx.semantic_types import EMPTY_SEMANTIC_INFO, SemanticInfo

PyFragmentInfo = SemanticInfo


def _names_from_expr(expr: ast.AST | None) -> set[str]:
    if expr is None:
        return set()
    out: set[str] = set()
    for node in ast.walk(expr):
        if isinstance(node, ast.Name):
            out.add(node.id)
        elif isinstance(node, ast.Attribute):
            out.add(node.attr)
    return out


def _collect_defines(tree: ast.Module) -> set[str]:
    defines: set[str] = set()
    for stmt in tree.body:
        if isinstance(stmt, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
            defines.add(stmt.name)
    return defines


def _collect_refs_and_calls(tree: ast.Module) -> tuple[set[str], set[str]]:
    refs: set[str] = set()
    calls: set[str] = set()

    for node in ast.walk(tree):
        if isinstance(node, ast.Name) and isinstance(node.ctx, ast.Load):
            refs.add(node.id)
        elif isinstance(node, ast.Attribute):
            refs.add(node.attr)

        if isinstance(node, ast.Call):
            func = node.func
            if isinstance(func, ast.Name):
                calls.add(func.id)
            elif isinstance(func, ast.Attribute):
                calls.add(func.attr)

    return refs, calls


def _extract_func_type_refs(node: ast.FunctionDef | ast.AsyncFunctionDef) -> set[str]:
    type_refs = _names_from_expr(node.returns)
    for a in node.args.args + node.args.kwonlyargs:
        type_refs |= _names_from_expr(a.annotation)
    if node.args.vararg is not None:
        type_refs |= _names_from_expr(node.args.vararg.annotation)
    if node.args.kwarg is not None:
        type_refs |= _names_from_expr(node.args.kwarg.annotation)
    return type_refs


def _collect_type_refs(tree: ast.Module) -> set[str]:
    type_refs: set[str] = set()
    for node in ast.walk(tree):
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            type_refs |= _extract_func_type_refs(node)
        elif isinstance(node, ast.AnnAssign):
            type_refs |= _names_from_expr(node.annotation)
    return type_refs


_EMPTY_INFO = EMPTY_SEMANTIC_INFO


def _try_parse(code: str) -> ast.Module | None:
    try:
        return ast.parse(code)
    except SyntaxError:
        return None


def _parse_with_fallbacks(code: str) -> ast.Module | None:
    tree = _try_parse(code)
    if tree is not None:
        return tree

    dedented = textwrap.dedent(code)
    tree = _try_parse(dedented)
    if tree is not None:
        return tree

    wrapped = f"def __wrapper__():\n{textwrap.indent(dedented, '    ')}\n"
    tree = _try_parse(wrapped)
    if tree is not None:
        return tree

    return None


def analyze_python_fragment(code: str) -> PyFragmentInfo:
    if not code.strip():
        return _EMPTY_INFO

    tree = _parse_with_fallbacks(code)
    if tree is None:
        return _EMPTY_INFO

    defines = _collect_defines(tree)
    refs, calls = _collect_refs_and_calls(tree)
    type_refs = _collect_type_refs(tree)

    return PyFragmentInfo(
        defines=frozenset(defines),
        references=frozenset(refs),
        calls=frozenset(calls),
        type_refs=frozenset(type_refs),
    )
