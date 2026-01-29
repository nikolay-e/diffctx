from __future__ import annotations

import math
import re
import subprocess
from pathlib import Path

import pytest
from hypothesis import given, settings
from hypothesis import strategies as st

from treemapper.diffctx import build_diff_context
from treemapper.diffctx.graph import Graph
from treemapper.diffctx.ppr import personalized_pagerank
from treemapper.diffctx.types import FragmentId


class TestPPRMathematicalInvariants:
    def _create_graph_with_edges(self, edges: list[tuple[FragmentId, FragmentId, float]]) -> Graph:
        graph = Graph()
        nodes = set()
        for src, dst, _ in edges:
            nodes.add(src)
            nodes.add(dst)
        for node in nodes:
            graph.add_node(node)
        for src, dst, weight in edges:
            graph.add_edge(src, dst, weight)
        return graph

    def test_ppr_normalization_sum_to_one(self) -> None:
        graph = self._create_graph_with_edges(
            [
                ("a", "b", 0.5),
                ("b", "c", 0.5),
                ("c", "a", 0.5),
                ("a", "d", 0.3),
            ]
        )
        seeds = {"a"}
        scores = personalized_pagerank(graph, seeds, alpha=0.6)
        total = sum(scores.values())
        assert abs(total - 1.0) < 1e-6, f"PPR scores sum to {total}, expected 1.0"

    def test_ppr_all_scores_non_negative(self) -> None:
        graph = self._create_graph_with_edges(
            [
                ("a", "b", 0.9),
                ("b", "c", 0.8),
                ("c", "d", 0.7),
                ("d", "a", 0.6),
            ]
        )
        seeds = {"a", "c"}
        scores = personalized_pagerank(graph, seeds, alpha=0.5)
        for node, score in scores.items():
            assert score >= 0, f"Node {node} has negative score: {score}"

    def test_ppr_seeds_have_higher_scores_than_average(self) -> None:
        graph = self._create_graph_with_edges(
            [
                ("seed1", "other1", 0.5),
                ("seed1", "other2", 0.5),
                ("seed2", "other3", 0.5),
                ("other1", "other4", 0.3),
                ("other2", "other5", 0.3),
            ]
        )
        seeds = {"seed1", "seed2"}
        scores = personalized_pagerank(graph, seeds, alpha=0.6)
        seed_scores = [scores[s] for s in seeds if s in scores]
        non_seed_scores = [scores[n] for n in scores if n not in seeds]
        if seed_scores and non_seed_scores:
            avg_seed = sum(seed_scores) / len(seed_scores)
            avg_non_seed = sum(non_seed_scores) / len(non_seed_scores)
            assert avg_seed > avg_non_seed, f"Seeds avg ({avg_seed:.4f}) should be > non-seeds avg ({avg_non_seed:.4f})"

    def test_ppr_deterministic_output(self) -> None:
        graph = self._create_graph_with_edges(
            [
                ("a", "b", 0.5),
                ("b", "c", 0.5),
                ("c", "a", 0.5),
            ]
        )
        seeds = {"a"}
        scores1 = personalized_pagerank(graph, seeds, alpha=0.6)
        scores2 = personalized_pagerank(graph, seeds, alpha=0.6)
        for node in scores1:
            assert (
                abs(scores1[node] - scores2[node]) < 1e-10
            ), f"Non-deterministic PPR: {node} has {scores1[node]} vs {scores2[node]}"

    def test_ppr_empty_graph_returns_empty(self) -> None:
        graph = Graph()
        scores = personalized_pagerank(graph, {"a"}, alpha=0.6)
        assert scores == {}

    def test_ppr_invalid_seeds_return_uniform(self) -> None:
        graph = self._create_graph_with_edges([("a", "b", 0.5)])
        scores = personalized_pagerank(graph, {"nonexistent"}, alpha=0.6)
        expected_uniform = 1.0 / 2
        for score in scores.values():
            assert abs(score - expected_uniform) < 1e-6

    def test_ppr_single_node_graph(self) -> None:
        graph = Graph()
        graph.add_node("solo")
        scores = personalized_pagerank(graph, {"solo"}, alpha=0.6)
        assert "solo" in scores
        assert abs(scores["solo"] - 1.0) < 1e-6

    def test_ppr_disconnected_components(self) -> None:
        graph = self._create_graph_with_edges(
            [
                ("a1", "a2", 0.5),
                ("a2", "a1", 0.5),
                ("b1", "b2", 0.5),
                ("b2", "b1", 0.5),
            ]
        )
        seeds = {"a1"}
        scores = personalized_pagerank(graph, seeds, alpha=0.6)
        assert abs(sum(scores.values()) - 1.0) < 1e-6

    @given(
        num_nodes=st.integers(min_value=2, max_value=20),
        num_edges=st.integers(min_value=1, max_value=50),
        alpha=st.floats(min_value=0.1, max_value=0.9, allow_nan=False, allow_infinity=False),
    )
    @settings(max_examples=50, deadline=None)
    def test_ppr_invariants_property_based(self, num_nodes: int, num_edges: int, alpha: float) -> None:
        nodes = [f"node_{i}" for i in range(num_nodes)]
        graph = Graph()
        for node in nodes:
            graph.add_node(node)
        for i in range(num_edges):
            src = nodes[i % num_nodes]
            dst = nodes[(i * 7 + 3) % num_nodes]
            if src != dst:
                weight = 0.1 + (i % 10) * 0.08
                graph.add_edge(src, dst, weight)
        seeds = {nodes[0]}
        scores = personalized_pagerank(graph, seeds, alpha=alpha)
        if scores:
            total = sum(scores.values())
            assert abs(total - 1.0) < 1e-5, f"Sum={total}, expected 1.0"
            for node, score in scores.items():
                assert score >= 0, f"Negative score for {node}: {score}"
                assert math.isfinite(score), f"Non-finite score for {node}: {score}"


