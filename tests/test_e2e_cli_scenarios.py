# tests/test_e2e_cli_scenarios.py
"""End-to-end CLI user-journey tests.

Every scenario invokes the real `python -m diffctx` subprocess against a real
filesystem / real git repo, mirroring how an actual user drives the tool. No
in-process shortcuts: exit codes, stdout, and stderr are all asserted exactly
as a shell user would observe them.
"""

from __future__ import annotations

import json

import pytest
import yaml

from tests.framework.pygit2_backend import Pygit2Repo
from tests.garbage_data import GARBAGE_FILES

from .conftest import run_diffctx_subprocess

EXIT_OK = 0
EXIT_RUNTIME = 1
EXIT_USAGE = 2
EXIT_ENVIRONMENT = 3
EXIT_EMPTY_DIFF = 4


@pytest.fixture
def diff_repo(tmp_path):
    repo = Pygit2Repo(tmp_path / "diff_repo")
    for rel_path, content in GARBAGE_FILES.items():
        repo.add_file(rel_path, content)
    repo.add_file("src/calc.py", "def add(a, b):\n    return a + b\n")
    repo.add_file("src/main.py", "from calc import add\n\n\ndef run():\n    return add(1, 2)\n")
    repo.commit("initial commit")
    repo.add_file(
        "src/calc.py",
        "def add(a, b):\n    return a + b\n\n\ndef subtract(a, b):\n    return a - b\n",
    )
    repo.add_file(
        "src/main.py",
        "from calc import add, subtract\n\n\ndef run():\n    return add(1, 2)\n\n\ndef run_sub():\n    return subtract(5, 3)\n",
    )
    repo.commit("add subtract function")
    return repo


@pytest.fixture
def graph_repo(tmp_path):
    repo = Pygit2Repo(tmp_path / "graph_repo")
    repo.add_file("src/calc.py", "def add(a, b):\n    return a + b\n")
    repo.add_file("src/main.py", "from calc import add\n\n\ndef run():\n    return add(1, 2)\n")
    repo.commit("initial commit")
    return repo


class TestTreeModeJourneys:
    def test_default_stdout_is_yaml_directory(self, temp_project):
        result = run_diffctx_subprocess([str(temp_project)])
        assert result.returncode == EXIT_OK
        tree = yaml.safe_load(result.stdout)
        assert tree["type"] == "directory"
        assert tree["name"] == temp_project.name

    def test_json_format_is_valid_json(self, temp_project):
        result = run_diffctx_subprocess([str(temp_project), "-f", "json"])
        assert result.returncode == EXIT_OK
        tree = json.loads(result.stdout)
        assert tree["type"] == "directory"

    @pytest.mark.parametrize("fmt", ["yaml", "json", "txt", "md"])
    def test_every_format_emits_nonempty_output(self, temp_project, fmt):
        result = run_diffctx_subprocess([str(temp_project), "-f", fmt])
        assert result.returncode == EXIT_OK
        assert result.stdout.strip()

    def test_no_content_omits_file_bodies(self, temp_project):
        with_content = run_diffctx_subprocess([str(temp_project)])
        without_content = run_diffctx_subprocess([str(temp_project), "--no-content"])
        assert "content:" in with_content.stdout
        assert "content:" not in without_content.stdout

    def test_max_depth_limits_traversal(self, temp_project):
        shallow = run_diffctx_subprocess([str(temp_project), "--max-depth", "1", "-f", "txt"])
        deep = run_diffctx_subprocess([str(temp_project), "-f", "txt"])
        assert shallow.returncode == EXIT_OK
        assert "main.py" in deep.stdout
        assert "main.py" not in shallow.stdout

    @pytest.mark.parametrize("fmt,ext", [("yaml", "yaml"), ("json", "json"), ("txt", "txt"), ("md", "md")])
    def test_save_writes_tree_file_with_correct_extension(self, temp_project, fmt, ext):
        result = run_diffctx_subprocess([str(temp_project), "-f", fmt, "--save"], cwd=temp_project)
        assert result.returncode == EXIT_OK
        saved = temp_project / f"tree.{ext}"
        assert saved.exists()
        assert saved.read_text(encoding="utf-8").strip()

    def test_output_file_writes_and_reports_path(self, temp_project):
        out = temp_project / "export.yaml"
        result = run_diffctx_subprocess([str(temp_project), "-o", str(out)])
        assert result.returncode == EXIT_OK
        assert out.exists()
        assert "Saved to" in result.stderr
        assert str(out) in result.stderr

    def test_dash_output_forces_stdout(self, temp_project):
        result = run_diffctx_subprocess([str(temp_project), "-o", "-"])
        assert result.returncode == EXIT_OK
        assert yaml.safe_load(result.stdout)["type"] == "directory"

    def test_single_file_argument(self, temp_project):
        target = temp_project / "src" / "main.py"
        result = run_diffctx_subprocess([str(target)])
        assert result.returncode == EXIT_OK
        node = yaml.safe_load(result.stdout)
        assert node["type"] == "file"
        assert "hello" in node["content"]

    @pytest.mark.parametrize("fmt", ["yaml", "json", "txt", "md"])
    def test_single_file_content_shown_in_every_format(self, temp_project, fmt):
        target = temp_project / "src" / "main.py"
        result = run_diffctx_subprocess([str(target), "-f", fmt])
        assert result.returncode == EXIT_OK
        assert "print('hello')" in result.stdout

    def test_single_file_no_content_omits_body(self, temp_project):
        target = temp_project / "src" / "main.py"
        result = run_diffctx_subprocess([str(target), "--no-content"])
        assert result.returncode == EXIT_OK
        assert "hello" not in result.stdout

    def test_glob_pattern_expands(self, temp_project):
        result = run_diffctx_subprocess([str(temp_project / "src" / "*.py")])
        assert result.returncode == EXIT_OK
        assert "main.py" in result.stdout
        assert "test.py" in result.stdout


