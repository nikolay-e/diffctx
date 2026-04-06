from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path

import tiktoken

_ENCODER = tiktoken.get_encoding("o200k_base")


def _count_tokens(text: str) -> int:
    return len(_ENCODER.encode(text))


@dataclass
class Selector:
    path: str | None = None
    symbol: str | None = None
    kind: str | None = None
    anchor: str | None = None
    any_of: list[Selector] | None = None


@dataclass
class DeclaredFragment:
    id: str
    selector: Selector


@dataclass
class Oracle:
    required: list[str] = field(default_factory=list)
    allowed: list[str] = field(default_factory=list)
    forbidden: list[str] = field(default_factory=list)


@dataclass
class Accept:
    symbol_match: str = "exact"
    kind_must_match: bool = False
    span_relation: str = "exact_or_enclosing"


@dataclass
class Fixtures:
    auto_garbage: bool = False
    distractors: dict[str, str] = field(default_factory=dict)


@dataclass
class XFailInfo:
    category: str | None = None
    reason: str | None = None
    issue: str | None = None


@dataclass
class YamlTestCase:
    name: str
    initial_files: dict[str, str]
    changed_files: dict[str, str]
    fragments: list[DeclaredFragment] = field(default_factory=list)
    oracle: Oracle = field(default_factory=Oracle)
    tags: list[str] = field(default_factory=list)
    commit_message: str = "Update files"
    fixtures: Fixtures = field(default_factory=Fixtures)
    accept: Accept = field(default_factory=Accept)
    xfail: XFailInfo = field(default_factory=XFailInfo)
    source_file: Path | None = None

    @property
    def id(self) -> str:
        return self.name

    def calculate_budget(self) -> int:
        fragment_overhead = 20
        all_files = {**self.initial_files, **self.changed_files, **self.fixtures.distractors}
        content_tokens = sum(_count_tokens(content) for content in all_files.values())
        estimated_fragments = max(len(all_files), 2)
        budget = content_tokens + estimated_fragments * fragment_overhead
        budget = int(budget * 2.5)
        return max(budget, 500)
