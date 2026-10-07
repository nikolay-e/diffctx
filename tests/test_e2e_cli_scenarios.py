# tests/test_e2e_cli_scenarios.py
"""End-to-end CLI user-journey tests.

Every scenario invokes the real `python -m diffctx` subprocess against a real
filesystem / real git repo, mirroring how an actual user drives the tool. No
in-process shortcuts: exit codes, stdout, and stderr are all asserted exactly
as a shell user would observe them.
"""

from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

import pytest
import yaml

from tests.framework.pygit2_backend import Pygit2Repo
from tests.garbage_data import GARBAGE_FILES

from .conftest import SRC_DIR, run_diffctx_subprocess

EXIT_OK = 0
EXIT_RUNTIME = 1
EXIT_USAGE = 2
EXIT_ENVIRONMENT = 3
EXIT_EMPTY_DIFF = 4
EXIT_TIMEOUT = 124

_STALLING_UNTRACKED_SCAN = """#!/bin/sh
for a in "$@"; do
  if [ "$a" = --others ]; then exec sleep 600; fi
done
exec "$REAL_GIT" "$@"
"""


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
    def test_default_stdout_is_md_directory(self, temp_project):
        result = run_diffctx_subprocess([str(temp_project)])
        assert result.returncode == EXIT_OK
        assert result.stdout.startswith(f"# {temp_project.name}/")

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
        with_content = run_diffctx_subprocess([str(temp_project), "-f", "yaml"])
        without_content = run_diffctx_subprocess([str(temp_project), "-f", "yaml", "--no-content"])
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

    def test_bare_save_writes_markdown(self, temp_project):
        result = run_diffctx_subprocess([".", "--save"], cwd=temp_project)
        assert result.returncode == EXIT_OK
        assert (temp_project / "tree.md").read_text(encoding="utf-8").startswith(f"# {temp_project.name}/")
        assert not (temp_project / "tree.yaml").exists()

    @pytest.mark.parametrize("paths", [[".", "src"], ["src", "src/deep"]])
    def test_a_directory_inside_another_requested_directory_renders_once(self, temp_project, paths):
        (temp_project / "src" / "deep").mkdir()
        (temp_project / "src" / "deep" / "leaf.py").write_text("x = 1\n", encoding="utf-8")
        result = run_diffctx_subprocess([*paths, "-f", "txt"], cwd=temp_project)
        assert result.returncode == EXIT_OK
        assert f"1 path(s) already inside another requested directory, skipped: {temp_project / paths[1]}" in result.stderr
        for name in ("main.py", "leaf.py", "deep/"):
            assert result.stdout.count(name) == 1, (name, result.stdout)

    def test_output_file_writes_and_reports_path(self, temp_project):
        out = temp_project / "export.yaml"
        result = run_diffctx_subprocess([str(temp_project), "-o", str(out)])
        assert result.returncode == EXIT_OK
        assert out.exists()
        assert "Saved to" in result.stderr
        assert str(out) in result.stderr

    def test_dash_output_forces_stdout(self, temp_project):
        result = run_diffctx_subprocess([str(temp_project), "-f", "yaml", "-o", "-"])
        assert result.returncode == EXIT_OK
        assert yaml.safe_load(result.stdout)["type"] == "directory"

    def test_single_file_argument(self, temp_project):
        target = temp_project / "src" / "main.py"
        result = run_diffctx_subprocess([str(target), "-f", "yaml"])
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

    def test_quiet_overrides_and_reports_the_log_level(self, temp_project):
        result = run_diffctx_subprocess([".", "-q", "--log-level", "debug"], cwd=temp_project)
        assert result.returncode == EXIT_OK
        assert "--log-level debug ignored with -q" in result.stderr
        assert "DEBUG" not in result.stderr

    def test_help_does_not_promise_tree_files_are_ignored_in_diff_mode(self, temp_project):
        result = run_diffctx_subprocess(["--help"], cwd=temp_project)
        assert "auto-ignored in tree mode only" in result.stdout
        assert "(auto-ignored)" not in result.stdout

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
            (["--max-depth", "-1"], EXIT_USAGE, "non-negative"),
            (["--max-file-bytes", "0"], EXIT_USAGE, "no-file-size-limit"),
            (["--max-file-bytes", "-5"], EXIT_USAGE, "non-negative"),
            (["-f", "xml"], EXIT_USAGE, "invalid choice"),
            (["--log-level", "trace"], EXIT_USAGE, "invalid choice"),
            (["nonexistent_dir_xyz"], EXIT_RUNTIME, "No matches"),
            ([".", "--diff", ""], EXIT_USAGE, "--diff requires a non-empty range"),
            ([".", "-o", "docs"], EXIT_USAGE, "is a directory"),
            ([".", "--save", "-o", "x.yaml"], EXIT_USAGE, "mutually exclusive"),
        ],
    )
    def test_invalid_invocation(self, temp_project, args, expected_exit, needle):
        result = run_diffctx_subprocess(args, cwd=temp_project)
        assert result.returncode == expected_exit, f"stderr: {result.stderr}"
        assert needle.lower() in result.stderr.lower()
        assert result.stdout == "", "a refused invocation must not print a tree"

    def test_diff_flags_without_diff_emit_warning(self, temp_project):
        result = run_diffctx_subprocess([str(temp_project), "--budget", "5000", "--alpha", "0.5"])
        assert result.returncode == EXIT_OK
        assert "ignored without --diff" in result.stderr
        assert "--budget" in result.stderr
        assert "--alpha" in result.stderr


