# tests/test_cli.py
import subprocess
import sys

import pytest

PYTHON_EXEC = sys.executable


def run_cli_command(args, cwd):
    """Запускает treemapper как отдельный процесс"""
    command = [PYTHON_EXEC, "-m", "treemapper"] + args

    result = subprocess.run(command, capture_output=True, text=True, cwd=cwd, encoding="utf-8", errors="replace")
    return result


def test_cli_help_short(temp_project):
    """Тест: вызов справки через -h"""
    result = run_cli_command(["-h"], cwd=temp_project)
    assert result.returncode == 0
    assert "usage: treemapper" in result.stdout.lower()
    assert "--help" in result.stdout
    assert "--output-file" in result.stdout
    assert "--verbosity" in result.stdout


def test_cli_help_long(temp_project):
    """Тест: вызов справки через --help"""
    result = run_cli_command(["--help"], cwd=temp_project)
    assert result.returncode == 0
    assert "usage: treemapper" in result.stdout.lower()
    assert "--help" in result.stdout


def test_cli_invalid_verbosity(temp_project):
    """Тест: неверный уровень verbosity"""
    result = run_cli_command(["-v", "5"], cwd=temp_project)
    assert result.returncode != 0

    assert (
        "invalid choice: '5'" in result.stderr or "invalid choice: 5" in result.stderr
    ), f"stderr does not contain expected invalid choice message for '5'. stderr: {result.stderr}"

    result_neg = run_cli_command(["-v", "-1"], cwd=temp_project)
    assert result_neg.returncode != 0

    assert (
        "invalid choice: '-1'" in result_neg.stderr or "invalid choice: -1" in result_neg.stderr
    ), f"stderr does not contain expected invalid choice message for '-1'. stderr: {result_neg.stderr}"


def test_cli_version_display(temp_project):
    """Тест: отображение версии (если будет добавлено)"""
    pytest.skip("Version display option ('--version') not implemented yet.")
