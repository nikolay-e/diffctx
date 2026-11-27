import subprocess

from .conftest import scrub_repo_locating_env


def test_a_fixture_repository_is_built_whatever_the_environment_names(tmp_path, monkeypatch):
    monkeypatch.setenv("GIT_INDEX_FILE", "/nonexistent/index")
    monkeypatch.setenv("GIT_DIR", "/nonexistent/git")
    scrub_repo_locating_env()
    (tmp_path / "a.txt").write_text("a\n")
    for args in (["init", "-q"], ["add", "-A"], ["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "c"]):
        subprocess.run(["git", *args], cwd=tmp_path, check=True, capture_output=True)
    assert (tmp_path / ".git").is_dir()
