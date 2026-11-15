"""An index-only round: sample commits as `measure` does, index both snapshots of each, report what is excluded and why.

A commit is excluded when either of its two snapshots is not indexed ok, or it has no single parent: the rule of
`measure.prepare`, without asking the engine anything. The indexes land in the cache `measure` reads, so a later
`measure --no-install` finds them. Each failure's full output is in `failure.log` beside its slot.

  python indexshare.py --repo NAME [--repo NAME]... [--commits 24] [--seed N] [--retry-failed] [--no-install] [--drop-trees] [--out rows.jsonl]
  python indexshare.py --adhoc NAME URL LANGUAGE [--sha SHA]     # a repository that is not in corpus.toml
"""

from __future__ import annotations

import argparse
import collections
import dataclasses
import json
import shutil
import sys
from pathlib import Path

import gitrepo
from corpus import CACHE, Corpus, Repo, load_corpus
from indexing import IndexResult, index_snapshot, setup_tools
from sampling import sample_commits

THRESHOLD = 0.20  # a repository whose excluded share is above this is replaced (Revision 18, C3)


def cause(result: IndexResult) -> str:
    """Why one snapshot is unusable, short enough to count: the indexer's exit, an empty index, a low file ratio."""
    if result.ok:
        return ""
    if result.reason == "incomplete":
        return "nothing indexed" if result.completeness.files_indexed == 0 else f"file ratio {result.completeness.file_ratio:.2f}"
    return result.reason.split(":")[0]


def index_sample(
    corpus: Corpus,
    repo: Repo,
    git_dir: Path,
    count: int,
    *,
    install: bool = True,
    retry_failed: bool = False,
    drop_trees: bool = False,
) -> list[dict]:
    rows = []
    for commit in sample_commits(corpus, repo, git_dir, count):
        base = gitrepo.parent_of(git_dir, commit)
        row: dict = {
            "repo": repo.name,
            "commit": commit,
            "base": base,
            "date": gitrepo.git("log", "-1", "--format=%cs", commit, git_dir=git_dir).strip(),
            "seed": corpus.sampling.seed,  # with the quota, the draw this row is a prefix of
            "quota": corpus.quota(repo),
            "causes": {},
        }
        if base is None:
            row["causes"]["commit"] = "no single parent"
        else:
            for side, snapshot in (("base", base), ("head", commit)):
                tree = gitrepo.export_tree(git_dir, snapshot, CACHE / "trees" / repo.name / snapshot)
                result = index_snapshot(corpus, repo, snapshot, tree, install=install, retry_failed=retry_failed)
                row[f"{side}_file_ratio"] = round(result.completeness.file_ratio, 3)
                if drop_trees:
                    shutil.rmtree(tree, ignore_errors=True)
                if not result.ok:
                    row["causes"][side] = cause(result)
        row["excluded"] = bool(row["causes"])
        rows.append(row)
    return rows


def summary(repo: str, rows: list[dict]) -> str:
    excluded = [row for row in rows if row["excluded"]]
    causes = collections.Counter(c for row in excluded for c in set(row["causes"].values()))
    share = len(excluded) / len(rows) if rows else 1.0
    verdict = "ok" if share <= THRESHOLD else f"above {THRESHOLD:.0%}"
    return f"{repo}\texcluded {len(excluded)}/{len(rows)} = {share:.1%}\t{verdict}\t{dict(causes)}"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="indexshare", description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--repo", action="append", default=[], help="a repository of corpus.toml (repeatable)")
    parser.add_argument("--adhoc", nargs=3, metavar=("NAME", "URL", "LANGUAGE"), help="a repository that is not in corpus.toml")
    parser.add_argument("--sha", help="the pin of --adhoc (default: the remote's HEAD)")
    parser.add_argument(
        "--commits",
        type=int,
        default=24,
        help="the first N commits of the seeded draw (default 24); `measure` takes the repository's whole quota, "
        "its language's [sampling.commits] count split over the language's repositories",
    )
    parser.add_argument("--seed", type=int, help="replaces the corpus seed")
    parser.add_argument("--retry-failed", action="store_true", help="index again snapshots whose cached index failed")
    parser.add_argument("--no-install", dest="install", action="store_false", help="do not install TypeScript dependencies")
    parser.add_argument(
        "--drop-trees", action="store_true", help="remove each exported snapshot once it is indexed (a long run's disk)"
    )
    parser.add_argument("--no-setup", dest="setup", action="store_false", help="do not install the pinned indexers first")
    parser.add_argument("--out", type=Path, help="one JSON line per sampled commit")
    options = parser.parse_args(argv)
    corpus = load_corpus()
    if options.seed is not None:
        corpus = dataclasses.replace(corpus, sampling=dataclasses.replace(corpus.sampling, seed=options.seed))
    repos = corpus.select(options.repo) if options.repo else []
    if options.adhoc:
        name, url, language = options.adhoc
        sha = options.sha or gitrepo.git("ls-remote", url, "HEAD").split()[0]
        repos.append(Repo(name, url, language, sha, ""))
    if not repos:
        parser.error("name a repository with --repo or --adhoc")
    if options.setup:
        setup_tools(corpus, {repo.language for repo in repos})
    sink = options.out.open("w", encoding="utf-8") if options.out else None
    for repo in repos:
        git_dir = gitrepo.mirror(repo.url, CACHE / "mirrors" / f"{repo.name}.git", repo.sha)
        rows = index_sample(
            corpus,
            repo,
            git_dir,
            options.commits,
            install=options.install,
            retry_failed=options.retry_failed,
            drop_trees=options.drop_trees,
        )
        if sink:
            sink.writelines(json.dumps(row) + "\n" for row in rows)
            sink.flush()
        print(summary(repo.name, rows), flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
