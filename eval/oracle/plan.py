"""The matrix of a CI round: one entry per corpus repository, read from `corpus.toml` and never listed in a workflow.

`python run.py plan --scope full|fast` prints `{"include": [...]}` for `fromJSON`. Each entry carries `pin`, the part of
the index cache key that changes exactly when the repository's index must be built again: its corpus record, its
indexer's version, the sampling, and the commands that build the index (`runners.py`, the JVM toolchain pins).
"""

from __future__ import annotations

import argparse
import hashlib
import json
from dataclasses import asdict
from pathlib import Path

import corpus as corpus_config
from corpus import Corpus, Repo

RECIPE_FILES = ("runners.py", "jvm-toolchains.toml")


def pin(corpus: Corpus, repo: Repo, recipe: bytes) -> str:
    material = json.dumps(
        [asdict(repo), corpus_config.indexer_pin(corpus, repo.language), asdict(corpus.sampling)], sort_keys=True, default=str
    )
    return hashlib.sha256(material.encode() + recipe).hexdigest()[:16]


def plan(corpus: Corpus, scope: str, root: Path = corpus_config.ROOT) -> dict:
    recipe = b"".join((root / name).read_bytes() for name in RECIPE_FILES)
    fast = scope == "fast"
    return {
        "include": [
            {"repo": repo.name, "language": repo.language, "lane": repo.language, "pin": pin(corpus, repo, recipe), "fast": fast}
            for repo in corpus.select(None, fast)
        ]
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="plan", description=__doc__)
    parser.add_argument("--scope", choices=("fast", "full"), required=True)
    options = parser.parse_args(argv)
    print(json.dumps(plan(corpus_config.load_corpus(), options.scope), separators=(",", ":")))
    return 0
