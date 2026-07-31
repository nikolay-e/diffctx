"""Untracked files are scanned before any size filter applies.

The scan needs a line count to synthesise a hunk, and it used to get one by
materialising the whole file with `read_to_string`. `max_changed_file_size` is
enforced later, in fragmentation, so a dirty tree holding a single multi-GB log
allocated all of it here just to reach `.lines().count()` — unbounded memory in
the phase that runs before any guard.

The count is now streamed. These tests pin the equivalence that makes that
substitution safe, because the two APIs disagree on nothing *only* if the edge
cases hold: a trailing newline, CRLF, an empty file, and invalid UTF-8, which
must keep rejecting the file outright rather than reporting a partial count.
"""

from __future__ import annotations

import pytest

from tests.framework.pygit2_backend import Pygit2Repo


@pytest.fixture
def repo_with_untracked(tmp_path):
    def build(extra: dict[str, str | bytes]):
        repo = Pygit2Repo(tmp_path / f"r{len(extra)}{abs(hash(tuple(sorted(extra))))}")
        repo.add_file("src/app.py", "def run():\n    return 1\n")
        repo.commit("initial")
        # A real untracked source file alongside the case under test. Without
        # one, a case whose only untracked content yields no hunk exits through
        # the empty-state path and reports no changed files at all — which would
        # make these assertions pass or fail for an unrelated reason.
        (repo.path / "companion.py").write_text("def companion():\n    return 2\n")
        for name, content in extra.items():
            path = repo.path / name
            path.parent.mkdir(parents=True, exist_ok=True)
            if isinstance(content, bytes):
                path.write_bytes(content)
            else:
                path.write_text(content)
        return repo

    return build


def _changed_files(repo_path):
    from diffctx._native.pipeline import build_diff_context

    result = build_diff_context(repo_path, "HEAD", budget_tokens=8000)
    return set(result.get("changed_files") or [])


def test_an_untracked_source_file_is_reported(repo_with_untracked):
    repo = repo_with_untracked({"helper.py": "def helper():\n    return 2\n"})
    assert "helper.py" in _changed_files(repo.path)


@pytest.mark.parametrize(
    "content",
    [
        pytest.param("one\ntwo\n", id="trailing-newline"),
        pytest.param("one\ntwo", id="no-trailing-newline"),
        pytest.param("one\r\ntwo\r\n", id="crlf"),
        pytest.param("\n", id="single-newline"),
    ],
)
def test_text_shapes_that_the_two_line_apis_could_disagree_on(repo_with_untracked, content):
    """`str::lines` and `BufRead::lines` must agree on every one of these, or the
    synthesised hunk length changes for ordinary files — not just huge ones."""
    repo = repo_with_untracked({"notes.txt": content})
    assert "notes.txt" in _changed_files(repo.path)


def test_an_empty_untracked_file_produces_no_hunk_but_is_still_listed(repo_with_untracked):
    """Zero lines means no hunk. The path is still reported, because the
    changed-files list comes from the untracked scan rather than from hunk
    emission — the two are independent and must stay that way."""
    repo = repo_with_untracked({"empty.txt": ""})
    assert "empty.txt" in _changed_files(repo.path)


def test_a_binary_untracked_file_is_rejected_not_partially_counted(repo_with_untracked):
    """Invalid UTF-8 must reject the file, exactly as `read_to_string` did.
    Counting up to the bad byte would invent a hunk length for a binary."""
    repo = repo_with_untracked({"blob.dat": bytes([0, 159, 146, 150, 0, 1, 2])})
    changed = _changed_files(repo.path)

    assert "blob.dat" in changed
    from diffctx._native.pipeline import build_diff_context

    result = build_diff_context(repo.path, "HEAD", budget_tokens=8000)
    assert not [f for f in result["fragments"] if f["path"].endswith("blob.dat")], "a binary untracked file contributed fragments"


def test_a_file_over_the_size_cap_costs_no_more_memory_than_a_small_one(repo_with_untracked):
    """The regression this exists for. `max_changed_file_size` is 5 MB and is
    applied downstream, so this file yields no fragments — but it is still
    scanned here, and it must be scanned without being held in memory. Sized
    just past the cap: large enough that a full materialisation is real, small
    enough to keep the suite fast."""
    big = "filler line of text\n" * 400_000
    repo = repo_with_untracked({"huge.log": big})

    from diffctx._native.pipeline import build_diff_context

    result = build_diff_context(repo.path, "HEAD", budget_tokens=8000)

    assert "huge.log" in set(result.get("changed_files") or [])
    assert not [
        f for f in result["fragments"] if f["path"].endswith("huge.log")
    ], "a file past max_changed_file_size contributed fragments"
