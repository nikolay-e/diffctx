from __future__ import annotations

import subprocess

import pytest

import diffctx
from tests.framework.pygit2_backend import Pygit2Repo


def _repo_with_change(tmp_path):
    repo = Pygit2Repo(tmp_path / "repo")
    repo.add_file("calc.py", "def add(a, b):\n    return a + b\n")
    repo.add_file("main.py", "from calc import add\n\ndef run():\n    return add(1, 2)\n")
    repo.commit("initial")
    repo.add_file("calc.py", "def add(a, b):\n    return a + b + 1\n")
    repo.commit("change add")
    return repo


@pytest.mark.parametrize(
    ("key", "value"),
    [
        ("diff.noprefix", "true"),
        ("diff.mnemonicPrefix", "true"),
        ("color.ui", "always"),
        ("diff.srcPrefix", "X/"),
        ("diff.dstPrefix", "Y/"),
    ],
)
def test_user_diff_config_cannot_empty_the_selection(tmp_path, key, value):
    """The diff parser keys off literal `--- a/` / `+++ b/` headers. Any of
    these settings in a user's global git config rewrote those headers, and
    every run silently returned zero fragments and no changed_files."""
    repo = _repo_with_change(tmp_path)
    subprocess.run(["git", "-C", str(repo.path), "config", key, value], check=True)

    ctx = diffctx.build_diff_context(root_dir=repo.path, diff_range="HEAD~1")

    assert ctx["changed_files"] == ["calc.py"], (key, value)
    assert ctx["fragment_count"] > 0, (key, value)


@pytest.mark.parametrize("crafted", ["a..--ext-diff", "main...--textconv", "HEAD..-p"])
def test_range_side_starting_with_a_dash_is_rejected(tmp_path, crafted):
    """A dash-leading side would land in argv after `--no-ext-diff
    --no-textconv` and undo them, letting repository config run external
    commands. The regex alone never enforced this: its leading character class
    is greedy over `.`, so it swallowed the separator."""
    repo = _repo_with_change(tmp_path)

    with pytest.raises(Exception, match=r"(?i)invalid diff range"):
        diffctx.build_diff_context(root_dir=repo.path, diff_range=crafted)


def test_ordinary_range_still_accepted(tmp_path):
    repo = _repo_with_change(tmp_path)
    ctx = diffctx.build_diff_context(root_dir=repo.path, diff_range="HEAD~1..HEAD")
    assert ctx["fragment_count"] > 0
