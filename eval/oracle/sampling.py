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
    """The first `count` of one seeded draw of the full sample, so the fast subset is part of the full corpus."""
    history = gitrepo.commits_with_paths(git_dir, repo.sha, corpus.sampling.history)
    pool = eligible(history, repo.language, corpus.sampling.max_code_files)
    order = {sha: position for position, (sha, _) in enumerate(history)}
    rng = random.Random(f"{corpus.sampling.seed}:{repo.name}")
    drawn = rng.sample(pool, min(corpus.sampling.commits, len(pool)))[:count]
    return sorted(drawn, key=order.__getitem__)
