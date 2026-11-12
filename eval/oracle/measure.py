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
import re
import shutil
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass, replace
from functools import cache
from pathlib import Path

import gitrepo
from changed import ChangedDefinition, changed_definitions
from compare import CONSTRUCTORS, compare, make_resolver, missing_definition, parse_answer, silent_loss
from corpus import Corpus, Repo, load_corpus
from delta import Delta, classify
from folding import mark_folding
from indexing import CACHE, index_snapshot
from model import UNCAPPED_BUDGET, Definition, Relation, without_span
from sampling import sample_commits
from scipindex import read_index
from stale import pairing, renamed_answer, still_named
from truth import Snapshot, relations_to

SCORED_STATUSES = ("modified", "added", "moved", "renamed")
RESULT_FILES = ("cells.jsonl", "relations.jsonl", "silent_loss.jsonl", "commits.jsonl", "indexes.jsonl", "unmapped.jsonl")
RUN_FILE = "run.json"
ANSWERS = "answers"  # the engine's raw answers, one file per query: a later scoring rule re-scores them with --replay


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


@cache
def supports_unfold(binary: Path) -> bool:
    """Whether the engine can list the rows its folding table folds (`--unfold`): one look at its help per binary."""
    try:
        done = subprocess.run([str(binary), "--help"], capture_output=True, timeout=60, check=False)
    except (OSError, subprocess.TimeoutExpired):
        return False
    return b"--unfold" in done.stdout


def engine_arguments(binary: Path, given: tuple[str, ...], folded: bool) -> tuple[str, ...]:
    """The extra arguments of every query: the ones asked for, and `--unfold` where the engine knows it, so the answers
    hold every row the oracle judges; `folded` measures the answers the engine prints by default instead."""
    if folded or "--unfold" in given or not supports_unfold(binary):
        return given
    return (*given, "--unfold")


def run_diffctx(binary: Path, cwd: Path, args: list[str], timeout: int) -> Run:
    with tempfile.TemporaryDirectory(prefix="oracle-home-", ignore_cleanup_errors=True) as home:
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


def budget_args(budget: int | None) -> list[str]:
    return [] if budget is None else ["--budget", str(budget)]


def answer_path(root: Path, repo: str, sha: str, label: str) -> Path:
    return root / ANSWERS / repo / f"{sha}-{re.sub(r'[^A-Za-z0-9_.-]', '_', label)}.json"


def engine_answer(run: RepoRun, sha: str, label: str, work: Path, args: list[str]) -> Run:
    """The engine's answer to one query: stored from `--replay`, else asked of the engine when one is given, and kept
    beside the results either way."""
    answered: Run | None = None
    if run.replay is not None:
        stored = answer_path(run.replay, run.repo.name, sha, label)
        if stored.is_file():
            answered = Run(True, json.loads(stored.read_text(encoding="utf-8")), 0.0)
        elif not run.binary.is_file():
            return Run(False, None, 0.0, "not stored")
    if answered is None:
        answered = run_diffctx(run.binary, work, args, run.corpus.diffctx_seconds)
    if answered.ok:
        kept = answer_path(run.sink.out, run.repo.name, sha, label)
        kept.parent.mkdir(parents=True, exist_ok=True)
        kept.write_text(json.dumps(answered.data, sort_keys=True), encoding="utf-8")
    return answered


def impact_args(sha: str, mode: str = "impact", budget: int | None = None, extra: tuple[str, ...] = ()) -> list[str]:
    return ["--diff", f"{sha}~1..{sha}", "--mode", mode, "-f", "json", *budget_args(budget), *extra]


def symbol_args(name: str, budget: int | None = None, extra: tuple[str, ...] = ()) -> list[str]:
    return ["--symbol", name, "-f", "json", *budget_args(budget), *extra]


def parse_budget(text: str) -> int:
    """`N` tokens, or `uncapped`: the budget the engine is asked to answer within."""
    if text == "uncapped":
        return UNCAPPED_BUDGET
    budget = int(text)
    if budget <= 0:
        raise argparse.ArgumentTypeError(f"a budget is a positive token count, not {text}")
    return budget


def engine_version(binary: Path) -> str:
    try:
        done = subprocess.run([str(binary), "--version"], capture_output=True, timeout=60, check=False)
    except subprocess.TimeoutExpired:
        return "unknown"
    return done.stdout.decode(errors="replace").strip() or "unknown"


