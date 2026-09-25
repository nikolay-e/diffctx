# tests/test_excluded_changes_visibility.py
"""#188: a changed file the tool withholds must be visible as withheld.

A reviewer reading diff-context output cannot tell a file the diff never
touched from one the tool filtered; the recorded failure is a "no tests"
review verdict against a change whose tests the tool had dropped. gitignore
exclusions are listed by path; `.diffctx/ignore` is a declared
confidentiality policy (#85), so its exclusions surface as a count only.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

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
    assert "withheld by exclusion policy" in md2
    assert "notes.md" not in md2


def test_omitted_changed_files_are_disclosed_in_md_and_text(tmp_path):
    repo = Pygit2Repo(tmp_path / "repo")
    for i in range(6):
        repo.add_file(f"src/mod_{i}.py", f"def fn_{i}(x):\n    return x + {i}\n")
    repo.commit("initial")
    for i in range(6):
        repo.add_file(f"src/mod_{i}.py", f"def fn_{i}(x):\n    return x + {i} + 1\n")
    repo.commit("bump all")

    result = diffctx.build_diff_context(root_dir=repo.path, diff_range="HEAD~1", budget_tokens=25)
    represented = {f["path"] for f in result.get("fragments") or []}
    omitted = [p for p in result.get("changed_files") or [] if p not in represented]
    assert omitted, "budget 25 over 6 changed files must force omission"

    # One list, omitted entries marked (#241): the second full copy of the
    # changed-file list cost ~1k tokens of paths the reader already had.
    md = diffctx.to_markdown(result)
    marked = {line.split("`")[1] for line in md.splitlines() if line.startswith("- ") and "omitted" in line}
    assert marked == set(omitted)
    assert "\u201comitted\u201d = no fragment of this file" in md
    assert md.count("**Changed files:**") == 1
    assert "not represented in the output" not in md, "the second, duplicate list came back"

    txt = diffctx.to_text(result)
    txt_marked = {line.strip().removesuffix(" (omitted)") for line in txt.splitlines() if line.endswith(" (omitted)")}
    assert txt_marked == set(omitted)


def test_a_changed_file_with_no_fragments_is_not_blamed_on_the_budget(tmp_path):
    """#290: a changed file the fragmenter yields nothing for was reported as
    budget-omitted with a degraded status, next to run-wide limits it never hit."""
    repo = Pygit2Repo(tmp_path / "repo")
    repo.add_file("src/app.py", "def a():\n    return 1\n")
    repo.commit("initial")
    repo.add_file("assets/icon.svg", '<svg xmlns="http://www.w3.org/2000/svg"><circle r="4"/></svg>\n')
    repo.commit("add icon")

    result = diffctx.build_diff_context(root_dir=repo.path, diff_range="HEAD~1")
    assert [c["path"] for c in result["changes"] if c.get("no_fragments")] == ["assets/icon.svg"]
    assert "evidence_budget_exceeded" not in (result.get("coverage") or {}).get("limit_reasons", [])
    assert (result.get("coverage") or {}).get("status") != "degraded"

    md = diffctx.to_markdown(result)
    assert "- `assets/icon.svg` — no fragments" in md
    assert "\u201cno fragments\u201d = the file yielded nothing to select" in md
    assert "(budget/selection)" not in md
    assert "assets/icon.svg (no fragments)" in diffctx.to_text(result)

    located = run_diffctx_subprocess([".", "--diff", "HEAD~1", "--mode", "locate", "-f", "json", "-q"], cwd=repo.path)
    coverage = json.loads(located.stdout).get("coverage") or {}
    assert coverage.get("no_fragment_changed_files") == ["assets/icon.svg"]
    assert "assets/icon.svg" not in coverage.get("unrepresented_changed_files", [])


def test_fully_represented_output_has_no_omission_footer(tmp_path):
    repo = Pygit2Repo(tmp_path / "repo")
    repo.add_file("src/app.py", "def a():\n    pass\n")
    repo.commit("initial")
    repo.add_file("src/app.py", "def a():\n    return 1\n")
    repo.commit("change")

    result = diffctx.build_diff_context(root_dir=repo.path, diff_range="HEAD~1")
    md = diffctx.to_markdown(result)
    assert "omitted" not in md


def _git(repo: Path, *args: str) -> None:
    env = {k: v for k, v in os.environ.items() if k not in ("GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE")}
    subprocess.run(["git", "-C", str(repo), *args], check=True, capture_output=True, env=env)


def _plain_repo(path: Path) -> Path:
    path.mkdir(parents=True)
    _git(path, "init", "-q", "-b", "main")
    _git(path, "config", "user.email", "test@test.com")
    _git(path, "config", "user.name", "Test")
    _git(path, "config", "commit.gpgsign", "false")
    return path


def _commit_all(repo: Path, message: str) -> None:
    _git(repo, "add", "-A")
    _git(repo, "commit", "-q", "-m", message)


def _assert_listed_unrepresented(repo: Path, diff_args: list[str], path: str) -> str:
    result = run_diffctx_subprocess([".", *diff_args, "-f", "json", "-q"], cwd=repo)
    doc = json.loads(result.stdout)
    assert doc["changed_files"] == [path], result.stderr
    assert doc["changes"] == [{"path": path, "class": doc["changes"][0]["class"], "represented": False, "no_fragments": True}]
    return str(result.stderr)


@pytest.mark.skipif(sys.platform == "win32", reason="NTFS has no executable bit for git to record")
def test_a_mode_only_change_is_listed_unrepresented(tmp_path: Path) -> None:
    repo = _plain_repo(tmp_path / "repo")
    (repo / "tool.py").write_text("def run():\n    return 1\n", encoding="utf-8")
    _commit_all(repo, "base")
    (repo / "tool.py").chmod(0o755)

    _assert_listed_unrepresented(repo, ["--diff"], "tool.py")


def test_a_submodule_pointer_bump_is_listed_unrepresented(tmp_path: Path) -> None:
    sub = _plain_repo(tmp_path / "sub")
    (sub / "lib.py").write_text("X = 1\n", encoding="utf-8")
    _commit_all(sub, "sub base")
    repo = _plain_repo(tmp_path / "repo")
    (repo / "app.py").write_text("def a():\n    return 1\n", encoding="utf-8")
    _commit_all(repo, "base")
    _git(repo, "-c", "protocol.file.allow=always", "submodule", "add", "-q", sub.resolve().as_uri(), "vendor/sub")
    _commit_all(repo, "add submodule")
    (sub / "lib.py").write_text("X = 2\n", encoding="utf-8")
    _commit_all(sub, "sub bump")
    _git(repo / "vendor" / "sub", "-c", "protocol.file.allow=always", "pull", "-q", "origin", "main")
    _commit_all(repo, "bump submodule")

    assert "matches HEAD" not in _assert_listed_unrepresented(repo, ["--diff", "HEAD~1..HEAD"], "vendor/sub")


def test_a_diff_attribute_hides_the_hunks_but_not_the_change(tmp_path: Path) -> None:
    repo = _plain_repo(tmp_path / "repo")
    (repo / ".gitattributes").write_text("*.py -diff\n", encoding="utf-8")
    (repo / "app.py").write_text("def a():\n    return 1\n", encoding="utf-8")
    _commit_all(repo, "base")
    (repo / "app.py").write_text("def a():\n    return 2\n", encoding="utf-8")
    _commit_all(repo, "edit")

    assert "matches HEAD" not in _assert_listed_unrepresented(repo, ["--diff", "HEAD~1..HEAD"], "app.py")
