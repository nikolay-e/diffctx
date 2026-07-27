from __future__ import annotations

import json
import subprocess
from pathlib import Path

import pytest
import yaml

import diffctx
from diffctx._native.pipeline import get_raw_diff_text

from .conftest import run_diffctx_subprocess
from .framework.pygit2_backend import Pygit2Repo

KEYFILE_MARKER = "RAWDIFF_KEYFILE_MARKER"
LOCK_MARKER = "RAWDIFF_LOCK_MARKER"
IGNORED_MARKER = "RAWDIFF_IGNORED_MARKER"


def _build_repo(tmp_path: Path) -> Pygit2Repo:
    repo = Pygit2Repo(tmp_path / "repo")
    repo.add_file(".diffctx/ignore", "notes.txt\n")
    repo.add_file("src/calc.py", "def add(a, b):\n    return a + b\n\n\ndef mul(a, b):\n    return a * b\n")
    repo.add_file("src/app.py", "from src.calc import add\n\n\ndef total(xs):\n    return sum(add(x, 0) for x in xs)\n")
    repo.add_file("uv.lock", f'checksum = "{LOCK_MARKER}_before"\n')
    repo.add_file("server.pem", f"-----BEGIN CERTIFICATE-----\n{KEYFILE_MARKER}_before\n")
    repo.add_file("notes.txt", f"{IGNORED_MARKER}_before\n")
    repo.commit("initial")

    repo.add_file("src/calc.py", "def add(a, b):\n    return a + b + 0\n\n\ndef mul(a, b):\n    return a * b\n")
    repo.add_file("uv.lock", f'checksum = "{LOCK_MARKER}_after"\n')
    repo.add_file("server.pem", f"-----BEGIN CERTIFICATE-----\n{KEYFILE_MARKER}_after\n")
    repo.add_file("notes.txt", f"{IGNORED_MARKER}_after\n")
    repo.commit("tweak add, bump lock, rotate key")
    return repo


def _run(repo: Pygit2Repo, *extra: str) -> subprocess.CompletedProcess[str]:
    result = run_diffctx_subprocess([str(repo.path), "--diff", "HEAD~1", *extra], cwd=repo.path)
    assert result.returncode == 0, result.stderr
    return result


