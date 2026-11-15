import json
import os
import shutil
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


UNPUBLISHED = Path(__file__).parent.parent / ".github/scripts/unpublished-release-tags.sh"


def _unpublished(repo: Path, tmp_path: Path, releases: list[dict], gh_fails: bool = False) -> subprocess.CompletedProcess:
    """The script against a stand-in `gh` that serves `releases` and applies
    the `--jq` filter the script passes, with the real jq."""
    stub = tmp_path / "bin"
    stub.mkdir()
    listing = tmp_path / "releases.json"
    listing.write_text(json.dumps(releases), encoding="utf-8")
    gh = stub / "gh"
    gh.write_text(
        "#!/usr/bin/env bash\n"
        + (
            "echo 'HTTP 401' >&2; exit 1\n"
            if gh_fails
            else 'while [[ $1 != --jq ]]; do shift; done; exec jq -r "$2" "$GH_RELEASES"\n'
        ),
        encoding="utf-8",
    )
    gh.chmod(0o755)
    env = _isolated_git_env()
    env.update(PATH=f"{stub}{os.pathsep}{os.environ['PATH']}", GH_RELEASES=str(listing), REPO="owner/repo")
    return subprocess.run(["bash", str(UNPUBLISHED)], cwd=repo, capture_output=True, text=True, env=env, check=False)


@pytest.mark.skipif(shutil.which("jq") is None, reason="the stand-in gh applies the script's filter with jq")
class TestUnpublishedReleaseTags:
    def _tagged(self, tmp_path: Path, *tags: str) -> Path:
        repo = tmp_path / "repo"
        repo.mkdir()
        _git(repo, "init", "-q", "-b", "main")
        _commit(repo, "one")
        for tag in tags:
            _git(repo, "tag", tag)
        return repo

    def test_a_draft_and_a_tag_without_a_release_are_named_a_published_one_is_not(self, tmp_path):
        repo = self._tagged(tmp_path, "v1.0.0", "v1.1.0", "v1.2.0", "not-a-release")
        releases = [{"tagName": "v1.0.0", "isDraft": False}, {"tagName": "v1.1.0", "isDraft": True}]
        result = _unpublished(repo, tmp_path, releases)
        assert result.returncode == 0, result.stderr
        assert result.stdout.split() == ["v1.1.0", "v1.2.0"]

    def test_every_tag_published_prints_nothing(self, tmp_path):
        repo = self._tagged(tmp_path, "v1.0.0")
        result = _unpublished(repo, tmp_path, [{"tagName": "v1.0.0", "isDraft": False}])
        assert (result.returncode, result.stdout) == (0, "")

    def test_a_failing_gh_fails_the_script_instead_of_passing_it(self, tmp_path):
        repo = self._tagged(tmp_path, "v1.0.0")
        result = _unpublished(repo, tmp_path, [], gh_fails=True)
        assert result.returncode != 0
