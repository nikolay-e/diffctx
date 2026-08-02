# tests/test_diff_duration_windows.py
"""`--diff <duration>` end-to-end: a real repo whose history straddles the
window, driven through the real CLI subprocess."""

from __future__ import annotations

import subprocess

import pytest
import yaml

from .conftest import run_diffctx_subprocess

EXIT_OK = 0
EXIT_ENVIRONMENT = 3
EXIT_EMPTY_DIFF = 4

_LONG_AGO = "2020-01-01T00:00:00+00:00"


def _git(repo, *args, when=None):
    env = None
    if when is not None:
        env = {"GIT_AUTHOR_DATE": when, "GIT_COMMITTER_DATE": when}
    subprocess.run(["git", "-C", str(repo), *args], check=True, capture_output=True, env=_merged_env(env))


def _merged_env(extra):
    import os

    if extra is None:
        return None
    env = os.environ.copy()
    env.update(extra)
    return env


@pytest.fixture
def straddling_repo(tmp_path):
    """One commit from 2020, one from just now, so any sane window separates them."""
    repo = tmp_path / "window_repo"
    repo.mkdir()
    _git(repo, "init", "-q", "-b", "main")
    _git(repo, "config", "user.email", "test@test.com")
    _git(repo, "config", "user.name", "Test")
    _git(repo, "config", "commit.gpgsign", "false")

    (repo / "ancient.py").write_text("def ancient():\n    return 'ancient'\n", encoding="utf-8")
    _git(repo, "add", "-A", when=_LONG_AGO)
    _git(repo, "commit", "-q", "-m", "ancient", when=_LONG_AGO)

    (repo / "recent.py").write_text("def recent():\n    return 'recent'\n", encoding="utf-8")
    _git(repo, "add", "-A")
    _git(repo, "commit", "-q", "-m", "recent")
    return repo


@pytest.fixture
def ancient_repo(tmp_path):
    """Every commit backdated and the tree clean, so a short window is empty."""
    repo = tmp_path / "ancient_repo"
    repo.mkdir()
    _git(repo, "init", "-q", "-b", "main")
    _git(repo, "config", "user.email", "test@test.com")
    _git(repo, "config", "user.name", "Test")
    _git(repo, "config", "commit.gpgsign", "false")
    (repo / "ancient.py").write_text("def ancient():\n    return 'ancient'\n", encoding="utf-8")
    _git(repo, "add", "-A", when=_LONG_AGO)
    _git(repo, "commit", "-q", "-m", "ancient", when=_LONG_AGO)
    return repo


def _changed_files(result):
    return yaml.safe_load(result.stdout).get("changed_files") or []


class TestDurationWindows:
    def test_window_covers_the_recent_commit_only(self, straddling_repo):
        result = run_diffctx_subprocess([".", "--diff", "24h", "-f", "yaml"], cwd=straddling_repo)
        assert result.returncode == EXIT_OK
        assert _changed_files(result) == ["recent.py"]

    def test_window_covers_uncommitted_and_untracked_work(self, straddling_repo):
        (straddling_repo / "ancient.py").write_text("def ancient():\n    return 'edited'\n", encoding="utf-8")
        (straddling_repo / "brand_new.py").write_text("def brand_new():\n    return 1\n", encoding="utf-8")
        result = run_diffctx_subprocess([".", "--diff", "24h", "-f", "yaml"], cwd=straddling_repo)
        assert result.returncode == EXIT_OK
        assert set(_changed_files(result)) == {"ancient.py", "brand_new.py", "recent.py"}

    def test_window_older_than_the_repo_covers_every_file(self, straddling_repo):
        result = run_diffctx_subprocess([".", "--diff", "520w", "-f", "yaml"], cwd=straddling_repo)
        assert result.returncode == EXIT_OK
        assert set(_changed_files(result)) == {"ancient.py", "recent.py"}

    @pytest.mark.parametrize(("short", "long"), [("1h30m", "90min"), ("1d", "24h"), ("2w", "14d")])
    def test_equivalent_durations_select_the_same_context(self, straddling_repo, short, long):
        a = run_diffctx_subprocess([".", "--diff", short, "-f", "yaml"], cwd=straddling_repo)
        b = run_diffctx_subprocess([".", "--diff", long, "-f", "yaml"], cwd=straddling_repo)
        assert a.returncode == b.returncode == EXIT_OK
        assert a.stdout == b.stdout

    def test_a_ref_named_like_a_duration_still_wins(self, straddling_repo):
        """A branch `24h` at HEAD makes the two readings disagree: as a ref the
        diff is empty, as a window it would carry the recent commit."""
        _git(straddling_repo, "branch", "24h", "HEAD")
        result = run_diffctx_subprocess([".", "--diff", "24h", "-f", "yaml"], cwd=straddling_repo)
        assert result.returncode == EXIT_EMPTY_DIFF
        assert _changed_files(result) == []

    def test_a_near_miss_of_the_grammar_is_still_a_revision(self, straddling_repo):
        result = run_diffctx_subprocess([".", "--diff", "8dd"], cwd=straddling_repo)
        assert result.returncode == EXIT_ENVIRONMENT
        assert "unknown git revision '8dd'" in result.stderr

    def test_mcp_fetch_reads_window_bodies_from_the_working_tree(self, straddling_repo):
        """A window ends at the working tree, so `git show 24h:file` is not the
        source of its bodies — uncommitted edits and untracked files have no
        committed blob to read."""
        from diffctx.mcp.fetch import fetch_fragments

        (straddling_repo / "recent.py").write_text("def recent():\n    return 'edited'\n", encoding="utf-8")
        (straddling_repo / "brand_new.py").write_text("def brand_new():\n    return 1\n", encoding="utf-8")
        bodies = fetch_fragments(straddling_repo, "24h", ["recent.py:1-2", "brand_new.py:1-2"], 1_000_000)
        assert "at working tree" in bodies
        assert "return 'edited'" in bodies
        assert "def brand_new():" in bodies

    def test_empty_window_on_a_clean_tree_advises_widening(self, ancient_repo):
        result = run_diffctx_subprocess([".", "--diff", "1h", "-f", "yaml"], cwd=ancient_repo)
        assert result.returncode == EXIT_EMPTY_DIFF
        assert "widen the window" in result.stderr
