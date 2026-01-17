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


@pytest.mark.parametrize("case", ALL_CASES, ids=lambda c: c.id)
def test_diff_yaml(yaml_test_runner: YamlTestRunner, case: YamlTestCase):
    context = yaml_test_runner.run_test_case(case)
    yaml_test_runner.verify_assertions(context, case)