class TestOutputFeedbackJourneys:
    def test_token_summary_on_stderr_by_default(self, temp_project):
        result = run_diffctx_subprocess([str(temp_project)])
        assert "tokens" in result.stderr
        assert "o200k_base" in result.stderr

    def test_quiet_suppresses_token_summary(self, temp_project):
        result = run_diffctx_subprocess([str(temp_project), "--quiet"])
        assert result.returncode == EXIT_OK
        assert "tokens" not in result.stderr
        assert result.stdout.strip()

    def test_quiet_suppresses_saved_message(self, temp_project):
        out = temp_project / "quiet.yaml"
        result = run_diffctx_subprocess([str(temp_project), "-o", str(out), "--quiet"])
        assert result.returncode == EXIT_OK
        assert out.exists()
        assert "Saved to" not in result.stderr

    @staticmethod
    def _make_many_small_files(directory, count=200, size=10_000):
        for i in range(count):
            (directory / f"file_{i}.txt").write_text("x" * size)

    def test_bare_invocation_warns_at_lower_size_threshold(self, tmp_path):
        """Regression (#87): a bare `diffctx` with no path argument at all —
        the most common "just try it" first invocation, and the easiest to
        run somewhere unintended — used to share the same 10 MB warning
        threshold as an explicit-path invocation, so a multi-MB accidental
        dump (e.g. in /tmp) produced zero advisory."""
        self._make_many_small_files(tmp_path)
        result = run_diffctx_subprocess([], cwd=tmp_path)
        assert result.returncode == EXIT_OK
        assert "Warning: output is" in result.stderr

    def test_explicit_path_keeps_higher_size_threshold(self, tmp_path):
        self._make_many_small_files(tmp_path)
        result = run_diffctx_subprocess(["."], cwd=tmp_path)
        assert result.returncode == EXIT_OK
        assert "Warning: output is" not in result.stderr