class TestDiffModeJourneys:
    def test_diff_selects_changed_symbols_and_excludes_garbage(self, diff_repo):
        result = run_diffctx_subprocess([".", "--diff", "HEAD~1..HEAD", "-f", "yaml"], cwd=diff_repo.path)
        assert result.returncode == EXIT_OK
        doc = yaml.safe_load(result.stdout)
        assert doc["type"] == "diff_context"
        assert doc["fragment_count"] > 0
        assert "subtract" in result.stdout
        # Seven of the ten injected files carry neither "GARBAGE" nor
        # "garbage_marker"; the lowercase prefix is the one thing all of them
        # share in both path and body.
        assert "garbage" not in result.stdout.lower()

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

    def test_empty_selection_still_names_the_changed_files(self, diff_repo):
        result = run_diffctx_subprocess([".", "--diff", "HEAD~1..HEAD", "--budget", "0", "-f", "yaml"], cwd=diff_repo.path)
        assert result.returncode == EXIT_EMPTY_DIFF
        doc = yaml.safe_load(result.stdout)
        assert doc["changed_files"], "an empty selection must still report the range's files"
        assert doc["commit_message"]

    def test_full_includes_all_changed_fragments(self, diff_repo):
        smart = run_diffctx_subprocess([".", "--diff", "HEAD~1..HEAD", "-f", "yaml"], cwd=diff_repo.path)
        full = run_diffctx_subprocess([".", "--diff", "HEAD~1..HEAD", "--full", "-f", "yaml"], cwd=diff_repo.path)
        assert full.returncode == EXIT_OK
        smart_doc = yaml.safe_load(smart.stdout)
        full_doc = yaml.safe_load(full.stdout)
        assert full_doc["fragment_count"] >= smart_doc["fragment_count"]

    def test_budget_bounds_output_size(self, diff_repo):
        small = run_diffctx_subprocess([".", "--diff", "HEAD~1..HEAD", "--budget", "400"], cwd=diff_repo.path)
        large = run_diffctx_subprocess([".", "--diff", "HEAD~1..HEAD", "--budget", "8000"], cwd=diff_repo.path)
        assert small.returncode == EXIT_OK
        assert large.returncode == EXIT_OK
        assert len(small.stdout) <= len(large.stdout)

    def test_a_budget_below_the_change_summary_is_refused_not_overrun(self, diff_repo):
        """The budget covers the artifact, and the change summary is charged
        first (#241). A budget that cannot hold the summary therefore leaves
        nothing to select with — which the CLI says, instead of silently
        emitting three times the number the user asked for."""
        # Above `overhead_per_fragment` (40) on purpose: a budget under that
        # selects nothing on any build, so it would pass before #241 too. This
        # one is refused only because the change summary is charged first.
        result = run_diffctx_subprocess([".", "--diff", "HEAD~1..HEAD", "--budget", "60"], cwd=diff_repo.path)
        assert result.returncode == EXIT_EMPTY_DIFF
        assert "too small to fit any fragment" in result.stderr

    @pytest.mark.parametrize("scoring", ["ego", "ppr", "bm25", "rrf"])
    def test_scoring_modes_all_produce_context(self, diff_repo, scoring):
        result = run_diffctx_subprocess([".", "--diff", "HEAD~1..HEAD", "--scoring", scoring, "-f", "yaml"], cwd=diff_repo.path)
        assert result.returncode == EXIT_OK
        assert yaml.safe_load(result.stdout)["type"] == "diff_context"

    def test_diff_outside_git_repo_is_environment_error(self, temp_project):
        result = run_diffctx_subprocess([".", "--diff", "HEAD~1..HEAD"], cwd=temp_project)
        assert result.returncode == EXIT_ENVIRONMENT
        assert "requires a git repository" in result.stderr

    def test_diff_in_a_bare_repository_is_a_branded_environment_error(self, diff_repo, tmp_path):
        bare = tmp_path / "bare.git"
        subprocess.run(["git", "clone", "--quiet", "--bare", str(diff_repo.path), str(bare)], check=True)
        result = run_diffctx_subprocess([".", "--diff", "HEAD~1..HEAD"], cwd=bare)
        assert result.returncode == EXIT_ENVIRONMENT
        assert f"--diff requires a working tree (bare repository: {bare}" in result.stderr
        assert "must be run in a work tree" not in result.stderr

    def test_tree_mode_flags_are_reported_as_ignored_with_diff(self, diff_repo):
        result = run_diffctx_subprocess(
            [".", "--diff", "HEAD~1..HEAD", "--max-depth", "1", "--max-file-bytes", "10", "--no-file-size-limit", "-q"],
            cwd=diff_repo.path,
        )
        assert result.returncode == EXIT_OK
        assert "tree-mode flags ignored with --diff: --max-depth, --max-file-bytes, --no-file-size-limit" in result.stderr

    def test_latency_is_logged_at_info_and_absent_from_the_json_artifact(self, diff_repo):
        result = run_diffctx_subprocess([".", "--diff", "HEAD~1..HEAD", "-f", "json", "--log-level", "info"], cwd=diff_repo.path)
        assert result.returncode == EXIT_OK
        assert "latency" not in json.loads(result.stdout)
        assert "INFO: latency (ms): " in result.stderr
        assert "total_ms=" in result.stderr

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
        assert "unknown git revision 'no_such_ref..HEAD'" in result.stderr
        assert "internal error" not in result.stderr

    def test_timeout_flag_accepted_and_diff_completes(self, diff_repo):
        result = run_diffctx_subprocess([".", "--diff", "HEAD~1..HEAD", "--timeout", "300", "-f", "yaml"], cwd=diff_repo.path)
        assert result.returncode == EXIT_OK
        assert yaml.safe_load(result.stdout)["type"] == "diff_context"

    def test_timeout_below_one_second_is_usage_error(self, diff_repo):
        result = run_diffctx_subprocess([".", "--diff", "HEAD~1..HEAD", "--timeout", "0"], cwd=diff_repo.path)
        assert result.returncode == EXIT_USAGE
        assert "--timeout must be >= 1" in result.stderr

    def test_timeout_without_diff_warns_and_is_ignored(self, temp_project):
        result = run_diffctx_subprocess([".", "--timeout", "5"], cwd=temp_project)
        assert result.returncode == EXIT_OK
        assert "diff-mode flags ignored without --diff" in result.stderr
        assert "--timeout" in result.stderr

    def test_expired_deadline_aborts_with_exit_124(self):
        """The wall-clock watchdog must hard-abort a pipeline that outlives
        --timeout (#70): a runaway Rust computation cannot be cancelled from
        Python, so the process exits 124 like the standalone binary. Exercised
        in a real subprocess with a genuinely slow (sleeping) pipeline call."""
        watchdog_script = (
            "import time\n"
            "from diffctx._app import _call_with_wall_clock_deadline\n"
            "_call_with_wall_clock_deadline(lambda: time.sleep(60), 1, 'diffctx')\n"
        )
        env = os.environ.copy()
        env["PYTHONPATH"] = str(SRC_DIR)
        env["DIFFCTX_TEST_WATCHDOG_GRACE_SECS"] = "1"
        result = subprocess.run(
            [sys.executable, "-c", watchdog_script],
            capture_output=True,
            text=True,
            env=env,
            timeout=30,
            check=False,
        )
        assert result.returncode == EXIT_TIMEOUT
        assert "wall-clock deadline" in result.stderr
        assert "--timeout" in result.stderr

    def test_a_pipeline_that_stops_at_its_deadline_still_answers(self):
        """#382: the engine returns a partial answer at --timeout; a watchdog
        firing at the same instant killed it and printed nothing at all."""
        script = (
            "import time\n"
            "from diffctx._app import _call_with_wall_clock_deadline\n"
            "print(_call_with_wall_clock_deadline(lambda: (time.sleep(1.5), 'partial')[1], 1, 'diffctx'))\n"
        )
        env = os.environ.copy()
        env["PYTHONPATH"] = str(SRC_DIR)
        env.pop("DIFFCTX_TEST_WATCHDOG_GRACE_SECS", None)
        result = subprocess.run([sys.executable, "-c", script], capture_output=True, text=True, env=env, timeout=30, check=False)
        assert result.returncode == 0, result.stderr
        assert result.stdout.strip() == "partial"

    @pytest.mark.skipif(sys.platform == "win32", reason="the stalling git is a POSIX shell script")
    @pytest.mark.parametrize("mode", ["pack", "locate"])
    def test_a_deadline_that_found_nothing_names_the_limit_not_a_clean_tree(self, tmp_path, mode):
        """The one change is untracked and listing it outlives --timeout: the
        run is partial, exit 0 with the limit named, never exit 4 with "the
        working tree matches HEAD" (#408)."""
        repo = Pygit2Repo(tmp_path / "repo")
        repo.add_file("pricing.py", "def total(items):\n    return sum(items)\n")
        repo.commit("initial")
        repo.add_file("refund.py", "from pricing import total\n\n\ndef refund(cart):\n    return -total(cart)\n")
        bin_dir = tmp_path / "bin"
        bin_dir.mkdir()
        fake = bin_dir / "git"
        fake.write_text(_STALLING_UNTRACKED_SCAN, encoding="utf-8")
        fake.chmod(0o755)
        env = {"PATH": f"{bin_dir}{os.pathsep}{os.environ['PATH']}", "REAL_GIT": shutil.which("git") or "git"}
        args = [".", "--diff", "--timeout", "3", "-f", "json", "-q", *(["--mode", "locate"] if mode == "locate" else [])]
        result = run_diffctx_subprocess(args, cwd=repo.path, env=env, timeout=60)
        assert result.returncode == EXIT_OK, result.stderr
        assert "deadline" in result.stderr
        assert "no semantic context" not in result.stderr
        assert "deadline" in json.loads(result.stdout)["coverage"]["limit_reasons"]

    @pytest.mark.parametrize("spec", ["staged", "--cached"])
    def test_an_empty_staged_pack_names_a_command_git_accepts(self, diff_repo, spec):
        """Nothing staged: the hint is `git diff --cached --stat`, not a range
        git rejects or a duration window that does not exist (#409)."""
        diff_repo.add_file("src/calc.py", "def add(a, b):\n    return b + a\n")
        result = run_diffctx_subprocess([".", f"--diff={spec}"], cwd=diff_repo.path)
        assert result.returncode == EXIT_EMPTY_DIFF, result.stderr
        assert "git diff --cached --stat" in result.stderr

    def test_diff_to_clipboard_writes_file_too(self, diff_repo, tmp_path):
        out = tmp_path / "diff.yaml"
        result = run_diffctx_subprocess([".", "--diff", "HEAD~1..HEAD", "-f", "yaml", "-o", str(out)], cwd=diff_repo.path)
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

    @pytest.mark.parametrize("fmt", ["json", "graphml"])
    def test_an_explicit_file_level_exports_file_nodes(self, graph_repo, fmt):
        """#352: `--level file` was ignored for JSON and GraphML; every node was a
        fragment (`path:start-end`) and the caller had to group them."""
        result = run_diffctx_subprocess(["graph", ".", "--level", "file", "-f", fmt], cwd=graph_repo.path)
        assert result.returncode == EXIT_OK, result.stderr
        assert "--level" not in result.stderr
        if fmt == "json":
            doc = json.loads(result.stdout)
            ids = {n["id"] for n in doc["nodes"]}
            assert ids
            assert all(":" not in i for i in ids), ids
            assert doc["node_count"] == len(ids)
            assert all({e["source"], e["target"]} <= ids for e in doc["edges"])
        else:
            assert not re.search(r'<node id="[^"]*:\d+-\d+"', result.stdout)

    def test_without_a_level_the_export_stays_per_fragment(self, graph_repo):
        doc = json.loads(run_diffctx_subprocess(["graph", ".", "-f", "json"], cwd=graph_repo.path).stdout)
        assert all(re.search(r":\d+-\d+$", n["id"]) for n in doc["nodes"])

    def test_a_file_level_summary_counts_and_ranks_files(self, graph_repo):
        result = run_diffctx_subprocess(["graph", ".", "--summary", "--level", "file"], cwd=graph_repo.path)
        assert result.returncode == EXIT_OK
        nodes = int(re.search(r"Nodes: (\d+)", result.stdout).group(1))
        files = int(re.search(r"Files: (\d+)", result.stdout).group(1))
        assert nodes == files
        ranked = result.stdout.split("most-referenced:")[1].splitlines() if "most-referenced:" in result.stdout else []
        assert not any(re.search(r":\d+\s+in_degree", line) for line in ranked), ranked

    def test_a_key_every_workflow_carries_links_no_source_file(self, tmp_path):
        """#297: `workflow_dispatch:` in every CI workflow split into the word
        `workflow`, and every source file that says "workflow" was linked to
        every workflow's concurrency block, outranking the changed code."""
        repo = Pygit2Repo(tmp_path / "workflows")
        for name in ("ci", "cd", "docker", "security", "automerge"):
            repo.add_file(
                f".github/workflows/{name}.yml",
                f"name: {name}\non:\n  push:\n  workflow_dispatch:\nconcurrency:\n"
                "  group: ${{ github.workflow }}-${{ github.ref }}\n  cancel-in-progress: true\n",
            )
        repo.add_file("app/runner.py", "def run_workflow(workflow):\n    return workflow.steps\n")
        repo.add_file("deploy.yml", "release_channel: stable\n")
        repo.add_file("app/release.py", "def channel(cfg):\n    return cfg['release_channel']\n")
        repo.commit("initial commit")
        result = run_diffctx_subprocess(["graph", ".", "-f", "json"], cwd=repo.path)
        assert result.returncode == EXIT_OK
        edges = json.loads(result.stdout)["edges"]
        linked = {(e["source"].split(":")[0], e["target"].split(":")[0]) for e in edges}
        workflow_to_code = {(a, b) for a, b in linked if ".github/workflows/" in a + b and "app/" in a + b}
        assert not workflow_to_code, workflow_to_code
        assert ("deploy.yml", "app/release.py") in linked, "a key one config file owns still links"

    @pytest.mark.parametrize("environments", [3, 4, 6])
    def test_a_setting_two_environments_share_still_links_its_reader(self, tmp_path, environments):
        repo = Pygit2Repo(tmp_path / "envs")
        for i in range(environments):
            extra = "payment_gateway_url: https://pay\n" if i < 2 else ""
            repo.add_file(f"config/env{i}.yaml", f"log_level: info\n{extra}")
        repo.add_file("app/pay.py", "def gateway(cfg):\n    return cfg['payment_gateway_url']\n")
        repo.commit("initial commit")
        # The tags fallback links the two files by the key as well; without it
        # the config builder's own vocabulary gate is what is measured.
        result = run_diffctx_subprocess(
            ["graph", ".", "-f", "json", "--level", "file"], cwd=repo.path, env={"DIFFCTX_DISABLE_BUILDERS": "tags"}
        )
        assert result.returncode == EXIT_OK
        linked = {(e["source"], e["target"]) for e in json.loads(result.stdout)["edges"]}
        assert any("app/pay.py" in a + b and "config/env0.yaml" in a + b for a, b in linked), linked

    def test_one_way_import_reports_no_cycles(self, graph_repo):
        result = run_diffctx_subprocess(["graph", ".", "--summary", "--level", "file"], cwd=graph_repo.path)
        assert result.returncode == EXIT_OK
        assert "No dependency cycles detected." in result.stdout

    def test_one_way_cross_directory_import_reports_no_cycles(self, tmp_path):
        repo = Pygit2Repo(tmp_path / "layered_repo")
        repo.add_file("pkg_a/entry.py", "from pkg_b.core import core_fn\n\n\ndef entry():\n    return core_fn()\n")
        repo.add_file("pkg_b/core.py", "def core_fn():\n    return 1\n")
        repo.commit("initial commit")
        result = run_diffctx_subprocess(["graph", ".", "--summary"], cwd=repo.path)
        assert result.returncode == EXIT_OK
        assert "No dependency cycles detected." in result.stdout

    def test_mutual_imports_report_a_cycle(self, tmp_path):
        repo = Pygit2Repo(tmp_path / "mutual_repo")
        repo.add_file("alpha.py", "from beta import beta_fn\n\n\ndef alpha_fn():\n    return beta_fn()\n")
        repo.add_file("beta.py", "from alpha import alpha_fn\n\n\ndef beta_fn():\n    return alpha_fn()\n")
        repo.commit("initial commit")
        result = run_diffctx_subprocess(["graph", ".", "--summary", "--level", "file"], cwd=repo.path)
        assert result.returncode == EXIT_OK
        assert "1 dependency cycle(s) detected" in result.stdout
        assert "alpha.py" in result.stdout
        assert "beta.py" in result.stdout

    def test_summary_edge_categories_are_shares(self, graph_repo):
        result = run_diffctx_subprocess(["graph", ".", "--summary"], cwd=graph_repo.path)
        assert result.returncode == EXIT_OK
        assert "Edge categories (% of discovered relations):" in result.stdout
        assert "%" in result.stdout.split("Edge categories")[1].splitlines()[1]

    def test_hotspots_report_git_churn(self, graph_repo):
        result = run_diffctx_subprocess(["graph", ".", "--summary"], cwd=graph_repo.path)
        assert result.returncode == EXIT_OK
        hotspot_lines = [line for line in result.stdout.splitlines() if "churn=" in line]
        assert hotspot_lines
        assert any("churn=0" not in line for line in hotspot_lines)

    def test_mermaid_edge_weights_are_normalized(self, graph_repo):
        result = run_diffctx_subprocess(["graph", ".", "--level", "file"], cwd=graph_repo.path)
        assert result.returncode == EXIT_OK
        edge_labels = re.findall(r'-->\|"([^"]+)"\|', result.stdout)
        assert edge_labels
        assert all(label.endswith("%") for label in edge_labels)

    @pytest.mark.parametrize("flags", [["-i", "extra.ignore"], ["-w", "only.whitelist"], ["--no-default-ignores"]])
    def test_path_spec_flags_are_refused_once_as_a_usage_error(self, graph_repo, flags):
        (graph_repo.path / "extra.ignore").write_text("*.py\n", encoding="utf-8")
        (graph_repo.path / "only.whitelist").write_text("src/**\n", encoding="utf-8")
        result = run_diffctx_subprocess(["graph", ".", *flags], cwd=graph_repo.path)
        assert result.returncode == EXIT_USAGE
        assert result.stdout == ""
        assert result.stderr.count("\n") == 1, f"exactly one line expected, got: {result.stderr!r}"
        assert flags[0] in result.stderr
        assert "is not supported with graph" in result.stderr
        assert "ignored" not in result.stderr

    def test_output_extension_selects_the_graph_format(self, graph_repo):
        out = graph_repo.path / "g.json"
        result = run_diffctx_subprocess(["graph", ".", "-o", str(out)], cwd=graph_repo.path)
        assert result.returncode == EXIT_OK
        assert "does not match" not in result.stderr
        assert "node_count" in json.loads(out.read_text(encoding="utf-8"))

    def test_explicit_format_wins_over_the_extension_with_a_warning(self, graph_repo):
        out = graph_repo.path / "g.json"
        result = run_diffctx_subprocess(["graph", ".", "-f", "mermaid", "-o", str(out)], cwd=graph_repo.path)
        assert result.returncode == EXIT_OK
        assert f"-f mermaid does not match the '{out}' extension; writing mermaid" in result.stderr
        assert out.read_text(encoding="utf-8").lstrip().startswith("graph LR")


