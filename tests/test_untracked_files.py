from __future__ import annotations

import pytest

from tests.framework.pygit2_backend import Pygit2Repo
from treemapper.diffctx import build_diff_context


@pytest.fixture
def git_repo_with_base(tmp_path):
    g = Pygit2Repo(tmp_path / "repo")
    g.add_file("base.py", "def base(): return 1\n")
    g.commit("initial")
    return g


def _fragment_paths(context: dict) -> list[str]:
    return [f["path"] for f in context.get("fragments", [])]


def _all_content(context: dict) -> str:
    return "\n".join(f.get("content", "") for f in context.get("fragments", []))


class TestUntrackedFilesIncluded:
    def test_untracked_file_appears_in_single_ref_diff(self, git_repo_with_base):
        g = git_repo_with_base
        (g.path / "untracked.py").write_text("def new_func(): return 42\n", encoding="utf-8")

        context = build_diff_context(root_dir=g.path, diff_range="HEAD")
        paths = _fragment_paths(context)
        assert any("untracked.py" in p for p in paths)

    def test_untracked_file_content_in_output(self, git_repo_with_base):
        g = git_repo_with_base
        (g.path / "untracked.py").write_text("def secret_sauce(): return 999\n", encoding="utf-8")

        context = build_diff_context(root_dir=g.path, diff_range="HEAD")
        content = _all_content(context)
        assert "secret_sauce" in content

    def test_multiple_untracked_files(self, git_repo_with_base):
        g = git_repo_with_base
        (g.path / "new_a.py").write_text("def func_a(): pass\n", encoding="utf-8")
        (g.path / "new_b.py").write_text("def func_b(): pass\n", encoding="utf-8")

        context = build_diff_context(root_dir=g.path, diff_range="HEAD")
        paths = _fragment_paths(context)
        assert any("new_a.py" in p for p in paths)
        assert any("new_b.py" in p for p in paths)

    def test_untracked_in_subdirectory(self, git_repo_with_base):
        g = git_repo_with_base
        (g.path / "pkg").mkdir()
        (g.path / "pkg" / "module.py").write_text("class Widget:\n    pass\n", encoding="utf-8")

        context = build_diff_context(root_dir=g.path, diff_range="HEAD")
        paths = _fragment_paths(context)
        assert any("pkg/module.py" in p for p in paths)

    def test_mixed_tracked_and_untracked_changes(self, git_repo_with_base):
        g = git_repo_with_base
        g.add_file("base.py", "def base(): return 2\n")
        (g.path / "brand_new.py").write_text("def brand_new(): pass\n", encoding="utf-8")

        context = build_diff_context(root_dir=g.path, diff_range="HEAD")
        paths = _fragment_paths(context)
        assert any("base.py" in p for p in paths)
        assert any("brand_new.py" in p for p in paths)

    def test_untracked_binary_file_excluded(self, git_repo_with_base):
        g = git_repo_with_base
        (g.path / "image.png").write_bytes(b"\x89PNG\r\n\x1a\n" + b"\x00" * 100)

        context = build_diff_context(root_dir=g.path, diff_range="HEAD")
        paths = _fragment_paths(context)
        assert not any("image.png" in p for p in paths)

    def test_untracked_gitignored_file_excluded(self, git_repo_with_base):
        g = git_repo_with_base
        g.add_file(".gitignore", "secret.py\n")
        g.commit("add gitignore")
        (g.path / "secret.py").write_text("API_KEY = 'leaked'\n", encoding="utf-8")  # pragma: allowlist secret

        context = build_diff_context(root_dir=g.path, diff_range="HEAD")
        paths = _fragment_paths(context)
        assert not any("secret.py" in p for p in paths)


class TestRangeDiffExcludesUntracked:
    def test_range_diff_ignores_untracked(self, git_repo_with_base):
        g = git_repo_with_base
        g.add_file("tracked_change.py", "def changed(): pass\n")
        g.commit("tracked change")
        (g.path / "untracked_after.py").write_text("def stray(): pass\n", encoding="utf-8")

        context = build_diff_context(root_dir=g.path, diff_range="HEAD~1..HEAD")
        paths = _fragment_paths(context)
        assert any("tracked_change.py" in p for p in paths)
        assert not any("untracked_after.py" in p for p in paths)


class TestStagedNewFilesIncluded:
    def test_staged_new_file_in_single_ref_diff(self, git_repo_with_base):
        g = git_repo_with_base
        g.add_file("staged.py", "def staged_func(): return True\n")
        g.stage_file("staged.py")

        context = build_diff_context(root_dir=g.path, diff_range="HEAD")
        paths = _fragment_paths(context)
        assert any("staged.py" in p for p in paths)

    def test_staged_and_untracked_both_included(self, git_repo_with_base):
        g = git_repo_with_base
        g.add_file("staged.py", "def staged(): pass\n")
        g.stage_file("staged.py")
        (g.path / "untracked.py").write_text("def untracked(): pass\n", encoding="utf-8")

        context = build_diff_context(root_dir=g.path, diff_range="HEAD")
        paths = _fragment_paths(context)
        assert any("staged.py" in p for p in paths)
        assert any("untracked.py" in p for p in paths)
