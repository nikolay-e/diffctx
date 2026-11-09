#!/usr/bin/env python3
"""Checks the Terraform cases' same-module relations against `terraform graph`.

The truth source of the Terraform cell for what one module reads of itself
(`var.*`, `local.*`, resources and data sources, `depends_on`): the edges of
`terraform graph -type=plan` on the directory, which read the configuration and
nothing else. It runs on a fixture that uses no provider but the built-in one,
needs no `init` and makes no network request; a directory that calls a module
needs `init` to install it, so the relations through `module.*` are the cases'
construction and are not checked here. A tool's edge is a dependency of the
node it starts at on the node it ends at, which is what a `reads` relation of
the case states.

The oracle is pinned to the Terraform it was validated with and fails loudly on
another: a bump is a reviewed change.

    terraform_graph.py [case.yaml ...]     # default: every Terraform case
    terraform_graph.py --edges DIR         # print a directory's edges

Exit status: 0 when every checked relation is an edge, 1 when one is not, 2
when the tool is not the pinned one or could not be run.
"""

from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

import yaml

PINNED = "1.16.2"
CASES = Path(__file__).resolve().parents[2] / "tests" / "cases" / "impact" / "terraform"
EDGE = re.compile(r'"\[root\] (?P<from>[^"]+)" -> "\[root\] (?P<to>[^"]+)"')
SUFFIX = re.compile(r" \((expand|close)\)$")


def terraform(*args: str, cwd: Path | None = None) -> subprocess.CompletedProcess[str]:
    env = {**os.environ, "CHECKPOINT_DISABLE": "1", "TF_IN_AUTOMATION": "1", "NO_COLOR": "1"}
    return subprocess.run(["terraform", *args], cwd=cwd, env=env, capture_output=True, text=True, timeout=120)


def check_version() -> None:
    try:
        found = json.loads(terraform("version", "-json").stdout)["terraform_version"]
    except (OSError, ValueError, KeyError, subprocess.SubprocessError) as error:
        sys.exit(f"terraform_graph: could not run terraform: {error}")
    if found != PINNED:
        sys.exit(f"terraform_graph: validated with Terraform {PINNED}, found {found}; a bump is a reviewed change")


def edges_of(directory: Path) -> set[tuple[str, str]] | None:
    """The dependencies of a directory as (address, address); None when the tool needs `init`."""
    done = terraform("graph", "-type=plan", cwd=directory)
    if done.returncode != 0:
        return None
    found: set[tuple[str, str]] = set()
    for line in done.stdout.splitlines():
        match = EDGE.search(line)
        if match:
            found.add((SUFFIX.sub("", match["from"]), SUFFIX.sub("", match["to"])))
    return found


def head_files(case: dict) -> dict[str, str]:
    repo = case["repo"]
    files = {**repo["initial_files"], **repo.get("changed_files", {})}
    for path in repo.get("deleted_files", []):
        files.pop(path, None)
    return files


def address(node: str) -> tuple[str, str] | None:
    """(directory, address) of `path::var.region`; None for a file or a bare name."""
    path, _, symbol = node.partition("::")
    if not symbol or "." not in symbol:
        return None
    return str(Path(path).parent), symbol


def check_case(path: Path) -> tuple[int, int, list[str]]:
    case = yaml.safe_load(path.read_text())
    # What a removal leaves behind is a stale name: the head holds no edge to it.
    if case["expect"].get("removed"):
        return 0, 0, []
    wanted: dict[str, list[tuple[str, str]]] = {}
    for relation in case["expect"].get("relations", []):
        if relation["kind"] != "reads":
            continue
        source, target = address(relation["from"]), address(relation["to"])
        if not source or not target or source[0] != target[0]:
            continue
        if source[1].startswith("module.") or target[1].startswith("module."):
            continue
        wanted.setdefault(source[0], []).append((source[1], target[1]))
    checked, skipped, failures = 0, 0, []
    if not wanted:
        return 0, 0, []
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        for relative, text in head_files(case).items():
            target = root / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(text)
        for directory, pairs in wanted.items():
            edges = edges_of(root / directory)
            if edges is None:
                skipped += len(pairs)
                continue
            for source, target in pairs:
                checked += 1
                if (source, target) not in edges:
                    failures.append(f"{path.stem}: terraform has no edge {source} -> {target} in {directory or '.'}")
    return checked, skipped, failures


def main(argv: list[str]) -> int:
    if shutil.which("terraform") is None:
        sys.exit("terraform_graph: terraform is not installed")
    check_version()
    if argv[:1] == ["--edges"]:
        for source, target in sorted(edges_of(Path(argv[1])) or []):
            print(f"{source} -> {target}")
        return 0
    cases = [Path(a) for a in argv] or sorted(CASES.glob("*.yaml"))
    total, skipped, failures = 0, 0, []
    for case in cases:
        checked, passed_over, wrong = check_case(case)
        total += checked
        skipped += passed_over
        failures += wrong
    print(f"terraform {PINNED}: {total} relation(s) checked, {skipped} need init and are not, {len(failures)} differ")
    for failure in failures:
        print(f"  {failure}")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
