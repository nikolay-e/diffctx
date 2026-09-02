from __future__ import annotations

import os
import stat
import subprocess
import sys
from pathlib import Path

import pytest

from tests.conftest import run_diffctx_subprocess
from tests.framework.pygit2_backend import Pygit2Repo

pytest.importorskip("mcp")

from diffctx.mcp.fetch import fetch_fragments
from diffctx.mcp.security import validate_dir_path, validate_repo_path


def _repo(tmp_path):
    repo = Pygit2Repo(tmp_path / "repo")
    repo.add_file("src/a.py", "def a():\n    return 1\n")
    repo.commit("base")
    repo.add_file("src/a.py", "def a():\n    return 2\n")
    repo.commit("change")
    return repo


def test_a_diff_ref_cannot_smuggle_a_git_option_into_show(tmp_path):
    """`rev` is whatever followed `..`, and with fragment_ids set the engine never
    validates it; `git show --output=<file> <path>` would make a read-only tool
    write to disk."""
    repo = _repo(tmp_path)
    target = tmp_path / "pwned"

    text = fetch_fragments(Path(repo.path), f"HEAD..--output={target}", ["src/a.py"], 1_000_000)

    # `git show --output=<target> src/a.py` writes to `<target>:src/a.py` — any
    # file starting with the planted name is the tool writing to disk.
    assert not list(tmp_path.glob("pwned*")), "git show wrote a file on behalf of a read-only tool"
    assert "return 2" in text or "Not found" in text


def test_a_fragment_id_past_the_end_of_the_file_is_refused_not_clamped(tmp_path):
    repo = _repo(tmp_path)

    text = fetch_fragments(Path(repo.path), "HEAD~1..HEAD", ["src/a.py:50-60"], 1_000_000)

    assert "Not found" in text
    assert "has 2 lines" in text
    assert "return 2" not in text


def test_an_explicit_secret_file_is_withheld_like_the_walk_withholds_it(tmp_path):
    """`diffctx id_rsa` and `diffctx '**/*'` used to print what `diffctx .` refuses."""
    root = tmp_path / "proj"
    root.mkdir()
    (root / "id_rsa").write_text("PRIVATE_KEY_LEAK\n")
    (root / "app.py").write_text("APP_OK = 1\n")

    explicit = run_diffctx_subprocess(["id_rsa", "app.py", "-f", "txt"], cwd=str(root))
    assert explicit.returncode == 0, explicit.stderr
    assert "PRIVATE_KEY_LEAK" not in explicit.stdout
    assert "APP_OK" in explicit.stdout
    assert "withheld by the secret-path policy" in explicit.stderr

    globbed = run_diffctx_subprocess(["*", "-f", "txt"], cwd=str(root))
    assert globbed.returncode == 0, globbed.stderr
    assert "APP_OK" in globbed.stdout
    assert "PRIVATE_KEY_LEAK" not in globbed.stdout


def test_the_allow_list_is_checked_before_the_filesystem_is(tmp_path, monkeypatch):
    """SECURITY.md: a path outside the roots is rejected before any filesystem
    call — so the refusal must not depend on whether the path exists."""
    allowed = tmp_path / "allowed"
    allowed.mkdir()
    monkeypatch.setenv("DIFFCTX_ALLOWED_PATHS", str(allowed))

    outside_missing = str(tmp_path / "nope" / "missing")
    outside_existing = _repo(tmp_path).path

    for bad in (outside_missing, outside_existing):
        with pytest.raises(ValueError) as refused:
            validate_repo_path(bad)
        assert "outside the roots" in str(refused.value), bad
        with pytest.raises(ValueError) as refused_dir:
            validate_dir_path(bad)
        assert "outside the roots" in str(refused_dir.value), bad


def test_a_broken_pipe_exits_141_in_tree_mode(tmp_path):
    root = tmp_path / "proj"
    root.mkdir()
    for i in range(300):
        (root / f"m{i}.py").write_text(f"def f{i}():\n    return {i}\n" * 40)

    script = f"{sys.executable} -m diffctx . -f md | head -c 1 >/dev/null; " 'echo "exit=${PIPESTATUS[0]}"'
    proc = subprocess.run(["bash", "-c", script], cwd=root, capture_output=True, text=True)

    assert "exit=141" in proc.stdout, proc.stdout + proc.stderr


def test_an_output_file_takes_the_mode_the_umask_gives_a_new_file(tmp_path):
    root = tmp_path / "proj"
    root.mkdir()
    (root / "app.py").write_text("APP_OK = 1\n")
    out = tmp_path / "out.md"

    old = os.umask(0o022)
    try:
        result = run_diffctx_subprocess([".", "-o", str(out)], cwd=str(root))
    finally:
        os.umask(old)

    assert result.returncode == 0, result.stderr
    assert stat.S_IMODE(out.stat().st_mode) == 0o644
