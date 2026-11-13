"""R-budget ratchet: product size only goes down; a drop is recorded in the same commit."""

import re
import subprocess
from pathlib import Path

import tomllib

ROOT = Path(__file__).parent.parent
BUDGET = ROOT / "budget.toml"
NATIVE_SRC = "crates/diffctx-native/src/"
BIG_FILE = 1500
ALLOW = re.compile(r"#!?\[allow\(")


def _product_files() -> list[str]:
    tracked = subprocess.run(
        ["git", "ls-files", "-z", "--", ":(glob)crates/*/src/**", "src"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    ).stdout.split("\0")
    return [p for p in tracked if p.endswith((".rs", ".py")) and (ROOT / p).is_file()]


def _lines(path: str) -> int:
    return (ROOT / path).read_bytes().count(b"\n")


def _root_modules(files: list[str]) -> int:
    names = set()
    for path in files:
        if not path.startswith(NATIVE_SRC) or not path.endswith(".rs"):
            continue
        parts = path[len(NATIVE_SRC) :].split("/")
        if len(parts) == 1 and parts[0] not in ("lib.rs", "main.rs"):
            names.add(parts[0][:-3])
        elif len(parts) == 2 and parts[1] == "mod.rs":
            names.add(parts[0])
    return len(names)


def _measured() -> dict:
    files = _product_files()
    sizes = {p: _lines(p) for p in files}
    rust = [p for p in files if p.endswith(".rs")]
    return {
        "product_lines": sum(sizes.values()),
        "allow_count": sum(len(ALLOW.findall((ROOT / p).read_text(encoding="utf-8"))) for p in rust),
        "root_modules": _root_modules(files),
        "big_files": {p: n for p, n in sizes.items() if n > BIG_FILE},
    }


def test_product_budget_is_a_ratchet():
    recorded = tomllib.loads(BUDGET.read_text(encoding="utf-8"))
    measured = _measured()
    problems = []
    for key in ("product_lines", "allow_count", "root_modules"):
        if measured[key] > recorded[key]:
            problems.append(f"{key} grew {recorded[key]} -> {measured[key]}")
        elif measured[key] < recorded[key]:
            problems.append(f"{key} dropped {recorded[key]} -> {measured[key]}")
    for path, lines in measured["big_files"].items():
        before = recorded["big_files"].get(path)
        if before is None:
            problems.append(f"{path} is a new file over {BIG_FILE} lines ({lines})")
        elif lines > before:
            problems.append(f"{path} grew {before} -> {lines}")
        elif lines < before:
            problems.append(f"{path} dropped {before} -> {lines}")
    for path, before in recorded["big_files"].items():
        if path not in measured["big_files"]:
            problems.append(f"{path} left the over-{BIG_FILE} list (was {before})")
    assert not problems, (
        "budget.toml no longer matches the tree: "
        + "; ".join(problems)
        + ". A drop: lower budget.toml in the same commit. Growth needs the owner's word, "
        "recorded as a `Budget-raise:` commit trailer quoting it (AGENTS.md R-budget)."
    )