class TestUsageErrorJourneys:
    @pytest.mark.parametrize(
        "args,expected_exit,needle",
        [
            (["--max-depth", "-1"], EXIT_RUNTIME, "non-negative"),
            (["--max-file-bytes", "0"], EXIT_RUNTIME, "no-file-size-limit"),
            (["--max-file-bytes", "-5"], EXIT_RUNTIME, "non-negative"),
            (["-f", "xml"], EXIT_USAGE, "invalid choice"),
            (["--log-level", "trace"], EXIT_USAGE, "invalid choice"),
            (["nonexistent_dir_xyz"], EXIT_RUNTIME, "No matches"),
        ],
    )
    def test_invalid_invocation(self, temp_project, args, expected_exit, needle):
        result = run_diffctx_subprocess(args, cwd=temp_project)
        assert result.returncode == expected_exit, f"stderr: {result.stderr}"
        assert needle.lower() in result.stderr.lower()

    def test_save_and_output_file_are_mutually_exclusive(self, temp_project):
        result = run_diffctx_subprocess([str(temp_project), "--save", "-o", "x.yaml"], cwd=temp_project)
        assert result.returncode == EXIT_RUNTIME
        assert "mutually exclusive" in result.stderr

    def test_output_file_pointing_at_directory(self, temp_project):
        result = run_diffctx_subprocess([str(temp_project), "-o", str(temp_project / "docs")])
        assert result.returncode == EXIT_RUNTIME
        assert "is a directory" in result.stderr

    def test_diff_flags_without_diff_emit_warning(self, temp_project):
        result = run_diffctx_subprocess([str(temp_project), "--budget", "5000", "--alpha", "0.5"])
        assert result.returncode == EXIT_OK
        assert "ignored without --diff" in result.stderr
        assert "--budget" in result.stderr
        assert "--alpha" in result.stderr


class TestDiffModeJourneys:
    def test_diff_selects_changed_symbols_and_excludes_garbage(self, diff_repo):
        result = run_diffctx_subprocess([".", "--diff", "HEAD~1..HEAD"], cwd=diff_repo.path)
        assert result.returncode == EXIT_OK
        doc = yaml.safe_load(result.stdout)
        assert doc["type"] == "diff_context"
        assert doc["fragment_count"] > 0
        assert "subtract" in result.stdout
        assert "GARBAGE" not in result.stdout
        assert "garbage_marker" not in result.stdout

    def test_diff_json_format_is_valid(self, diff_repo):
        result = run_diffctx_subprocess([".", "--diff", "HEAD~1..HEAD", "-f", "json"], cwd=diff_repo.path)
        assert result.returncode == EXIT_OK
        doc = json.loads(result.stdout)
        assert doc["type"] == "diff_context"
        assert doc["fragment_count"] >= 1

    def test_bare_diff_defaults_to_head(self, diff_repo):
        result = run_diffctx_subprocess([".", "--diff"], cwd=diff_repo.path)
        assert result.returncode == EXIT_EMPTY_DIFF
        assert "no semantic context" in result.stderr

    def test_full_includes_all_changed_fragments(self, diff_repo):
        smart = run_diffctx_subprocess([".", "--diff", "HEAD~1..HEAD"], cwd=diff_repo.path)
        full = run_diffctx_subprocess([".", "--diff", "HEAD~1..HEAD", "--full"], cwd=diff_repo.path)
        assert full.returncode == EXIT_OK
        smart_doc = yaml.safe_load(smart.stdout)
        full_doc = yaml.safe_load(full.stdout)
        assert full_doc["fragment_count"] >= smart_doc["fragment_count"]

    def test_budget_bounds_output_size(self, diff_repo):
        small = run_diffctx_subprocess([".", "--diff", "HEAD~1..HEAD", "--budget", "50"], cwd=diff_repo.path)
        large = run_diffctx_subprocess([".", "--diff", "HEAD~1..HEAD", "--budget", "8000"], cwd=diff_repo.path)
        assert small.returncode == EXIT_OK
        assert large.returncode == EXIT_OK
        assert len(small.stdout) <= len(large.stdout)

    @pytest.mark.parametrize("scoring", ["ego", "ppr", "bm25"])
    def test_scoring_modes_all_produce_context(self, diff_repo, scoring):
        result = run_diffctx_subprocess([".", "--diff", "HEAD~1..HEAD", "--scoring", scoring], cwd=diff_repo.path)
        assert result.returncode == EXIT_OK
        assert yaml.safe_load(result.stdout)["type"] == "diff_context"

    def test_diff_outside_git_repo_is_environment_error(self, temp_project):
        result = run_diffctx_subprocess([".", "--diff", "HEAD~1..HEAD"], cwd=temp_project)
        assert result.returncode == EXIT_ENVIRONMENT
        assert "requires a git repository" in result.stderr

    def test_diff_in_repo_with_no_commits_is_clean_environment_error(self, tmp_path):
        """Regression (#86): a `git init`-only repo (no commits yet) used to
        leak a raw `fatal: ambiguous argument 'HEAD'` git error plus git's
        own unrelated `--` separator advice, instead of a diffctx-native
        message like the "not a git repository at all" case already had."""
        import subprocess

        subprocess.run(["git", "init", "-q"], cwd=tmp_path, check=True)
        result = run_diffctx_subprocess([".", "--diff"], cwd=tmp_path)
        assert result.returncode == EXIT_ENVIRONMENT
        assert "requires at least one commit" in result.stderr
        assert "ambiguous argument" not in result.stderr
        assert "Use '--' to separate paths" not in result.stderr

    def test_diff_invalid_range_fails_cleanly(self, diff_repo):
        result = run_diffctx_subprocess([".", "--diff", "no_such_ref..HEAD"], cwd=diff_repo.path)
        assert result.returncode == EXIT_ENVIRONMENT
        assert "git error" in result.stderr
        assert "internal error" not in result.stderr

    def test_diff_to_clipboard_writes_file_too(self, diff_repo, tmp_path):
        out = tmp_path / "diff.yaml"
        result = run_diffctx_subprocess([".", "--diff", "HEAD~1..HEAD", "-o", str(out)], cwd=diff_repo.path)
        assert result.returncode == EXIT_OK
        assert out.exists()
        assert "diff_context" in out.read_text(encoding="utf-8")


