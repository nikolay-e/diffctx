from __future__ import annotations

import subprocess

import pytest

from treemapper.diffctx import build_diff_context


@pytest.fixture
def git_repo_with_base(tmp_path):
    repo = tmp_path / "repo"
    repo.mkdir()
    subprocess.run(["git", "init"], cwd=repo, capture_output=True, check=True)
    subprocess.run(["git", "config", "user.email", "test@test.com"], cwd=repo, capture_output=True, check=True)
    subprocess.run(["git", "config", "user.name", "Test"], cwd=repo, capture_output=True, check=True)

    (repo / "base.py").write_text("def base(): return 1\n", encoding="utf-8")
    subprocess.run(["git", "add", "-A"], cwd=repo, capture_output=True, check=True)
    subprocess.run(["git", "commit", "-m", "initial"], cwd=repo, capture_output=True, check=True)
    return repo


def _fragment_paths(context: dict) -> list[str]:
    return [f["path"] for f in context.get("fragments", [])]


def _all_content(context: dict) -> str:
    return "\n".join(f.get("content", "") for f in context.get("fragments", []))


class TestUntrackedFilesIncluded:
    def test_untracked_file_appears_in_single_ref_diff(self, git_repo_with_base):
        repo = git_repo_with_base
        (repo / "untracked.py").write_text("def new_func(): return 42\n", encoding="utf-8")

        context = build_diff_context(root_dir=repo, diff_range="HEAD")
        paths = _fragment_paths(context)
        assert any("untracked.py" in p for p in paths)

    def test_untracked_file_content_in_output(self, git_repo_with_base):
        repo = git_repo_with_base
        (repo / "untracked.py").write_text("def secret_sauce(): return 999\n", encoding="utf-8")

        context = build_diff_context(root_dir=repo, diff_range="HEAD")
        content = _all_content(context)
        assert "secret_sauce" in content

    def test_multiple_untracked_files(self, git_repo_with_base):
        repo = git_repo_with_base
        (repo / "new_a.py").write_text("def func_a(): pass\n", encoding="utf-8")
        (repo / "new_b.py").write_text("def func_b(): pass\n", encoding="utf-8")

        context = build_diff_context(root_dir=repo, diff_range="HEAD")
        paths = _fragment_paths(context)
        assert any("new_a.py" in p for p in paths)
        assert any("new_b.py" in p for p in paths)

    def test_untracked_in_subdirectory(self, git_repo_with_base):
        repo = git_repo_with_base
        (repo / "pkg").mkdir()
        (repo / "pkg" / "module.py").write_text("class Widget:\n    pass\n", encoding="utf-8")

        context = build_diff_context(root_dir=repo, diff_range="HEAD")
        paths = _fragment_paths(context)
        assert any("pkg/module.py" in p for p in paths)

    def test_mixed_tracked_and_untracked_changes(self, git_repo_with_base):
        repo = git_repo_with_base
        (repo / "base.py").write_text("def base(): return 2\n", encoding="utf-8")
        (repo / "brand_new.py").write_text("def brand_new(): pass\n", encoding="utf-8")

        context = build_diff_context(root_dir=repo, diff_range="HEAD")
        paths = _fragment_paths(context)
        assert any("base.py" in p for p in paths)
        assert any("brand_new.py" in p for p in paths)

    def test_untracked_binary_file_excluded(self, git_repo_with_base):
        repo = git_repo_with_base
        (repo / "image.png").write_bytes(b"\x89PNG\r\n\x1a\n" + b"\x00" * 100)

        context = build_diff_context(root_dir=repo, diff_range="HEAD")
        paths = _fragment_paths(context)
        assert not any("image.png" in p for p in paths)

    def test_untracked_gitignored_file_excluded(self, git_repo_with_base):
        repo = git_repo_with_base
        (repo / ".gitignore").write_text("secret.py\n", encoding="utf-8")
        subprocess.run(["git", "add", ".gitignore"], cwd=repo, capture_output=True, check=True)
        subprocess.run(["git", "commit", "-m", "add gitignore"], cwd=repo, capture_output=True, check=True)
        (repo / "secret.py").write_text("API_KEY = 'leaked'\n", encoding="utf-8")  # pragma: allowlist secret

        context = build_diff_context(root_dir=repo, diff_range="HEAD")
        paths = _fragment_paths(context)
        assert not any("secret.py" in p for p in paths)


class TestRangeDiffExcludesUntracked:
    def test_range_diff_ignores_untracked(self, git_repo_with_base):
        repo = git_repo_with_base
        (repo / "tracked_change.py").write_text("def changed(): pass\n", encoding="utf-8")
        subprocess.run(["git", "add", "-A"], cwd=repo, capture_output=True, check=True)
        subprocess.run(["git", "commit", "-m", "tracked change"], cwd=repo, capture_output=True, check=True)
        (repo / "untracked_after.py").write_text("def stray(): pass\n", encoding="utf-8")

        context = build_diff_context(root_dir=repo, diff_range="HEAD~1..HEAD")
        paths = _fragment_paths(context)
        assert any("tracked_change.py" in p for p in paths)
        assert not any("untracked_after.py" in p for p in paths)


class TestStagedNewFilesIncluded:
    def test_staged_new_file_in_single_ref_diff(self, git_repo_with_base):
        repo = git_repo_with_base
        (repo / "staged.py").write_text("def staged_func(): return True\n", encoding="utf-8")
        subprocess.run(["git", "add", "staged.py"], cwd=repo, capture_output=True, check=True)

        context = build_diff_context(root_dir=repo, diff_range="HEAD")
        paths = _fragment_paths(context)
        assert any("staged.py" in p for p in paths)

    def test_staged_and_untracked_both_included(self, git_repo_with_base):
        repo = git_repo_with_base
        (repo / "staged.py").write_text("def staged(): pass\n", encoding="utf-8")
        subprocess.run(["git", "add", "staged.py"], cwd=repo, capture_output=True, check=True)
        (repo / "untracked.py").write_text("def untracked(): pass\n", encoding="utf-8")

        context = build_diff_context(root_dir=repo, diff_range="HEAD")
        paths = _fragment_paths(context)
        assert any("staged.py" in p for p in paths)
        assert any("untracked.py" in p for p in paths)
