"""Seeded commit sampling: single-parent commits that touch a few Tier-1 code files."""

from __future__ import annotations

import random
from pathlib import Path

import gitrepo
from corpus import Corpus, Repo, is_code_path


def eligible(commits: list[tuple[str, list[str]]], language: str, max_code_files: int) -> list[str]:
    out: list[str] = []
    for sha, paths in commits:
        code = [p for p in paths if is_code_path(language, p)]
        if 1 <= len(code) <= max_code_files:
            out.append(sha)
    return out


def sample_commits(corpus: Corpus, repo: Repo, git_dir: Path, count: int) -> list[str]:
    """The first `count` of one seeded draw of the repository's quota, so the fast subset is part of the full corpus.

    A commit an earlier run sampled is not in the pool: its row was read, and a set whose rows were read is a tuning set.
    The draw's size is fixed (the quota), never `count`: `random.sample` picks its algorithm by size, so a draw of another
    size is not a prefix of this one.
    """
    history = gitrepo.commits_with_paths(git_dir, repo.sha, corpus.sampling.history)
    spent = corpus.spent.get(repo.name, frozenset())
    pool = [sha for sha in eligible(history, repo.language, corpus.sampling.max_code_files) if sha not in spent]
    order = {sha: position for position, (sha, _) in enumerate(history)}
    rng = random.Random(f"{corpus.sampling.seed}:{repo.name}")
    drawn = rng.sample(pool, min(corpus.quota(repo), len(pool)))[:count]
    return sorted(drawn, key=order.__getitem__)
