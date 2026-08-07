# tests/test_gitignore_negation_diff.py
"""#193: a gitignore negation (`!file`) must not exclude the file from --diff.

`git check-ignore -v` prints a record when the last matching pattern is a
negation; the path is then explicitly NOT ignored. Treating any record as an
exclusion inverted the meaning: a repository that un-ignores `SECURITY.md`
had that newly added file silently dropped from the review context.
"""

from __future__ import annotations

import diffctx
from tests.framework.pygit2_backend import Pygit2Repo


def test_a_negated_path_stays_in_the_diff_context(tmp_path):
    repo = Pygit2Repo(tmp_path / "repo")
    repo.add_file("base.txt", "a\n")
    repo.commit("base")

    repo.add_file(".gitignore", "*.tmp\n!NEWDOC.md\n")
    repo.add_file("NEWDOC.md", "# New doc\n\nSome text here.\n")
    repo.add_file("base.txt", "a\nb\n")
    repo.commit("add md")

    result = diffctx.build_diff_context(root_dir=repo.path, diff_range="HEAD~1")

    assert "NEWDOC.md" in (result.get("changed_files") or [])
    assert not result.get("ignored_changes"), "nothing here is ignored"
    assert not result.get("policy_excluded_count")
    rendered = diffctx.to_yaml(result)
    assert "Some text here." in rendered


def test_a_diffctx_ignore_negation_reincludes_the_path(tmp_path):
    repo = Pygit2Repo(tmp_path / "repo")
    repo.add_file("keep.md", "kept original\n")
    repo.add_file("drop.md", "dropped original\n")
    repo.commit("base")

    repo.add_file(".diffctx/ignore", "*.md\n!keep.md\n")
    repo.add_file("keep.md", "kept KEEP_CHANGED\n")
    repo.add_file("drop.md", "dropped DROP_CHANGED\n")
    repo.commit("change both")

    result = diffctx.build_diff_context(root_dir=repo.path, diff_range="HEAD~1")
    rendered = diffctx.to_yaml(result)

    # The negation is the user re-including a path in the policy's own terms.
    assert "KEEP_CHANGED" in rendered
    # The positive policy pattern still withholds, as a count (#188/#85).
    assert "DROP_CHANGED" not in rendered
    assert result.get("policy_excluded_count") == 1
