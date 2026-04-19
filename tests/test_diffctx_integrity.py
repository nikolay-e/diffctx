from __future__ import annotations

from pathlib import Path
from typing import Any

import pytest
from hypothesis import given, settings
from hypothesis import strategies as st

from tests.framework.pygit2_backend import Pygit2Repo
from treemapper.diffctx import build_diff_context


def _extract_content(context: dict[str, Any]) -> str:
    parts = []
    for frag in context.get("fragments", []):
        if "content" in frag:
            parts.append(frag["content"])
        if "path" in frag:
            parts.append(frag["path"])
    return "\n".join(parts)


class TestDiffContextNegativeCases:
    @pytest.fixture
    def git_repo(self, tmp_path: Path) -> Path:
        g = Pygit2Repo(tmp_path / "test_repo")
        g.add_file("file.py", "x = 1\n")
        g.commit("init")
        g.add_file("file.py", "x = 2\n")
        g.commit("change")
        return g.path

    def test_invalid_diff_range_raises_error(self, git_repo: Path) -> None:
        with pytest.raises(Exception):
            build_diff_context(
                root_dir=git_repo,
                diff_range="nonexistent123..alsonotexists456",
                budget_tokens=1000,
            )

    def test_minimal_budget_still_returns_structure(self, git_repo: Path) -> None:
        context = build_diff_context(
            root_dir=git_repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=1,
        )
        assert "fragments" in context or context.get("type") == "diff_context"

    def test_not_a_git_repo_raises_error(self, tmp_path: Path) -> None:
        non_repo = tmp_path / "not_a_repo"
        non_repo.mkdir()
        (non_repo / "file.txt").write_text("content", encoding="utf-8")
        with pytest.raises(Exception):
            build_diff_context(
                root_dir=non_repo,
                diff_range="HEAD~1..HEAD",
                budget_tokens=1000,
            )

    def test_multi_file_change_includes_all_changed_files(self, tmp_path: Path) -> None:
        g = Pygit2Repo(tmp_path / "multi_file_repo")
        g.add_file("alpha.py", "def alpha():\n    return 1\n")
        g.add_file("beta.py", "def beta():\n    return 2\n")
        g.add_file("gamma.py", "def gamma():\n    return 3\n")
        g.commit("init")
        g.add_file("alpha.py", "def alpha():\n    return 10\n")
        g.add_file("beta.py", "def beta():\n    return 20\n")
        g.commit("change two files")
        context = build_diff_context(
            root_dir=g.path,
            diff_range="HEAD~1..HEAD",
            budget_tokens=5000,
        )
        all_content = _extract_content(context)
        assert "alpha" in all_content, "First changed file must be included"
        assert "beta" in all_content, "Second changed file must be included"

    def test_binary_file_in_diff_does_not_crash(self, tmp_path: Path) -> None:
        g = Pygit2Repo(tmp_path / "binary_repo")
        g.add_file("code.py", "x = 1\n")
        g.add_file_binary("image.bin", b"\x89PNG\r\n\x1a\n" + bytes(range(256)))
        g.commit("init")
        g.add_file("code.py", "x = 2\n")
        g.add_file_binary("image.bin", b"\x89PNG\r\n\x1a\n" + bytes(reversed(range(256))))
        g.commit("change")
        context = build_diff_context(
            root_dir=g.path,
            diff_range="HEAD~1..HEAD",
            budget_tokens=5000,
        )
        assert "fragments" in context or context.get("type") == "diff_context"

    def test_empty_diff_returns_empty_fragments(self, git_repo: Path) -> None:
        from tests.framework.pygit2_backend import _get_repo, _resolve_commit

        repo = _get_repo(git_repo)
        head = _resolve_commit(repo, "HEAD")
        head_sha = str(head.id)
        context = build_diff_context(
            root_dir=git_repo,
            diff_range=f"{head_sha}..{head_sha}",
            budget_tokens=1000,
        )
        fragments = context.get("fragments", [])
        assert len(fragments) == 0


