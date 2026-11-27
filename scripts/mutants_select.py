#!/usr/bin/env python3
"""The selection arithmetic of .github/workflows/mutants.yml.

Every mutant is a full name as `cargo mutants --list` prints it. The fast phase leaves, per package, the
survivors (missed or timed out) and the mutants it never ran; `survive` turns them into the heavy jobs,
`slice` makes one heavy job run exactly its share, `verdict` says which survivors no heavy job judged.
cargo-mutants cannot be asked for an exact list: `--examine-re` and `--exclude-re` skip the mutants of
the struct-field genre (`delete field`), which then run in every job. `--iterate` skips what an earlier
run caught, so a slice is run as "everything else was caught".
"""

import argparse
import json
import math
import os
import sys
from pathlib import Path

KINDS = ("caught", "missed", "timeout", "unviable")
SUMMARY = {"CaughtMutant": "caught", "MissedMutant": "missed", "Timeout": "timeout", "Unviable": "unviable"}


def read(path: Path) -> list[str]:
    try:
        return [line.rstrip("\n") for line in path.read_text().splitlines() if line.strip()]
    except FileNotFoundError:
        return []


def outcomes(d: Path) -> dict[str, list[str]]:
    """The mutants of one cargo-mutants output by verdict: outcomes.json has them all, unviable.txt is not always written."""
    by_kind: dict[str, list[str]] = {k: [] for k in KINDS}
    try:
        found = json.loads((d / "outcomes.json").read_text())["outcomes"]
    except FileNotFoundError:
        return by_kind
    for o in found:
        if "Mutant" in o["scenario"] and o["summary"] in SUMMARY:
            by_kind[SUMMARY[o["summary"]]].append(o["scenario"]["Mutant"]["name"])
    return by_kind


