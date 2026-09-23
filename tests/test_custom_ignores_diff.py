from __future__ import annotations

import json

import pytest

import diffctx
from tests.framework.pygit2_backend import Pygit2Repo

from .conftest import run_diffctx_subprocess

EXIT_USAGE = 2


def _two_commit_repo(tmp_path):
    repo = Pygit2Repo(tmp_path / "repo")
    repo.add_file("app.py", "def a():\n    pass\n")
    repo.commit("initial")
    repo.add_file("app.py", "def a():\n    return 1\n")
    repo.commit("change app")
    (repo.path / "custom.ignore").write_text("*.py\n", encoding="utf-8")
    (repo.path / "custom.whitelist").write_text("app.py\n", encoding="utf-8")
    return repo


@pytest.mark.parametrize(
    "flags",
    [["-i", "custom.ignore"], ["-w", "custom.whitelist"], ["--no-default-ignores"], ["--no-ignores"]],
)
def test_tree_mode_ignore_flags_are_refused_with_diff(tmp_path, flags):
    """A `-i secrets.ignore` the pipeline never applies must not pass as if
    the exclusion took effect: the refusal is the only thing standing between
    a caller and a secret they believed excluded."""
    repo = _two_commit_repo(tmp_path)
    result = run_diffctx_subprocess([".", "--diff", "HEAD~1..HEAD", *flags], cwd=repo.path)
    assert result.returncode == EXIT_USAGE, result.stderr
    assert result.stdout == ""
    assert flags[0] in result.stderr
    assert "is not supported with --diff" in result.stderr


def test_a_saved_tree_file_is_reported_by_the_next_bare_diff(tmp_path):
    """The Rust pipeline does not read the tree walker's noise list, so a
    `tree.md` written by `--save` is an untracked change like any other; the
    help text says so instead of promising an auto-ignore that never happens."""
    repo = _two_commit_repo(tmp_path)
    saved = run_diffctx_subprocess([".", "--diff", "HEAD~1..HEAD", "--save", "-q"], cwd=repo.path)
    assert saved.returncode == 0, saved.stderr
    assert (repo.path / "tree.md").is_file()
    result = run_diffctx_subprocess([".", "--diff", "-f", "json", "-q"], cwd=repo.path)
    assert "tree.md" in json.loads(result.stdout)["changed_files"]


def test_diff_context_excludes_diffctx_ignore_pattern(tmp_path):
    """Regression (#85): --diff only ever excluded a hardcoded secret-key
    filename list (is_secret_path); a file explicitly excluded via
    .diffctx/ignore still had its changed content surfaced in full."""
    repo = Pygit2Repo(tmp_path / "repo")
    repo.add_file(".diffctx/ignore", "*.secret\n")
    repo.add_file("app.py", "def a():\n    pass\n")
    repo.add_file("passwords.secret", "unrelated\n")
    repo.commit("initial")

    repo.add_file("app.py", "def a():\n    return 1\n")
    repo.add_file("passwords.secret", "LEAK_SECRET_CHANGED\n")
    repo.commit("change app and secret")

    for full in (False, True):
        rendered = diffctx.to_yaml(diffctx.build_diff_context(root_dir=repo.path, diff_range="HEAD~1", full=full))
        assert "LEAK_SECRET_CHANGED" not in rendered, full
        assert "passwords.secret" not in rendered, full
        assert "app.py" in rendered, full


def test_diff_context_keeps_paths_reincluded_by_a_negation(tmp_path):
    """Regression (#153): pandoc excludes every dotted root entry with `/*.*`
    and re-includes `!.github/**`. git keeps the tracked workflow file, but
    `check-ignore --no-index` reports it ignored *through the excluded parent*
    — git forbids re-including a file under an excluded directory. Honouring
    that turned a real one-file change into an empty selection (exit 4)."""
    repo = Pygit2Repo(tmp_path / "repo")
    # `/*.*` matches the ignore file and `.github` themselves, so the fixture
    # has to stage them the way pandoc's history did: explicitly.
    repo.add_file(".gitignore", "/*.*\n!.github/**\n!README.md\n")
    repo.add_file(".github/workflows/ci.yml", "name: ci\njobs:\n  a:\n    runs-on: ubuntu-latest\n")
    repo.add_file("README.md", "# project\n")
    repo.stage_file(".gitignore")
    repo.stage_file(".github/workflows/ci.yml")
    repo.commit("initial")

    repo.add_file(
        ".github/workflows/ci.yml",
        "name: ci\njobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: echo KEPT_WORKFLOW_CHANGE\n",
    )
    repo.stage_file(".github/workflows/ci.yml")
    repo.commit("bump the runner")

    for full in (False, True):
        ctx = diffctx.build_diff_context(root_dir=repo.path, diff_range="HEAD~1", full=full)
        assert ".github/workflows/ci.yml" in ctx["changed_files"], full
        assert "KEPT_WORKFLOW_CHANGE" in diffctx.to_yaml(ctx), full


def test_diff_context_excludes_nested_gitignore_pattern(tmp_path):
    """Regression (#85): a nested (non-root) .gitignore pattern had no
    effect on --diff output — only the hardcoded secret-key filter applied.

    The ignored file must be staged explicitly: `commit()` adds via
    `index.add_all()`, which honours .gitignore, so an unstaged drop.tmp never
    enters the diff at all and the assertions below would hold no matter what
    diffctx does with nested patterns."""
    repo = Pygit2Repo(tmp_path / "repo")
    repo.add_file("sub/.gitignore", "*.tmp\n")
    repo.add_file("sub/app.py", "def a():\n    pass\n")
    repo.add_file("sub/drop.tmp", "unrelated\n")
    repo.stage_file("sub/drop.tmp")
    repo.commit("initial")

    repo.add_file("sub/app.py", "def a():\n    return 1\n")
    repo.add_file("sub/drop.tmp", "LEAK_TMP_CHANGED\n")
    repo.stage_file("sub/drop.tmp")
    repo.commit("change app and tmp file")

    for full in (False, True):
        result = diffctx.build_diff_context(root_dir=repo.path, diff_range="HEAD~1", full=full)
        rendered = diffctx.to_yaml(result)
        # What #85 protects is the content: it must never surface.
        assert "LEAK_TMP_CHANGED" not in rendered, full
        # The exclusion itself is no longer silent (#188): the path is declared
        # under ignored_changes — and appears nowhere else.
        fragment_paths = {f.get("path") for f in result.get("fragments", [])}
        assert "sub/drop.tmp" not in fragment_paths, full
        assert "sub/drop.tmp" not in (result.get("changed_files") or []), full
        if not full:
            assert result.get("ignored_changes") == ["sub/drop.tmp"], full
        assert "app.py" in rendered, full
