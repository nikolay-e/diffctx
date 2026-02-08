from __future__ import annotations

import subprocess
from pathlib import Path

from tests.conftest import GARBAGE_FILES, GARBAGE_MARKERS
from tests.framework.types import YamlTestCase


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

    def verify_assertions(self, context: dict, case: YamlTestCase) -> None:
        all_content = self._extract_all_content(context)
        fragment_paths = self._extract_fragment_paths(context)

        for pattern in case.must_include:
            assert pattern in all_content, (
                f"[{case.name}] Expected '{pattern}' in context, but not found.\n" + f"Fragment paths: {fragment_paths}"
            )

        for file_path in case.must_include_files:
            assert any(file_path in p for p in fragment_paths), (
                f"[{case.name}] Expected file '{file_path}' in fragments, but not found.\n" + f"Fragment paths: {fragment_paths}"
            )

        for content_block in case.must_include_content:
            normalized_block = content_block.rstrip("\n")
            assert normalized_block in all_content, (
                f"[{case.name}] Expected content block not found in context.\n"
                f"Expected (first 200 chars):\n{normalized_block[:200]}\n"
                f"Fragment paths: {fragment_paths}"
            )

        for pattern in case.must_not_include:
            assert pattern not in all_content, f"[{case.name}] Expected '{pattern}' to NOT be in context, but it was found"

        if case.add_garbage_files and not case.skip_garbage_check:
            for marker in GARBAGE_MARKERS:
                assert marker not in all_content, (
                    f"[{case.name}] Garbage marker '{marker}' found in context! "
                    f"Algorithm included unrelated code that should have been excluded."
                )

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
