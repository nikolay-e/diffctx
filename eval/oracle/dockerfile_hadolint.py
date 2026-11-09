#!/usr/bin/env python3
"""Checks the Dockerfile cases' stage relations against hadolint.

The truth source of the Dockerfile cell for the stage graph: hadolint reads each
`COPY --from` against the `FROM` aliases before it. DL3022 ("COPY --from should
reference a previously defined FROM alias") flags a name no earlier stage has,
DL3023 ("COPY --from cannot reference its own FROM alias") one that names its own
stage; the ids and titles are from hadolint's wiki. It runs offline and needs no
Docker daemon. A stage is named by its alias or by its position, and the tool
accepts a position.

A case's `reads` relation to a stage is checked on the line it states:

- `resolved`: hadolint raises none of the two rules there;
- `candidate`: the name differs from the stage's only by case, which the cell does
  not decide (the Dockerfile reference says nothing on case, BuildKit lowercases
  names, hadolint reads them as written): hadolint must raise DL3022 there, so that
  the disagreement stays pinned and visible.

A relation to an `ARG` or `ENV` is no stage graph: hadolint has no rule for it, and
the case's construction is its truth. The oracle is pinned to the hadolint it was
validated with and fails loudly on another: a bump is a reviewed change.

    dockerfile_hadolint.py [case.yaml ...]     # default: every Dockerfile case

Exit status: 0 when every checked relation agrees, 1 when one does not, 2 when the
tool is not the pinned one or could not be run.
"""

from __future__ import annotations

import json
import re
import subprocess
import sys
import tempfile
from pathlib import Path

import yaml

PINNED = "2.15.1"
CASES = Path(__file__).resolve().parents[2] / "tests" / "cases" / "impact" / "dockerfile"
RULES = ("DL3022", "DL3023")
STAGE = re.compile(r"^\s*FROM\s+(?:--\S+\s+)*\S+(?:\s+AS\s+(?P<name>\S+))?", re.IGNORECASE)

# What the tool answers on shapes known by construction: (text, hits as (line, rule)).
FIXTURES = [
    (
        "FROM alpine:3.20 AS build\nRUN echo hi > /a\nFROM alpine:3.20 AS final\n"
        "COPY --from=build /a /b\nCOPY --from=0 /a /c\nCOPY --from=Build /a /d\n"
        "COPY --from=late /a /e\nFROM alpine:3.20 AS late\nCOPY --from=final /a /f\n",
        [(6, "DL3022"), (7, "DL3022")],
    ),
    ("FROM alpine:3.20 AS one\nCOPY --from=one /a /b\n", [(2, "DL3023")]),
]


def hadolint(text: str) -> list[tuple[int, str]]:
    with tempfile.TemporaryDirectory() as tmp:
        path = Path(tmp) / "Dockerfile"
        path.write_text(text)
        done = subprocess.run(
            ["hadolint", "--no-fail", "-f", "json", str(path)],
            capture_output=True,
            text=True,
            timeout=60,
        )
    if done.returncode != 0:
        sys.exit(f"dockerfile_hadolint: hadolint failed: {done.stderr.strip()}")
    return sorted((item["line"], item["code"]) for item in json.loads(done.stdout) if item["code"] in RULES)


def check_version() -> None:
    try:
        found = subprocess.run(["hadolint", "--version"], capture_output=True, text=True, check=True).stdout
    except (OSError, subprocess.SubprocessError) as error:
        print(f"dockerfile_hadolint: could not run hadolint: {error}", file=sys.stderr)
        sys.exit(2)
    if PINNED not in found:
        print(
            f"dockerfile_hadolint: validated with hadolint {PINNED}, found {found.strip()}; a bump is a reviewed change",
            file=sys.stderr,
        )
        sys.exit(2)


def head_files(case: dict) -> dict[str, str]:
    repo = case["repo"]
    files = {**repo["initial_files"], **repo.get("changed_files", {})}
    for path in repo.get("deleted_files", []):
        files.pop(path, None)
    return files


def stages(text: str) -> set[str]:
    """The names a `--from` can use in a file: aliases as written and positions."""
    found: set[str] = set()
    for index, match in enumerate(m for line in text.splitlines() if (m := STAGE.match(line))):
        found.add(str(index))
        if match["name"]:
            found.add(match["name"])
    return found


def check_fixtures() -> list[str]:
    failures = []
    for text, expected in FIXTURES:
        found = hadolint(text)
        if found != expected:
            failures.append(f"fixture {text.splitlines()[0]!r}: expected {expected}, hadolint says {found}")
    return failures


def check_case(path: Path) -> list[str]:
    case = yaml.safe_load(path.read_text())
    files = head_files(case)
    hits = {name: {line for line, _ in hadolint(text)} for name, text in files.items() if Path(name).name == "Dockerfile"}
    failures = []
    for relation in case.get("expect", {}).get("relations", []):
        source, _, _ = relation["from"].partition("::")
        _, _, symbol = relation["to"].partition("::")
        if source not in hits or symbol not in stages(files[source]):
            continue
        line = relation.get("site")
        resolution = relation.get("resolution", "resolved")
        flagged = line in hits[source]
        if resolution == "resolved" and flagged:
            failures.append(f"{path.name}: {source}:{line} is resolved, hadolint flags it")
        if resolution == "candidate" and not flagged:
            failures.append(f"{path.name}: {source}:{line} is a case-fold candidate, hadolint does not flag it")
    return failures


def main(argv: list[str]) -> int:
    check_version()
    cases = [Path(a) for a in argv] or sorted(CASES.glob("*.yaml"))
    failures = check_fixtures()
    for case in cases:
        failures.extend(check_case(case))
    for failure in failures:
        print(f"dockerfile_hadolint: {failure}", file=sys.stderr)
    print(f"dockerfile_hadolint: {len(cases)} cases, {len(FIXTURES)} fixtures, {len(failures)} disagreements")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
