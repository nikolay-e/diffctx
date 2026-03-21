from pathlib import Path

import pytest

from tests.framework import load_test_cases_from_dir
from tests.framework.runner import YamlTestRunner
from tests.framework.types import YamlTestCase

CASES_DIR = Path(__file__).parent / "cases" / "diff"

ALL_CASES = load_test_cases_from_dir(CASES_DIR) if CASES_DIR.exists() else []


@pytest.fixture
def yaml_test_runner(tmp_path):
    return YamlTestRunner(tmp_path)


def test_cases_loaded():
    assert CASES_DIR.exists(), f"Test cases directory not found: {CASES_DIR}"
    assert len(ALL_CASES) > 0, "No test cases loaded from cases directory"


MIN_INDIVIDUAL_SCORE = 10.0


@pytest.mark.parametrize("case", ALL_CASES, ids=lambda c: c.id)
def test_diff_yaml(yaml_test_runner: YamlTestRunner, case: YamlTestCase, record_property):
    context = yaml_test_runner.run_test_case(case)
    breakdown = yaml_test_runner.score_test_case(context, case)

    record_property("score", breakdown.score)
    record_property("recall", round(breakdown.recall * 100, 1))
    record_property("noise_rate", round(breakdown.noise_rate * 100, 1))
    record_property("garbage_rate", round(breakdown.garbage_rate * 100, 1))
    record_property("diff_covered", breakdown.diff_covered)
    record_property("enrichment", round(breakdown.enrichment * 100))
    record_property("diff_tokens", breakdown.diff_tokens)
    record_property("context_tokens", breakdown.context_tokens)

    assert (
        breakdown.score >= MIN_INDIVIDUAL_SCORE
    ), f"[{case.id}] score {breakdown.score:.1f}% below minimum {MIN_INDIVIDUAL_SCORE}%"
    if case.must_include_files:
        assert breakdown.diff_covered, f"[{case.id}] diff lines not covered by context"


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
