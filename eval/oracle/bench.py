"""Wall-time baseline (T18): p50 and p95 of impact per commit, `--symbol` and pack over the sampled commits."""

from __future__ import annotations

import argparse
import json
import math
import os
import platform
import random
import subprocess
from pathlib import Path

import gitrepo
from changed import changed_definitions
from corpus import load_corpus
from indexing import CACHE
from measure import diffctx_binary, impact_args, run_diffctx, symbol_args
from sampling import sample_commits
from scipindex import Index
from truth import Snapshot

OPERATIONS = ("impact", "symbol", "pack")


def percentile(values: list[float], q: float) -> float:
    """Nearest rank: the smallest value with at least q of the sample at or below it."""
    ordered = sorted(values)
    return ordered[max(0, math.ceil(q * len(ordered)) - 1)]


def summarize(samples: list[float], errors: int) -> dict:
    if not samples:
        return {"n": 0, "errors": errors}
    return {
        "n": len(samples),
        "errors": errors,
        "p50": round(percentile(samples, 0.5), 3),
        "p95": round(percentile(samples, 0.95), 3),
        "max": round(max(samples), 3),
    }


def symbol_name(git_dir: Path, repo_name: str, base: str, head: str, language: str, rng: random.Random) -> str | None:
    """The name of one definition the commit changes, found by AST containment alone: no index is needed to time a query."""
    snapshots = {}
    for side, sha in (("base", base), ("head", head)):
        snapshots[side] = Snapshot(gitrepo.export_tree(git_dir, sha, CACHE / "trees" / repo_name / sha), Index(), language)
    changed = changed_definitions(
        gitrepo.diff_changes(git_dir, base, head), snapshots["base"].definitions, snapshots["head"].definitions
    )
    names = [c.head.name for c in changed if c.head is not None and c.status in ("modified", "added", "moved", "renamed")]
    return rng.choice(names) if names else None


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="bench", description=__doc__)
    parser.add_argument("--repo", action="append")
    parser.add_argument("--commits", type=int)
    parser.add_argument("--fast", action="store_true")
    parser.add_argument("--repeats", type=int, default=1, help="runs per command and commit; every run is one sample")
    parser.add_argument("--out", type=Path, default=Path("out/bench.json"))
    options = parser.parse_args(argv)
    corpus = load_corpus()
    binary = diffctx_binary()
    samples: dict[str, dict[str, list[float]]] = {}
    errors: dict[str, dict[str, int]] = {}
    for repo in corpus.select(options.repo, options.fast):
        git_dir = gitrepo.mirror(repo.url, CACHE / "mirrors" / f"{repo.name}.git", repo.sha)
        count = options.commits or (corpus.sampling.fast_commits if options.fast else corpus.sampling.commits)
        rng = random.Random(f"{corpus.sampling.seed}:{repo.name}:bench")
        per_op = samples.setdefault(repo.name, {op: [] for op in OPERATIONS})
        failed = errors.setdefault(repo.name, dict.fromkeys(OPERATIONS, 0))
        for sha in sample_commits(corpus, repo, git_dir, count):
            base = gitrepo.parent_of(git_dir, sha)
            if base is None:
                continue
            work = gitrepo.checkout(CACHE / "work" / repo.name, git_dir, sha)
            name = symbol_name(git_dir, repo.name, base, sha, repo.language, rng)
            commands = {"impact": impact_args(sha), "pack": impact_args(sha, "pack")}
            if name:
                commands["symbol"] = symbol_args(name)
            for op, args in commands.items():
                for _ in range(options.repeats):
                    result = run_diffctx(binary, work, args, corpus.diffctx_seconds)
                    if result.ok:
                        per_op[op].append(result.seconds)
                    else:
                        failed[op] += 1
            print(f"[{repo.name}] {sha[:10]} timed", flush=True)
    merged = {op: [s for repo in samples.values() for s in repo[op]] for op in OPERATIONS}
    version = subprocess.run([str(binary), "--version"], capture_output=True, text=True, check=False).stdout.strip()
    report = {
        "diffctx": version,
        "host": {"system": platform.system(), "machine": platform.machine(), "cpus": os.cpu_count()},
        "sampling": {"seed": corpus.sampling.seed, "repeats": options.repeats},
        "repos": {name: {op: summarize(per[op], errors[name][op]) for op in OPERATIONS} for name, per in samples.items()},
        "all": {op: summarize(merged[op], sum(e[op] for e in errors.values())) for op in OPERATIONS},
    }
    options.out.parent.mkdir(parents=True, exist_ok=True)
    options.out.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(json.dumps(report["all"], sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
