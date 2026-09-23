from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path
from typing import Any

import pytest

from diffctx._native import GitError, build_diff_context
from tests.conftest import run_diffctx_subprocess

EXIT_OK = 0
EXIT_ENVIRONMENT = 3
EXIT_EMPTY_DIFF = 4

_SCRUBBED = ("GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE")


def _env() -> dict[str, str]:
    return {k: v for k, v in os.environ.items() if k not in _SCRUBBED}


def _git(repo: Path, *args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    return subprocess.run(["git", "-C", str(repo), *args], check=check, capture_output=True, text=True, env=_env())


def _init(repo: Path) -> Path:
    repo.mkdir(parents=True, exist_ok=True)
    _git(repo, "init", "-q", "-b", "main")
    _git(repo, "config", "user.email", "test@test.com")
    _git(repo, "config", "user.name", "Test")
    _git(repo, "config", "commit.gpgsign", "false")
    return repo


def _commit(repo: Path, message: str) -> None:
    _git(repo, "add", "-A")
    _git(repo, "commit", "-q", "-m", message)


def _write(repo: Path, rel: str, content: str | bytes) -> None:
    target = repo / rel
    target.parent.mkdir(parents=True, exist_ok=True)
    if isinstance(content, bytes):
        target.write_bytes(content)
    else:
        target.write_text(content, encoding="utf-8", newline="\n")


def _run(repo: Path, *args: str) -> subprocess.CompletedProcess[str]:
    result: subprocess.CompletedProcess[str] = run_diffctx_subprocess([".", *args, "-q"], cwd=repo)
    return result


def _json(result: subprocess.CompletedProcess[str]) -> dict[str, Any]:
    assert "Traceback" not in result.stderr, result.stderr
    doc: dict[str, Any] = json.loads(result.stdout)
    return doc


def _three_commit_repo(tmp_path: Path) -> Path:
    repo = _init(tmp_path / "origin")
    for i in range(3):
        _write(repo, "app.py", f"def f():\n    return {i}\n")
        _commit(repo, f"c{i}")
    return repo


def test_a_shallow_clone_names_the_missing_history_then_deepening_fixes_it(tmp_path: Path) -> None:
    origin = _three_commit_repo(tmp_path)
    clone = tmp_path / "clone"
    subprocess.run(
        ["git", "clone", "-q", "--depth", "1", origin.resolve().as_uri(), str(clone)],
        check=True,
        capture_output=True,
        env=_env(),
    )

    shallow = _run(clone, "--diff", "HEAD~1", "-f", "json")
    assert shallow.returncode == EXIT_ENVIRONMENT, shallow.stderr
    assert "fetch-depth" in shallow.stderr
    assert "git log --oneline" not in shallow.stderr
    with pytest.raises(GitError) as raised:
        build_diff_context(clone, "HEAD~1")
    assert "shallow clone" in str(raised.value)
    assert "--deepen" in str(raised.value)
    assert "fetch-depth" in str(raised.value)

    _git(clone, "fetch", "-q", "--deepen=1")
    deepened = _run(clone, "--diff", "HEAD~1", "-f", "json")
    assert deepened.returncode == EXIT_OK, deepened.stderr
    assert _json(deepened)["changed_files"] == ["app.py"]


def test_an_untracked_nested_repository_is_not_a_changed_file(tmp_path: Path) -> None:
    repo = _init(tmp_path / "repo")
    _write(repo, "app.py", "def f():\n    return 1\n")
    _commit(repo, "base")
    nested = _init(repo / "vendor" / "nested")
    _write(nested, "lib.py", "def g():\n    return 1\n")
    _commit(nested, "nested base")
    _write(repo, "app.py", "def f():\n    return 2\n")

    doc = _json(_run(repo, "--diff", "-f", "json"))
    assert doc["changed_files"] == ["app.py"]


def test_committed_conflict_markers_are_ordinary_text(tmp_path: Path) -> None:
    repo = _init(tmp_path / "repo")
    _write(repo, "app.py", "def f():\n    return 1\n")
    _commit(repo, "base")
    _write(repo, "app.py", "def f():\n<<<<<<< HEAD\n    return 1\n=======\n    return 2\n>>>>>>> other\n")
    _commit(repo, "commit the markers")

    result = _run(repo, "--diff", "HEAD~1..HEAD", "-f", "json")
    assert result.returncode in (EXIT_OK, EXIT_EMPTY_DIFF), result.stderr
    assert _json(result)["changed_files"] == ["app.py"]


def test_full_with_raw_diff_carries_both_the_patch_and_the_fragments(tmp_path: Path) -> None:
    repo = _init(tmp_path / "repo")
    _write(repo, "app.py", "def f():\n    return 1\n")
    _commit(repo, "base")
    _write(repo, "app.py", "def f():\n    return 2\n")
    _commit(repo, "edit")

    doc = _json(_run(repo, "--diff", "HEAD~1..HEAD", "--full", "--with-raw-diff", "-f", "json"))
    assert "+    return 2" in doc["raw_diff"]
    assert doc["fragment_count"] >= 1
    assert any("return 2" in f.get("content", "") for f in doc["fragments"])


def test_a_duration_window_works_in_locate_mode(tmp_path: Path) -> None:
    repo = _init(tmp_path / "repo")
    old = "2020-01-01T00:00:00+00:00"
    _write(repo, "ancient.py", "def ancient():\n    return 0\n")
    _git(repo, "add", "-A")
    subprocess.run(
        ["git", "-C", str(repo), "commit", "-q", "-m", "ancient"],
        check=True,
        capture_output=True,
        env={**_env(), "GIT_AUTHOR_DATE": old, "GIT_COMMITTER_DATE": old},
    )
    _write(repo, "recent.py", "def recent():\n    return 1\n")
    _commit(repo, "recent")

    result = _run(repo, "--diff", "24h", "--mode", "locate")
    assert result.returncode == EXIT_OK, result.stderr
    doc = _json(result)
    assert doc["schema"] == "diffctx.locate.v1"
    assert doc["changed_files"] == ["recent.py"]
    assert any(i.get("role") == "changed" and i["path"] == "recent.py" for i in doc["items"])


def test_a_linked_worktree_runs_from_a_subdirectory(tmp_path: Path) -> None:
    repo = _init(tmp_path / "repo")
    _write(repo, "pkg/mod.py", "def f():\n    return 1\n")
    _commit(repo, "base")
    linked = tmp_path / "linked"
    _git(repo, "worktree", "add", "-q", "-b", "feature", str(linked))
    _write(linked, "pkg/mod.py", "def f():\n    return 2\n")

    result = run_diffctx_subprocess([".", "--diff", "-f", "json", "-q"], cwd=linked / "pkg")
    assert result.returncode == EXIT_OK, result.stderr
    assert _json(result)["changed_files"] == ["pkg/mod.py"]


def test_a_conflicted_merge_is_reported_not_crashed_on(tmp_path: Path) -> None:
    repo = _init(tmp_path / "repo")
    _write(repo, "app.py", "def f():\n    return 1\n")
    _commit(repo, "base")
    _git(repo, "checkout", "-q", "-b", "other")
    _write(repo, "app.py", "def f():\n    return 2\n")
    _commit(repo, "other")
    _git(repo, "checkout", "-q", "main")
    _write(repo, "app.py", "def f():\n    return 3\n")
    _commit(repo, "main")
    assert _git(repo, "merge", "other", check=False).returncode != 0

    result = _run(repo, "--diff", "-f", "json")
    assert result.returncode in (EXIT_OK, EXIT_EMPTY_DIFF), result.stderr
    assert _json(result)["changed_files"] == ["app.py"]


def test_an_intent_to_add_file_is_a_changed_file(tmp_path: Path) -> None:
    repo = _init(tmp_path / "repo")
    _write(repo, "app.py", "def f():\n    return 1\n")
    _commit(repo, "base")
    _write(repo, "new.py", "def g():\n    return 1\n")
    _git(repo, "add", "-N", "new.py")

    doc = _json(_run(repo, "--diff", "-f", "json"))
    assert doc["changed_files"] == ["new.py"]


def test_an_index_only_removal_reads_as_the_deletion_the_next_commit_makes(tmp_path: Path) -> None:
    repo = _init(tmp_path / "repo")
    _write(repo, "app.py", "def f():\n    return 1\n")
    _write(repo, "tracked.py", "def t():\n    return 1\n")
    _commit(repo, "base")
    _git(repo, "rm", "-q", "--cached", "tracked.py")

    result = _run(repo, "--diff", "-f", "json")
    assert (repo / "tracked.py").is_file()
    assert result.returncode == EXIT_OK, result.stderr
    doc = _json(result)
    assert doc["deleted_files"] == ["tracked.py"]
    assert "changed_files" not in doc


def _fragment_set(doc: dict[str, Any]) -> set[tuple[str, str, str | None]]:
    return {(f["path"], f["lines"], f.get("symbol")) for f in doc["fragments"]}


def test_a_latin1_file_fragments_the_same_before_and_after_commit(tmp_path: Path) -> None:
    repo = _init(tmp_path / "repo")
    _write(repo, "cafe.py", b"def caf():\n    return 1\n")
    _commit(repo, "base")
    _write(repo, "cafe.py", b"def caf\xe9():\n    return '\xe9'\n")

    uncommitted = _json(_run(repo, "--diff", "-f", "json"))
    _commit(repo, "latin-1")
    committed = _json(_run(repo, "--diff", "HEAD~1..HEAD", "-f", "json"))

    assert _fragment_set(uncommitted) == _fragment_set(committed)
    assert _fragment_set(committed)
    for doc in (uncommitted, committed):
        assert "non_utf8_content" in doc["coverage"]["limit_reasons"]
        assert doc["coverage"]["lossy_files"] == ["cafe.py"]


def test_a_byte_order_mark_does_not_reach_the_fragment(tmp_path: Path) -> None:
    repo = _init(tmp_path / "repo")
    _write(repo, "util.py", "def helper():\n    return 1\n")
    _write(repo, "app.py", b"\xef\xbb\xbfimport util\n\n\ndef main():\n    return util.helper()\n")
    _commit(repo, "base")
    _write(repo, "app.py", b"\xef\xbb\xbfimport util\n\n\ndef main():\n    return util.helper() + 1\n")
    _commit(repo, "edit")

    doc = _json(_run(repo, "--diff", "HEAD~1..HEAD", "-f", "json"))
    app = [f for f in doc["fragments"] if f["path"] == "app.py"]
    assert app
    assert all(not f.get("content", "").startswith("﻿") for f in app)


@pytest.fixture(autouse=True)
def _no_ambient_git_dir(monkeypatch: pytest.MonkeyPatch) -> None:
    for name in _SCRUBBED:
        monkeypatch.delenv(name, raising=False)


@pytest.mark.skipif(sys.platform == "win32", reason="the executable bit is not a file mode on Windows")
def test_a_mode_only_change_is_not_called_a_clean_tree(tmp_path: Path) -> None:
    repo = _init(tmp_path / "repo")
    _write(repo, "run.sh", "echo hi\n")
    _commit(repo, "base")
    (repo / "run.sh").chmod(0o755)

    result = _run(repo, "--diff", "-f", "json")
    assert result.returncode == EXIT_EMPTY_DIFF, result.stderr
    assert "matches HEAD" not in result.stderr
    assert "no text hunks" in result.stderr
    assert "run.sh" in _json(result)["changed_files"]


@pytest.mark.parametrize("scope", [["pkg"], ["pkg/a.py"], ["pkg", "lib/c.py"]])
def test_path_arguments_narrow_the_diff(tmp_path: Path, scope: list[str]) -> None:
    repo = _init(tmp_path / "repo")
    for rel in ("pkg/a.py", "other/b.py", "lib/c.py"):
        _write(repo, rel, "def f():\n    return 1\n")
    _commit(repo, "base")
    for rel in ("pkg/a.py", "other/b.py", "lib/c.py"):
        _write(repo, rel, "def f():\n    return 2\n")

    changed = _json(run_diffctx_subprocess([*scope, "--diff", "-f", "json", "-q"], cwd=repo))["changed_files"]
    expected = {"pkg": ["pkg/a.py"], "pkg/a.py": ["pkg/a.py"], "lib/c.py": ["lib/c.py"]}
    assert sorted(changed) == sorted(p for s in scope for p in expected[s])


def test_locate_honours_path_arguments(tmp_path: Path) -> None:
    repo = _init(tmp_path / "repo")
    for rel in ("pkg/a.py", "other/b.py"):
        _write(repo, rel, "def f():\n    return 1\n")
    _commit(repo, "base")
    for rel in ("pkg/a.py", "other/b.py"):
        _write(repo, rel, "def f():\n    return 2\n")

    doc = _json(run_diffctx_subprocess(["pkg", "--diff", "--mode", "locate", "-q"], cwd=repo))
    assert {item["path"] for item in doc["items"]} == {"pkg/a.py"}


@pytest.mark.parametrize(("commits", "total", "heading"), [(25, 25, "> newest 20 of 25 commits:"), (3, None, "> 3 commits:")])
def test_a_long_range_says_how_many_commits_the_list_left_out(
    tmp_path: Path, commits: int, total: int | None, heading: str
) -> None:
    repo = _init(tmp_path / "repo")
    _write(repo, "app.py", "def f():\n    return 0\n")
    _commit(repo, "base")
    for i in range(1, commits + 1):
        _write(repo, "app.py", f"def f():\n    return {i}\n")
        _commit(repo, f"step {i}")

    doc = _json(_run(repo, "--diff", f"HEAD~{commits}..HEAD", "-f", "json"))
    assert len(doc["commit_messages"]) == min(commits, 20)
    assert doc.get("commit_count") == total
    locate = _json(_run(repo, "--diff", f"HEAD~{commits}..HEAD", "--mode", "locate"))
    assert locate.get("commit_count") == total
    md = run_diffctx_subprocess([".", "--diff", f"HEAD~{commits}..HEAD", "-q"], cwd=repo).stdout
    assert heading in md