class TestDiffContextNegativeCases:
    @pytest.fixture
    def git_repo(self, tmp_path: Path) -> Path:
        repo = tmp_path / "test_repo"
        repo.mkdir()
        subprocess.run(["git", "init"], cwd=repo, capture_output=True, check=True)
        subprocess.run(["git", "config", "user.email", "t@t.com"], cwd=repo, capture_output=True, check=True)
        subprocess.run(["git", "config", "user.name", "T"], cwd=repo, capture_output=True, check=True)
        (repo / "file.py").write_text("x = 1\n", encoding="utf-8")
        subprocess.run(["git", "add", "-A"], cwd=repo, capture_output=True, check=True)
        subprocess.run(["git", "commit", "-m", "init"], cwd=repo, capture_output=True, check=True)
        (repo / "file.py").write_text("x = 2\n", encoding="utf-8")
        subprocess.run(["git", "add", "-A"], cwd=repo, capture_output=True, check=True)
        subprocess.run(["git", "commit", "-m", "change"], cwd=repo, capture_output=True, check=True)
        return repo

    def test_invalid_diff_range_raises_error(self, git_repo: Path) -> None:
        with pytest.raises(Exception):
            build_diff_context(
                root_dir=git_repo,
                diff_range="nonexistent123..alsonotexists456",
                budget_tokens=1000,
            )

    def test_budget_zero_still_returns_structure(self, git_repo: Path) -> None:
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

    def test_empty_diff_returns_empty_fragments(self, git_repo: Path) -> None:
        head_sha = subprocess.run(
            ["git", "rev-parse", "HEAD"],
            cwd=git_repo,
            capture_output=True,
            text=True,
            check=True,
        ).stdout.strip()
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
        repo = tmp_path / "realistic_repo"
        repo.mkdir()
        subprocess.run(["git", "init"], cwd=repo, capture_output=True, check=True)
        subprocess.run(["git", "config", "user.email", "t@t.com"], cwd=repo, capture_output=True, check=True)
        subprocess.run(["git", "config", "user.name", "T"], cwd=repo, capture_output=True, check=True)
        main_module = repo / "src" / "main_feature.py"
        main_module.parent.mkdir(parents=True, exist_ok=True)
        main_module.write_text(
            """
def process_user_data(user_id: int) -> dict:
    return {"id": user_id, "status": "active"}

def validate_input(data: dict) -> bool:
    return "id" in data
""",
            encoding="utf-8",
        )
        unrelated1 = repo / "utils" / "math_helpers.py"
        unrelated1.parent.mkdir(parents=True, exist_ok=True)
        unrelated1.write_text(
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
            encoding="utf-8",
        )
        unrelated2 = repo / "services" / "email_service.py"
        unrelated2.parent.mkdir(parents=True, exist_ok=True)
        unrelated2.write_text(
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
            encoding="utf-8",
        )
        unrelated3 = repo / "config" / "settings_loader.py"
        unrelated3.parent.mkdir(parents=True, exist_ok=True)
        unrelated3.write_text(
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
            encoding="utf-8",
        )
        subprocess.run(["git", "add", "-A"], cwd=repo, capture_output=True, check=True)
        subprocess.run(["git", "commit", "-m", "initial"], cwd=repo, capture_output=True, check=True)
        base_sha = subprocess.run(
            ["git", "rev-parse", "HEAD"], cwd=repo, capture_output=True, text=True, check=True
        ).stdout.strip()
        main_module.write_text(
            """
def process_user_data(user_id: int) -> dict:
    validated = validate_input({"id": user_id})
    if not validated:
        raise ValueError("Invalid user_id")
    return {"id": user_id, "status": "active", "validated": True}

def validate_input(data: dict) -> bool:
    return "id" in data and isinstance(data["id"], int)
""",
            encoding="utf-8",
        )
        subprocess.run(["git", "add", "-A"], cwd=repo, capture_output=True, check=True)
        subprocess.run(["git", "commit", "-m", "improve user processing"], cwd=repo, capture_output=True, check=True)
        return repo, base_sha

    def test_unrelated_code_excluded_without_garbage_keyword(self, repo_with_realistic_unrelated_code: tuple[Path, str]) -> None:
        repo, base_sha = repo_with_realistic_unrelated_code
        context = build_diff_context(
            root_dir=repo,
            diff_range=f"{base_sha}..HEAD",
            budget_tokens=2000,
        )
        all_content = self._extract_content(context)
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

    def _extract_content(self, context: dict) -> str:
        parts = []
        for frag in context.get("fragments", []):
            if "content" in frag:
                parts.append(frag["content"])
            if "path" in frag:
                parts.append(frag["path"])
        return "\n".join(parts)


