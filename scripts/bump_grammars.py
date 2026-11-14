#!/usr/bin/env python3
"""Moves the tree-sitter grammars to their latest releases, and records what that buys.

bump_grammars.py bump
    Rewrites the version requirement of every `tree-sitter-*` dependency of
    crates/diffctx-native/Cargo.toml to the latest release and updates those
    packages in Cargo.lock. Exact pins (`=x.y.z`) are reviewed, not taken. Prints
    a Markdown table of what moved.

bump_grammars.py witnesses BASE HEAD
    BASE and HEAD are outputs of `examples/grammar_scan` before and after the
    bump. A blob that is `error` in BASE and `ok` in HEAD is one a newer grammar
    reads and the pinned one did not: it is added to
    crates/diffctx-native/src/facts/grammar_witnesses.txt, which is how a parse
    error is named `grammar_outdated` rather than `parse_error` (facts/grammar.rs).
"""

from __future__ import annotations

import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MANIFEST = ROOT / "crates/diffctx-native/Cargo.toml"
WITNESSES = ROOT / "crates/diffctx-native/src/facts/grammar_witnesses.txt"
DEPENDENCY = re.compile(r'^(?P<name>tree-sitter-[\w-]+)\s*=\s*(?P<head>\{\s*version\s*=\s*|)"(?P<req>[^"]+)"')


def latest(name: str) -> str:
    # From a directory that depends on nothing, so the answer is the registry's.
    with tempfile.TemporaryDirectory() as scratch:
        env = {**os.environ, "CARGO_TERM_COLOR": "never"}
        out = subprocess.run(
            ["cargo", "info", name],
            cwd=scratch,
            env=env,
            capture_output=True,
            text=True,
            check=True,
        ).stdout
    for line in out.splitlines():
        if line.startswith("version:"):
            return line.split()[1]
    raise SystemExit(f"cargo info {name}: no version line:\n{out}")


def requirement(version: str, like: str) -> str:
    """`version` cut to as many components as the requirement it replaces."""
    keep = max(len(like.split(".")), 2)
    return ".".join(version.split(".")[:keep])


def bump() -> None:
    lines = MANIFEST.read_text().splitlines(keepends=True)
    moved: list[tuple[str, str, str]] = []
    for at, line in enumerate(lines):
        found = DEPENDENCY.match(line)
        if not found or found["req"].startswith("="):
            continue
        name, old = found["name"], found["req"]
        new = requirement(latest(name), old)
        if new != old:
            lines[at] = line.replace(f'"{old}"', f'"{new}"', 1)
            moved.append((name, old, new))
    MANIFEST.write_text("".join(lines))
    # Whatever lies inside a requirement that did not change moves too.
    names = [m["name"] for line in lines if (m := DEPENDENCY.match(line)) and not m["req"].startswith("=")]
    subprocess.run(
        ["cargo", "update", *[a for n in names for a in ("-p", n)]],
        cwd=ROOT,
        check=True,
    )
    print("| grammar | was | now |\n|---|---|---|")
    for name, old, new in moved:
        print(f"| {name} | {old} | {new} |")
    if not moved:
        print("| (no requirement moved; the lock was refreshed) | | |")


def scan(path: str) -> dict[tuple[str, str], str]:
    out: dict[tuple[str, str], str] = {}
    for line in Path(path).read_text().splitlines():
        language, oid, status = line.split()
        # A blob read twice (two paths) is broken if any reading broke.
        if out.get((language, oid)) != "error":
            out[(language, oid)] = status
    return out


def witnesses(base: str, head: str) -> None:
    before, after = scan(base), scan(head)
    found = {key for key, status in before.items() if status == "error" and after.get(key) == "ok"}
    text = WITNESSES.read_text().splitlines()
    comments = [line for line in text if line.startswith("#")]
    kept = {tuple(line.split(" ", 1)) for line in text if line and not line.startswith("#")}
    merged = sorted(kept | found)
    WITNESSES.write_text("\n".join(comments + [f"{language} {oid}" for language, oid in merged]) + "\n")
    print(f"{len(found)} new witness(es), {len(merged)} in all", file=sys.stderr)


def main(argv: list[str]) -> int:
    if argv[:1] == ["bump"] and len(argv) == 1:
        bump()
    elif argv[:1] == ["witnesses"] and len(argv) == 3:
        witnesses(argv[1], argv[2])
    else:
        print(__doc__, file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
