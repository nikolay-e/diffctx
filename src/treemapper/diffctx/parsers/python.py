from __future__ import annotations

import ast
from pathlib import Path

from ..types import Fragment
from .base import create_code_gap_fragments, create_fragment_from_lines


class PythonAstStrategy:
    priority = 95

    def can_handle(self, path: Path, _content: str) -> bool:
        return path.suffix.lower() in {".py", ".pyw", ".pyi"}

    def fragment(self, path: Path, content: str) -> list[Fragment]:
        lines = content.splitlines()
        if not lines:
            return []

        try:
            tree = ast.parse(content)
        except SyntaxError:
            return []

        fragments: list[Fragment] = []
        covered: list[tuple[int, int]] = []

        for node in ast.walk(tree):
            frag = None
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
                frag = self._create_function_fragment(path, lines, node)
            elif isinstance(node, ast.ClassDef):
                frag = self._create_class_fragment(path, lines, node)

            if frag:
                fragments.append(frag)
                covered.append((frag.start_line, frag.end_line))

        fragments.extend(create_code_gap_fragments(path, lines, covered))
        return fragments

    def _get_node_range(self, node: ast.FunctionDef | ast.AsyncFunctionDef | ast.ClassDef) -> tuple[int, int] | None:
        if not hasattr(node, "lineno") or not hasattr(node, "end_lineno") or node.end_lineno is None:
            return None

        start = max(1, node.lineno)
        for dec in getattr(node, "decorator_list", []) or []:
            dec_line = getattr(dec, "lineno", None)
            if isinstance(dec_line, int):
                start = min(start, dec_line)

        end = max(start, node.end_lineno)
        return start, end

    def _create_function_fragment(
        self, path: Path, lines: list[str], node: ast.FunctionDef | ast.AsyncFunctionDef
    ) -> Fragment | None:
        range_ = self._get_node_range(node)
        if not range_:
            return None
        return create_fragment_from_lines(path, lines, range_[0], range_[1], "function", symbol_name=node.name)

    def _create_class_fragment(self, path: Path, lines: list[str], node: ast.ClassDef) -> Fragment | None:
        range_ = self._get_node_range(node)
        if not range_:
            return None
        return create_fragment_from_lines(path, lines, range_[0], range_[1], "class", symbol_name=node.name)