class TestIdentityJourneys:
    def test_version_matches_package(self, temp_project):
        import diffctx

        result = run_diffctx_subprocess(["--version"], cwd=temp_project)
        assert result.returncode == EXIT_OK
        assert result.stdout.strip() == f"diffctx {diffctx.__version__}"

    @staticmethod
    def _console_script():
        script = shutil.which("diffctx", path=str(Path(sys.executable).parent)) or shutil.which("diffctx")
        if script is None:
            pytest.skip("the diffctx console script is not installed next to this interpreter")
        return script

    def test_console_script_reports_the_same_version(self, temp_project):
        script = self._console_script()
        via_module = run_diffctx_subprocess(["-v"], cwd=temp_project)
        via_script = subprocess.run([script, "-v"], cwd=temp_project, capture_output=True, text=True, check=False)
        assert via_script.returncode == via_module.returncode == EXIT_OK
        assert via_script.stdout == via_module.stdout

    def test_console_script_runs_diff_mode_like_the_module(self, diff_repo):
        script = self._console_script()
        args = [".", "--diff", "HEAD~1..HEAD", "-q"]
        via_module = run_diffctx_subprocess(args, cwd=diff_repo.path)
        via_script = subprocess.run([script, *args], cwd=diff_repo.path, capture_output=True, encoding="utf-8", check=False)
        assert via_script.returncode == via_module.returncode == EXIT_OK
        assert via_script.stdout == via_module.stdout

    def test_help_lists_diff_and_graph(self, temp_project):
        result = run_diffctx_subprocess(["--help"], cwd=temp_project)
        assert result.returncode == EXIT_OK
        assert "--diff" in result.stdout
        assert "graph" in result.stdout
