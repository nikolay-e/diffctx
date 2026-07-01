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


def test_diff_context_excludes_nested_gitignore_pattern(tmp_path):
    """Regression (#85): a nested (non-root) .gitignore pattern had no
    effect on --diff output — only the hardcoded secret-key filter applied."""
    repo = Pygit2Repo(tmp_path / "repo")
    repo.add_file("sub/.gitignore", "*.tmp\n")
    repo.add_file("sub/app.py", "def a():\n    pass\n")
    repo.add_file("sub/drop.tmp", "unrelated\n")
    repo.commit("initial")

    repo.add_file("sub/app.py", "def a():\n    return 1\n")
    repo.add_file("sub/drop.tmp", "LEAK_TMP_CHANGED\n")
    repo.commit("change app and tmp file")

    for full in (False, True):
        rendered = diffctx.to_yaml(diffctx.build_diff_context(root_dir=repo.path, diff_range="HEAD~1", full=full))
        assert "LEAK_TMP_CHANGED" not in rendered, full
        assert "drop.tmp" not in rendered, full
        assert "app.py" in rendered, full
