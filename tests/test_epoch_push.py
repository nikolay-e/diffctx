import os
import subprocess
from pathlib import Path

import pytest

SCRIPT = Path(__file__).resolve().parents[1] / "scripts/epoch-push.sh"


@pytest.mark.skipif(os.name == "nt", reason="epoch delivery runs under POSIX bash")
@pytest.mark.parametrize("contribution", ["body", "author", "coauthor"])
def test_epoch_preserves_commit_evidence_and_contributors(tmp_path, contribution):
    env = {**os.environ, "GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_NOSYSTEM": "1"}

    def git(*args, **kwargs):
        return subprocess.check_output(["git", *args], cwd=tmp_path, env=env, text=True, **kwargs).strip()

    git("init", "--bare", "remote.git")
    git("init", "-b", "main", "work")
    tmp_path = tmp_path / "work"
    git("remote", "add", "origin", "../remote.git")
    git("config", "user.name", "Maintainer")
    git("config", "user.email", "maintainer@example.com")
    git("commit", "--allow-empty", "-m", "initial")
    git("push", "origin", "main")
    git("switch", "-c", "next")
    git("commit", "--allow-empty", "-m", "fix: first")
    message = "fix: second\n\nPreserve the measured gate and its reason.\n\nRefs #123"
    args = ["commit", "--allow-empty", "-m", message]
    if contribution == "coauthor":
        args += ["-m", "Co-authored-by: Contributor <contributor@example.com>"]
    if contribution == "author":
        args += ["--author=Contributor <contributor@example.com>"]
    git(*args)
    before = git("rev-parse", "HEAD")
    result = subprocess.run(["bash", str(SCRIPT)], cwd=tmp_path, env=env, text=True, capture_output=True)
    if contribution == "body":
        assert result.returncode == 0, result.stderr
        assert message in git("log", "-1", "--format=%B")
        assert git("rev-list", "--count", "origin/main..HEAD") == "1"
        assert git("rev-parse", "HEAD") == git("rev-parse", "origin/next")
    else:
        assert result.returncode != 0, result.stdout
        assert git("rev-parse", "HEAD") == before
        assert not git("branch", "-r", "--list", "origin/next")
