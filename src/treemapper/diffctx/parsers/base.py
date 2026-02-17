from __future__ import annotations

from collections.abc import Callable
from pathlib import Path
from typing import Protocol, runtime_checkable

from ..types import Fragment, FragmentId, extract_identifiers

_MIN_FRAGMENT_LINES = 1
MIN_FRAGMENT_LINES = _MIN_FRAGMENT_LINES
_GENERIC_MAX_LINES = 200
_GENERIC_MAX_EXTENSION = 100
_MIN_FRAGMENT_WORDS = 10

_BRACKET_PAIRS = {"{": "}", "[": "]", "(": ")"}

_CLOSE_BRACKETS = set(_BRACKET_PAIRS.values())


@runtime_checkable
class FragmentationStrategy(Protocol):
    priority: int

    def can_handle(self, path: Path, content: str) -> bool: ...

    def fragment(self, path: Path, content: str) -> list[Fragment]: ...


def _process_char_in_string(char: str, string_char: str, escape_count: int) -> tuple[bool, int]:
    if char == "\\":
        return True, escape_count + 1
    if char == string_char and escape_count % 2 == 0:
        return False, 0
    return True, 0


def _process_char_outside_string(char: str, stack: list[str]) -> tuple[bool, str]:
    if char in ('"', "'", "`"):
        return True, char
    if char in _BRACKET_PAIRS:
        stack.append(_BRACKET_PAIRS[char])
    elif char in _CLOSE_BRACKETS and stack and stack[-1] == char:
        stack.pop()
    return False, ""


def compute_bracket_balance(text: str) -> int:
    stack: list[str] = []
    in_string = False
    string_char = ""
    escape_count = 0

    for char in text:
        if in_string:
            in_string, escape_count = _process_char_in_string(char, string_char, escape_count)
        else:
            in_string, string_char = _process_char_outside_string(char, stack)
            escape_count = 0

    return len(stack)


def _is_comment_or_blank(line: str) -> bool:
    stripped = line.strip()
    if not stripped:
        return True
    return stripped.startswith("//") or stripped.startswith("#") or stripped.startswith("/*") or stripped.startswith("*")


def _is_top_level_close(line: str) -> bool:
    stripped = line.strip()
    return stripped == "}" or stripped == "};" or stripped.startswith("}")


def _find_first_balanced_point(lines: list[str], start_idx: int, target_end_idx: int) -> int | None:
    for end_idx in range(start_idx, target_end_idx + 1):
        text = "\n".join(lines[start_idx : end_idx + 1])
        if compute_bracket_balance(text) == 0 and _is_top_level_close(lines[end_idx]):
            if end_idx + 1 > target_end_idx or _is_comment_or_blank(lines[end_idx + 1]):
                return end_idx
    return None


def find_balanced_end_line(
    lines: list[str], start_idx: int, target_end_idx: int, max_extension: int = _GENERIC_MAX_EXTENSION
) -> int:
    if target_end_idx >= len(lines):
        target_end_idx = len(lines) - 1

    text_to_target = "\n".join(lines[start_idx : target_end_idx + 1])
    if compute_bracket_balance(text_to_target) == 0:
        first_balanced = _find_first_balanced_point(lines, start_idx, target_end_idx)
        if first_balanced is not None and first_balanced < target_end_idx:
            return first_balanced
        return target_end_idx

    max_end = min(len(lines) - 1, target_end_idx + max_extension)

    for end_idx in range(target_end_idx + 1, max_end + 1):
        text = "\n".join(lines[start_idx : end_idx + 1])
        if compute_bracket_balance(text) == 0:
            return end_idx

    for end_idx in range(target_end_idx - 1, start_idx - 1, -1):
        text = "\n".join(lines[start_idx : end_idx + 1])
        if compute_bracket_balance(text) == 0:
            return end_idx

    return target_end_idx


_SENTENCE_ENDINGS = (".", "?", "!", '."', '?"', '!"', ".'", "?'", "!'")


def find_sentence_boundary(lines: list[str], start_idx: int, target_end_idx: int) -> int:
    if target_end_idx >= len(lines):
        target_end_idx = len(lines) - 1

    for end_idx in range(target_end_idx, start_idx, -1):
        line = lines[end_idx].rstrip()
        if not line.endswith(_SENTENCE_ENDINGS):
            continue

        if end_idx + 1 >= len(lines):
            return end_idx

        next_line = lines[end_idx + 1].lstrip()
        if not next_line or next_line[0].isupper():
            return end_idx

    return target_end_idx


def get_indent_level(line: str) -> int:
    stripped = line.lstrip()
    if not stripped:
        return -1
    return len(line) - len(stripped)


def find_indent_safe_end_line(lines: list[str], start_idx: int, target_end_idx: int) -> int:
    if target_end_idx >= len(lines):
        target_end_idx = len(lines) - 1

    if target_end_idx + 1 >= len(lines):
        return target_end_idx

    next_indent = get_indent_level(lines[target_end_idx + 1])
    if next_indent <= 0:
        return target_end_idx

    target_indent = get_indent_level(lines[target_end_idx])
    if target_indent < 0:
        target_indent = 0

    if next_indent <= target_indent:
        return target_end_idx

    for end_idx in range(target_end_idx - 1, start_idx, -1):
        current_indent = get_indent_level(lines[end_idx])
        if current_indent < 0:
            continue

        if end_idx + 1 < len(lines):
            following_indent = get_indent_level(lines[end_idx + 1])
            if following_indent < 0 or following_indent <= current_indent:
                return end_idx

    return target_end_idx


