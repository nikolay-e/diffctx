from __future__ import annotations

import diffctx
from tests.framework.pygit2_backend import Pygit2Repo


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
        rendered = diffctx.to_yaml(diffctx.build_diff_context(root_dir=repo.path, diff_range="HEAD~1", full=full))
        assert "LEAK_TMP_CHANGED" not in rendered, full
        assert "drop.tmp" not in rendered, full
        assert "app.py" in rendered, full
