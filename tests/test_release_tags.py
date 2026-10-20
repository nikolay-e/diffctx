import os
import subprocess
import sys
from pathlib import Path

import pytest

SCRIPT = Path(__file__).parent.parent / ".github/scripts/release-tags.sh"

pytestmark = pytest.mark.skipif(
    sys.platform == "win32",
    reason="bash on a Windows runner may resolve to WSL's; channel-parity runs the script on Linux",
)


def _git(repo: Path, *args: str) -> str:
    return subprocess.run(
        ["git", *args], cwd=repo, check=True, capture_output=True, text=True, env=_isolated_git_env()
    ).stdout.strip()


def _isolated_git_env() -> dict[str, str]:
    env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
    env.update(
        GIT_CONFIG_GLOBAL=os.devnull,
        GIT_CONFIG_NOSYSTEM="1",
        GIT_AUTHOR_NAME="t",
        GIT_AUTHOR_EMAIL="t@example.com",
        GIT_COMMITTER_NAME="t",
        GIT_COMMITTER_EMAIL="t@example.com",
    )
    return env


def _commit(repo: Path, message: str) -> None:
    (repo / "f.txt").write_text(message, encoding="utf-8")
    _git(repo, "add", "f.txt")
    _git(repo, "commit", "-q", "-m", message)


def _off_main(repo: Path) -> list[str]:
    out = subprocess.run(
        ["bash", str(SCRIPT), "main"], cwd=repo, check=True, capture_output=True, text=True, env=_isolated_git_env()
    ).stdout
    return out.split()


def test_a_release_tag_moved_onto_a_sibling_of_main_is_named(tmp_path):
    """The 1.16.0 and 1.18.3 shape (#500): the tag names a commit with main's
    parent and content that main itself never reached."""
    repo = tmp_path / "repo"
    repo.mkdir()
    _git(repo, "init", "-q", "-b", "main")
    _commit(repo, "base")
    _git(repo, "tag", "-a", "v1.0.0", "-m", "release")
    _git(repo, "checkout", "-q", "-b", "release")
    _commit(repo, "release 1.1.0")
    _git(repo, "tag", "-a", "v1.1.0", "-m", "release")
    _git(repo, "tag", "v1.2.0-lightweight")
    _git(repo, "checkout", "-q", "main")
    _commit(repo, "release 1.1.0 again")
    _git(repo, "tag", "not-a-release")
    _git(repo, "branch", "-D", "release")

    assert _off_main(repo) == ["v1.1.0", "v1.2.0-lightweight"]


def test_tags_on_main_are_not_named(tmp_path):
    repo = tmp_path / "repo"
    repo.mkdir()
    _git(repo, "init", "-q", "-b", "main")
    _commit(repo, "one")
    _git(repo, "tag", "-a", "v1.0.0", "-m", "release")
    _commit(repo, "two")
    _git(repo, "tag", "v1.1.0")

    assert _off_main(repo) == []
