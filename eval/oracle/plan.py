"""The matrix of a CI round: one entry per corpus repository, read from `corpus.toml` and never listed in a workflow.

`python run.py plan --scope full|fast` prints `{"include": [...]}` for `fromJSON`.
"""

from __future__ import annotations

import argparse
import json

import corpus as corpus_config
from corpus import Corpus


def plan(corpus: Corpus, scope: str) -> dict:
    fast = scope == "fast"
    return {
        "include": [
            {"repo": repo.name, "language": repo.language, "lane": repo.language, "fast": fast}
            for repo in corpus.select(None, fast)
        ]
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="plan", description=__doc__)
    parser.add_argument("--scope", choices=("fast", "full"), required=True)
    options = parser.parse_args(argv)
    print(json.dumps(plan(corpus_config.load_corpus(), options.scope), separators=(",", ":")))
    return 0
