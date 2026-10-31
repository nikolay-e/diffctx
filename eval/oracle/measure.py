"""Measure diffctx against the SCIP oracle on sampled commits and sampled definitions.

Per commit: both snapshots are indexed, hunks become changed definitions by AST containment, SCIP gives the
relations that reach each, and diffctx's `--diff C~1..C --mode impact -f json` answer is scored per
(target, relation). Per sampled definition: `--symbol NAME -f json` against the callers at one snapshot.
A snapshot whose index failed or is incomplete excludes the commit and is counted, never scored as empty.
"""

from __future__ import annotations

import argparse
import json
import os
import random
import subprocess
import tempfile
import time
from dataclasses import dataclass, replace
from pathlib import Path

import gitrepo
from changed import ChangedDefinition, changed_definitions
from compare import compare, make_resolver, parse_answer
from corpus import Corpus, Repo, load_corpus
from indexing import CACHE, index_snapshot
from model import Definition, Relation
from sampling import sample_commits
from scipindex import read_index
from truth import Snapshot, relations_to

SCORED_STATUSES = ("modified", "added", "moved", "renamed")
RESULT_FILES = ("cells.jsonl", "relations.jsonl", "commits.jsonl", "indexes.jsonl", "unmapped.jsonl")


@dataclass
class Run:
    ok: bool
    data: dict | None
    seconds: float
    error: str = ""


def diffctx_binary() -> Path:
    found = os.environ.get("DIFFCTX_BIN")
    if not found or not Path(found).is_file():
        raise SystemExit("set DIFFCTX_BIN to the diffctx binary under test (a release build)")
    return Path(found)


def run_diffctx(binary: Path, cwd: Path, args: list[str], timeout: int) -> Run:
    with tempfile.TemporaryDirectory(prefix="oracle-home-") as home:
        env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "HOME": home, "LC_ALL": "C", "GIT_CONFIG_GLOBAL": "/dev/null"}
        started = time.perf_counter()
        try:
            done = subprocess.run([str(binary), ".", *args], cwd=cwd, env=env, capture_output=True, timeout=timeout, check=False)
        except subprocess.TimeoutExpired:
            return Run(False, None, time.perf_counter() - started, "timeout")
        seconds = time.perf_counter() - started
    if done.returncode != 0:
        return Run(False, None, seconds, f"exit {done.returncode}: {done.stderr.decode(errors='replace')[-300:]}")
    try:
        return Run(True, json.loads(done.stdout), seconds)
    except json.JSONDecodeError as error:
        return Run(False, None, seconds, f"invalid json: {error}")


def impact_args(sha: str, mode: str = "impact") -> list[str]:
    return ["--diff", f"{sha}~1..{sha}", "--mode", mode, "-f", "json"]


def symbol_args(name: str) -> list[str]:
    return ["--symbol", name, "-f", "json"]


def _hunk_lines(changes: list[gitrepo.FileChange], side: str) -> dict[str, set[int]]:
    lines: dict[str, set[int]] = {}
    for change in changes:
        path = change.new_path if side == "head" else change.old_path
        if path:
            for hunk in change.hunks:
                lines.setdefault(path, set()).update(hunk.new_lines if side == "head" else hunk.old_lines)
    return lines


def mark_in_diff(relations: list[Relation], changed_keys: set[str], hunk_lines: dict[str, set[int]]) -> list[Relation]:
    """A relation whose caller is itself changed, or whose site is in a hunk, lies inside the diff."""
    marked = []
    for r in relations:
        path = r.from_key.rsplit("::", 1)[0]
        marked.append(replace(r, in_diff=r.from_key in changed_keys or r.site in hunk_lines.get(path, ())))
    return marked


class Sink:
    def __init__(self, out: Path, fresh: bool = True) -> None:
        out.mkdir(parents=True, exist_ok=True)
        self.out = out
        if fresh:
            for name in RESULT_FILES:
                (out / name).unlink(missing_ok=True)

    def write(self, name: str, rows: list[dict]) -> None:
        if rows:
            with (self.out / name).open("a", encoding="utf-8") as handle:
                handle.writelines(json.dumps(row, sort_keys=True) + "\n" for row in rows)


@dataclass
class Prepared:
    base: Snapshot
    head: Snapshot
    changes: list[gitrepo.FileChange]
    changed: list[ChangedDefinition]


@dataclass
class RepoRun:
    corpus: Corpus
    repo: Repo
    git_dir: Path
    sink: Sink
    binary: Path
    install: bool
    retry_failed: bool
    symbols_left: int


def prepare(run: RepoRun, base: str, sha: str, record: dict) -> Prepared | None:
    """Both snapshots indexed and the changed definitions found; None, with the reason, when an index is unusable."""
    indexed = {}
    for side, commit in (("base", base), ("head", sha)):
        tree = gitrepo.export_tree(run.git_dir, commit, CACHE / "trees" / run.repo.name / commit)
        result = index_snapshot(run.corpus, run.repo, commit, tree, install=run.install, retry_failed=run.retry_failed)
        indexed[side] = (tree, result)
        run.sink.write("indexes.jsonl", [{"repo": run.repo.name, "commit": commit, "side": side, **result.summary()}])
    failed = [f"{side}: {indexed[side][1].reason}" for side in indexed if not indexed[side][1].ok]
    if failed:
        record["reason"] = "; ".join(failed)
        return None
    snapshots = {}
    for side, (tree, result) in indexed.items():
        assert result.path is not None
        snapshots[side] = Snapshot(tree, read_index(result.path), run.repo.language)
    changes = gitrepo.diff_changes(run.git_dir, base, sha)
    changed = changed_definitions(changes, snapshots["base"].definitions, snapshots["head"].definitions)
    return Prepared(snapshots["base"], snapshots["head"], changes, changed)