class TestRealisticGarbageFiltering:
    @pytest.fixture
    def repo_with_realistic_unrelated_code(self, tmp_path: Path) -> tuple[Path, str]:
        g = Pygit2Repo(tmp_path / "realistic_repo")

        g.add_file(
            "src/main_feature.py",
            """
def process_user_data(user_id: int) -> dict:
    return {"id": user_id, "status": "active"}

def validate_input(data: dict) -> bool:
    return "id" in data
""",
        )
        g.add_file(
            "utils/math_helpers.py",
            """
def calculate_fibonacci(n: int) -> int:
    FIBONACCI_MARKER_UNIQUE_12345 = True
    if n <= 1:
        return n
    return calculate_fibonacci(n - 1) + calculate_fibonacci(n - 2)

def compute_prime_factors(num: int) -> list:
    PRIME_MARKER_UNIQUE_67890 = True
    factors = []
    d = 2
    while d * d <= num:
        while num % d == 0:
            factors.append(d)
            num //= d
        d += 1
    if num > 1:
        factors.append(num)
    return factors

class MathUtilities:
    MATH_CLASS_MARKER_ABCDE = "math_utils"

    def factorial(self, n: int) -> int:
        if n <= 1:
            return 1
        return n * self.factorial(n - 1)
""",
        )
        g.add_file(
            "services/email_service.py",
            """
class EmailSender:
    EMAIL_SENDER_MARKER_FGHIJ = "email"

    def send_notification(self, recipient: str, subject: str) -> bool:
        NOTIFICATION_MARKER_KLMNO = True
        return True

    def validate_email(self, email: str) -> bool:
        return "@" in email

def format_email_body(template: str, params: dict) -> str:
    EMAIL_FORMAT_MARKER_PQRST = True
    return template.format(**params)
""",
        )
        g.add_file(
            "config/settings_loader.py",
            """
import os

CONFIG_LOADER_MARKER_UVWXY = "config"

def load_environment_config() -> dict:
    ENV_CONFIG_MARKER_ZABCD = True
    return {
        "database_url": os.getenv("DATABASE_URL", "localhost"),
        "api_key": os.getenv("API_KEY", ""),
    }

class ConfigurationManager:
    CONFIG_MANAGER_MARKER_EFGHI = "manager"

    def __init__(self):
        self.settings = {}

    def get(self, key: str, default=None):
        return self.settings.get(key, default)
""",
        )
        base_sha = g.commit("initial")

        g.add_file(
            "src/main_feature.py",
            """
def process_user_data(user_id: int) -> dict:
    validated = validate_input({"id": user_id})
    if not validated:
        raise ValueError("Invalid user_id")
    return {"id": user_id, "status": "active", "validated": True}

def validate_input(data: dict) -> bool:
    return "id" in data and isinstance(data["id"], int)
""",
        )
        g.commit("improve user processing")
        return g.path, base_sha

    def test_unrelated_code_excluded_without_garbage_keyword(self, repo_with_realistic_unrelated_code: tuple[Path, str]) -> None:
        repo, base_sha = repo_with_realistic_unrelated_code
        context = build_diff_context(
            root_dir=repo,
            diff_range=f"{base_sha}..HEAD",
            budget_tokens=2000,
        )
        all_content = _extract_content(context)
        assert "process_user_data" in all_content, "Changed function should be included"
        assert "validate_input" in all_content, "Related function should be included"
        unrelated_markers = [
            "FIBONACCI_MARKER_UNIQUE_12345",
            "PRIME_MARKER_UNIQUE_67890",
            "MATH_CLASS_MARKER_ABCDE",
            "EMAIL_SENDER_MARKER_FGHIJ",
            "NOTIFICATION_MARKER_KLMNO",
            "EMAIL_FORMAT_MARKER_PQRST",
            "CONFIG_LOADER_MARKER_UVWXY",
            "ENV_CONFIG_MARKER_ZABCD",
            "CONFIG_MANAGER_MARKER_EFGHI",
        ]
        for marker in unrelated_markers:
            assert marker not in all_content, (
                f"Unrelated code marker '{marker}' should NOT be in context. "
                f"Algorithm is including irrelevant code without 'garbage' keyword."
            )


class TestAssertionPrecision:
    def test_fragment_line_range_accuracy(self, tmp_path: Path) -> None:
        g = Pygit2Repo(tmp_path / "line_range_repo")
        code = """def first_function():
    return 1

def second_function():
    return 2

def third_function():
    return 3
"""
        g.add_file("functions.py", code)
        g.commit("init")
        modified_code = """def first_function():
    return 1

def second_function():
    return 42

def third_function():
    return 3
"""
        g.add_file("functions.py", modified_code)
        g.commit("change second")
        context = build_diff_context(
            root_dir=g.path,
            diff_range="HEAD~1..HEAD",
            budget_tokens=5000,
        )
        all_content = self._extract_content(context)
        assert "second_function" in all_content, "Modified function must be included"
        assert "return 42" in all_content, "Modified line must be included"

    def _extract_content(self, context: dict[str, Any]) -> str:
        parts = []
        for frag in context.get("fragments", []):
            if "content" in frag:
                parts.append(frag["content"])
        return "\n".join(parts)


