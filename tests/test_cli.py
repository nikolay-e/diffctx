# tests/test_cli.py
import pytest
import yaml

import diffctx

from .conftest import run_diffctx_subprocess


@pytest.mark.parametrize("flag", ["-h", "--help"])
def test_cli_help(temp_project, flag):
    result = run_diffctx_subprocess([flag], cwd=temp_project)
    assert result.returncode == 0
    assert "usage: diffctx" in result.stdout.lower()
    assert "--help" in result.stdout
    assert "--output-file" in result.stdout
    assert "--log-level" in result.stdout


@pytest.mark.parametrize("invalid_value", ["verbose", "quiet"])
def test_cli_invalid_log_level(temp_project, invalid_value):
    result = run_diffctx_subprocess(["--log-level", invalid_value], cwd=temp_project)
    assert result.returncode != 0
    assert "invalid choice" in result.stderr.lower(), f"stderr: {result.stderr}"


def test_cli_version_display(temp_project):
    result = run_diffctx_subprocess(["--version"], cwd=temp_project)
    assert result.returncode == 0
    assert "diffctx" in result.stdout.lower()


def test_main_module_execution(temp_project):
    output_file = temp_project / "output" / "output.yaml"
    result = run_diffctx_subprocess([str(temp_project), "-f", "yaml", "-o", str(output_file)])
    assert result.returncode == 0
    assert output_file.exists()
    tree_data = yaml.safe_load(output_file.read_text(encoding="utf-8"))
    assert tree_data["type"] == "directory"
    assert tree_data["name"] == temp_project.name


def test_output_file_saved_message(temp_project):
    output_file = temp_project / "saved.yaml"
    result = run_diffctx_subprocess([str(temp_project), "-o", str(output_file)])
    assert result.returncode == 0
    assert "Saved to" in result.stderr
    assert str(output_file) in result.stderr


def test_run_injects_program_name_in_help(capsys):
    with pytest.raises(SystemExit) as exc:
        diffctx.run(["--help"], prog="treemapper", version="9.9.9")
    assert exc.value.code == 0
    out = capsys.readouterr().out
    assert "usage: treemapper" in out.lower()
    assert "diffctx" not in out.split("\n")[0].lower()


def test_run_injects_version(capsys):
    with pytest.raises(SystemExit) as exc:
        diffctx.run(["--version"], prog="treemapper", version="9.9.9")
    assert exc.value.code == 0
    assert capsys.readouterr().out.strip() == "treemapper 9.9.9"


def test_run_defaults_to_diffctx_identity(capsys):
    with pytest.raises(SystemExit) as exc:
        diffctx.run(["--version"])
    assert exc.value.code == 0
    out = capsys.readouterr().out.strip()
    assert out == f"diffctx {diffctx.__version__}"


def test_run_executes_tree_mode(temp_project):
    output_file = temp_project / "via_run.yaml"
    diffctx.run([str(temp_project), "-f", "yaml", "-o", str(output_file)], prog="treemapper")
    assert output_file.exists()
    tree_data = yaml.safe_load(output_file.read_text(encoding="utf-8"))
    assert tree_data["type"] == "directory"


class TestScoringChoicesTrackTheEngine:
    """The two CLIs each enumerated the accepted `--scoring` values in their own
    literal, and both went stale the moment a mode was added: `pit` parsed in the
    engine, in the MCP server and in the eval harness, while
    `diffctx --scoring pit` rejected it as an invalid value. Both now read the
    engine's list, and this pins that they do."""

    def test_every_engine_mode_is_offered_by_the_cli(self):
        from diffctx._diffctx import SCORING_MODES
        from diffctx.cli import _build_main_parser

        parser = _build_main_parser()
        action = next(a for a in parser._actions if "--scoring" in (a.option_strings or []))
        assert set(action.choices) == set(SCORING_MODES)

    def test_the_engine_actually_accepts_each_offered_mode(self, tmp_path):
        """A name in the list that the engine rejects is the same defect pointing
        the other way — the CLI would accept an argument that then fails."""
        import subprocess
        import sys

        from diffctx._diffctx import SCORING_MODES
        from tests.framework.pygit2_backend import Pygit2Repo

        repo = Pygit2Repo(tmp_path / "scoring_repo")
        repo.add_file("a.py", "def f():\n    return 1\n")
        base = repo.commit("initial")
        repo.add_file("a.py", "def f():\n    return 2\n")
        head = repo.commit("change")

        for mode in SCORING_MODES:
            proc = subprocess.run(
                [sys.executable, "-m", "diffctx", ".", "--diff", f"{base}..{head}", "--scoring", mode, "-q"],
                cwd=repo.path,
                capture_output=True,
                text=True,
                timeout=120,
            )
            assert proc.returncode == 0, f"--scoring {mode}: {proc.stderr[-400:]}"
