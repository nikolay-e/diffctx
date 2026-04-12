import os
from pathlib import Path

import pytest

from tests.framework import load_test_cases_from_dir
from tests.framework.runner import YamlTestRunner
from tests.framework.types import YamlTestCase

CASES_DIR = Path(__file__).parent / "cases" / "diff"

ALL_CASES = load_test_cases_from_dir(CASES_DIR) if CASES_DIR.exists() else []

_DISCOVER_MODE_XFAIL: frozenset[str] = frozenset(
    {
        "fragments_014_markdown_empty_sections_filtered",
        "go_011_channel_receive",
        "go_016_error_wrapping",
        "go_017_variadic_function",
        "go_018_function_type",
        "go_025_struct_embedding",
        "go_027_custom_type",
        "go_033_error_interface",
        "go_035_http_handler",
        "helm_044_capabilities",
        "java_006_entity_relations",
        "javascript_002_output_decorator",
        "javascript_004_http_client",
        "javascript_005_observable_pipe",
        "javascript_016_type_import_resolves_definition",
        "javascript_022_implements_interface",
        "javascript_053_route_handler",
        "javascript_061_usereducer",
        "javascript_068_story",
        "javascript_extended_009_type_change",
        "jvm_and_compiled_043_java_streams_api",
        "jvm_and_compiled_048_scala_akka_actor",
        "jvm_and_compiled_060_swift_async_await",
        "jvm_and_compiled_061_swift_codable",
        "jvm_and_compiled_070_swift_result_type",
        "jvm_and_compiled_072_swift_swiftui_view",
        "lua_004_redis_script",
        "nix_003_overlay",
        "patterns_042_raise_sites",
        "patterns_044_json",
        "php_015_laravel_controller",
        "ruby_004_extend",
        "ruby_006_attr_accessor",
        "ruby_011_symbol_to_proc",
        "ruby_014_metaprogramming",
        "ruby_017_rails_callback",
        "rust_003_basic_struct",
        "rust_007_module_use",
        "rust_012_generic_function",
        "rust_013_result_error_handling",
        "rust_015_const_and_static",
        "rust_016_use_crate_module",
        "rust_021_cfg_feature",
        "rust_022_unsafe_ffi",
        "rust_027_arc_mutex",
        "rust_030_include_str",
        "rust_033_clone_trait",
        "rust_034_copy_trait",
        "scala_009_higher_kinded_type",
        "scala_015_akka_actor",
        "swift_020_associated_type",
        "terraform_003_api_gateway_route",
        "terraform_018_eventbridge_rule",
        "terraform_025_acm_certificate",
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

    scoring_mode = os.environ.get("DIFFCTX_SCORING", "auto")
    if scoring_mode == "discover" and case.id in _DISCOVER_MODE_XFAIL:
        request.node.add_marker(pytest.mark.xfail(reason="discover mode: ego-graph noise on small repos", strict=True))

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
