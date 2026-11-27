import os
import subprocess
import sys
from pathlib import Path

import pytest

GATE = Path(__file__).resolve().parents[1] / "scripts/git-env-gate.sh"


@pytest.mark.skipif(os.name == "nt", reason="the gate runs under POSIX bash")
def test_suites_that_build_repositories_leave_the_enclosing_repository_alone():
    """A pre-commit hook in a linked worktree names the shared repository in GIT_DIR, and a
    `git init` that inherited it set core.bare = true there (fj#199). The Rust half of the
    gate needs a build: run `scripts/git-env-gate.sh rust` by hand."""
    result = subprocess.run(
        ["bash", str(GATE), "python"],
        env={**os.environ, "GATE_PYTHON": sys.executable},
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == 0, result.stderr[-2000:] + result.stdout[-2000:]