CODE_EXTENSIONS = {
    ".py",
    ".pyw",
    ".pyi",
    ".js",
    ".jsx",
    ".mjs",
    ".cjs",
    ".ts",
    ".tsx",
    ".mts",
    ".cts",
    ".vue",
    ".svelte",
    ".go",
    ".rs",
    ".java",
    ".kt",
    ".kts",
    ".scala",
    ".c",
    ".h",
    ".cpp",
    ".hpp",
    ".cc",
    ".cxx",
    ".cs",
    ".swift",
    ".rb",
    ".rake",
    ".php",
    ".lua",
    ".sh",
    ".bash",
    ".zsh",
    ".pl",
    ".pm",
    ".r",
    ".R",
    ".dart",
    ".zig",
    ".nim",
    ".v",
    ".ex",
    ".exs",
    ".hs",
    ".ml",
    ".mli",
}

YAML_EXTENSIONS = {".yaml", ".yml"}
INDENT_EXTENSIONS = {".py", ".pyw", ".pyi"} | YAML_EXTENSIONS


def is_code_file(path: Path) -> bool:
    return path.suffix.lower() in CODE_EXTENSIONS


def is_indent_based_file(path: Path) -> bool:
    return path.suffix.lower() in INDENT_EXTENSIONS


def find_smart_split_point(lines: list[str], start_idx: int, target_end_idx: int, path: Path) -> int:
    if target_end_idx >= len(lines):
        target_end_idx = len(lines) - 1

    if is_indent_based_file(path):
        indent_end = find_indent_safe_end_line(lines, start_idx, target_end_idx)
        if indent_end != target_end_idx:
            return indent_end

    if is_code_file(path):
        return find_balanced_end_line(lines, start_idx, target_end_idx)

    return target_end_idx


def build_covered_set(covered: list[tuple[int, int]]) -> set[int]:
    result: set[int] = set()
    for start, end in covered:
        result.update(range(start, end + 1))
    return result


def find_uncovered_lines(total_lines: int, covered_set: set[int]) -> list[int]:
    return [ln for ln in range(1, total_lines + 1) if ln not in covered_set]


def group_into_gaps(uncovered_lines: list[int]) -> list[tuple[int, int]]:
    if not uncovered_lines:
        return []

    gaps: list[tuple[int, int]] = []
    gap_start = uncovered_lines[0]
    gap_end = uncovered_lines[0]

    for ln in uncovered_lines[1:]:
        if ln == gap_end + 1:
            gap_end = ln
        else:
            gaps.append((gap_start, gap_end))
            gap_start = ln
            gap_end = ln
    gaps.append((gap_start, gap_end))
    return gaps


def trim_blank_lines(lines: list[str], start: int, end: int) -> tuple[int, int]:
    while start <= end and not lines[start - 1].strip():
        start += 1
    while end >= start and not lines[end - 1].strip():
        end -= 1
    return start, end


def create_gap_fragment(path: Path, lines: list[str], start: int, end: int) -> Fragment | None:
    if start > end or end - start + 1 < _MIN_FRAGMENT_LINES:
        return None

    snippet = "\n".join(lines[start - 1 : end])
    if not snippet.strip():
        return None
    if not snippet.endswith("\n"):
        snippet += "\n"

    return Fragment(
        id=FragmentId(path=path, start_line=start, end_line=end),
        kind="chunk",
        content=snippet,
        identifiers=extract_identifiers(snippet, profile="code"),
    )


def create_code_gap_fragments(path: Path, lines: list[str], covered: list[tuple[int, int]]) -> list[Fragment]:
    if not lines:
        return []

    covered_set = build_covered_set(covered)
    uncovered_lines = find_uncovered_lines(len(lines), covered_set)
    if not uncovered_lines:
        return []

    gaps = group_into_gaps(uncovered_lines)
    fragments: list[Fragment] = []

    for start, end in gaps:
        start, end = trim_blank_lines(lines, start, end)
        frag = create_gap_fragment(path, lines, start, end)
        if frag:
            fragments.append(frag)

    return fragments


MIN_FRAGMENT_LINES = _MIN_FRAGMENT_LINES
GENERIC_MAX_LINES = _GENERIC_MAX_LINES
MIN_FRAGMENT_WORDS = _MIN_FRAGMENT_WORDS


def check_library_available(import_fn: Callable[[], None]) -> bool:
    try:
        import_fn()
        return True
    except ImportError:
        return False


def create_snippet(lines: list[str], start_line: int, end_line: int) -> str | None:
    snippet = "\n".join(lines[start_line - 1 : end_line])
    if not snippet.strip():
        return None
    if not snippet.endswith("\n"):
        snippet += "\n"
    return snippet


def create_fragment_from_lines(
    path: Path,
    lines: list[str],
    start_line: int,
    end_line: int,
    kind: str,
    profile: str = "code",
    symbol_name: str | None = None,
) -> Fragment | None:
    snippet = create_snippet(lines, start_line, end_line)
    if snippet is None:
        return None
    return Fragment(
        id=FragmentId(path=path, start_line=start_line, end_line=end_line),
        kind=kind,
        content=snippet,
        identifiers=extract_identifiers(snippet, profile=profile),
        symbol_name=symbol_name,
    )
