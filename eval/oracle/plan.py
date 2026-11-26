"""The matrix of a CI round: one entry per corpus repository, read from the corpus files and never listed in a workflow.

`python run.py plan --scope full|fast [--corpus FILE]... [--blind] [--index-only]` prints the matrix for `fromJSON` and the
round's mode. Several corpus files are planned together only to build indexes: a measure round reads one corpus.
"""

from __future__ import annotations

import argparse
import hashlib
import json

import corpus as corpus_config
import jvmtoolchains
import runners
from corpus import ROOT, Corpus, Repo

DEFAULT_CORPUS = "corpus.toml"
CACHE_NAMESPACE = "oracle-index-v2"  # apart from `oracle-index-<repo>-<run>`, the keys the first CI rounds saved


def jvm_toolchain_digest() -> str:
    """What a JVM build runs on besides the sources: the pinned JDKs and Maven, and the two container images."""
    pins = jvmtoolchains.PINS_FILE.read_bytes()
    return hashlib.sha256(pins + f"{runners.SANDBOX_IMAGE}{runners.MAVEN_IMAGE}".encode()).hexdigest()


def index_inputs(corpus: Corpus, repo: Repo) -> dict:
    """Everything that decides which snapshots of `repo` get indexed and how a cached index reads: never the run."""
    package, version = corpus_config.indexer_pin(corpus, repo.language)
    sampling = corpus.sampling
    return {
        "repo": repo.name,
        "url": repo.url,
        "sha": repo.sha,
        "indexer": [package, version],
        "toolchains": jvm_toolchain_digest() if repo.language in ("kotlin", "java") else "",
        "seed": sampling.seed,
        "quota": corpus.quota(repo),
        "history": sampling.history,
        "max_code_files": sampling.max_code_files,
        "spent": sorted(corpus.spent.get(repo.name, ())),
        "min_file_ratio": corpus.min_file_ratio,
    }


def index_key(corpus: Corpus, repo: Repo) -> str:
    """The cache key of a repository's indexes: its name and a digest of `index_inputs`. A run that changes none of them
    asks for the entry an earlier run saved, so one entry holds each index once."""
    digest = hashlib.sha256(json.dumps(index_inputs(corpus, repo), sort_keys=True).encode()).hexdigest()[:20]
    return f"{CACHE_NAMESPACE}-{repo.name}-{digest}"


def plan(corpus: Corpus, scope: str, file: str = DEFAULT_CORPUS) -> dict:
    fast = scope == "fast"
    return {
        "include": [
            {
                "repo": repo.name,
                "language": repo.language,
                "lane": repo.language,
                "fast": fast,
                "corpus": file,
                "index_key": index_key(corpus, repo),
                # corpus.toml's indexes were saved under per-run keys: the newest of them seeds the first exact key
                "legacy_prefix": f"oracle-index-{repo.name}-" if file == DEFAULT_CORPUS else "",
            }
            for repo in corpus.select(None, fast)
        ]
    }


def round_plan(files: list[str], scope: str, blind: bool = False, index_only: bool = False) -> dict:
    """The matrix of every file, and how the round runs: a holdout is always blind."""
    if len(files) > 1 and not index_only:
        raise SystemExit("a measure round reads one corpus; several are planned only to index")
    loaded = [corpus_config.load_corpus(ROOT / file) for file in files]
    return {
        "matrix": {
            "include": [e for corpus, file in zip(loaded, files, strict=True) for e in plan(corpus, scope, file)["include"]]
        },
        "blind": blind or any(corpus.holdout for corpus in loaded),
        "index_only": index_only,
        "default_corpus": files == [DEFAULT_CORPUS],
        "corpus": files[0] if len(files) == 1 else "",
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="plan", description=__doc__)
    parser.add_argument("--scope", choices=("fast", "full"), required=True)
    parser.add_argument(
        "--corpus", action="append", default=[], help=f"a corpus file under eval/oracle (default {DEFAULT_CORPUS})"
    )
    parser.add_argument("--blind", action="store_true", help="a holdout is always blind")
    parser.add_argument("--index-only", action="store_true")
    options = parser.parse_args(argv)
    print(
        json.dumps(
            round_plan(options.corpus or [DEFAULT_CORPUS], options.scope, options.blind, options.index_only),
            separators=(",", ":"),
        )
    )
    return 0
