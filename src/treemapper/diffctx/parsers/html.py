from __future__ import annotations

from pathlib import Path

from ..types import Fragment, FragmentId, extract_identifiers
from .base import MIN_FRAGMENT_LINES, check_library_available, create_snippet


def _import_lxml() -> None:
    from lxml import html  # noqa: F401


class HTMLStrategy:
    priority = 55

    def __init__(self) -> None:
        self._available = check_library_available(_import_lxml)

    def can_handle(self, path: Path, _content: str) -> bool:
        if not self._available:
            return False
        return path.suffix.lower() in {".html", ".htm", ".xhtml", ".xml"}

    def fragment(self, path: Path, content: str) -> list[Fragment]:
        from lxml import html

        lines = content.splitlines()
        if not lines:
            return []

        try:
            tree = html.fromstring(content)
        except Exception:
            return []

        fragments: list[Fragment] = []
        semantic_tags = {
            "section",
            "article",
            "main",
            "header",
            "footer",
            "nav",
            "aside",
            "div",
            "p",
            "h1",
            "h2",
            "h3",
            "h4",
            "h5",
            "h6",
        }

        for elem in tree.iter():
            if elem.tag not in semantic_tags:
                continue

            source_line = getattr(elem, "sourceline", None)
            if source_line is None:
                continue

            elem_html: str = html.tostring(elem, encoding="unicode")  # pyright: ignore[reportAssignmentType]
            elem_lines = elem_html.count("\n") + 1
            end_line = min(source_line + elem_lines - 1, len(lines))

            if end_line - source_line + 1 < MIN_FRAGMENT_LINES:
                continue

            snippet = create_snippet(lines, source_line, end_line)
            if snippet is None:
                continue

            kind = "section" if elem.tag in {"section", "article", "main"} else "block"
            fragments.append(
                Fragment(
                    id=FragmentId(path=path, start_line=source_line, end_line=end_line),
                    kind=kind,
                    content=snippet,
                    identifiers=extract_identifiers(snippet, profile="docs"),
                )
            )

        return self._deduplicate_nested(fragments)

    def _deduplicate_nested(self, fragments: list[Fragment]) -> list[Fragment]:
        if len(fragments) <= 1:
            return fragments

        fragments.sort(key=lambda f: (f.start_line, -f.end_line))
        result: list[Fragment] = []

        for frag in fragments:
            if result and result[-1].start_line <= frag.start_line <= result[-1].end_line:
                continue
            result.append(frag)

        return result