def write_run_meta(out: Path, binary: Path, budget: int | None, append: bool, engine_args: tuple[str, ...] = ()) -> None:
    """Which engine and budget made these results: the report's curve and comparisons read it back."""
    meta = {"engine": engine_version(binary), "binary": binary.name, "budget": budget, "engine_args": list(engine_args)}
    path = out / RUN_FILE
    if append and path.exists():
        before = json.loads(path.read_text(encoding="utf-8"))
        if (before["engine"], before["budget"], before.get("engine_args", [])) != (meta["engine"], budget, meta["engine_args"]):
            raise SystemExit(f"--append: {path} holds {before['engine']} at budget {before['budget']}, not this run")
        return
    path.write_text(json.dumps(meta, sort_keys=True) + "\n", encoding="utf-8")


def _hunk_lines(changes: list[gitrepo.FileChange], side: str) -> dict[str, set[int]]:
    lines: dict[str, set[int]] = {}
    for change in changes:
        path = change.new_path if side == "head" else change.old_path
        if path:
            for hunk in change.hunks:
                lines.setdefault(path, set()).update(hunk.new_lines if side == "head" else hunk.old_lines)
    return lines


def enclosing_keys(key: str) -> list[str]:
    """`p::A.b.c` and the definitions around it: `p::A.b`, `p::A`."""
    path, _, qualname = key.rpartition("::")
    parts = qualname.split(".")
    return [key] + [f"{path}::{'.'.join(parts[:n])}" for n in range(len(parts) - 1, 0, -1)]


def mark_in_diff(
    relations: list[Relation], changed_keys: set[str], hunk_lines: dict[str, set[int]], added: bool = False
) -> list[Relation]:
    """A use inside a definition the range changes is `also_changed` and scored (R14), unless the target is added:
    nothing outside the diff can depend on what the diff introduces, and its users are in the diff (R14'). A use on a
    changed line of no definition (module code) lies inside the diff and is not scored."""
    marked = []
    for r in relations:
        path = r.from_key.rsplit("::", 1)[0]
        if r.from_key in enclosing_keys(r.to_key)[1:]:
            # The definition that holds the target uses it in its own body (a test's local helper, a nested function):
            # the use is part of the target's change, not a caller the engine could be asked for.
            marked.append(replace(r, in_diff=any(key in changed_keys for key in enclosing_keys(r.from_key))))
        elif any(key in changed_keys for key in enclosing_keys(r.from_key)):
            marked.append(replace(r, in_diff=True) if added else replace(r, moved=(*r.moved, "R14")))
        else:
            marked.append(replace(r, in_diff=r.site in hunk_lines.get(path, ())))
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
    budget: int | None = None
    replay: Path | None = None
    engine_args: tuple[str, ...] = ()


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
    changed = changed_definitions(
        changes,
        snapshots["base"].definitions,
        snapshots["head"].definitions,
        snapshots["base"].inert_lines,
        snapshots["head"].inert_lines,
    )
    return Prepared(snapshots["base"], snapshots["head"], changes, changed)


def symbol_candidates(changed: list[ChangedDefinition]) -> list[ChangedDefinition]:
    """Definitions a `--symbol NAME` query can name: not a bare `constructor` or `__init__`, every class has one (R13)."""
    return [c for c in changed if c.head is not None and c.status in SCORED_STATUSES and c.head.name not in CONSTRUCTORS]


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
    asks = run.replay is None or run.binary.is_file()
    work = gitrepo.checkout(CACHE / "work" / repo.name, run.git_dir, sha) if asks else Path()
    resolver = make_resolver(prepared.head.definitions, prepared.base.definitions)
    context = {"repo": repo.name, "language": repo.language, "commit": sha}
    counts = {"changed": len(prepared.changed), "layout_only": 0, "unmapped": 0, "scored": 0}
    impact = engine_answer(run, sha, "impact", work, impact_args(sha, budget=run.budget, extra=run.engine_args))
    record.update(status="measured", impact_seconds=round(impact.seconds, 3), impact_error=impact.error)
    if impact.ok and impact.data is not None:
        record["impact_schema"] = impact.data.get("schema", "diffctx.impact.v1")
    if impact.ok and impact.data is not None:
        answer = parse_answer(impact.data, resolver)
        changed_keys = {c.target.key for c in prepared.changed}
        missing = [
            lost
            for item in prepared.changed
            if (lost := _score_changed(run, prepared, item, changed_keys, answer, {**context, "mode": "impact"}, counts))
        ]
        write_silent_loss(run.sink, {**context, "mode": "impact"}, missing, answer)
    candidates = symbol_candidates(prepared.changed)
    if candidates and run.symbols_left > 0:
        run.symbols_left -= 1
        _score_symbol(run, prepared, rng.choice(candidates), work, resolver, {**context, "mode": "symbol"}, record)
    record.update(counts)
    record["filtered"] = dict(prepared.head.filters.dropped + prepared.base.filters.dropped)
    return record


