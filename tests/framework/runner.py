from __future__ import annotations

import subprocess
from pathlib import Path

from tests.conftest import GARBAGE_FILES, GARBAGE_MARKERS
from tests.framework.scoring import (
    ScoreBreakdown,
    check_diff_coverage,
    compute_context_token_count,
    compute_diff_lines,
    compute_diff_token_count,
)
from tests.framework.types import YamlTestCase


def _match_path(candidate: str, target: str) -> bool:
    return candidate == target or candidate.endswith(f"/{target}")


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
        self.repo.mkdir()
        subprocess.run(["git", "init"], cwd=self.repo, capture_output=True, check=True)
        subprocess.run(["git", "config", "user.email", "test@test.com"], cwd=self.repo, capture_output=True, check=True)
        subprocess.run(["git", "config", "user.name", "Test"], cwd=self.repo, capture_output=True, check=True)

    def add_file(self, path: str, content: str) -> Path:
        file_path = self.repo / path
        file_path.parent.mkdir(parents=True, exist_ok=True)
        file_path.write_text(content, encoding="utf-8")
        return file_path

    def commit(self, message: str) -> str:
        subprocess.run(["git", "add", "-A"], cwd=self.repo, capture_output=True, check=True)
        result = subprocess.run(
            ["git", "commit", "-m", message, "--allow-empty"],
            cwd=self.repo,
            capture_output=True,
            text=True,
        )
        if result.returncode != 0:
            subprocess.run(["git", "commit", "-m", message], cwd=self.repo, capture_output=True, check=True)
        rev = subprocess.run(["git", "rev-parse", "HEAD"], cwd=self.repo, capture_output=True, text=True, check=True)
        return rev.stdout.strip()

    def run_test_case(self, case: YamlTestCase) -> dict:
        from treemapper.diffctx import build_diff_context

        if case.add_garbage_files:
            for path, content in GARBAGE_FILES.items():
                self.add_file(path, content)
            self.commit("Add unrelated garbage files")

        for path, content in case.initial_files.items():
            self.add_file(path, content)
        base_sha = self.commit("Initial commit")

        for path, content in case.changed_files.items():
            self.add_file(path, content)
        self.commit(case.commit_message)

        budget = case.calculate_budget()

        context = build_diff_context(
            root_dir=self.repo,
            diff_range=f"{base_sha}..HEAD",
            budget_tokens=budget,
        )

        return context

    def score_test_case(self, context: dict, case: YamlTestCase) -> ScoreBreakdown:
        fragment_paths = self._extract_fragment_paths(context)
        all_content = self._extract_all_content(context)
        content_by_file = self._extract_content_by_file(context)

        expected_files = set(case.must_include_files) | set(case.must_include_content_from.keys())
        excluded_files = set(case.must_not_include_files) | set(case.must_not_include)
        diff_lines = compute_diff_lines(
            case.initial_files,
            case.changed_files,
            expected_files=expected_files if expected_files else None,
            excluded_files=excluded_files,
        )
        diff_covered, uncovered = check_diff_coverage(all_content, diff_lines)

        expected_details = self._check_expected(case, fragment_paths, all_content, content_by_file)
        noise_details = self._check_noise(case, fragment_paths, all_content, context)
        garbage_hits, garbage_total = self._check_garbage(case, all_content)

        return ScoreBreakdown(
            diff_covered=diff_covered,
            uncovered_lines=uncovered,
            expected_hits=sum(1 for _, hit in expected_details if hit),
            expected_total=len(expected_details),
            expected_details=expected_details,
            noise_hits=sum(1 for _, leaked in noise_details if leaked),
            noise_total=len(noise_details),
            noise_details=noise_details,
            garbage_hits=garbage_hits,
            garbage_total=garbage_total,
            diff_tokens=compute_diff_token_count(case.initial_files, case.changed_files),
            context_tokens=compute_context_token_count(context),
        )

    def _check_expected(
        self,
        case: YamlTestCase,
        fragment_paths: list[str],
        all_content: str,
        content_by_file: dict[str, str],
    ) -> list[tuple[str, bool]]:
        details: list[tuple[str, bool]] = []
        for pattern in case.must_include:
            details.append((f"pattern: {pattern[:80]}", pattern in all_content))
        for file_path in case.must_include_files:
            hit = any(_match_path(p, file_path) for p in fragment_paths)
            details.append((f"file: {file_path}", hit))
        for content_block in case.must_include_content:
            normalized = content_block.rstrip("\n")
            details.append((f"content: {normalized[:80]}", normalized in all_content))
        for file_path, snippets in case.must_include_content_from.items():
            file_content = self._find_file_content(content_by_file, file_path)
            for snippet in snippets:
                normalized = snippet.rstrip("\n")
                hit = file_content is not None and normalized in file_content
                details.append((f"from {file_path}: {normalized[:60]}", hit))
        return details

    def _check_noise(
        self,
        case: YamlTestCase,
        fragment_paths: list[str],
        all_content: str,
        context: dict,
    ) -> list[tuple[str, bool]]:
        details: list[tuple[str, bool]] = []
        for pattern in case.must_not_include:
            details.append((f"noise: {pattern[:80]}", pattern in all_content))
        for file_path in case.must_not_include_files:
            leaked = any(_match_path(p, file_path) for p in fragment_paths)
            details.append((f"noise_file: {file_path}", leaked))
        if case.max_fragments is not None:
            frag_count = len(context.get("fragments", []))
            if frag_count > case.max_fragments:
                details.append((f"excess_fragments: {frag_count}/{case.max_fragments}", True))
        if case.max_files is not None:
            unique_files = len(set(fragment_paths))
            if unique_files > case.max_files:
                details.append((f"excess_files: {unique_files}/{case.max_files}", True))
        return details

    def _check_garbage(self, case: YamlTestCase, all_content: str) -> tuple[int, int]:
        if not case.add_garbage_files or case.skip_garbage_check:
            return 0, 0
        garbage_hits = sum(1 for marker in GARBAGE_MARKERS if marker in all_content)
        return garbage_hits, len(GARBAGE_MARKERS)

    def verify_assertions(self, context: dict, case: YamlTestCase) -> None:
        breakdown = self.score_test_case(context, case)
        diag = f"Score: {breakdown.score}%\nSelected fragments:\n{_format_fragment_summary(context)}"

        assert breakdown.diff_covered, (
            f"[{case.name}] Changed code missing from context.\n"
            f"Uncovered lines:\n" + "\n".join(f"  {line}" for line in breakdown.uncovered_lines[:10]) + f"\n{diag}"
        )

        for desc, hit in breakdown.expected_details:
            assert hit, f"[{case.name}] Expected not found: {desc}\n{diag}"

        for desc, leaked in breakdown.noise_details:
            assert not leaked, f"[{case.name}] Unwanted found: {desc}\n{diag}"

        if case.add_garbage_files and not case.skip_garbage_check:
            assert breakdown.garbage_hits == 0, f"[{case.name}] {breakdown.garbage_hits} garbage markers leaked.\n{diag}"

    def _find_file_content(self, content_by_file: dict[str, str], target: str) -> str | None:
        for path, content in content_by_file.items():
            if _match_path(path, target):
                return content
        return None

    def _extract_all_content(self, context: dict) -> str:
        parts = []
        for frag in context.get("fragments", []):
            if "content" in frag:
                parts.append(frag["content"])
            if "path" in frag:
                parts.append(frag["path"])
        return "\n".join(parts)

    def _extract_fragment_paths(self, context: dict) -> list[str]:
        return [frag["path"] for frag in context.get("fragments", []) if "path" in frag]

    def _extract_content_by_file(self, context: dict) -> dict[str, str]:
        by_file: dict[str, list[str]] = {}
        for frag in context.get("fragments", []):
            path = frag.get("path", "")
            if path not in by_file:
                by_file[path] = []
            if "content" in frag:
                by_file[path].append(frag["content"])
        return {path: "\n".join(parts) for path, parts in by_file.items()}
