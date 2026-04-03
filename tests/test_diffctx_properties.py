from __future__ import annotations

from pathlib import Path

import pytest
from hypothesis import HealthCheck, given, settings
from hypothesis import strategies as st

from tests.conftest import GARBAGE_FILES, GARBAGE_MARKERS
from tests.framework.pygit2_backend import Pygit2Repo
from treemapper.diffctx import build_diff_context
from treemapper.tokens import count_tokens

pytestmark = pytest.mark.timeout(120)


def _extract_content(context: dict) -> str:
    parts = []
    for frag in context.get("fragments", []):
        if "content" in frag:
            parts.append(frag["content"])
        if "path" in frag:
            parts.append(frag["path"])
    return "\n".join(parts)


def _fragment_paths(context: dict) -> set[str]:
    return {frag["path"] for frag in context.get("fragments", []) if "path" in frag}


def _total_content_tokens(context: dict) -> int:
    total = 0
    for frag in context.get("fragments", []):
        content = frag.get("content", "")
        if content:
            total += count_tokens(content).count
    return total


PYTHON_FUNCTIONS = [
    "def compute_hash(data: str) -> str:\n    return hash(data)\n",
    "def parse_config(path: str) -> dict:\n    return {}\n",
    "def validate_token(token: str) -> bool:\n    return len(token) > 0\n",
    "def merge_records(a: list, b: list) -> list:\n    return a + b\n",
    "def format_output(value: int) -> str:\n    return str(value)\n",
    "def calculate_score(points: list) -> float:\n    return sum(points) / len(points) if points else 0.0\n",
    "def encode_payload(data: bytes) -> str:\n    return data.hex()\n",
    "def decode_payload(text: str) -> bytes:\n    return bytes.fromhex(text)\n",
]


class TestBudgetMonotonicity:
    @given(
        budget_small=st.integers(min_value=50, max_value=500),
        budget_multiplier=st.integers(min_value=2, max_value=20),
    )
    @settings(max_examples=20, deadline=None, suppress_health_check=[HealthCheck.function_scoped_fixture])
    def test_more_budget_yields_superset_of_paths(
        self, tmp_path_factory: pytest.TempPathFactory, budget_small: int, budget_multiplier: int
    ) -> None:
        budget_large = budget_small * budget_multiplier
        tmp_path = tmp_path_factory.mktemp("monotonicity")
        g = Pygit2Repo(tmp_path / "repo")

        g.add_file("main.py", "def main():\n    return 'initial'\n")
        g.add_file("helper.py", "def helper():\n    return 'help'\n")
        g.add_file(
            "utils.py",
            "def util_a():\n    return 1\n\ndef util_b():\n    return 2\n",
        )
        g.commit("init")

        g.add_file("main.py", "def main():\n    return 'modified'\n")
        g.commit("change")

        ctx_small = build_diff_context(
            root_dir=g.path,
            diff_range="HEAD~1..HEAD",
            budget_tokens=budget_small,
        )
        ctx_large = build_diff_context(
            root_dir=g.path,
            diff_range="HEAD~1..HEAD",
            budget_tokens=budget_large,
        )

        paths_small = _fragment_paths(ctx_small)
        paths_large = _fragment_paths(ctx_large)

        assert paths_small <= paths_large, (
            f"Budget monotonicity violated: small budget ({budget_small}) produced paths "
            f"{paths_small - paths_large} not in large budget ({budget_large}) result"
        )


class TestBudgetEnforcement:
    @given(
        budget=st.integers(min_value=50, max_value=5000),
    )
    @settings(max_examples=20, deadline=None, suppress_health_check=[HealthCheck.function_scoped_fixture])
    def test_total_tokens_never_exceed_budget(self, tmp_path_factory: pytest.TempPathFactory, budget: int) -> None:
        tmp_path = tmp_path_factory.mktemp("enforcement")
        g = Pygit2Repo(tmp_path / "repo")

        for i, func_code in enumerate(PYTHON_FUNCTIONS):
            g.add_file(f"module_{i}.py", func_code)
        g.commit("init")

        g.add_file("module_0.py", "def compute_hash(data: str) -> str:\n    return str(hash(data))\n")
        g.commit("change")

        ctx = build_diff_context(
            root_dir=g.path,
            diff_range="HEAD~1..HEAD",
            budget_tokens=budget,
        )

        content_tokens = _total_content_tokens(ctx)
        overhead_per_fragment = 10
        fragment_count = ctx.get("fragment_count", len(ctx.get("fragments", [])))
        estimated_overhead = fragment_count * overhead_per_fragment
        total_estimated = content_tokens + estimated_overhead

        assert total_estimated <= budget, (
            f"Budget exceeded: estimated {total_estimated} tokens "
            f"(content={content_tokens}, overhead~{estimated_overhead}) "
            f"vs budget={budget}"
        )


