from pathlib import Path

import pytest

from tests.framework import load_test_cases_from_dir
from tests.framework.runner import YamlTestRunner
from tests.framework.types import YamlTestCase

CASES_DIR = Path(__file__).parent / "cases" / "diff"

ALL_CASES = load_test_cases_from_dir(CASES_DIR) if CASES_DIR.exists() else []

_DISCOVER_MODE_XFAIL: frozenset[str] = frozenset(
    {
        "bal3_1hop_034_php_bug_fix_validator",
        "bal3_1hop_036_php_single_file_cache",
        "bal3_1hop_038_php_new_function_queue",
        "bal3_1hop_078_csharp_new_method_repository",
        "bal3_bugfix_014_ruby_wrong_boolean",
        "java_025_kotlin_data_class",
        "jvm_and_compiled_037_java_override_method",
        "jvm_and_compiled_048_scala_akka_actor",
        "jvm_and_compiled_051_scala_for_comprehension",
        "jvm_and_compiled_055_scala_pattern_matching",
        "jvm_and_compiled_059_swift_actor",
        "lua_004_redis_script",
        "scala_013_partial_function",
        "scala_015_akka_actor",
        "scala_022_sealed_trait_adt",
        "scala_026_application_conf",
        "scripting_004_interface",
        "scripting_006_static_method",
    }
)


@pytest.fixture
def yaml_test_runner(tmp_path):
    return YamlTestRunner(tmp_path)


def test_cases_loaded():
    assert CASES_DIR.exists(), f"Test cases directory not found: {CASES_DIR}"
    assert len(ALL_CASES) > 0, "No test cases loaded from cases directory"


MIN_INDIVIDUAL_SCORE = 10.0


@pytest.mark.parametrize("case", ALL_CASES, ids=lambda c: c.id)
def test_diff_yaml(yaml_test_runner: YamlTestRunner, case: YamlTestCase, record_property, request):
    if case.xfail.reason or case.xfail.category:
        reason = case.xfail.reason or f"category: {case.xfail.category}"
        request.node.add_marker(pytest.mark.xfail(reason=reason, strict=True))

    if case.id in _DISCOVER_MODE_XFAIL:
        request.node.add_marker(pytest.mark.xfail(reason="discovery precision tradeoff", strict=True))

    context = yaml_test_runner.run_test_case(case)
    breakdown = yaml_test_runner.score_test_case(context, case)

    fragments = context.get("fragments", [])
    frag_count = len(fragments)
    unique_files = len({f.get("path", "") for f in fragments if f.get("path")})

    record_property("score", breakdown.score)
    record_property("recall", round(breakdown.required_recall * 100, 1))
    record_property("noise_rate", round(breakdown.forbidden_rate * 100, 1))
    record_property("garbage_rate", 0)
    record_property("diff_covered", breakdown.diff_covered)
    record_property("enrichment", round(breakdown.enrichment * 100))
    record_property("diff_tokens", breakdown.diff_tokens)
    record_property("context_tokens", breakdown.context_tokens)
    record_property("fragment_count", frag_count)
    record_property("unique_files", unique_files)

    effective_min = MIN_INDIVIDUAL_SCORE
    assert breakdown.score >= effective_min, (
        f"[{case.id}] score {breakdown.score:.1f}% below minimum {effective_min}%\n"
        f"  missing required: {breakdown.missing_required}\n"
        f"  present forbidden: {breakdown.present_forbidden}"
    )


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
