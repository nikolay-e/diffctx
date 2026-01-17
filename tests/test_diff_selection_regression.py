from pathlib import Path

import pytest

from treemapper.diffctx import build_diff_context
from treemapper.diffctx.select import lazy_greedy_select
from treemapper.diffctx.types import Fragment, FragmentId
from treemapper.diffctx.utility import UtilityState, apply_fragment, marginal_gain, utility_value


def _extract_fragments_from_tree(tree: dict) -> list[dict]:
    if tree.get("type") == "diff_context":
        return tree.get("fragments", [])
    return []


def _make_fragment(path: str, start: int, end: int, content: str = "", tokens: int = 100) -> Fragment:
    frag = Fragment(
        id=FragmentId(Path(path), start, end),
        kind="function",
        content=content or f"content {start}-{end}",
        identifiers=frozenset(content.split() if content else []),
    )
    frag.token_count = tokens
    return frag


@pytest.fixture
def diff_project(git_with_commits):
    return git_with_commits


class TestCoreIdentification:
    def test_sel_core_001_hunk_fully_inside_fragment(self, diff_project):
        diff_project.add_file(
            "module.py",
            """\
def long_function():
    line1 = 1
    line2 = 2
    line3 = 3
    line4 = 4
    line5 = 5
    line6 = 6
    line7 = 7
    line8 = 8
    return line8
""",
        )
        diff_project.commit("Initial")

        diff_project.add_file(
            "module.py",
            """\
def long_function():
    line1 = 1
    line2 = 2
    line3 = 3
    modified = "changed"
    line5 = 5
    line6 = 6
    line7 = 7
    line8 = 8
    return line8
""",
        )
        diff_project.commit("Modify middle")

        tree = build_diff_context(
            root_dir=diff_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        fragments = _extract_fragments_from_tree(tree)
        assert len(fragments) >= 1
        func_frag = next((f for f in fragments if "long_function" in f.get("content", "")), None)
        assert func_frag is not None

    def test_sel_core_002_deletion_hunk_handled(self, diff_project):
        diff_project.add_file(
            "delete_test.py",
            """\
def func1():
    pass

def func2_to_delete():
    x = 1
    y = 2
    return x + y

def func3():
    pass
""",
        )
        diff_project.commit("Initial")

        diff_project.add_file(
            "delete_test.py",
            """\
def func1():
    pass

def func3():
    pass
""",
        )
        diff_project.commit("Delete func2")

        tree = build_diff_context(
            root_dir=diff_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        fragments = _extract_fragments_from_tree(tree)
        assert len(fragments) >= 1

    def test_sel_core_003_addition_hunk_handled(self, diff_project):
        diff_project.add_file(
            "add_test.py",
            """\
def func1():
    pass

def func2():
    pass
""",
        )
        diff_project.commit("Initial")

        diff_project.add_file(
            "add_test.py",
            """\
def func1():
    pass

def new_function():
    x = 1
    y = 2
    return x + y

def func2():
    pass
""",
        )
        diff_project.commit("Add new function")

        tree = build_diff_context(
            root_dir=diff_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        fragments = _extract_fragments_from_tree(tree)
        assert len(fragments) >= 1
        new_func = next((f for f in fragments if "new_function" in f.get("content", "")), None)
        assert new_func is not None

    def test_sel_core_004_new_function_at_end_of_file(self, diff_project):
        """Regression test: new function added at end of file must be included.

        The hunk may start on empty line before function definition,
        but the function fragment must still be detected as core.
        """
        diff_project.add_file(
            "calculator.py",
            """\
def add(a, b):
    return a + b

def sub(a, b):
    return a - b

def mul(a, b):
    return a * b
""",
        )
        diff_project.commit("Initial calculator")

        diff_project.add_file(
            "calculator.py",
            """\
def add(a, b):
    return a + b

def sub(a, b):
    return a - b

def mul(a, b):
    return a * b

def div(a, b):
    if b == 0:
        raise ValueError("division by zero")
    return a / b
""",
        )
        diff_project.commit("Add div function")

        tree = build_diff_context(
            root_dir=diff_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        fragments = _extract_fragments_from_tree(tree)
        div_func = next((f for f in fragments if "def div" in f.get("content", "")), None)
        assert div_func is not None, "div function must be included in diff context"

        mul_func = next((f for f in fragments if "def mul" in f.get("content", "")), None)
        assert mul_func is None, "mul function should NOT be included (not changed)"


class TestBudgetBehavior:
    def test_sel_budget_001_respects_token_limit(self):
        fragments = [
            _make_fragment("a.py", 1, 10, "func_a code", tokens=500),
            _make_fragment("b.py", 1, 10, "func_b code", tokens=500),
            _make_fragment("c.py", 1, 10, "func_c code", tokens=500),
        ]
        core_ids = {fragments[0].id}
        rel = {f.id: 1.0 for f in fragments}
        concepts = frozenset(["func_a", "func_b", "func_c"])

        result = lazy_greedy_select(
            fragments=fragments,
            core_ids=core_ids,
            rel=rel,
            concepts=concepts,
            budget_tokens=800,
            tau=0.0,
        )

        total_tokens = sum(f.token_count for f in result.selected)
        assert total_tokens <= 800

    def test_sel_budget_002_core_larger_than_budget_partial(self):
        fragments = [
            _make_fragment("big.py", 1, 100, "huge_function", tokens=2000),
            _make_fragment("small.py", 1, 10, "tiny_func", tokens=100),
        ]
        core_ids = {fragments[0].id}
        rel = {f.id: 1.0 for f in fragments}
        concepts = frozenset(["huge_function", "tiny_func"])

        result = lazy_greedy_select(
            fragments=fragments,
            core_ids=core_ids,
            rel=rel,
            concepts=concepts,
            budget_tokens=500,
            tau=0.0,
        )

        assert result.reason in ("budget_exhausted", "no_candidates")

    def test_sel_budget_003_overhead_accounted(self, diff_project):
        diff_project.add_file(
            "overhead.py",
            """\
def func():
    pass
""",
        )
        diff_project.commit("Initial")

        diff_project.add_file(
            "overhead.py",
            """\
def func():
    return 1
""",
        )
        diff_project.commit("Change")

        tree = build_diff_context(
            root_dir=diff_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=50,
        )

        fragments = _extract_fragments_from_tree(tree)
        assert isinstance(fragments, list)


class TestTauStopping:
    def test_sel_tau_001_zero_no_early_stop(self):
        fragments = [
            _make_fragment("a.py", 1, 10, "high_value code", tokens=100),
            _make_fragment("b.py", 1, 10, "medium_value code", tokens=100),
            _make_fragment("c.py", 1, 10, "low_value code", tokens=100),
        ]
        core_ids = set()
        rel = {
            fragments[0].id: 1.0,
            fragments[1].id: 0.5,
            fragments[2].id: 0.1,
        }
        concepts = frozenset(["high_value", "medium_value", "low_value"])

        result = lazy_greedy_select(
            fragments=fragments,
            core_ids=core_ids,
            rel=rel,
            concepts=concepts,
            budget_tokens=10000,
            tau=0.0,
        )

        assert result.reason != "stopped_by_tau"

    def test_sel_tau_002_high_tau_early_stop(self):
        fragments = [
            _make_fragment("a.py", 1, 10, "high_value", tokens=100),
            _make_fragment("b.py", 1, 10, "medium_value", tokens=100),
            _make_fragment("c.py", 1, 10, "low_value", tokens=100),
            _make_fragment("d.py", 1, 10, "very_low", tokens=100),
            _make_fragment("e.py", 1, 10, "minimal", tokens=100),
            _make_fragment("f.py", 1, 10, "tiny", tokens=100),
            _make_fragment("g.py", 1, 10, "micro", tokens=100),
            _make_fragment("h.py", 1, 10, "nano", tokens=100),
        ]
        core_ids = set()
        rel = {
            fragments[0].id: 1.0,
            fragments[1].id: 0.8,
            fragments[2].id: 0.6,
            fragments[3].id: 0.001,
            fragments[4].id: 0.001,
            fragments[5].id: 0.001,
            fragments[6].id: 0.001,
            fragments[7].id: 0.001,
        }
        concepts = frozenset(["high_value", "medium_value", "low_value", "very_low", "minimal", "tiny", "micro", "nano"])

        result = lazy_greedy_select(
            fragments=fragments,
            core_ids=core_ids,
            rel=rel,
            concepts=concepts,
            budget_tokens=10000,
            tau=0.5,
        )

        assert len(result.selected) < len(fragments)


class TestSelectionReasons:
    def test_sel_reason_001_no_candidates(self):
        result = lazy_greedy_select(
            fragments=[],
            core_ids=set(),
            rel={},
            concepts=frozenset(),
            budget_tokens=10000,
            tau=0.0,
        )

        assert result.reason == "no_candidates"
        assert len(result.selected) == 0

    def test_sel_reason_002_budget_exhausted(self):
        fragments = [
            _make_fragment("a.py", 1, 10, "func_a", tokens=600),
            _make_fragment("b.py", 1, 10, "func_b", tokens=600),
        ]
        core_ids = {fragments[0].id, fragments[1].id}
        rel = {f.id: 1.0 for f in fragments}
        concepts = frozenset(["func_a", "func_b"])

        result = lazy_greedy_select(
            fragments=fragments,
            core_ids=core_ids,
            rel=rel,
            concepts=concepts,
            budget_tokens=700,
            tau=0.0,
        )

        assert result.reason == "budget_exhausted"

    def test_sel_reason_003_no_utility_empty_concepts(self):
        fragments = [
            _make_fragment("a.py", 1, 10, "", tokens=100),
        ]
        core_ids = set()
        rel = {fragments[0].id: 0.0}
        concepts = frozenset()

        result = lazy_greedy_select(
            fragments=fragments,
            core_ids=core_ids,
            rel=rel,
            concepts=concepts,
            budget_tokens=10000,
            tau=0.0,
        )

        assert result.reason in ("no_utility", "no_candidates", "stopped_by_tau")


class TestExpansionBehavior:
    def test_sel_expand_001_rare_identifier_expansion(self, diff_project):
        diff_project.add_file(
            "core.py",
            """\
from rare_module import unique_helper

def main():
    return unique_helper()
""",
        )
        diff_project.add_file(
            "rare_module.py",
            """\
def unique_helper():
    return 42
""",
        )
        diff_project.commit("Initial")

        diff_project.add_file(
            "core.py",
            """\
from rare_module import unique_helper

def main():
    result = unique_helper()
    return result * 2
""",
        )
        diff_project.commit("Modify main")

        tree = build_diff_context(
            root_dir=diff_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        fragments = _extract_fragments_from_tree(tree)
        paths = {f.get("path", "") for f in fragments}
        assert any("core.py" in p for p in paths)

    def test_sel_expand_002_forward_dependency(self, diff_project):
        diff_project.add_file(
            "lib.py",
            """\
def helper_function():
    return 42
""",
        )
        diff_project.add_file(
            "app.py",
            """\
from lib import helper_function

def main():
    return helper_function()
""",
        )
        diff_project.commit("Initial")

        diff_project.add_file(
            "app.py",
            """\
from lib import helper_function

def main():
    x = helper_function()
    return x + 1
""",
        )
        diff_project.commit("Modify app")

        tree = build_diff_context(
            root_dir=diff_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        fragments = _extract_fragments_from_tree(tree)
        assert len(fragments) >= 1


class TestUtilityFunctions:
    def test_util_001_marginal_gain_diminishes(self):
        frag = _make_fragment("a.py", 1, 10, "concept_a concept_b", tokens=100)
        concepts = frozenset(["concept_a", "concept_b"])
        state = UtilityState()

        gain1 = marginal_gain(frag, 1.0, concepts, state)
        apply_fragment(frag, 1.0, concepts, state)

        gain2 = marginal_gain(frag, 1.0, concepts, state)

        assert gain2 < gain1

    def test_util_002_utility_value_accumulates(self):
        frag1 = _make_fragment("a.py", 1, 10, "concept_a", tokens=100)
        frag2 = _make_fragment("b.py", 1, 10, "concept_b", tokens=100)
        concepts = frozenset(["concept_a", "concept_b"])
        state = UtilityState()

        val0 = utility_value(state)
        apply_fragment(frag1, 1.0, concepts, state)
        val1 = utility_value(state)
        apply_fragment(frag2, 1.0, concepts, state)
        val2 = utility_value(state)

        assert val0 < 1e-9
        assert val1 > val0
        assert val2 > val1

    def test_util_003_empty_concepts_fallback(self):
        frag = _make_fragment("a.py", 1, 10, "some content", tokens=100)
        concepts = frozenset()
        state = UtilityState()

        gain = marginal_gain(frag, 0.5, concepts, state)

        assert gain == pytest.approx(0.05, rel=0.01)


class TestBestSingletonGuard:
    def test_sel_singleton_001_singleton_vs_greedy(self):
        small_frags = [_make_fragment(f"small{i}.py", 1, 5, f"small_concept_{i}", tokens=50) for i in range(10)]
        big_frag = _make_fragment("big.py", 1, 100, " ".join(f"small_concept_{i}" for i in range(10)), tokens=400)
        all_frags = [*small_frags, big_frag]

        core_ids = set()
        rel = {f.id: 0.1 for f in small_frags}
        rel[big_frag.id] = 1.0
        concepts = frozenset([f"small_concept_{i}" for i in range(10)])

        result = lazy_greedy_select(
            fragments=all_frags,
            core_ids=core_ids,
            rel=rel,
            concepts=concepts,
            budget_tokens=500,
            tau=0.0,
        )

        assert len(result.selected) >= 1


class TestDeterminism:
    def test_sel_determinism_001_same_output(self, diff_project):
        diff_project.add_file(
            "determinism.py",
            """\
def func_a():
    return 1

def func_b():
    return 2

def func_c():
    return 3
""",
        )
        diff_project.commit("Initial")

        diff_project.add_file(
            "determinism.py",
            """\
def func_a():
    return 10

def func_b():
    return 20

def func_c():
    return 30
""",
        )
        diff_project.commit("Modify all")

        results = []
        for _ in range(3):
            tree = build_diff_context(
                root_dir=diff_project.repo,
                diff_range="HEAD~1..HEAD",
                budget_tokens=10000,
            )
            fragments = _extract_fragments_from_tree(tree)
            results.append([(f.get("path"), f.get("lines")) for f in fragments])

        assert results[0] == results[1] == results[2]
