# tests/test_excluded_changes_visibility.py
"""#188: a changed file the tool withholds must be visible as withheld.

A reviewer reading diff-context output cannot tell a file the diff never
touched from one the tool filtered; the recorded failure is a "no tests"
review verdict against a change whose tests the tool had dropped. gitignore
exclusions are listed by path; `.diffctx/ignore` is a declared
confidentiality policy (#85), so its exclusions surface as a count only.
"""

from __future__ import annotations

import diffctx
from tests.conftest import run_diffctx_subprocess
from tests.framework.pygit2_backend import Pygit2Repo


def _two_commit_repo(tmp_path, ignore_file: str, ignore_content: str) -> Pygit2Repo:
    """The ignore rule arrives after the file is tracked — the shape real
    repos have (#188's reporter had tracked tests under an ignored path), and
    the only shape this fixture can build: staging respects ignore rules, so
    a file ignored from birth never enters the diff at all."""
    repo = Pygit2Repo(tmp_path / "repo")
    repo.add_file("src/app.py", "def a():\n    pass\n")
    repo.add_file("notes.md", "# QA notes\noriginal\n")
    repo.commit("initial")

    repo.add_file(ignore_file, ignore_content)
    repo.add_file("src/app.py", "def a():\n    return 1\n")
    repo.add_file("notes.md", "# QA notes\nchanged alongside code\n")
    repo.commit("change code and notes")
    return repo


def test_gitignore_excluded_changed_file_is_listed_by_path(tmp_path):
    repo = _two_commit_repo(tmp_path, ".gitignore", "*.md\n")
    result = diffctx.build_diff_context(root_dir=repo.path, diff_range="HEAD~1")
    rendered = diffctx.to_yaml(result)

    assert result.get("ignored_changes") == ["notes.md"]
    assert "ignored_changes" in rendered
    assert "notes.md" in rendered
    # The excluded file's content must still stay out.
    assert "changed alongside code" not in rendered


def test_policy_excluded_changed_file_surfaces_as_count_without_the_path(tmp_path):
    repo = _two_commit_repo(tmp_path, ".diffctx/ignore", "*.md\n")
    result = diffctx.build_diff_context(root_dir=repo.path, diff_range="HEAD~1")
    rendered = diffctx.to_yaml(result)

    assert result.get("policy_excluded_count") == 1
    assert "policy_excluded_count" in rendered
    # The policy's whole point: neither the path nor the content reappears.
    assert "notes.md" not in rendered
    assert "changed alongside code" not in rendered


def test_policy_excluded_count_is_files_not_hunks(tmp_path):
    repo = Pygit2Repo(tmp_path / "repo")
    repo.add_file("src/app.py", "def a():\n    pass\n")
    middle = "\n".join(f"line {i}" for i in range(40))
    repo.add_file("notes.md", f"# QA notes\n{middle}\ntail\n")
    repo.commit("initial")

    repo.add_file(".diffctx/ignore", "*.md\n")
    lines = [f"line {i}" for i in range(40)]
    lines[0] = "line 0 changed"
    lines[39] = "line 39 changed"
    repo.add_file("notes.md", "# QA notes\n" + "\n".join(lines) + "\ntail\n")
    repo.add_file("src/app.py", "def a():\n    return 1\n")
    repo.commit("two separated edits inside the withheld file")

    result = diffctx.build_diff_context(root_dir=repo.path, diff_range="HEAD~1")
    assert result.get("policy_excluded_count") == 1


def test_exclusion_only_change_still_renders_the_withheld_notice(tmp_path):
    repo = Pygit2Repo(tmp_path / "repo")
    repo.add_file("src/app.py", "def a():\n    pass\n")
    repo.add_file("notes.md", "# QA notes\noriginal\n")
    repo.commit("initial")

    repo.add_file(".gitignore", "*.md\n")
    repo.commit("ignore rule arrives")

    repo.add_file("notes.md", "# QA notes\nonly the excluded file changed\n")
    repo.commit("touch only the excluded file")

    result = diffctx.build_diff_context(root_dir=repo.path, diff_range="HEAD~1")
    assert result.get("ignored_changes") == ["notes.md"]
    for rendered in (
        diffctx.to_yaml(result),
        diffctx.to_markdown(result),
        diffctx.to_text(result),
    ):
        assert "notes.md" in rendered
        assert "only the excluded file changed" not in rendered

    proc = run_diffctx_subprocess([str(repo.path), "--diff", "HEAD~1"], cwd=str(repo.path))
    assert proc.returncode == 0, proc.stderr
    assert "notes.md" in proc.stdout
    assert "no semantic context" not in proc.stderr


def test_markdown_render_carries_both_exclusion_notes(tmp_path):
    repo = _two_commit_repo(tmp_path, ".gitignore", "*.md\n")
    result = diffctx.build_diff_context(root_dir=repo.path, diff_range="HEAD~1")
    md = diffctx.to_markdown(result)
    assert "Changed but excluded by ignore rules" in md
    assert "notes.md" in md

    repo2 = _two_commit_repo(tmp_path / "p2", ".diffctx/ignore", "*.md\n")
    result2 = diffctx.build_diff_context(root_dir=repo2.path, diff_range="HEAD~1")
    md2 = diffctx.to_markdown(result2)
    assert "withheld by `.diffctx/ignore`" in md2
    assert "notes.md" not in md2