class TestBudgetEdgeCases:
    @pytest.fixture
    def large_repo(self, tmp_path: Path) -> tuple[Path, str]:
        g = Pygit2Repo(tmp_path / "large_repo")
        for i in range(10):
            g.add_file(f"module_{i}.py", f"def function_{i}():\n    return {i}\n" * 20)
        base = g.commit("init")
        g.add_file("module_0.py", "def function_0():\n    return 999\n")
        g.commit("change")
        return g.path, base

    def test_very_small_budget_still_includes_core(self, large_repo: tuple[Path, str]) -> None:
        repo, base = large_repo
        context = build_diff_context(
            root_dir=repo,
            diff_range=f"{base}..HEAD",
            budget_tokens=50,
        )
        fragments = context.get("fragments", [])
        assert len(fragments) >= 1, "Even tiny budget should include at least core change"

    def test_large_budget_includes_more_context(self, large_repo: tuple[Path, str]) -> None:
        repo, base = large_repo
        small_context = build_diff_context(
            root_dir=repo,
            diff_range=f"{base}..HEAD",
            budget_tokens=100,
        )
        large_context = build_diff_context(
            root_dir=repo,
            diff_range=f"{base}..HEAD",
            budget_tokens=10000,
        )
        small_frags = len(small_context.get("fragments", []))
        large_frags = len(large_context.get("fragments", []))
        assert large_frags >= small_frags, "Larger budget should include at least as many fragments"


class TestRandomizedGarbageFiltering:
    @given(
        num_unrelated_files=st.integers(min_value=2, max_value=5),
        identifier_seed=st.integers(min_value=1000, max_value=9999),
    )
    @settings(max_examples=10, deadline=None)
    def test_randomized_unrelated_code_excluded(
        self, tmp_path_factory: pytest.TempPathFactory, num_unrelated_files: int, identifier_seed: int
    ) -> None:
        tmp_path = tmp_path_factory.mktemp(f"repo_{identifier_seed}")
        g = Pygit2Repo(tmp_path / "test_repo")
        g.add_file("main_feature.py", "def main_function():\n    return 'initial'\n")
        markers = []
        for i in range(num_unrelated_files):
            marker = f"UNIQUE_MARKER_{identifier_seed}_{i}_XYZ"
            markers.append(marker)
            g.add_file(
                f"unrelated_{identifier_seed}_{i}.py",
                f"{marker} = True\ndef helper_{identifier_seed}_{i}():\n    return '{marker}'\n",
            )
        g.commit("init")
        g.add_file("main_feature.py", "def main_function():\n    return 'modified'\n")
        g.commit("change")
        context = build_diff_context(
            root_dir=g.path,
            diff_range="HEAD~1..HEAD",
            budget_tokens=500,
        )
        all_content = _extract_content(context)
        assert "main_function" in all_content, "Changed function must be included"
        for marker in markers:
            assert (
                marker not in all_content
            ), f"Randomized marker '{marker}' should NOT be in context. Algorithm is including unrelated code."


class TestSelectionTauThreshold:
    def test_tau_controls_fragment_count(self, tmp_path: Path) -> None:
        g = Pygit2Repo(tmp_path / "tau_repo")

        chain_size = 15
        lines: list[str] = []
        for i in range(chain_size):
            lines.append(f"def func_{i}():")
            if i + 1 < chain_size:
                lines.append(f"    return func_{i + 1}()")
            else:
                lines.append(f"    return {i}")
            lines.append("")
        g.add_file("chain.py", "\n".join(lines))
        g.commit("init chain")

        modified_lines = list(lines)
        modified_lines[1] = "    result = func_1()\n    return result + 0"
        g.add_file("chain.py", "\n".join(modified_lines))
        g.commit("change func_0")

        context_loose = build_diff_context(
            root_dir=g.path,
            diff_range="HEAD~1..HEAD",
            budget_tokens=50000,
            tau=0.001,
        )
        context_tight = build_diff_context(
            root_dir=g.path,
            diff_range="HEAD~1..HEAD",
            budget_tokens=50000,
            tau=0.95,
        )

        fragments_loose = len(context_loose.get("fragments", []))
        fragments_tight = len(context_tight.get("fragments", []))

        assert fragments_tight <= fragments_loose, (
            f"Tight tau=0.95 selected {fragments_tight} fragments but loose tau=0.001 "
            f"selected only {fragments_loose}; tighter tau must not select more"
        )
