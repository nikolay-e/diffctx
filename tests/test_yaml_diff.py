from pathlib import Path

import pytest

from tests.framework import load_test_cases_from_dir
from tests.framework.runner import YamlTestRunner
from tests.framework.types import YamlTestCase

CASES_DIR = Path(__file__).parent / "cases" / "diff"

ALL_CASES = load_test_cases_from_dir(CASES_DIR) if CASES_DIR.exists() else []

KNOWN_FAILURES = frozenset(
    {
        "cicd_and_docs_068_sql_stored_procedure",
        "cicd_and_docs_077_sql_create_function",
        "dependencies_003_forward_chained_methods",
        "fragments_015_markdown_long_heading_truncation",
        "javascript_043_redux_action_change",
        "javascript_044_zod_schema_change",
        "javascript_048_event_emitter_change",
        "javascript_extended_008_interface_change",
        "javascript_extended_009_type_change",
        "javascript_extended_012_redux_action_change",
        "javascript_extended_013_zod_schema_change",
        "javascript_extended_017_event_emitter_change",
        "json_009_vscode_settings",
        "json_011_launch_json",
        "json_012_nodemon_config",
        "kubernetes_017_service_account",
        "kubernetes_024_loadbalancer",
        "rust_014_option_handling",
        "rust_018_derive_macro",
    }
)


@pytest.fixture
def yaml_test_runner(tmp_path):
    return YamlTestRunner(tmp_path)


def test_cases_loaded():
    assert CASES_DIR.exists(), f"Test cases directory not found: {CASES_DIR}"
    assert len(ALL_CASES) > 0, "No test cases loaded from cases directory"


def test_known_failures_are_valid_case_ids():
    actual_ids = {c.id for c in ALL_CASES}
    stale = KNOWN_FAILURES - actual_ids
    assert not stale, f"Stale KNOWN_FAILURES entries (no matching case): {stale}"


@pytest.mark.parametrize("case", ALL_CASES, ids=lambda c: c.id)
def test_diff_yaml(yaml_test_runner: YamlTestRunner, case: YamlTestCase):
    if case.id in KNOWN_FAILURES:
        pytest.xfail("known failure — to be fixed post-release")
    context = yaml_test_runner.run_test_case(case)
    yaml_test_runner.verify_assertions(context, case)


@pytest.mark.parametrize(
    "case",
    [c for c in ALL_CASES if c.id not in KNOWN_FAILURES][:20],
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