def write(path: Path, names: list[str]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("".join(f"{n}\n" for n in names))


def artifacts(root: Path, prefix: str, pkg: str) -> list[Path]:
    return sorted(root.glob(f"{prefix}-{pkg}-*"), key=lambda d: int(d.name.rsplit("-", 1)[1]))


def judged_names(roots: list[Path], pkg: str) -> set[str]:
    return {n for root in roots for d in artifacts(root, "heavy", pkg) for names in outcomes(d).values() for n in names}


def survive(a: argparse.Namespace) -> None:
    fast, out = Path(a.fast), Path(a.out)
    done_roots = [Path(r) for r in a.done]
    pkgs = sorted({d.name.split("-")[1] for d in fast.glob("fast-*")})
    todo_by_pkg: dict[str, list[str]] = {}
    rows = []
    for pkg in pkgs:
        listed: list[str] = []
        judged: set[str] = set()
        survivors: set[str] = set()
        for d in artifacts(fast, "fast", pkg):
            listed += read(d / "shard-list.txt")
            by_kind = outcomes(d)
            judged.update(*by_kind.values())
            survivors.update(by_kind["missed"], by_kind["timeout"])
        unrun = [n for n in listed if n not in judged]
        survivors.update(unrun)
        ordered = [n for n in listed if n in survivors]
        done = judged_names(done_roots, pkg)
        todo_by_pkg[pkg] = [n for n in ordered if n not in done]
        write(out / f"survivors-{pkg}.txt", ordered)
        write(out / f"todo-{pkg}.txt", todo_by_pkg[pkg])
        write(out / f"unrun-{pkg}.txt", unrun)
        rows.append((pkg, len(listed), len(judged), len(ordered) - len(unrun), len(unrun), len(done), len(todo_by_pkg[pkg])))
    per_job = a.per_job
    while sum(math.ceil(len(t) / per_job) for t in todo_by_pkg.values()) > a.max_jobs:
        per_job += 1
    jobs = []
    for pkg, todo in todo_by_pkg.items():
        of = math.ceil(len(todo) / per_job)
        jobs += [{"pkg": pkg, "shard": k, "of": of, "count": len(todo[k::of])} for k in range(of)]
    print("| package | listed | judged by fast | fast survivors | never run by fast | judged earlier | heavy todo |")
    print("|---|---|---|---|---|---|---|")
    for row in rows:
        print("| " + " | ".join(map(str, row)) + " |")
    print(f"\nheavy jobs: {len(jobs)} at {per_job} mutants or fewer")
    github_output = os.environ.get("GITHUB_OUTPUT")
    if github_output:
        with open(github_output, "a") as fh:
            fh.write(f"matrix={json.dumps({'include': jobs})}\njobs={len(jobs)}\n")


def slice_(a: argparse.Namespace) -> None:
    todo = read(Path(a.todo))
    mine = todo[a.index :: a.of]
    full = read(Path(a.list))
    absent = sorted(set(mine) - set(full))
    if absent:
        sys.exit(f"{len(absent)} survivors are not in this tree's mutant list (a different commit?), first: {absent[0]}")
    keep = set(mine)
    out = Path(a.out)
    write(out / "mutants.out" / "caught.txt", [n for n in full if n not in keep])
    write(out / "mutants.out" / "unviable.txt", [])
    write(out / "slice.txt", mine)
    print(f"{a.pkg} {a.index + 1}/{a.of}: {len(mine)} of {len(full)} mutants")


def verdict(a: argparse.Namespace) -> None:
    survivors_dir, out = Path(a.survivors), Path(a.out)
    roots = [Path(r) for r in a.heavy]
    lines = [
        "| package | survivors of the fast suites | caught | missed | timeout | unviable | unjudged |",
        "|---|---|---|---|---|---|---|",
    ]
    unjudged_total = 0
    for survivors_file in sorted(survivors_dir.glob("survivors-*.txt")):
        pkg = survivors_file.stem.split("-", 1)[1]
        survivors = read(survivors_file)
        by_kind: dict[str, set[str]] = {k: set() for k in KINDS}
        for root in roots:
            for d in artifacts(root, "heavy", pkg):
                for k, names in outcomes(d).items():
                    by_kind[k].update(names)
        judged = set().union(*by_kind.values())
        unjudged = [n for n in survivors if n not in judged]
        unjudged_total += len(unjudged)
        survived = [n for n in survivors if n in by_kind["missed"] or n in by_kind["timeout"]]
        write(out / f"missed-{pkg}.txt", survived)
        write(out / f"unjudged-{pkg}.txt", unjudged)
        counts = [len([n for n in survivors if n in by_kind[k]]) for k in KINDS]
        lines.append(f"| {pkg} | {len(survivors)} | " + " | ".join(map(str, counts)) + f" | {len(unjudged)} |")
    text = "\n".join(lines) + "\n"
    (out / "verdict.md").write_text(text)
    print(text)
    if unjudged_total:
        sys.exit(f"{unjudged_total} survivors have no heavy verdict: the table above is a sample, not the answer")


def counts(a: argparse.Namespace) -> None:
    by_kind = outcomes(Path(a.dir))
    print(", ".join(f"{k} {len(by_kind[k])}" for k in KINDS))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(required=True)
    s = sub.add_parser("survive", help="fast artifacts -> survivors, heavy todo lists, heavy matrix")
    s.add_argument("--fast", required=True, help="directory holding the fast-<pkg>-<k> artifacts")
    s.add_argument("--done", nargs="*", default=[], help="directories of heavy-<pkg>-<j> artifacts of an earlier run")
    s.add_argument("--out", required=True)
    s.add_argument("--per-job", type=int, required=True)
    s.add_argument("--max-jobs", type=int, default=40)
    s.set_defaults(run=survive)
    c = sub.add_parser("slice", help="make `cargo mutants --iterate` run one share of the todo list")
    c.add_argument("--pkg", required=True)
    c.add_argument("--index", type=int, required=True)
    c.add_argument("--of", type=int, required=True)
    c.add_argument("--list", required=True, help="file with the output of `cargo mutants --list`")
    c.add_argument("--todo", required=True)
    c.add_argument("--out", required=True)
    c.set_defaults(run=slice_)
    v = sub.add_parser("verdict", help="which survivors no heavy job judged")
    v.add_argument("--survivors", required=True, help="directory with survivors-<pkg>.txt")
    v.add_argument("--heavy", nargs="+", required=True, help="directories of heavy-<pkg>-<j> artifacts")
    v.add_argument("--out", required=True)
    v.set_defaults(run=verdict)
    n = sub.add_parser("counts", help="one line of verdict counts for a fast or heavy output directory")
    n.add_argument("dir")
    n.set_defaults(run=counts)
    args = parser.parse_args()
    args.run(args)


if __name__ == "__main__":
    main()
