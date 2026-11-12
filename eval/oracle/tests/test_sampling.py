import dataclasses
import subprocess
import tomllib
from pathlib import Path

import gitrepo
import sampling
from corpus import LANGUAGES, ROOT, Repo, load_corpus

EARLIER_SEED = 20261031  # the seed of every run before the fresh one: its commits are in spent_commits.txt


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


def with_spent(corpus, repo: Repo, spent: set[str]):
    return dataclasses.replace(corpus, spent={repo.name: frozenset(spent)})


def test_the_fresh_seed_is_recorded_and_is_not_an_earlier_one():
    raw = tomllib.loads((ROOT / "corpus.toml").read_text(encoding="utf-8"))

    assert isinstance(raw["sampling"]["seed"], int)
    assert raw["sampling"]["seed"] != EARLIER_SEED
    assert load_corpus().sampling.seed == raw["sampling"]["seed"]


def test_the_same_seed_draws_the_same_commits_and_another_seed_draws_others(tmp_path: Path):
    head = history(tmp_path / "repo", 40)
    corpus = load_corpus()
    repo = Repo("t", "", "python", head, "MIT")
    sized = dataclasses.replace(corpus.sampling, language_commits=dict.fromkeys(LANGUAGES, 10))
    corpus = dataclasses.replace(corpus, repos=(repo,), sampling=sized)
    git_dir = tmp_path / "repo" / ".git"

    first = sampling.sample_commits(corpus, repo, git_dir, 10)
    again = sampling.sample_commits(dataclasses.replace(corpus), repo, git_dir, 10)
    reseeded = dataclasses.replace(corpus, sampling=dataclasses.replace(corpus.sampling, seed=corpus.sampling.seed + 1))
    other = sampling.sample_commits(reseeded, repo, git_dir, 10)

    assert first == again and len(first) == 10
    assert set(first) != set(other)


def test_no_commit_an_earlier_run_sampled_is_drawn(tmp_path: Path):
    head = history(tmp_path / "repo", 14)
    git_dir = tmp_path / "repo" / ".git"
    repo = Repo("t", "", "python", head, "MIT")
    corpus = load_corpus()
    sized = dataclasses.replace(corpus.sampling, language_commits=dict.fromkeys(LANGUAGES, 100))
    corpus = dataclasses.replace(corpus, repos=(repo,), sampling=sized)
    pool = [sha for sha, _ in gitrepo.commits_with_paths(git_dir, head, corpus.sampling.history)]
    spent = set(pool[:5]) | set(pool[9:11])

    drawn = sampling.sample_commits(with_spent(corpus, repo, spent), repo, git_dir, 100)

    assert not set(drawn) & spent
    assert set(drawn) == set(pool) - spent


def test_the_counts_per_language_follow_the_sizing_input(tmp_path: Path):
    head = history(tmp_path / "repo", 30)
    git_dir = tmp_path / "repo" / ".git"
    corpus = load_corpus()
    repos = tuple(Repo(name, "", "python", head, "MIT") for name in ("a", "b", "c"))
    sized = dataclasses.replace(
        corpus,
        repos=repos,
        spent={},
        sampling=dataclasses.replace(corpus.sampling, language_commits={**corpus.sampling.language_commits, "python": 7}),
    )

    assert [sized.quota(r) for r in repos] == [3, 2, 2]
    assert [len(sampling.sample_commits(sized, r, git_dir, 100)) for r in repos] == [3, 2, 2]
    assert [len(sampling.sample_commits(sized, r, git_dir, 1)) for r in repos] == [1, 1, 1]


def test_the_real_corpus_sizes_every_indexed_language_and_spends_every_earlier_commit_list():
    corpus = load_corpus()

    assert set(corpus.sampling.language_commits) == set(LANGUAGES)
    for language in LANGUAGES:
        repos = [r for r in corpus.repos if r.language == language]
        assert sum(corpus.quota(r) for r in repos) == corpus.sampling.language_commits[language]
    assert corpus.sampling.fast_commits < min(corpus.quota(r) for r in corpus.repos)
    assert all(corpus.spent.get(r.name) for r in corpus.repos)
    assert all(len(sha) == 40 for shas in corpus.spent.values() for sha in shas)