class TestGraphModeJourneys:
    def test_default_graph_is_mermaid(self, graph_repo):
        result = run_diffctx_subprocess(["graph", "."], cwd=graph_repo.path)
        assert result.returncode == EXIT_OK
        assert result.stdout.lstrip().startswith("graph LR")

    def test_graph_json_is_valid(self, graph_repo):
        result = run_diffctx_subprocess(["graph", ".", "-f", "json"], cwd=graph_repo.path)
        assert result.returncode == EXIT_OK
        doc = json.loads(result.stdout)
        assert "node_count" in doc
        assert "edge_count" in doc

    def test_graph_graphml_is_xml(self, graph_repo):
        result = run_diffctx_subprocess(["graph", ".", "-f", "graphml"], cwd=graph_repo.path)
        assert result.returncode == EXIT_OK
        assert "<graphml" in result.stdout

    def test_graph_summary_reports_statistics(self, graph_repo):
        result = run_diffctx_subprocess(["graph", ".", "--summary"], cwd=graph_repo.path)
        assert result.returncode == EXIT_OK
        assert "summary" in result.stdout.lower()
        assert "Nodes:" in result.stdout

    @pytest.mark.parametrize("level", ["fragment", "file", "directory"])
    def test_graph_levels_all_render(self, graph_repo, level):
        result = run_diffctx_subprocess(["graph", ".", "--level", level], cwd=graph_repo.path)
        assert result.returncode == EXIT_OK
        assert result.stdout.strip()


class TestIdentityJourneys:
    def test_version_matches_package(self, temp_project):
        import diffctx

        result = run_diffctx_subprocess(["--version"], cwd=temp_project)
        assert result.returncode == EXIT_OK
        assert result.stdout.strip() == f"diffctx {diffctx.__version__}"

    def test_help_lists_diff_and_graph(self, temp_project):
        result = run_diffctx_subprocess(["--help"], cwd=temp_project)
        assert result.returncode == EXIT_OK
        assert "--diff" in result.stdout
        assert "graph" in result.stdout