def _delta(prepared: Prepared, item: ChangedDefinition, counts: dict) -> Delta:
    """The oracle's classification of the change; one it cannot read is `other`, which folds nothing."""
    try:
        return classify(item, prepared.base, prepared.head)
    except Exception:  # a definition the trees cannot place must not exclude its commit
        counts["delta_errors"] = counts.get("delta_errors", 0) + 1
        return Delta("other", "other")


def _score_changed(
    run: RepoRun, prepared: Prepared, item: ChangedDefinition, changed_keys, answer, context: dict, counts: dict
) -> dict | None:
    """Scores one changed definition; returns its record when the answer lost it without a word (a silent loss)."""
    if item.status == "layout":
        counts["layout_only"] += 1
        return None
    snapshot = prepared.head if item.snapshot == "head" else prepared.base
    target = item.target
    if not snapshot.symbols_of(target):
        counts["unmapped"] += 1
        run.sink.write("unmapped.jsonl", [{**context, "target": target.key, "status": item.status}])
        return None
    relations = mark_in_diff(
        relations_to(snapshot, target), changed_keys, _hunk_lines(prepared.changes, item.snapshot), item.status == "added"
    )
    if item.status == "removed":
        paired = pairing(prepared.head, prepared.base, target, list(answer.targets))
        if paired is not None:
            answer = renamed_answer(answer, paired[1], paired[0])
        names = {target.name, *({without_span(paired[0]).rpartition("::")[2].split(".")[-1]} if paired else set())}
        relations = still_named(relations, prepared.head, prepared.changed, names)
    relations = mark_folding(relations, _delta(prepared, item, counts), snapshot)
    counts["scored"] += 1
    _write_scored(run.sink, {**context, "status": item.status, "snapshot": item.snapshot}, target, relations, answer)
    return missing_definition(target, answer)


def write_silent_loss(sink: Sink, context: dict, missing: list[dict], answer) -> None:
    """One row per scored impact answer, losses or none: a zero is then a witnessed zero, not an absent file."""
    sink.write("silent_loss.jsonl", [{**context, **silent_loss(missing, answer)}])


def _score_symbol(
    run: RepoRun, prepared: Prepared, item: ChangedDefinition, work: Path, resolver, context: dict, record: dict
) -> None:
    target = item.target
    answer_run = engine_answer(
        run, context["commit"], f"symbol-{target.name}", work, symbol_args(target.name, run.budget, run.engine_args)
    )
    record.update(symbol_seconds=round(answer_run.seconds, 3), symbol_error=answer_run.error)
    if not answer_run.ok or answer_run.data is None or not prepared.head.symbols_of(target):
        return
    answer = parse_answer(answer_run.data, resolver)
    folded = mark_folding(relations_to(prepared.head, target), _delta(prepared, item, {}), prepared.head)
    _write_scored(run.sink, {**context, "status": "sampled", "snapshot": "head"}, target, folded, answer)


def _write_scored(sink: Sink, context: dict, target: Definition, relations: list[Relation], answer) -> None:
    cells, details = compare(target, relations, answer)
    for row in (*cells, *details):
        row.update(context)
    sink.write("cells.jsonl", cells)
    sink.write("relations.jsonl", details)


def measure_repo(run: RepoRun, count: int) -> int:
    """Measure the sampled commits of one repository; returns how many ended `measured`."""
    commits = sample_commits(run.corpus, run.repo, run.git_dir, count)
    rng = random.Random(f"{run.corpus.sampling.seed}:{run.repo.name}:symbols")
    measured = 0
    for number, sha in enumerate(commits, 1):
        try:
            record = measure_commit(run, sha, rng)
        except Exception as error:  # one broken commit is counted and reported; the round goes on
            record = {"repo": run.repo.name, "commit": sha, "status": "error", "reason": f"{type(error).__name__}: {error}"}
        measured += record["status"] == "measured"
        run.sink.write("commits.jsonl", [record])
        print(f"[{run.repo.name} {number}/{len(commits)}] {sha[:10]} {record['status']} {record.get('reason', '')}", flush=True)
    return measured


