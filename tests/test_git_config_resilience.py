from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path
from typing import Any

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


def _repo_with_two_distant_edits(path: Path) -> Pygit2Repo:
    repo = Pygit2Repo(path)
    body = "".join(f"def f{i}(x):\n    return x + {i}\n" for i in range(1, 6))
    repo.add_file("funcs.py", body)
    repo.commit("initial")
    repo.add_file("funcs.py", body.replace("x + 1\n", "x - 1\n").replace("x + 4\n", "x - 4\n"))
    repo.commit("edit f1 and f4")
    return repo


def _hunk_shape(ctx: dict[str, Any]) -> tuple[list[tuple[str, str]], list[tuple[str, str]]]:
    changed = sorted((f["path"], f["lines"]) for f in ctx["fragments"] if f.get("role") == "changed")
    classes = sorted((c["path"], c["class"]) for c in ctx.get("changes") or [])
    return changed, classes


@pytest.mark.parametrize(
    ("config", "env"),
    [
        pytest.param(("diff.interHunkContext", "5"), {}, id="interHunkContext"),
        pytest.param(None, {"GIT_DIFF_OPTS": "-u3"}, id="GIT_DIFF_OPTS"),
    ],
)
def test_user_diff_config_cannot_reshape_the_hunks(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, config: tuple[str, str] | None, env: dict[str, str]
) -> None:
    clean = _repo_with_two_distant_edits(tmp_path / "clean")
    hostile = _repo_with_two_distant_edits(tmp_path / "hostile")
    if config is not None:
        subprocess.run(["git", "-C", str(hostile.path), "config", *config], check=True)
    expected = _hunk_shape(diffctx.build_diff_context(root_dir=clean.path, diff_range="HEAD~1..HEAD"))
    for name, value in env.items():
        monkeypatch.setenv(name, value)
    actual = _hunk_shape(diffctx.build_diff_context(root_dir=hostile.path, diff_range="HEAD~1..HEAD"))

    assert actual == expected
    assert len(expected[0]) == 2, expected


def test_a_git_config_git_refuses_is_reported_in_gits_words(tmp_path: Path) -> None:
    repo = _repo_with_change(tmp_path)
    config = repo.path / ".git" / "config"
    config.write_text(config.read_text() + "[core\n")

    result = subprocess.run(
        [sys.executable, "-m", "diffctx", ".", "--diff", "HEAD"],
        cwd=repo.path,
        capture_output=True,
        text=True,
        env={**os.environ, "PYTHONPATH": str(Path(diffctx.__file__).parents[1])},
    )

    assert result.returncode == 3, result.stderr
    assert "fatal:" in result.stderr
    assert "not a git repository" not in result.stderr


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
