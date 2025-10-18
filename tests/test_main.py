# tests/test_main.py
from .conftest import run_treemapper_subprocess


def test_main_module_execution(temp_project):
    """Test running treemapper as a module with python -m."""
    output_file = temp_project / "output.yaml"

    result = run_treemapper_subprocess([str(temp_project), "-o", str(output_file)])

    assert result.returncode == 0
    assert output_file.exists()


def test_main_module_help(temp_project):
    """Test help output when running as module."""
    result = run_treemapper_subprocess(["--help"])

    assert result.returncode == 0
    assert "treemapper" in result.stdout.lower()
    assert "usage" in result.stdout.lower()


def test_main_module_version(temp_project):
    """Test version output when running as module."""
    result = run_treemapper_subprocess(["--version"])

    assert result.returncode == 0
    assert "treemapper" in result.stdout.lower()
