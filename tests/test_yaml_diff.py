from collections import Counter
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


def _count_fragments_per_file(context: dict) -> dict[str, int]:
    counts: Counter[str] = Counter()
    for frag in context.get("fragments", []):
        path = frag.get("path", "")
        if path:
            counts[path] += 1
    return dict(counts)


@pytest.mark.parametrize("case", ALL_CASES, ids=lambda c: c.id)
def test_diff_yaml(yaml_test_runner: YamlTestRunner, case: YamlTestCase, record_property, request):
    if case.xfail:
        request.node.add_marker(pytest.mark.xfail(reason=case.xfail, strict=True))
    context = yaml_test_runner.run_test_case(case)
    breakdown = yaml_test_runner.score_test_case(context, case)

    fragments = context.get("fragments", [])
    frag_count = len(fragments)
    unique_files = len({f.get("path", "") for f in fragments if f.get("path")})

    record_property("score", breakdown.score)
    record_property("recall", round(breakdown.recall * 100, 1))
    record_property("noise_rate", round(breakdown.noise_rate * 100, 1))
    record_property("garbage_rate", round(breakdown.garbage_rate * 100, 1))
    record_property("diff_covered", breakdown.diff_covered)
    record_property("enrichment", round(breakdown.enrichment * 100))
    record_property("diff_tokens", breakdown.diff_tokens)
    record_property("context_tokens", breakdown.context_tokens)
    record_property("fragment_count", frag_count)
    record_property("unique_files", unique_files)

    effective_min = case.min_score if case.min_score is not None else MIN_INDIVIDUAL_SCORE
    assert breakdown.score >= effective_min, f"[{case.id}] score {breakdown.score:.1f}% below minimum {effective_min}%"
    if case.must_include_files:
        assert breakdown.diff_covered, f"[{case.id}] diff lines not covered by context"

    if case.max_fragments_per_file is not None:
        per_file = _count_fragments_per_file(context)
        for path, count in per_file.items():
            assert (
                count <= case.max_fragments_per_file
            ), f"[{case.id}] {path} has {count} fragments, exceeds max_fragments_per_file={case.max_fragments_per_file}"

    if case.max_enrichment is not None:
        assert (
            breakdown.enrichment <= case.max_enrichment
        ), f"[{case.id}] enrichment {breakdown.enrichment:.1f}x exceeds max_enrichment={case.max_enrichment}"

    if case.min_recall is not None:
        assert (
            breakdown.recall >= case.min_recall
        ), f"[{case.id}] recall {breakdown.recall:.1%} below min_recall={case.min_recall:.0%}"

    if case.max_noise_rate is not None:
        assert (
            breakdown.noise_rate <= case.max_noise_rate
        ), f"[{case.id}] noise_rate {breakdown.noise_rate:.1%} exceeds max_noise_rate={case.max_noise_rate:.0%}"

    if case.max_context_tokens is not None:
        assert (
            breakdown.context_tokens <= case.max_context_tokens
        ), f"[{case.id}] {breakdown.context_tokens} context tokens exceeds max_context_tokens={case.max_context_tokens}"


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
