import subprocess
import sys
from pathlib import Path

import pytest

import gitrepo
import indexing
import indexshare
import runners
from corpus import Repo, load_corpus

# Indexes every .go file of the snapshot when the module file exists and writes an empty index otherwise: what scip-go
# does on a commit from before Go modules (nothing indexed).
FAKE_INDEXER = """
import pathlib, sys
files = [p.as_posix() for p in sorted(pathlib.Path(".").rglob("*.go"))] if pathlib.Path("go.mod").exists() else []
documents = b"".join(b"\\x12" + bytes([len(d)]) + d for d in (b"\\x0a" + bytes([len(f.encode())]) + f.encode() for f in files))
pathlib.Path(sys.argv[1]).write_bytes(documents)
"""


def commit(path: Path, files: dict[str, str], message: str) -> None:
    env = {**gitrepo.git_env(), "GIT_AUTHOR_DATE": "2026-01-01T00:00:00Z", "GIT_COMMITTER_DATE": "2026-01-01T00:00:00Z"}
    for name, text in files.items():
        (path / name).write_text(text)
    subprocess.run(["git", "add", "-A"], cwd=path, env=env, check=True)
    identity = ["-c", "user.name=t", "-c", "user.email=t@example.invalid"]
    subprocess.run(["git", *identity, "commit", "-q", "-m", message], cwd=path, env=env, check=True)


def test_the_commits_that_predate_the_module_file_are_the_excluded_share_and_the_cause_is_named(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    repo_dir = tmp_path / "repo"
    repo_dir.mkdir()
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=repo_dir, env=gitrepo.git_env(), check=True)
    commit(repo_dir, {"README.md": "x"}, "root")
    for n in range(3):
        commit(repo_dir, {"a.go": f"package a // {n}\n"}, f"before modules {n}")
    commit(repo_dir, {"go.mod": "module t\n", "a.go": "package a // 3\n"}, "adopt modules")
    for n in range(4, 6):
        commit(repo_dir, {"a.go": f"package a // {n}\n"}, f"after modules {n}")
    head = gitrepo.git("rev-parse", "HEAD", cwd=repo_dir).strip()
    cache = tmp_path / "cache"
    monkeypatch.setattr(indexing, "CACHE", cache)
    monkeypatch.setattr(indexshare, "CACHE", cache)
    monkeypatch.setitem(
        runners.PLANNERS, "go", lambda ctx: runners.Plan([sys.executable, "-c", FAKE_INDEXER, str(ctx.out)], {"PATH": ""})
    )

    corpus = load_corpus()
    repo = Repo("t", "", "go", head, "MIT")

    rows = indexshare.index_sample(corpus, repo, repo_dir / ".git", count=6)

    assert len(rows) == 6
    assert {(row["seed"], row["quota"]) for row in rows} == {(corpus.sampling.seed, corpus.quota(repo))}
    excluded = [row for row in rows if row["excluded"]]
    assert len(excluded) == 4  # three before modules, and the one that adds go.mod (its parent has none)
    assert all("nothing indexed" in row["causes"].values() for row in excluded)
    assert indexshare.summary("t", rows).startswith("t\texcluded 4/6 = 66.7%\tabove 20%")


def test_the_commits_help_names_the_default_and_the_quota_that_measure_draws(capsys: pytest.CaptureFixture[str]):
    with pytest.raises(SystemExit):
        indexshare.main(["--help"])

    help_text = " ".join(capsys.readouterr().out.split())
    assert "default 24" in help_text
    assert "quota" in help_text
    assert "draws 40" not in help_text


def test_a_round_installs_dependencies_as_measure_does_and_can_drop_each_tree_once_indexed(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    repo_dir = tmp_path / "repo"
    repo_dir.mkdir()
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=repo_dir, env=gitrepo.git_env(), check=True)
    for n in range(3):
        commit(repo_dir, {"go.mod": "module t\n", "a.go": f"package a // {n}\n"}, f"c{n}")
    head = gitrepo.git("rev-parse", "HEAD", cwd=repo_dir).strip()
    cache = tmp_path / "cache"
    monkeypatch.setattr(indexing, "CACHE", cache)
    monkeypatch.setattr(indexshare, "CACHE", cache)
    seen: list[bool] = []

    def plan(ctx: runners.Context) -> runners.Plan:
        seen.append(ctx.install)
        return runners.Plan([sys.executable, "-c", FAKE_INDEXER, str(ctx.out)], {"PATH": ""})

    monkeypatch.setitem(runners.PLANNERS, "go", plan)
    repo = Repo("t", "", "go", head, "MIT")
    corpus = load_corpus()

    indexshare.index_sample(corpus, repo, repo_dir / ".git", count=2)
    assert seen and all(seen)
    assert any((cache / "trees" / "t").iterdir())

    seen.clear()
    monkeypatch.setattr(indexing, "CACHE", tmp_path / "second")
    monkeypatch.setattr(indexshare, "CACHE", tmp_path / "second")
    indexshare.index_sample(corpus, repo, repo_dir / ".git", count=2, install=False, drop_trees=True)
    assert seen and not any(seen)
    assert not any((tmp_path / "second" / "trees" / "t").iterdir())
