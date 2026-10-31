import subprocess
from pathlib import Path

import gitrepo
import sampling
from corpus import Repo, load_corpus


def history(path: Path, count: int) -> str:
    env = {**gitrepo.git_env(), "GIT_AUTHOR_DATE": "2026-01-01T00:00:00Z", "GIT_COMMITTER_DATE": "2026-01-01T00:00:00Z"}
    identity = ["-c", "user.name=t", "-c", "user.email=t@example.invalid"]
    path.mkdir()
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=path, env=env, check=True)
    for n in range(count):
        (path / "m.py").write_text(f"x = {n}\n")
        subprocess.run(["git", "add", "-A"], cwd=path, env=env, check=True)
        subprocess.run(["git", *identity, "commit", "-q", "-m", f"c{n}"], cwd=path, env=env, check=True)
    return subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=path, env=env, capture_output=True, text=True, check=True
    ).stdout.strip()


def test_eligible_keeps_commits_touching_a_few_code_files():
    log = [
        ("a", ["x.py"]),
        ("b", ["README.md"]),
        ("c", ["a.py", "b.py", "c.py"]),
        ("d", ["x.ts", "y.py"]),
    ]

    assert sampling.eligible(log, "python", 2) == ["a", "d"]
    assert sampling.eligible(log, "typescript", 2) == ["d"]


def test_the_draw_is_repeatable_and_the_fast_subset_lies_inside_the_full_one(tmp_path: Path):
    head = history(tmp_path / "repo", 14)
    corpus = load_corpus()
    repo = Repo("t", "", "python", head, "MIT")
    git_dir = tmp_path / "repo" / ".git"

    full = sampling.sample_commits(corpus, repo, git_dir, 8)
    fast = sampling.sample_commits(corpus, repo, git_dir, 3)

    assert full == sampling.sample_commits(corpus, repo, git_dir, 8)
    assert len(set(full)) == 8 and set(fast) <= set(full)
