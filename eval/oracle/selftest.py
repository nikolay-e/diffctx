"""Fixtures with a known right answer: a body-only edit, a deletion and a move, in Python and TypeScript.

The unit tests check the oracle's own answer on them. `selftest.py --bin DIFFCTX_BIN` checks the engine under
test on the same commits: a build that ignores a body-only change, drops a caller or answers empty without
saying why makes it fail, which is how the oracle's gates are shown able to fail at all.
"""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path

import gitrepo
from changed import changed_definitions
from compare import compare, make_resolver, parse_answer
from measure import impact_args, run_diffctx
from model import Relation
from scipindex import Index
from truth import Snapshot

IDENTITY = ("-c", "user.name=oracle", "-c", "user.email=oracle@example.invalid")


@dataclass(frozen=True)
class Fixture:
    language: str
    base: dict[str, str]
    head: dict[str, str]
    changed: dict[str, str]  # definition key on its reporting side -> status
    caller: tuple[str, str]  # (caller key, body-only target key): a call both snapshots show


PYTHON = Fixture(
    language="python",
    base={
        "shop/__init__.py": "",
        "shop/pricing.py": (
            "RATE = 2\n\n\ndef total(items):\n    return sum(items)\n\n\ndef helper(x):\n    return x + 1\n\n\ndef mover(x):\n    return x * 3\n"
        ),
        "shop/checkout.py": "from shop.pricing import total\n\n\ndef charge(items):\n    return total(items)\n",
    },
    head={
        "shop/pricing.py": "RATE = 2\n\n\ndef total(items):\n    return sum(items) * RATE\n",
        "shop/util.py": "def mover(x):\n    return x * 3\n",
    },
    changed={
        "shop/pricing.py::total": "modified",
        "shop/pricing.py::helper": "removed",
        "shop/pricing.py::mover": "moved",
    },
    caller=("shop/checkout.py::charge", "shop/pricing.py::total"),
)

TYPESCRIPT = Fixture(
    language="typescript",
    base={
        "src/pricing.ts": (
            "export const RATE = 2;\n\n"
            "export function total(items: number[]): number {\n  return items.reduce((a, b) => a + b, 0);\n}\n\n"
            "export function helper(x: number): number {\n  return x + 1;\n}\n\n"
            "export function mover(x: number): number {\n  return x * 3;\n}\n"
        ),
        "src/checkout.ts": (
            'import { total } from "./pricing";\n\nexport function charge(items: number[]): number {\n  return total(items);\n}\n'
        ),
    },
    head={
        "src/pricing.ts": (
            "export const RATE = 2;\n\n"
            "export function total(items: number[]): number {\n  return items.reduce((a, b) => a + b, 0) * RATE;\n}\n"
        ),
        "src/util.ts": "export function mover(x: number): number {\n  return x * 3;\n}\n",
    },
    changed={
        "src/pricing.ts::total": "modified",
        "src/pricing.ts::helper": "removed",
        "src/pricing.ts::mover": "moved",
    },
    caller=("src/checkout.ts::charge", "src/pricing.ts::total"),
)

FIXTURES = {"python": PYTHON, "typescript": TYPESCRIPT}


def _git(repo: Path, *args: str) -> str:
    env = {**gitrepo.git_env(), "GIT_AUTHOR_DATE": "2026-01-01T00:00:00Z", "GIT_COMMITTER_DATE": "2026-01-01T00:00:00Z"}
    done = subprocess.run(["git", *IDENTITY, *args], cwd=repo, env=env, capture_output=True, text=True, check=True)
    return done.stdout.strip()


def build(fixture: Fixture, repo: Path) -> tuple[str, str]:
    """A git repository with the base commit and the head commit; returns both ids."""
    repo.mkdir(parents=True, exist_ok=True)
    _git(repo, "init", "-q", "-b", "main")
    for path, text in fixture.base.items():
        (repo / path).parent.mkdir(parents=True, exist_ok=True)
        (repo / path).write_text(text, encoding="utf-8")
    _git(repo, "add", "-A")
    _git(repo, "commit", "-q", "-m", "base")
    base = _git(repo, "rev-parse", "HEAD")
    for path, text in fixture.head.items():
        (repo / path).parent.mkdir(parents=True, exist_ok=True)
        (repo / path).write_text(text, encoding="utf-8")
    _git(repo, "add", "-A")
    _git(repo, "commit", "-q", "-m", "head")
    return base, _git(repo, "rev-parse", "HEAD")


def snapshots(fixture: Fixture, repo: Path, base: str, head: str) -> tuple[Snapshot, Snapshot]:
    git_dir = repo / ".git"
    trees = repo.parent / (repo.name + "-trees")
    before = Snapshot(gitrepo.export_tree(git_dir, base, trees / base), Index(), fixture.language)
    after = Snapshot(gitrepo.export_tree(git_dir, head, trees / head), Index(), fixture.language)
    return before, after


def oracle_changed(fixture: Fixture, repo: Path, base: str, head: str) -> dict[str, str]:
    """The oracle's own answer: changed definition key -> status."""
    before, after = snapshots(fixture, repo, base, head)
    changes = gitrepo.diff_changes(repo / ".git", base, head)
    found = changed_definitions(changes, before.definitions, after.definitions)
    return {c.base.key if c.base is not None else c.target.key: c.status for c in found}


def engine_problems(fixture: Fixture, repo: Path, base: str, head: str, binary: Path) -> list[str]:
    """What the engine under test gets wrong about the fixture's body-only change, as readable lines."""
    before, after = snapshots(fixture, repo, base, head)
    work = gitrepo.checkout(repo.parent / (repo.name + "-work"), repo / ".git", head)
    run = run_diffctx(binary, work, impact_args(head), 120)
    if not run.ok or run.data is None:
        return [f"diffctx failed: {run.error}"]
    answer = parse_answer(run.data, make_resolver(after.definitions, before.definitions))
    caller_key, target_key = fixture.caller
    target = next(d for d in after.definitions(target_key.split("::")[0]) if d.key == target_key)
    cells, details = compare(target, [Relation(caller_key, target_key, "calls", 0)], answer)
    problems = []
    for cell in cells:
        if cell["fn"]:
            problems.append(f"{target_key}: caller {caller_key} missing from the answer")
        if cell["fcn"]:
            problems.append(f"{target_key}: empty answer that names no limit")
    problems += [f"{target_key}: extra caller {d['from']}" for d in details if d["status"] == "fp"]
    return problems


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="selftest", description=__doc__)
    parser.add_argument("--bin", type=Path, default=os.environ.get("DIFFCTX_BIN"))
    options = parser.parse_args(argv)
    if options.bin is None:
        parser.error("give --bin or set DIFFCTX_BIN")
    failed = False
    with tempfile.TemporaryDirectory(prefix="oracle-selftest-") as scratch:
        for name, fixture in FIXTURES.items():
            repo = Path(scratch) / name
            base, head = build(fixture, repo)
            for problem in engine_problems(fixture, repo, base, head, Path(options.bin)):
                print(f"{name}: {problem}", file=sys.stderr)
                failed = True
    print("engine selftest " + ("FAILED" if failed else "passed"))
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
