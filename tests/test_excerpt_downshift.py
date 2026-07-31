"""End-to-end coverage for the excerpt downshift (#149, closing #105 / #107).

The user-visible contract: a small change inside a large fragment produces a
window around the change, not the whole fragment. Every scenario drives the real
CLI against a real git repo, because the defect these tests pin was never
visible at unit level — it lived in how the selector, the post-passes and the
renderer combined.
"""

from __future__ import annotations

import json

import pytest

from tests.framework.pygit2_backend import Pygit2Repo

from .conftest import run_diffctx_subprocess

EXIT_OK = 0


def _fragments(repo_path, *extra_args):
    result = run_diffctx_subprocess(
        [".", "--diff", "HEAD~1..HEAD", "-f", "json", "-q", *extra_args],
        cwd=repo_path,
    )
    assert result.returncode == EXIT_OK, result.stderr
    return json.loads(result.stdout).get("fragments", [])


def _changed(fragments):
    return [f for f in fragments if f.get("role") == "changed"]


def _span(fragment):
    start, _, end = fragment["lines"].partition("-")
    return int(end or start) - int(start) + 1


@pytest.fixture
def flat_shell_repo(tmp_path):
    """#107: a bash script with no function boundaries is one flat fragment."""
    repo = Pygit2Repo(tmp_path / "flat_shell")
    body = "\n".join(f"echo step_{i}" for i in range(1, 121))
    repo.add_file("script.sh", f"#!/bin/bash\nset -euo pipefail\n{body}\n")
    repo.commit("initial")
    repo.add_file("script.sh", f"#!/bin/bash\nset -euo pipefail\n{body}\n".replace("echo step_61", "echo step_61_changed"))
    repo.commit("one line changed")
    return repo


@pytest.fixture
def generic_config_repo(tmp_path):
    """#105: a file with no tree-sitter grammar becomes a whole-file chunk."""
    repo = Pygit2Repo(tmp_path / "generic_config")
    body = "\n".join(f"set(VAR_{i} value_{i})" for i in range(1, 401))
    repo.add_file("CMakeLists.txt", f"cmake_minimum_required(VERSION 3.20)\nproject(demo)\n{body}\n")
    repo.commit("initial")
    repo.add_file(
        "CMakeLists.txt",
        f"cmake_minimum_required(VERSION 3.20)\nproject(demo)\n{body}\n".replace(
            "set(VAR_200 value_200)", "set(VAR_200 value_200_changed)"
        ),
    )
    repo.commit("one line changed")
    return repo


@pytest.fixture
def large_function_repo(tmp_path):
    """Forgejo issue 2: a multi-line hunk promotes the core to the enclosing
    definition, which then used to ship whole."""
    repo = Pygit2Repo(tmp_path / "large_function")
    body = "\n".join(f"  const step_{i} = {i};" for i in range(1, 240))
    repo.add_file("crawl.js", f"function crawlPage(url) {{\n{body}\n  return url;\n}}\n")
    repo.commit("initial")
    repo.add_file(
        "crawl.js",
        f"function crawlPage(url) {{\n{body}\n  return url;\n}}\n".replace(
            "  const step_120 = 120;",
            "  const step_120 = 120;\n  const injected_a = 1;\n  const injected_b = 2;",
        ),
    )
    repo.commit("two lines added inside the function")
    return repo