class TestAssertionPrecision:
    def test_word_boundary_matching(self) -> None:
        content = "def main_helper():\n    pass\ndef domain():\n    pass"
        pattern_main = r"\bmain\b"
        pattern_helper = r"\bmain_helper\b"
        assert not re.search(pattern_main, content), "main should not match main_helper"
        assert re.search(pattern_helper, content), "main_helper should match"

    def test_fragment_line_range_accuracy(self, tmp_path: Path) -> None:
        repo = tmp_path / "line_range_repo"
        repo.mkdir()
        subprocess.run(["git", "init"], cwd=repo, capture_output=True, check=True)
        subprocess.run(["git", "config", "user.email", "t@t.com"], cwd=repo, capture_output=True, check=True)
        subprocess.run(["git", "config", "user.name", "T"], cwd=repo, capture_output=True, check=True)
        code = """def first_function():
    return 1

def second_function():
    return 2

def third_function():
    return 3
"""
        (repo / "functions.py").write_text(code, encoding="utf-8")
        subprocess.run(["git", "add", "-A"], cwd=repo, capture_output=True, check=True)
        subprocess.run(["git", "commit", "-m", "init"], cwd=repo, capture_output=True, check=True)
        modified_code = """def first_function():
    return 1

def second_function():
    return 42

def third_function():
    return 3
"""
        (repo / "functions.py").write_text(modified_code, encoding="utf-8")
        subprocess.run(["git", "add", "-A"], cwd=repo, capture_output=True, check=True)
        subprocess.run(["git", "commit", "-m", "change second"], cwd=repo, capture_output=True, check=True)
        context = build_diff_context(
            root_dir=repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=5000,
        )
        all_content = self._extract_content(context)
        assert "second_function" in all_content, "Modified function must be included"
        assert "return 42" in all_content, "Modified line must be included"

    def _extract_content(self, context: dict) -> str:
        parts = []
        for frag in context.get("fragments", []):
            if "content" in frag:
                parts.append(frag["content"])
        return "\n".join(parts)


