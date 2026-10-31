"""Entry point for the oracle: `python run.py <command> [options]`; run.sh calls it in order."""

from __future__ import annotations

import argparse
import sys

import adjudicate
import bench
import measure
import report
import selftest
from corpus import load_corpus
from indexing import setup_tools


def setup(argv: list[str]) -> int:
    """Install what the selected repositories (same options as `measure`) need."""
    parser = argparse.ArgumentParser(prog="setup")
    parser.add_argument("--repo", action="append")
    parser.add_argument("--commits", type=int)
    parser.add_argument("--fast", action="store_true")
    options = parser.parse_args(argv)
    corpus = load_corpus()
    setup_tools(corpus, {repo.language for repo in corpus.select(options.repo, options.fast)})
    return 0


COMMANDS = {
    "setup": setup,
    "measure": measure.main,
    "bench": bench.main,
    "report": report.main,
    "adjudicate": adjudicate.main,
    "selftest": selftest.main,
}


def main(argv: list[str]) -> int:
    if not argv or argv[0] not in COMMANDS:
        print(f"usage: run.py {{{'|'.join(COMMANDS)}}} [options]", file=sys.stderr)
        return 2
    return COMMANDS[argv[0]](argv[1:])


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
