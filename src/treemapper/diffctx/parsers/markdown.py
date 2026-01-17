from __future__ import annotations

import re
from pathlib import Path

from ..types import Fragment
from .base import check_library_available, create_fragment_from_lines


def _import_mistune() -> None:
    import mistune  # noqa: F401


class MistuneMarkdownStrategy:
    priority = 90

    def __init__(self) -> None:
        self._available = check_library_available(_import_mistune)

    def can_handle(self, path: Path, _content: str) -> bool:
        if not self._available:
            return False
        return path.suffix.lower() in {".md", ".markdown", ".mdx"}

    def fragment(self, path: Path, content: str) -> list[Fragment]:
        import mistune

        lines = content.splitlines()
        if not lines:
            return []

        md = mistune.create_markdown(renderer=None)
        tokens = md(content)

        if not tokens:
            return []

        heading_lines = self._find_all_headings(lines)
        if not heading_lines:
            return []

        fragments: list[Fragment] = []
        heading_idx = 0

        for token in tokens:
            if not isinstance(token, dict):
                continue

            token_type = token.get("type")
            if token_type == "heading" and heading_idx < len(heading_lines):
                start_line, level = heading_lines[heading_idx]
                heading_idx += 1

                end_line = self._find_section_end(lines, start_line, level, heading_lines[heading_idx:])

                frag = create_fragment_from_lines(path, lines, start_line, end_line, "section", "docs")
                if frag:
                    fragments.append(frag)

        return fragments if fragments else []

    def _find_all_headings(self, lines: list[str]) -> list[tuple[int, int]]:
        headings: list[tuple[int, int]] = []
        for i, line in enumerate(lines):
            stripped = line.lstrip()
            if not stripped.startswith("#"):
                continue
            level = len(stripped) - len(stripped.lstrip("#"))
            if level <= 6 and (len(stripped) == level or stripped[level] == " "):
                headings.append((i + 1, level))
        return headings

    def _find_section_end(self, lines: list[str], _start_line: int, level: int, remaining_headings: list[tuple[int, int]]) -> int:
        for next_line, next_level in remaining_headings:
            if next_level <= level:
                return next_line - 1
        return len(lines)


class RegexMarkdownStrategy:
    priority = 85
    _HEADING_RE = re.compile(r"^(#{1,6}) (.+)$")

    def can_handle(self, path: Path, _content: str) -> bool:
        return path.suffix.lower() in {".md", ".markdown", ".mdx"}

    def fragment(self, path: Path, content: str) -> list[Fragment]:
        lines = content.splitlines()
        if not lines:
            return []

        headings: list[tuple[int, int, str]] = []
        for i, line in enumerate(lines):
            match = self._HEADING_RE.match(line)
            if match:
                headings.append((i + 1, len(match.group(1)), match.group(2).strip()))

        if not headings:
            return []

        fragments: list[Fragment] = []

        for idx, (start_line, level, _title) in enumerate(headings):
            end_line = len(lines)
            for next_line, next_level, _ in headings[idx + 1 :]:
                if next_level <= level:
                    end_line = next_line - 1
                    break

            if end_line < start_line:
                continue

            frag = create_fragment_from_lines(path, lines, start_line, end_line, "section", "docs")
            if frag:
                fragments.append(frag)

        return fragments
