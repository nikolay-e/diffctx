from __future__ import annotations

import difflib
from dataclasses import dataclass, field


@dataclass
class ScoreBreakdown:
    required_hits: int = 0
    required_total: int = 0
    missing_required: list[str] = field(default_factory=list)

    forbidden_hits: int = 0
    forbidden_total: int = 0
    present_forbidden: list[str] = field(default_factory=list)

    diff_covered: bool = True
    uncovered_lines: list[str] = field(default_factory=list)

    diff_tokens: int = 0
    context_tokens: int = 0

    @property
    def required_recall(self) -> float:
        if self.required_total == 0:
            return 1.0
        return self.required_hits / self.required_total

    @property
    def forbidden_rate(self) -> float:
        if self.forbidden_total == 0:
            return 0.0
        return self.forbidden_hits / self.forbidden_total

    @property
    def enrichment(self) -> float:
        if self.diff_tokens == 0:
            return 0.0
        return self.context_tokens / self.diff_tokens

    @property
    def score(self) -> float:
        return round(100.0 * self.required_recall * (1.0 - self.forbidden_rate), 1)

    @property
    def passed(self) -> bool:
        return not self.missing_required and not self.present_forbidden


def _extract_added_lines(initial_content: str, changed_content: str) -> list[str]:
    initial_lines = initial_content.splitlines()
    changed_lines = changed_content.splitlines()
    matcher = difflib.SequenceMatcher(None, initial_lines, changed_lines)
    added = []
    for tag, _, _, j1, j2 in matcher.get_opcodes():
        if tag in ("replace", "insert"):
            added.extend(changed_lines[j1:j2])
    return [line.strip() for line in added if line.strip()]


def compute_diff_lines(
    initial_files: dict[str, str],
    changed_files: dict[str, str],
) -> dict[str, list[str]]:
    result: dict[str, list[str]] = {}
    for path, changed_content in changed_files.items():
        initial_content = initial_files.get(path, "")
        if initial_content == changed_content:
            continue
        significant = _extract_added_lines(initial_content, changed_content)
        if significant:
            result[path] = significant
    return result


def compute_diff_token_count(
    initial_files: dict[str, str],
    changed_files: dict[str, str],
) -> int:
    from tests.framework.types import _count_tokens

    total = 0
    for path, changed_content in changed_files.items():
        initial_content = initial_files.get(path, "")
        if initial_content == changed_content:
            continue
        initial_lines = initial_content.splitlines()
        changed_lines = changed_content.splitlines()
        matcher = difflib.SequenceMatcher(None, initial_lines, changed_lines)
        diff_parts: list[str] = []
        for tag, i1, i2, j1, j2 in matcher.get_opcodes():
            if tag in ("replace", "insert"):
                diff_parts.extend(changed_lines[j1:j2])
            elif tag == "delete":
                diff_parts.extend(initial_lines[i1:i2])
        if diff_parts:
            total += _count_tokens("\n".join(diff_parts))
    return total


def compute_context_token_count(context: dict) -> int:
    from tests.framework.types import _count_tokens

    total = 0
    for frag in context.get("fragments", []):
        content = frag.get("content", "")
        if content:
            total += _count_tokens(content)
    return total


def check_diff_coverage(all_content: str, diff_lines: dict[str, list[str]]) -> tuple[bool, list[str]]:
    uncovered = []
    for path, lines in diff_lines.items():
        for line in lines:
            if line not in all_content:
                uncovered.append(f"{path}: {line}")
    return len(uncovered) == 0, uncovered