def replayed_repositories(run: Path) -> list[str]:
    """The repositories a run answered: a replay scores those, however many the corpus holds now."""
    records = (json.loads(x) for x in (run / "commits.jsonl").read_text(encoding="utf-8").splitlines() if x.strip())
    return sorted({r["repo"] for r in records if r.get("status") == "measured"})


def unmeasured_languages(measured: dict[str, int]) -> list[str]:
    """Languages of the selected repositories none of whose commits was measured: the only reason the round fails."""
    return sorted(language for language, count in measured.items() if count == 0)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="measure", description=__doc__)
    parser.add_argument("--repo", action="append", help="repository name from corpus.toml (repeatable); default all")
    parser.add_argument("--commits", type=int, help="sampled commits per repository (overrides the corpus value)")
    parser.add_argument(
        "--fast", action="store_true", help="the `oracle` label subset: one repository per language, fast_commits each"
    )
    parser.add_argument(
        "--budget",
        type=parse_budget,
        help="tokens the engine answers within (`--budget N`; `uncapped` = the cap never bites); default: the engine's own",
    )
    parser.add_argument(
        "--engine-arg",
        action="append",
        default=[],
        metavar="ARG",
        help="an extra argument for the engine (repeatable), such as the switch that lists the rows it folds; kept in run.json",
    )
    parser.add_argument(
        "--folded",
        action="store_true",
        help="do not pass --unfold: score the answers the engine prints by default, folded lines instead of rows",
    )
    parser.add_argument(
        "--replay",
        type=Path,
        metavar="RUN",
        help="score the answers a measure run kept (RUN/answers) instead of asking an engine; --budget is the run's",
    )
    parser.add_argument("--out", type=Path, default=Path("out/latest"))
    parser.add_argument("--append", action="store_true", help="keep the results already in --out")
    parser.add_argument("--no-install", dest="install", action="store_false", help="do not install TypeScript dependencies")
    parser.add_argument("--retry-failed", action="store_true", help="index again snapshots whose cached index failed")
    options = parser.parse_args(argv)
    corpus = load_corpus()
    sink = Sink(options.out, fresh=not options.append)
    if options.replay is None:
        binary, budget = diffctx_binary(), options.budget
        engine_args = engine_arguments(binary, tuple(options.engine_arg), options.folded)
        write_run_meta(options.out, binary, budget, options.append, engine_args)
    else:
        stored = json.loads((options.replay / RUN_FILE).read_text(encoding="utf-8"))
        binary, budget, engine_args = Path("replay"), stored["budget"], tuple(stored.get("engine_args", []))
        if os.environ.get("DIFFCTX_BIN"):  # a query the replayed run never asked goes to the engine that made it
            binary = diffctx_binary()
            if binary.name != stored.get("binary"):
                raise SystemExit(f"--replay: {options.replay} was made by {stored.get('binary')}, not {binary.name}")
        shutil.copy(options.replay / RUN_FILE, options.out / RUN_FILE)
    measured: dict[str, int] = {}
    selected = options.repo or (replayed_repositories(options.replay) if options.replay else None)
    for repo in corpus.select(selected, options.fast):
        count = options.commits or (corpus.sampling.fast_commits if options.fast else corpus.sampling.commits)
        measured.setdefault(repo.language, 0)
        try:  # one repository that cannot be fetched or indexed is counted in the output files; the round goes on
            git_dir = gitrepo.mirror(repo.url, CACHE / "mirrors" / f"{repo.name}.git", repo.sha)
            run = RepoRun(
                corpus,
                repo,
                git_dir,
                sink,
                binary,
                options.install,
                options.retry_failed,
                corpus.sampling.definitions_per_repo,
                budget,
                options.replay,
                engine_args,
            )
            measured[repo.language] += measure_repo(run, count)
        except Exception as error:
            reason = f"{type(error).__name__}: {error}"
            sink.write("commits.jsonl", [{"repo": repo.name, "language": repo.language, "status": "error", "reason": reason}])
            print(f"[{repo.name}] failed: {reason}", flush=True)
    print("measured commits per language: " + ", ".join(f"{lang} {n}" for lang, n in sorted(measured.items())), flush=True)
    missing = unmeasured_languages(measured)
    if missing:
        print(f"FAILED: no {' and no '.join(missing)} commit was measured", file=sys.stderr)
    return 1 if missing else 0


if __name__ == "__main__":
    raise SystemExit(main())
