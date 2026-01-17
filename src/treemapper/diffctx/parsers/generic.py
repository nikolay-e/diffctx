from __future__ import annotations

from pathlib import Path

from ..stopwords import TokenProfile
from ..types import Fragment, FragmentId, extract_identifiers
from .base import GENERIC_MAX_LINES, find_smart_split_point


class GenericStrategy:
    priority = 0

    def can_handle(self, _path: Path, _content: str) -> bool:
        return True

    def fragment(self, path: Path, content: str) -> list[Fragment]:
        lines = content.splitlines()
        if not lines:
            return []

        fragments: list[Fragment] = []
        total = len(lines)
        start_idx = 0

        while start_idx < total:
            target_end_idx = min(total - 1, start_idx + GENERIC_MAX_LINES - 1)
            end_idx = find_smart_split_point(lines, start_idx, target_end_idx, path)

            snippet = "\n".join(lines[start_idx : end_idx + 1])
            if not snippet.endswith("\n"):
                snippet += "\n"

            profile = TokenProfile.from_path(str(path))

            fragments.append(
                Fragment(
                    id=FragmentId(path=path, start_line=start_idx + 1, end_line=end_idx + 1),
                    kind="chunk",
                    content=snippet,
                    identifiers=extract_identifiers(snippet, profile=profile),
                )
            )
            start_idx = end_idx + 1

        return fragments