def measure_commit(run: RepoRun, sha: str, rng: random.Random) -> dict:
    repo = run.repo
    base = gitrepo.parent_of(run.git_dir, sha)
    record: dict = {"repo": repo.name, "language": repo.language, "commit": sha, "base": base, "status": "excluded"}
    if base is None:
        record["reason"] = "no single parent"
        return record
    prepared = prepare(run, base, sha, record)
    if prepared is None:
        return record
    work = gitrepo.checkout(CACHE / "work" / repo.name, run.git_dir, sha)
    resolver = make_resolver(prepared.head.definitions, prepared.base.definitions)
    context = {"repo": repo.name, "language": repo.language, "commit": sha}
    counts = {"changed": len(prepared.changed), "layout_only": 0, "unmapped": 0, "scored": 0}
    impact = run_diffctx(run.binary, work, impact_args(sha), run.corpus.diffctx_seconds)
    record.update(status="measured", impact_seconds=round(impact.seconds, 3), impact_error=impact.error)
    if impact.ok and impact.data is not None:
        answer = parse_answer(impact.data, resolver)
        changed_keys = {c.target.key for c in prepared.changed}
        for item in prepared.changed:
            _score_changed(run, prepared, item, changed_keys, answer, {**context, "mode": "impact"}, counts)
    candidates = [c for c in prepared.changed if c.head is not None and c.status in SCORED_STATUSES]
    if candidates and run.symbols_left > 0:
        run.symbols_left -= 1
        _score_symbol(run, prepared, rng.choice(candidates), work, resolver, {**context, "mode": "symbol"}, record)
    record.update(counts)
    return record


def _score_changed(
    run: RepoRun, prepared: Prepared, item: ChangedDefinition, changed_keys, answer, context: dict, counts: dict
) -> None:
    if item.status == "layout":
        counts["layout_only"] += 1
        return
    snapshot = prepared.head if item.snapshot == "head" else prepared.base
    target = item.target
    if not snapshot.symbols_of(target):
        counts["unmapped"] += 1
        run.sink.write("unmapped.jsonl", [{**context, "target": target.key, "status": item.status}])
        return
    relations = mark_in_diff(relations_to(snapshot, target), changed_keys, _hunk_lines(prepared.changes, item.snapshot))
    counts["scored"] += 1
    _write_scored(run.sink, {**context, "status": item.status, "snapshot": item.snapshot}, target, relations, answer)


def _score_symbol(
    run: RepoRun, prepared: Prepared, item: ChangedDefinition, work: Path, resolver, context: dict, record: dict
) -> None:
    target = item.target
    answer_run = run_diffctx(run.binary, work, symbol_args(target.name), run.corpus.diffctx_seconds)
    record.update(symbol_seconds=round(answer_run.seconds, 3), symbol_error=answer_run.error)
    if not answer_run.ok or answer_run.data is None or not prepared.head.symbols_of(target):
        return
    answer = parse_answer(answer_run.data, resolver)
    _write_scored(
        run.sink, {**context, "status": "sampled", "snapshot": "head"}, target, relations_to(prepared.head, target), answer
    )


def _write_scored(sink: Sink, context: dict, target: Definition, relations: list[Relation], answer) -> None:
    cells, details = compare(target, relations, answer)
    for row in (*cells, *details):
        row.update(context)
    sink.write("cells.jsonl", cells)
    sink.write("relations.jsonl", details)


def measure_repo(run: RepoRun, count: int) -> int:
    commits = sample_commits(run.corpus, run.repo, run.git_dir, count)
    rng = random.Random(f"{run.corpus.sampling.seed}:{run.repo.name}:symbols")
    failures = 0
    for number, sha in enumerate(commits, 1):
        try:
            record = measure_commit(run, sha, rng)
        except Exception as error:  # one broken commit is counted and reported; the round goes on
            record = {"repo": run.repo.name, "commit": sha, "status": "error", "reason": f"{type(error).__name__}: {error}"}
            failures += 1
        run.sink.write("commits.jsonl", [record])
        print(f"[{run.repo.name} {number}/{len(commits)}] {sha[:10]} {record['status']} {record.get('reason', '')}", flush=True)
    return failures


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="measure", description=__doc__)
    parser.add_argument("--repo", action="append", help="repository name from corpus.toml (repeatable); default all")
    parser.add_argument("--commits", type=int, help="sampled commits per repository (overrides the corpus value)")
    parser.add_argument(
        "--fast", action="store_true", help="the `oracle` label subset: one repository per language, fast_commits each"
    )
    parser.add_argument("--out", type=Path, default=Path("out/latest"))
    parser.add_argument("--append", action="store_true", help="keep the results already in --out")
    parser.add_argument("--no-install", dest="install", action="store_false", help="do not install TypeScript dependencies")
    parser.add_argument("--retry-failed", action="store_true", help="index again snapshots whose cached index failed")
    options = parser.parse_args(argv)
    corpus = load_corpus()
    binary = diffctx_binary()
    sink = Sink(options.out, fresh=not options.append)
    failures = 0
    for repo in corpus.select(options.repo, options.fast):
        git_dir = gitrepo.mirror(repo.url, CACHE / "mirrors" / f"{repo.name}.git", repo.sha)
        count = options.commits or (corpus.sampling.fast_commits if options.fast else corpus.sampling.commits)
        run = RepoRun(
            corpus, repo, git_dir, sink, binary, options.install, options.retry_failed, corpus.sampling.definitions_per_repo
        )
        failures += measure_repo(run, count)
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
