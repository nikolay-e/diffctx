# tests/test_cli.py
import pytest
import yaml

from .conftest import run_treemapper_subprocess


@pytest.mark.parametrize("flag", ["-h", "--help"])
def test_cli_help(temp_project, flag):
    result = run_treemapper_subprocess([flag], cwd=temp_project)
    assert result.returncode == 0
    assert "usage: treemapper" in result.stdout.lower()
    assert "--help" in result.stdout
    assert "--output-file" in result.stdout
    assert "--log-level" in result.stdout


@pytest.mark.parametrize("invalid_value", ["verbose", "quiet"])
def test_cli_invalid_log_level(temp_project, invalid_value):
    result = run_treemapper_subprocess(["--log-level", invalid_value], cwd=temp_project)
    assert result.returncode != 0
    assert "invalid choice" in result.stderr.lower(), f"stderr: {result.stderr}"


def test_cli_version_display(temp_project):
    result = run_treemapper_subprocess(["--version"], cwd=temp_project)
    assert result.returncode == 0
    assert "treemapper" in result.stdout.lower()


def test_main_module_execution(temp_project):
    output_file = temp_project / "output" / "output.yaml"
    result = run_treemapper_subprocess([str(temp_project), "-o", str(output_file)])
    assert result.returncode == 0
    assert output_file.exists()
    tree_data = yaml.safe_load(output_file.read_text(encoding="utf-8"))
    assert tree_data["type"] == "directory"
    assert tree_data["name"] == temp_project.name


def test_output_file_saved_message(temp_project):
    output_file = temp_project / "saved.yaml"
    result = run_treemapper_subprocess([str(temp_project), "-o", str(output_file)])
    assert result.returncode == 0
    assert "Saved to" in result.stderr
    assert str(output_file) in result.stderr
