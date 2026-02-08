from __future__ import annotations

import pytest

from tests.framework.runner import YamlTestRunner


@pytest.fixture
def yaml_test_runner(tmp_path):
    return YamlTestRunner(tmp_path)
