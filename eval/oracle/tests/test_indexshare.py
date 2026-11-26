import dataclasses
import json
import os
import re
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


def test_a_failed_index_build_leaves_its_log_under_out_and_the_round_says_where(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
):
    repo_dir = tmp_path / "repo"
    repo_dir.mkdir()
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=repo_dir, env=gitrepo.git_env(), check=True)
    for n in range(2):
        commit(repo_dir, {"go.mod": "module t\n", "a.go": f"package a // {n}\n"}, f"c{n}")
    head = gitrepo.git("rev-parse", "HEAD", cwd=repo_dir).strip()
    cache = tmp_path / "cache"
    monkeypatch.setattr(indexing, "CACHE", cache)
    monkeypatch.setattr(indexshare, "CACHE", cache)
    script = "echo What went wrong >&2; i=0; while [ $i -lt 100 ]; do echo padding-$i; i=$((i+1)); done; exit 1"
    monkeypatch.setitem(runners.PLANNERS, "go", lambda ctx: runners.Plan(["sh", "-c", script], {"PATH": os.environ["PATH"]}))
    logs = tmp_path / "out" / "stderr"

    rows = indexshare.index_sample(load_corpus(), Repo("t", "", "go", head, "MIT"), repo_dir / ".git", count=1, logs=logs)

    assert rows[0]["causes"] == {"base": "indexer exited 1", "head": "indexer exited 1"}
    kept = logs / f"t-{head[:12]}-head.log"
    assert "What went wrong" in kept.read_text() and "padding-99" in kept.read_text()
    assert f"the index build's log is kept at {kept}" in capsys.readouterr().err


def fast_failing_repository(tmp_path: Path, monkeypatch: pytest.MonkeyPatch, script: str, **limits: int):
    """Four commits of a Go module whose indexer is `script` (it appends a line to `runs` each time it starts)."""
    repo_dir = tmp_path / "repo"
    repo_dir.mkdir()
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=repo_dir, env=gitrepo.git_env(), check=True)
    for n in range(4):
        commit(repo_dir, {"go.mod": "module t\n", "a.go": f"package a // {n}\n"}, f"c{n}")
    head = gitrepo.git("rev-parse", "HEAD", cwd=repo_dir).strip()
    cache = tmp_path / "cache"
    runs = tmp_path / "runs"
    for module in (indexing, indexshare):
        monkeypatch.setattr(module, "CACHE", cache)
    monkeypatch.setattr(gitrepo, "mirror", lambda url, dest, sha: repo_dir / ".git")
    corpus = dataclasses.replace(load_corpus(), **limits)
    monkeypatch.setattr(indexshare, "load_corpus", lambda: corpus)
    plan = runners.Plan(["sh", "-c", f'echo run >> "{runs}"; {script}'], {"PATH": os.environ["PATH"]})
    monkeypatch.setitem(runners.PLANNERS, "go", lambda ctx: plan)
    return repo_dir, head, cache, runs, corpus


def only_failure_log(cache: Path) -> str:
    (log,) = cache.glob(f"index/t/*/*/{indexing.FAILURE_LOG}")
    return log.read_text()


def test_an_indexer_that_times_out_stops_its_repository_and_keeps_what_it_printed(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
):
    _, head, cache, runs, _ = fast_failing_repository(
        tmp_path, monkeypatch, "echo compiling module; exec sleep 60", index_seconds=1
    )

    status = indexshare.main(["--adhoc", "t", "unused", "go", "--sha", head, "--no-setup", "--commits", "4"])

    output = capsys.readouterr().out
    assert status == 1
    assert runs.read_text().count("run") == 1  # neither the head of that commit nor any other commit was tried
    assert re.search(r"^[0-9a-f]{12}: timeout in [\d.]+s$", output, re.M)
    assert "t\tnot indexed: timeout at commit 1 of the sample" in output
    assert "compiling module" in only_failure_log(cache)
    (meta,) = cache.glob("index/t/*/*/meta.json")
    assert json.loads(meta.read_text())["reason"] == "timeout" and not json.loads(meta.read_text())["ok"]


def test_a_snapshot_refused_with_429_after_every_wait_stops_its_repository(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
):
    repo_dir, head, cache, runs, corpus = fast_failing_repository(
        tmp_path, monkeypatch, "echo 'Received status code 429 from server' >&2; exit 1"
    )
    monkeypatch.setattr(indexing, "RATE_LIMIT_WAITS", (0, 0))

    rows = indexshare.index_sample(corpus, Repo("t", "", "go", head, "MIT"), repo_dir / ".git", count=4)

    output = capsys.readouterr().out
    assert runs.read_text().count("run") == 3  # the first run and one after each wait, and nothing for another snapshot
    assert [(row["causes"], row.get("stopped")) for row in rows] == [({"base": "rate-limited"}, "rate-limited")]
    assert re.search(r"^[0-9a-f]{12}: rate-limited in [\d.]+s$", output, re.M)
    assert "status code 429" in only_failure_log(cache)


def test_an_indexer_that_exits_is_not_a_sign_that_the_next_snapshots_fail_too(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
):
    repo_dir, head, _, runs, corpus = fast_failing_repository(tmp_path, monkeypatch, "echo broken >&2; exit 1")

    rows = indexshare.index_sample(corpus, Repo("t", "", "go", head, "MIT"), repo_dir / ".git", count=3)

    snapshots = {sha for row in rows if row["base"] for sha in (row["base"], row["commit"])}
    assert len(rows) == 3 and not any("stopped" in row for row in rows)
    assert runs.read_text().count("run") == len(snapshots) > 2
    assert len(re.findall(r"^[0-9a-f]{12}: indexer exited 1: broken in [\d.]+s$", capsys.readouterr().out, re.M)) == len(
        snapshots
    )
