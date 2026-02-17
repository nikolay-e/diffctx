from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path

import tiktoken

_ENCODER = tiktoken.get_encoding("o200k_base")


def _count_tokens(text: str) -> int:
    return len(_ENCODER.encode(text))


@dataclass
class YamlTestCase:
    name: str
    initial_files: dict[str, str]
    changed_files: dict[str, str]
    must_include: list[str] = field(default_factory=list)
    must_include_files: list[str] = field(default_factory=list)
    must_include_content: list[str] = field(default_factory=list)
    must_not_include: list[str] = field(default_factory=list)
    commit_message: str = "Update files"
    min_budget: int | None = None
    add_garbage_files: bool = True
    skip_garbage_check: bool = False
    source_file: Path | None = None

    def calculate_budget(self) -> int:
        fragment_overhead = 20
        all_files = {**self.initial_files, **self.changed_files}
        content_tokens = sum(_count_tokens(content) for content in all_files.values())
        estimated_fragments = max(len(all_files), 2)
        budget = content_tokens + (estimated_fragments * fragment_overhead)
        budget = int(budget * 2.5)
        floor = self.min_budget if self.min_budget is not None else 500
        return max(floor, budget)

    @property
    def id(self) -> str:
        return self.name
