from __future__ import annotations

import difflib
from dataclasses import dataclass, field

NOISE_PENALTY_WEIGHT = 0.25
GARBAGE_PENALTY_WEIGHT = 0.50


@dataclass
class ScoreBreakdown:
    diff_covered: bool
    uncovered_lines: list[str] = field(default_factory=list)

    expected_hits: int = 0
    expected_total: int = 0
    expected_details: list[tuple[str, bool]] = field(default_factory=list)

    noise_hits: int = 0
    noise_total: int = 0
    noise_details: list[tuple[str, bool]] = field(default_factory=list)

    garbage_hits: int = 0
    garbage_total: int = 0

    diff_tokens: int = 0
    context_tokens: int = 0

    @property
    def recall(self) -> float:
        if self.expected_total == 0:
            return 1.0
        return self.expected_hits / self.expected_total

    @property
    def noise_rate(self) -> float:
        if self.noise_total == 0:
            return 0.0
        return self.noise_hits / self.noise_total

    @property
    def garbage_rate(self) -> float:
        if self.garbage_total == 0:
            return 0.0
        return self.garbage_hits / self.garbage_total

    @property
    def enrichment(self) -> float:
        if self.diff_tokens == 0:
            return 0.0
        return self.context_tokens / self.diff_tokens

    @property
    def score(self) -> float:
        if not self.diff_covered:
            return 0.0
        noise_factor = max(0.0, 1.0 - NOISE_PENALTY_WEIGHT * self.noise_rate)
        garbage_factor = max(0.0, 1.0 - GARBAGE_PENALTY_WEIGHT * self.garbage_rate)
        return round(100.0 * self.recall * noise_factor * garbage_factor, 1)


def _match_path_loose(candidate: str, target: str) -> bool:
    return candidate == target or candidate.endswith(f"/{target}") or target.endswith(f"/{candidate}") or target in candidate


def _should_skip_file(
    path: str,
    expected_files: set[str] | None,
    excluded_files: set[str] | None,
) -> bool:
    if excluded_files and any(_match_path_loose(path, ex) for ex in excluded_files):
        return True
    if expected_files and not any(_match_path_loose(path, ef) for ef in expected_files):
        return True
    return False


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
    expected_files: set[str] | None = None,
    excluded_files: set[str] | None = None,
) -> dict[str, list[str]]:
    result: dict[str, list[str]] = {}
    for path, changed_content in changed_files.items():
        if _should_skip_file(path, expected_files, excluded_files):
            continue
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
