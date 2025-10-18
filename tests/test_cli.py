# tests/test_cli.py
from .conftest import run_treemapper_subprocess


def run_cli_command(args, cwd):
    """Runs treemapper as a separate process"""
    return run_treemapper_subprocess(args, cwd=cwd)


def test_cli_help_short(temp_project):
    """Test: help invocation via -h"""
    result = run_cli_command(["-h"], cwd=temp_project)
    assert result.returncode == 0
    assert "usage: treemapper" in result.stdout.lower()
    assert "--help" in result.stdout
    assert "--output-file" in result.stdout
    assert "--verbosity" in result.stdout


def test_cli_help_long(temp_project):
    """Test: help invocation via --help"""
    result = run_cli_command(["--help"], cwd=temp_project)
    assert result.returncode == 0
    assert "usage: treemapper" in result.stdout.lower()
    assert "--help" in result.stdout


def test_cli_invalid_verbosity(temp_project):
    """Test: invalid verbosity level"""
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
    """Test: version display"""
    result = run_cli_command(["--version"], cwd=temp_project)
    assert result.returncode == 0
    assert "treemapper" in result.stdout.lower()