class TestRawDiffBundle:
    def test_markdown_embeds_fenced_diff_before_fragments(self, tmp_path):
        repo = _build_repo(tmp_path)

        rendered = _run(repo, "-f", "md", "--with-raw-diff").stdout

        assert "## Raw diff" in rendered
        assert "```diff\n" in rendered
        assert "-    return a + b\n" in rendered
        assert "+    return a + b + 0\n" in rendered
        assert rendered.index("## Raw diff") < rendered.index("src/calc.py:1-2")

    def test_yaml_block_scalar_round_trips_to_the_patch(self, tmp_path):
        repo = _build_repo(tmp_path)

        parsed = yaml.safe_load(_run(repo, "-f", "yaml", "--with-raw-diff").stdout)

        raw_diff = parsed["raw_diff"]
        assert raw_diff.startswith("diff --git ")
        assert "+    return a + b + 0" in raw_diff.splitlines()
        assert raw_diff.endswith("\n")

    def test_json_carries_the_patch_as_a_string_field(self, tmp_path):
        repo = _build_repo(tmp_path)

        parsed = json.loads(_run(repo, "-f", "json", "--with-raw-diff").stdout)

        keys = list(parsed)
        assert isinstance(parsed["raw_diff"], str)
        assert keys.index("raw_diff") < keys.index("fragments")

    def test_text_indents_the_patch_under_a_raw_diff_header(self, tmp_path):
        repo = _build_repo(tmp_path)

        rendered = _run(repo, "-f", "txt", "--with-raw-diff").stdout

        assert "  raw diff:\n" in rendered
        assert "    diff --git a/src/calc.py b/src/calc.py\n" in rendered

    @pytest.mark.parametrize("output_format", ["md", "yaml", "json", "txt"])
    def test_absent_without_the_flag(self, tmp_path, output_format):
        repo = _build_repo(tmp_path)

        rendered = _run(repo, "-f", output_format).stdout

        assert "raw_diff" not in rendered
        assert "Raw diff" not in rendered
        assert "diff --git" not in rendered

    def test_selection_is_identical_with_and_without_the_flag(self, tmp_path):
        repo = _build_repo(tmp_path)

        without = json.loads(_run(repo, "-f", "json").stdout)
        with_raw = json.loads(_run(repo, "-f", "json", "--with-raw-diff").stdout)

        with_raw.pop("raw_diff")
        without.pop("latency", None)
        with_raw.pop("latency", None)
        assert with_raw == without

    @pytest.mark.parametrize("output_format", ["md", "yaml", "json", "txt"])
    def test_patch_omits_secret_lock_and_ignored_sections(self, tmp_path, output_format):
        repo = _build_repo(tmp_path)

        rendered = _run(repo, "-f", output_format, "--with-raw-diff").stdout

        assert KEYFILE_MARKER not in rendered
        assert LOCK_MARKER not in rendered
        assert "server.pem" not in rendered
        for excluded in ("server.pem", "uv.lock", "notes.txt"):
            assert f"diff --git a/{excluded}" not in rendered

    def test_patch_carries_no_secret_lock_or_ignored_content(self, tmp_path):
        repo = _build_repo(tmp_path)

        raw_diff = yaml.safe_load(_run(repo, "-f", "yaml", "--with-raw-diff").stdout)["raw_diff"]

        assert KEYFILE_MARKER not in raw_diff
        assert LOCK_MARKER not in raw_diff
        assert IGNORED_MARKER not in raw_diff
        assert "src/calc.py" in raw_diff

    def test_stderr_reports_the_patch_share_of_the_token_summary(self, tmp_path):
        repo = _build_repo(tmp_path)

        stderr = _run(repo, "-f", "md", "--with-raw-diff").stderr

        assert "tokens (o200k_base)" in stderr
        assert "raw diff (not charged to --budget)" in stderr

    def test_budget_caps_selection_only(self, tmp_path):
        repo = _build_repo(tmp_path)

        capped = json.loads(_run(repo, "-f", "json", "--budget", "0", "--with-raw-diff").stdout)

        assert capped.get("fragments", []) == []
        assert "+    return a + b + 0" in capped["raw_diff"]

    def test_warns_when_used_outside_diff_mode(self, tmp_path):
        repo = _build_repo(tmp_path)

        result = run_diffctx_subprocess([str(repo.path), "--with-raw-diff"], cwd=repo.path)

        assert result.returncode == 0
        assert "--with-raw-diff" in result.stderr
        assert "ignored without --diff" in result.stderr

    def test_patch_keeps_sections_that_carry_no_hunks(self, tmp_path):
        repo = Pygit2Repo(tmp_path / "repo")
        repo.add_file("old_name.py", "def a():\n    return 1\n")
        repo.add_file("keep.py", "def c():\n    return 3\n")
        repo.add_file_binary("img.png", b"\x89PNG\x00\x01binary\x00")
        repo.commit("initial")
        (repo.path / "old_name.py").rename(repo.path / "new_name.py")
        repo.add_file("keep.py", "def c():\n    return 33\n")
        repo.add_file_binary("img.png", b"\x89PNG\x00\x02binary2\x00")
        repo.commit("rename, edit, touch binary")

        raw_diff = yaml.safe_load(_run(repo, "-f", "yaml", "--with-raw-diff").stdout)["raw_diff"]

        assert "rename to new_name.py" in raw_diff
        assert "Binary files a/img.png and b/img.png differ" in raw_diff

    def test_python_api_attaches_the_patch(self, tmp_path):
        repo = _build_repo(tmp_path)

        result = diffctx.build_diff_context(root_dir=repo.path, diff_range="HEAD~1", with_raw_diff=True)

        assert result["raw_diff"] == get_raw_diff_text(repo.path, "HEAD~1")
        assert KEYFILE_MARKER not in result["raw_diff"]