class TestDownshiftEmitsAWindowNotTheWholeFragment:
    def test_flat_shell_script_emits_a_window(self, flat_shell_repo):
        changed = _changed(_fragments(flat_shell_repo.path))
        assert changed, "the change disappeared from the output"
        assert sum(_span(f) for f in changed) < 40, (
            "a one-line change still ships most of a 122-line script: " f"{[(f['lines'], f['kind']) for f in changed]}"
        )

    def test_file_without_a_grammar_emits_a_window(self, generic_config_repo):
        changed = _changed(_fragments(generic_config_repo.path))
        assert changed, "the change disappeared from the output"
        assert sum(_span(f) for f in changed) < 40, (
            "a one-line change still ships most of a 402-line file: " f"{[(f['lines'], f['kind']) for f in changed]}"
        )

    def test_large_function_emits_a_window(self, large_function_repo):
        changed = _changed(_fragments(large_function_repo.path))
        assert changed, "the change disappeared from the output"
        assert sum(_span(f) for f in changed) < 40, (
            "two changed lines still ship a 240-line function: " f"{[(f['lines'], f['kind']) for f in changed]}"
        )

    def test_the_window_actually_contains_the_changed_lines(self, flat_shell_repo):
        """A window that omits the change would be smaller *and* useless — the
        signature fallback does exactly that, which is why it is not the
        substitute for a mostly-unchanged core."""
        fragments = _fragments(flat_shell_repo.path)
        joined = "\n".join(f.get("content") or "" for f in fragments)
        assert "step_61_changed" in joined, "the downshifted window dropped the change itself"

    def test_downshifted_output_still_marks_the_change(self, generic_config_repo):
        """The excerpt's id is a synthetic span, absent from `core_ids`. Before
        `render` was aligned with `locate`, downshifting stripped the `changed`
        role and the reader could not tell what the diff touched."""
        fragments = _fragments(generic_config_repo.path)
        assert _changed(fragments), (
            "no fragment carries role=changed after the downshift: "
            f"{[(f['lines'], f['kind'], f.get('role')) for f in fragments]}"
        )

    def test_granularity_no_longer_depends_on_leftover_budget(self, generic_config_repo):
        """The old rule consulted the excerpt only when the budget forced it, so
        the same file shipped whole at a generous budget and excerpted at a tight
        one. The changed span must now be the same either way."""
        generous = sum(_span(f) for f in _changed(_fragments(generic_config_repo.path, "--budget", "8000")))
        tight = sum(_span(f) for f in _changed(_fragments(generic_config_repo.path, "--budget", "600")))
        assert generous == tight, f"budget still decides granularity: {generous} vs {tight} lines"


class TestDownshiftLeavesGenuinelyChangedFragmentsAlone:
    def test_a_change_spread_across_a_fragment_keeps_the_fragment(self, tmp_path):
        """Downshifting here would only lose context: there is no unchanged bulk
        to trim, so the fragment must survive intact."""
        repo = Pygit2Repo(tmp_path / "rewritten")
        before = "\n".join(f"  const keep_{i} = {i};" for i in range(1, 25))
        repo.add_file("app.js", f"function rewritten() {{\n{before}\n}}\n")
        repo.commit("initial")
        after = "\n".join(f"  const keep_{i} = {i * 10};" for i in range(1, 25))
        repo.add_file("app.js", f"function rewritten() {{\n{after}\n}}\n")
        repo.commit("every line changed")

        changed = _changed(_fragments(repo.path))
        assert changed, "the rewrite disappeared from the output"
        assert max(_span(f) for f in changed) >= 20, (
            "a fully rewritten fragment was trimmed to a window: " f"{[(f['lines'], f['kind']) for f in changed]}"
        )

    def test_a_small_fragment_is_never_downshifted(self, tmp_path):
        """Below the minimum parent size there is nothing to save, and a window
        would just add a synthetic span for no benefit."""
        repo = Pygit2Repo(tmp_path / "small")
        repo.add_file("tiny.py", "def add(a, b):\n    return a + b\n")
        repo.commit("initial")
        repo.add_file("tiny.py", "def add(a, b):\n    return a + b + 0\n")
        repo.commit("one line changed")

        changed = _changed(_fragments(repo.path))
        assert changed
        assert all(
            f["kind"] != "excerpt" for f in changed
        ), f"a tiny fragment was needlessly excerpted: {[(f['lines'], f['kind']) for f in changed]}"
