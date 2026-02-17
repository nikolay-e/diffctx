from __future__ import annotations

import re
from dataclasses import dataclass, field
from pathlib import Path

from .stopwords import TokenProfile
from .tokenizer import extract_tokens as _extract_tokens_nlp

_IDENT_RE = re.compile(r"[A-Za-z_]\w*")


@dataclass(frozen=True)
class FragmentId:
    path: Path
    start_line: int
    end_line: int

    def __post_init__(self) -> None:
        path_str = str(self.path)
        object.__setattr__(self, "_path_str", path_str)
        object.__setattr__(self, "_hash", hash((path_str, self.start_line, self.end_line)))

    def __str__(self) -> str:
        return f"{self._path_str}:{self.start_line}-{self.end_line}"  # type: ignore[attr-defined]

    def __hash__(self) -> int:
        return self._hash  # type: ignore[attr-defined,no-any-return]

    def __eq__(self, other: object) -> bool:
        if not isinstance(other, FragmentId):
            return NotImplemented
        return (
            self._hash == other._hash  # type: ignore[attr-defined]
            and self._path_str == other._path_str  # type: ignore[attr-defined]
            and self.start_line == other.start_line
            and self.end_line == other.end_line
        )

    def __lt__(self, other: object) -> bool:
        if not isinstance(other, FragmentId):
            return NotImplemented
        return (self._path_str, self.start_line, self.end_line) < (other._path_str, other.start_line, other.end_line)  # type: ignore[attr-defined]


@dataclass
class Fragment:
    id: FragmentId
    kind: str
    content: str
    identifiers: frozenset[str] = field(default_factory=frozenset)
    token_count: int = 0
    symbol_name: str | None = None

    @property
    def path(self) -> Path:
        return self.id.path

    @property
    def start_line(self) -> int:
        return self.id.start_line

    @property
    def end_line(self) -> int:
        return self.id.end_line

    @property
    def line_count(self) -> int:
        return self.end_line - self.start_line + 1


@dataclass(frozen=True)
class DiffHunk:
    path: Path
    new_start: int
    new_len: int
    old_start: int = 0
    old_len: int = 0

    @property
    def end_line(self) -> int:
        if self.new_len == 0:
            return self.new_start
        return self.new_start + self.new_len - 1

    @property
    def is_deletion(self) -> bool:
        return self.new_len == 0 and self.old_len > 0

    @property
    def is_addition(self) -> bool:
        return self.old_len == 0 and self.new_len > 0

    @property
    def core_selection_range(self) -> tuple[int, int]:
        if self.is_deletion:
            anchor = max(1, self.new_start)
            return (anchor, anchor)
        return (self.new_start, self.end_line)


def extract_identifiers(
    text: str,
    profile: str = "code",
    *,
    skip_stopwords: bool = False,
    use_nlp: bool = False,
) -> frozenset[str]:
    if use_nlp and profile != "code":
        return _extract_tokens_nlp(text, profile=profile, use_nlp=True)

    raw = _IDENT_RE.findall(text)
    min_len = TokenProfile.get_min_len(profile)
    if skip_stopwords:
        stopwords = TokenProfile.get_stopwords(profile)
        return frozenset({ident.lower() for ident in raw if len(ident) >= min_len and ident.lower() not in stopwords})
    # Normalize to lowercase to match concepts (also lowercase)
    return frozenset({ident.lower() for ident in raw if len(ident) >= min_len})


def extract_identifier_list(
    text: str,
    profile: str = "code",
    *,
    skip_stopwords: bool = True,
    use_nlp: bool = False,
) -> list[str]:
    if use_nlp and profile != "code":
        from .tokenizer import extract_token_list

        return extract_token_list(text, profile=profile, use_nlp=True)

    raw = _IDENT_RE.findall(text)
    min_len = TokenProfile.get_min_len(profile)
    if skip_stopwords:
        stopwords = TokenProfile.get_stopwords(profile)
        return [ident for ident in raw if len(ident) >= min_len and ident.lower() not in stopwords]
    return [ident for ident in raw if len(ident) >= min_len]
