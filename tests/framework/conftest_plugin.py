from __future__ import annotations

from pathlib import Path

import pytest

from tests.framework.loader import load_test_cases_from_dir
from tests.framework.runner import YamlTestRunner
from tests.framework.types import YamlTestCase

CASES_DIR = Path(__file__).parent.parent / "cases"


def get_all_yaml_cases(subdir: str | None = None) -> list[YamlTestCase]:
    target_dir = CASES_DIR / subdir if subdir else CASES_DIR
    if not target_dir.exists():
        return []
    return load_test_cases_from_dir(target_dir)


def get_diff_cases(language: str | None = None) -> list[YamlTestCase]:
    diff_dir = CASES_DIR / "diff"
    if not diff_dir.exists():
        return []
    if language:
        lang_dir = diff_dir / language
        if not lang_dir.exists():
            return []
        return load_test_cases_from_dir(lang_dir)
    return load_test_cases_from_dir(diff_dir)


@pytest.fixture
def yaml_test_runner(tmp_path):
    return YamlTestRunner(tmp_path)


def make_diff_test_function(cases: list[YamlTestCase], name: str = "test_yaml_diff"):
    @pytest.mark.parametrize("case", cases, ids=lambda c: c.id)
    def test_func(yaml_test_runner: YamlTestRunner, case: YamlTestCase):
        context = yaml_test_runner.run_test_case(case)
        yaml_test_runner.verify_assertions(context, case)

    test_func.__name__ = name
    return test_func
