from __future__ import annotations

import json

from tests.conftest import run_diffctx_subprocess


def test_files_only_invocation_roots_at_their_common_parent(tmp_path):
    project = tmp_path / "project"
    (project / "pkg").mkdir(parents=True)
    (project / "pkg" / "a.py").write_text("A = 1\n")
    (project / "pkg" / "b.py").write_text("B = 2\n")
    elsewhere = tmp_path / "elsewhere"
    elsewhere.mkdir()

    # Run from an unrelated directory and name only files: the tree used to
    # be rooted at the cwd, so ignore files and relative paths were resolved
    # against `elsewhere`.
    proc = run_diffctx_subprocess(
        [str(project / "pkg" / "a.py"), str(project / "pkg" / "b.py"), "-f", "json"],
        cwd=str(elsewhere),
    )
    assert proc.returncode == 0, proc.stderr
    assert json.loads(proc.stdout)["name"] == "pkg"
