from pathlib import Path

import pytest

from tests.framework import load_test_cases_from_dir
from tests.framework.runner import YamlTestRunner
from tests.framework.scoring import ScoreBreakdown
from tests.framework.types import YamlTestCase

CASES_DIR = Path(__file__).parent / "cases" / "diff"

ALL_CASES = load_test_cases_from_dir(CASES_DIR) if CASES_DIR.exists() else []


_score_results: list[tuple[str, ScoreBreakdown]] = []


def _collect_score(case_id: str, breakdown: ScoreBreakdown) -> None:
    _score_results.append((case_id, breakdown))


def get_score_results() -> list[tuple[str, ScoreBreakdown]]:
    return _score_results


@pytest.fixture
def yaml_test_runner(tmp_path):
    return YamlTestRunner(tmp_path)


def test_cases_loaded():
    assert CASES_DIR.exists(), f"Test cases directory not found: {CASES_DIR}"
    assert len(ALL_CASES) > 0, "No test cases loaded from cases directory"


@pytest.mark.parametrize("case", ALL_CASES, ids=lambda c: c.id)
def test_diff_yaml(yaml_test_runner: YamlTestRunner, case: YamlTestCase, record_property):
    context = yaml_test_runner.run_test_case(case)
    breakdown = yaml_test_runner.score_test_case(context, case)
    _collect_score(case.id, breakdown)

    record_property("score", breakdown.score)
    record_property("recall", round(breakdown.recall * 100, 1))
    record_property("noise_rate", round(breakdown.noise_rate * 100, 1))
    record_property("garbage_rate", round(breakdown.garbage_rate * 100, 1))
    record_property("diff_covered", breakdown.diff_covered)
    record_property("enrichment", round(breakdown.enrichment * 100))
    record_property("diff_tokens", breakdown.diff_tokens)
    record_property("context_tokens", breakdown.context_tokens)


@pytest.mark.parametrize(
    "case",
    ALL_CASES[:20],
    ids=lambda c: c.id,
)
def test_diff_yaml_structure(yaml_test_runner: YamlTestRunner, case: YamlTestCase):
    context = yaml_test_runner.run_test_case(case)

    assert "fragments" in context, f"[{case.id}] Missing 'fragments' key in context"
    fragments = context["fragments"]
    assert isinstance(fragments, list), f"[{case.id}] 'fragments' is not a list"

    if "fragment_count" in context:
        assert context["fragment_count"] == len(
            fragments
        ), f"[{case.id}] fragment_count ({context['fragment_count']}) != len(fragments) ({len(fragments)})"

    for i, frag in enumerate(fragments):
        assert "path" in frag, f"[{case.id}] Fragment {i} missing 'path'"
        assert "content" in frag, f"[{case.id}] Fragment {i} missing 'content'"
