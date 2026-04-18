from __future__ import annotations

from pathlib import Path

from tests.conftest import GARBAGE_FILES, GARBAGE_MARKERS
from tests.framework.pygit2_backend import Pygit2Repo
from tests.framework.scoring import (
    ScoreBreakdown,
    check_diff_coverage,
    compute_context_token_count,
    compute_diff_lines,
    compute_diff_token_count,
)
from tests.framework.types import Accept, DeclaredFragment, Oracle, Selector, YamlTestCase


def _match_path(candidate: str, target: str) -> bool:
    return candidate == target or candidate.endswith(f"/{target}")


def _symbol_matches(frag_symbol: str, expected: str, mode: str) -> bool:
    if mode == "prefix":
        return frag_symbol.startswith(expected)
    if mode == "substring":
        return expected in frag_symbol
    return frag_symbol == expected


def _anchor_present(fragment: dict, anchor: str) -> bool:
    return anchor in fragment.get("content", "") or anchor in fragment.get("path", "")


def _matches_selector(fragment: dict, selector: Selector, accept: Accept) -> bool:
    if selector.any_of:
        return any(_matches_selector(fragment, s, accept) for s in selector.any_of)

    if selector.path is not None:
        if not _match_path(fragment.get("path", ""), selector.path):
            return False

    if selector.symbol is not None:
        if not _symbol_matches(fragment.get("symbol") or "", selector.symbol, accept.symbol_match):
            return False

    if selector.kind is not None and accept.kind_must_match:
        if fragment.get("kind") != selector.kind:
            return False

    if selector.anchor is not None and not _anchor_present(fragment, selector.anchor):
        return False

    return True


def _evaluate_oracle(
    output_fragments: list[dict],
    declared: list[DeclaredFragment],
    oracle: Oracle,
    accept: Accept,
) -> tuple[set[str], list[str], list[str]]:
    matched_ids: set[str] = set()
    for out_frag in output_fragments:
        for decl in declared:
            if _matches_selector(out_frag, decl.selector, accept):
                matched_ids.add(decl.id)

    missing_required = [fid for fid in oracle.required if fid not in matched_ids]
    present_forbidden = [fid for fid in oracle.forbidden if fid in matched_ids]
    return matched_ids, missing_required, present_forbidden


def _format_fragment_summary(context: dict) -> str:
    lines = []
    for frag in context.get("fragments", []):
        path = frag.get("path", "?")
        kind = frag.get("kind", "?")
        symbol = frag.get("symbol", "")
        frag_lines = frag.get("lines", "?")
        content = frag.get("content", "")
        tokens = len(content.split())
        label = f"  {path}:{frag_lines} ({kind}"
        if symbol:
            label += f" {symbol}"
        label += f", ~{tokens}w)"
        lines.append(label)
    return "\n".join(lines)


class YamlTestRunner:
    def __init__(self, tmp_path: Path):
        self.repo = tmp_path / "test_repo"
        self._git = Pygit2Repo(self.repo)

    def add_file(self, path: str, content: str) -> Path:
        return self._git.add_file(path, content)

    def commit(self, message: str) -> str:
        return self._git.commit(message)

    def run_test_case(self, case: YamlTestCase) -> dict:
        from treemapper.diffctx import build_diff_context

        if case.fixtures.auto_garbage:
            for path, content in GARBAGE_FILES.items():
                self.add_file(path, content)
            self.commit("Add unrelated garbage files")

        for path, content in case.fixtures.distractors.items():
            self.add_file(path, content)

        for path, content in case.initial_files.items():
            self.add_file(path, content)
        base_sha = self.commit("Initial commit")

        for path, content in case.changed_files.items():
            self.add_file(path, content)
        self.commit(case.commit_message)

        budget = case.calculate_budget()

        return build_diff_context(
            root_dir=self.repo,
            diff_range=f"{base_sha}..HEAD",
            budget_tokens=budget,
        )

    def score_test_case(self, context: dict, case: YamlTestCase) -> ScoreBreakdown:
        output_fragments = context.get("fragments", [])
        all_content = self._extract_all_content(context)

        diff_lines = compute_diff_lines(case.initial_files, case.changed_files)
        diff_covered, uncovered = check_diff_coverage(all_content, diff_lines)

        _, missing_required, present_forbidden = _evaluate_oracle(output_fragments, case.fragments, case.oracle, case.accept)

        garbage_hit = 0
        if case.fixtures.auto_garbage:
            garbage_hit = sum(1 for m in GARBAGE_MARKERS if m in all_content)

        present_forbidden_with_garbage = list(present_forbidden)
        if garbage_hit > 0:
            present_forbidden_with_garbage.append(f"[garbage:{garbage_hit}]")

        return ScoreBreakdown(
            required_hits=len(case.oracle.required) - len(missing_required),
            required_total=len(case.oracle.required),
            missing_required=missing_required,
            forbidden_hits=len(present_forbidden_with_garbage),
            forbidden_total=len(case.oracle.forbidden) + (len(GARBAGE_MARKERS) if case.fixtures.auto_garbage else 0),
            present_forbidden=present_forbidden_with_garbage,
            diff_covered=diff_covered,
            uncovered_lines=uncovered,
            diff_tokens=compute_diff_token_count(case.initial_files, case.changed_files),
            context_tokens=compute_context_token_count(context),
        )

    def verify_assertions(self, context: dict, case: YamlTestCase) -> None:
        output_fragments = context.get("fragments", [])
        all_content = self._extract_all_content(context)
        diag = f"Selected fragments:\n{_format_fragment_summary(context)}"

        diff_lines = compute_diff_lines(case.initial_files, case.changed_files)
        diff_covered, uncovered = check_diff_coverage(all_content, diff_lines)

        assert diff_covered, (
            f"[{case.name}] Changed code missing from context.\n"
            f"Uncovered:\n" + "\n".join(f"  {line}" for line in uncovered[:10]) + f"\n{diag}"
        )

        _, missing_required, present_forbidden = _evaluate_oracle(output_fragments, case.fragments, case.oracle, case.accept)

        for fid in missing_required:
            assert False, f"[{case.name}] Required fragment not found: {fid}\n{diag}"

        for fid in present_forbidden:
            assert False, f"[{case.name}] Forbidden fragment present: {fid}\n{diag}"

        if case.fixtures.auto_garbage:
            leaked = [m for m in GARBAGE_MARKERS if m in all_content]
            assert not leaked, f"[{case.name}] Garbage markers leaked: {leaked[:5]}\n{diag}"

    def _extract_all_content(self, context: dict) -> str:
        parts = []
        for frag in context.get("fragments", []):
            if "content" in frag:
                parts.append(frag["content"])
            if "path" in frag:
                parts.append(frag["path"])
        return "\n".join(parts)