class TestSelectionIdempotency:
    @given(
        budget=st.integers(min_value=100, max_value=3000),
    )
    @settings(max_examples=20, deadline=None, suppress_health_check=[HealthCheck.function_scoped_fixture])
    def test_identical_input_produces_identical_output(self, tmp_path_factory: pytest.TempPathFactory, budget: int) -> None:
        tmp_path = tmp_path_factory.mktemp("idempotent")
        g = Pygit2Repo(tmp_path / "repo")

        g.add_file("app.py", "def run():\n    return 'v1'\n")
        g.add_file("lib.py", "def support():\n    return 'ok'\n")
        g.commit("init")

        g.add_file("app.py", "def run():\n    return 'v2'\n")
        g.commit("change")

        ctx1 = build_diff_context(
            root_dir=g.path,
            diff_range="HEAD~1..HEAD",
            budget_tokens=budget,
        )
        ctx2 = build_diff_context(
            root_dir=g.path,
            diff_range="HEAD~1..HEAD",
            budget_tokens=budget,
        )

        paths1 = _fragment_paths(ctx1)
        paths2 = _fragment_paths(ctx2)
        assert paths1 == paths2, f"Non-deterministic selection: run1 paths={paths1}, run2 paths={paths2}"

        frags1 = [(f["path"], f.get("lines"), f.get("content", "")) for f in ctx1.get("fragments", [])]
        frags2 = [(f["path"], f.get("lines"), f.get("content", "")) for f in ctx2.get("fragments", [])]
        assert frags1 == frags2, "Non-deterministic selection: fragment details differ between runs"


class TestGarbageExclusionProperty:
    @given(
        num_garbage_files=st.integers(min_value=3, max_value=8),
        seed=st.integers(min_value=1000, max_value=9999),
        budget=st.integers(min_value=200, max_value=2000),
    )
    @settings(max_examples=15, deadline=None, suppress_health_check=[HealthCheck.function_scoped_fixture])
    def test_random_unrelated_files_never_appear_in_context(
        self,
        tmp_path_factory: pytest.TempPathFactory,
        num_garbage_files: int,
        seed: int,
        budget: int,
    ) -> None:
        tmp_path = tmp_path_factory.mktemp(f"garbage_{seed}")
        g = Pygit2Repo(tmp_path / "repo")

        g.add_file("feature.py", "def feature():\n    return 'initial'\n")

        markers = []
        for i in range(num_garbage_files):
            marker = f"PROP_GARBAGE_{seed}_{i}_ZZZZ"
            markers.append(marker)
            g.add_file(
                f"noise_{seed}_{i}/junk_{i}.py",
                f"{marker} = True\ndef junk_{seed}_{i}():\n    return '{marker}'\n",
            )
        g.commit("init")

        g.add_file("feature.py", "def feature():\n    return 'modified'\n")
        g.commit("change")

        ctx = build_diff_context(
            root_dir=g.path,
            diff_range="HEAD~1..HEAD",
            budget_tokens=budget,
        )

        all_content = _extract_content(ctx)
        assert "feature" in all_content, "Changed function must be in context"

        for marker in markers:
            assert marker not in all_content, f"Garbage marker '{marker}' leaked into context — algorithm included unrelated code"

    def test_standard_garbage_files_excluded(self, tmp_path: Path) -> None:
        g = Pygit2Repo(tmp_path / "repo")

        g.add_file("target.py", "def target():\n    return 'before'\n")
        for rel_path, content in GARBAGE_FILES.items():
            g.add_file(rel_path, content)
        g.commit("init")

        g.add_file("target.py", "def target():\n    return 'after'\n")
        g.commit("change")

        ctx = build_diff_context(
            root_dir=g.path,
            diff_range="HEAD~1..HEAD",
            budget_tokens=1000,
        )

        all_content = _extract_content(ctx)
        assert "target" in all_content, "Changed function must be in context"

        for marker in GARBAGE_MARKERS:
            assert (
                marker not in all_content
            ), f"Standard garbage marker '{marker}' found in context — algorithm is including known unrelated code"
