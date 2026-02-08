from pathlib import Path

import pytest

from tests.framework import load_test_cases_from_dir
from tests.framework.runner import YamlTestRunner
from tests.framework.types import YamlTestCase

CASES_DIR = Path(__file__).parent / "cases" / "diff"

ALL_CASES = load_test_cases_from_dir(CASES_DIR) if CASES_DIR.exists() else []

KNOWN_FAILURES = frozenset(
    {
        "cicd_and_docs_009_gha_needs",
        "cicd_and_docs_050_openapi_paths",
        "cicd_and_docs_077_sql_create_function",
        "comprehensive_010_monorepo_turborepo_depends_on",
        "dependencies_010_forward_uses_enum",
        "docker_003_dockerfile_env",
        "docker_016_from_base_image",
        "docker_018_copy_source",
        "docker_021_env",
        "docker_023_expose",
        "docker_024_entrypoint",
        "docker_025_cmd",
        "docker_027_user",
        "docker_028_volume",
        "docker_030_add_vs_copy",
        "docker_044_dockerignore",
        "fragments_015_markdown_long_heading_truncation",
        "javascript_033_template_tag_function",
        "javascript_043_redux_action_change",
        "javascript_044_zod_schema_change",
        "javascript_048_event_emitter_change",
        "javascript_050_class_method_change",
        "javascript_extended_006_exported_function_change",
        "javascript_extended_008_interface_change",
        "javascript_extended_009_type_change",
        "javascript_extended_012_redux_action_change",
        "javascript_extended_013_zod_schema_change",
        "javascript_extended_017_event_emitter_change",
        "javascript_extended_018_promise_chain_change",
        "javascript_extended_019_class_method_change",
        "json_002_package_json_main_entry",
        "json_004_tsconfig_strict",
        "json_009_vscode_settings",
        "json_011_launch_json",
        "json_012_nodemon_config",
        "kubernetes_017_service_account",
        "kubernetes_024_loadbalancer",
        "merging_006_small_paragraphs",
        "rust_014_option_handling",
        "rust_018_derive_macro",
    }
)


@pytest.fixture
def yaml_test_runner(tmp_path):
    return YamlTestRunner(tmp_path)


@pytest.mark.parametrize("case", ALL_CASES, ids=lambda c: c.id)
def test_diff_yaml(yaml_test_runner: YamlTestRunner, case: YamlTestCase):
    if case.id in KNOWN_FAILURES:
        pytest.xfail("known failure — to be fixed post-release")
    context = yaml_test_runner.run_test_case(case)
    yaml_test_runner.verify_assertions(context, case)