class TestBudgetEdgeCases:
    @pytest.fixture
    def large_repo(self, tmp_path: Path) -> tuple[Path, str]:
        repo = tmp_path / "large_repo"
        repo.mkdir()
        subprocess.run(["git", "init"], cwd=repo, capture_output=True, check=True)
        subprocess.run(["git", "config", "user.email", "t@t.com"], cwd=repo, capture_output=True, check=True)
        subprocess.run(["git", "config", "user.name", "T"], cwd=repo, capture_output=True, check=True)
        for i in range(10):
            (repo / f"module_{i}.py").write_text(
                f"def function_{i}():\n    return {i}\n" * 20,
                encoding="utf-8",
            )
        subprocess.run(["git", "add", "-A"], cwd=repo, capture_output=True, check=True)
        subprocess.run(["git", "commit", "-m", "init"], cwd=repo, capture_output=True, check=True)
        base = subprocess.run(["git", "rev-parse", "HEAD"], cwd=repo, capture_output=True, text=True, check=True).stdout.strip()
        (repo / "module_0.py").write_text("def function_0():\n    return 999\n", encoding="utf-8")
        subprocess.run(["git", "add", "-A"], cwd=repo, capture_output=True, check=True)
        subprocess.run(["git", "commit", "-m", "change"], cwd=repo, capture_output=True, check=True)
        return repo, base

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
        repo = tmp_path / "test_repo"
        repo.mkdir()
        subprocess.run(["git", "init"], cwd=repo, capture_output=True, check=True)
        subprocess.run(["git", "config", "user.email", "t@t.com"], cwd=repo, capture_output=True, check=True)
        subprocess.run(["git", "config", "user.name", "T"], cwd=repo, capture_output=True, check=True)
        main_file = repo / "main_feature.py"
        main_file.write_text("def main_function():\n    return 'initial'\n", encoding="utf-8")
        markers = []
        for i in range(num_unrelated_files):
            marker = f"UNIQUE_MARKER_{identifier_seed}_{i}_XYZ"
            markers.append(marker)
            unrelated = repo / f"unrelated_{identifier_seed}_{i}.py"
            unrelated.write_text(
                f"{marker} = True\ndef helper_{identifier_seed}_{i}():\n    return '{marker}'\n",
                encoding="utf-8",
            )
        subprocess.run(["git", "add", "-A"], cwd=repo, capture_output=True, check=True)
        subprocess.run(["git", "commit", "-m", "init"], cwd=repo, capture_output=True, check=True)
        main_file.write_text("def main_function():\n    return 'modified'\n", encoding="utf-8")
        subprocess.run(["git", "add", "-A"], cwd=repo, capture_output=True, check=True)
        subprocess.run(["git", "commit", "-m", "change"], cwd=repo, capture_output=True, check=True)
        context = build_diff_context(
            root_dir=repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=500,
        )
        all_content = self._extract_content(context)
        assert "main_function" in all_content, "Changed function must be included"
        for marker in markers:
            assert (
                marker not in all_content
            ), f"Randomized marker '{marker}' should NOT be in context. Algorithm is including unrelated code."

    def _extract_content(self, context: dict) -> str:
        parts = []
        for frag in context.get("fragments", []):
            if "content" in frag:
                parts.append(frag["content"])
            if "path" in frag:
                parts.append(frag["path"])
        return "\n".join(parts)


class TestGraphBuildingIntegrity:
    def test_graph_self_loops_dont_break_ppr(self) -> None:
        graph = Graph()
        graph.add_node("a")
        graph.add_node("b")
        graph.add_edge("a", "a", 0.5)
        graph.add_edge("a", "b", 0.5)
        scores = personalized_pagerank(graph, {"a"}, alpha=0.6)
        assert abs(sum(scores.values()) - 1.0) < 1e-6, "Self-loops should not break normalization"
        assert all(s >= 0 for s in scores.values()), "Self-loops should not produce negative scores"

    def test_graph_invalid_edge_weights_filtered(self) -> None:
        graph = Graph()
        graph.add_node("a")
        graph.add_node("b")
        graph.add_edge("a", "b", float("inf"))
        neighbors_after_inf = graph.neighbors("a")
        assert "b" not in neighbors_after_inf or math.isfinite(
            neighbors_after_inf.get("b", 0)
        ), "Infinite weight edge should be filtered"
        graph.add_edge("a", "b", float("nan"))
        neighbors_after_nan = graph.neighbors("a")
        for weight in neighbors_after_nan.values():
            assert math.isfinite(weight), f"NaN weight should be filtered, got {weight}"

    def test_graph_zero_weight_edge_filtered(self) -> None:
        graph = Graph()
        graph.add_node("a")
        graph.add_node("b")
        graph.add_edge("a", "b", 0.0)
        neighbors = graph.neighbors("a")
        assert "b" not in neighbors, "Zero-weight edge should be filtered"

    def test_graph_negative_weight_edge_filtered(self) -> None:
        graph = Graph()
        graph.add_node("a")
        graph.add_node("b")
        graph.add_edge("a", "b", -0.5)
        neighbors = graph.neighbors("a")
        assert "b" not in neighbors, "Negative-weight edge should be filtered"
