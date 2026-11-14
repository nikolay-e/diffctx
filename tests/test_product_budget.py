"""R-budget ratchet: product size only goes down; a drop is recorded in the same commit.

A `[debt.*]` row in budget.toml is growth taken on loan, and only where it says: `covers` are new paths (a glob ending
`/**`, or a file) whose lines count against the row, `grows` are existing files that may add `plus` lines over their
recorded `base`. Every other product line obeys the plain ratchet. The row is repaid at the commit that makes its
`due_when` gone. A `covers` entry `crates/diffctx-native/src/<dir>/**` that is a root module stays out of `root_modules`
only while its row stands.
"""

import re
import subprocess
import tomllib
from pathlib import Path

ROOT = Path(__file__).parent.parent
BUDGET = ROOT / "budget.toml"
NATIVE_SRC = "crates/diffctx-native/src/"
BIG_FILE = 1500
ALLOW = re.compile(r"#!?\[allow\(")
PRODUCT_SUFFIXES = (".rs", ".py", ".scm", ".toml", ".yaml", ".yml", ".json")


def _product_files() -> list[str]:
    tracked = subprocess.run(
        ["git", "ls-files", "-z", "--", ":(glob)crates/*/src/**", "src"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    ).stdout.split("\0")
    return [p for p in tracked if p.endswith(PRODUCT_SUFFIXES) and (ROOT / p).is_file()]


def _lines(path: str) -> int:
    return (ROOT / path).read_bytes().count(b"\n")


def _root_modules(files: list[str], covers: tuple[str, ...] | list[str] = ()) -> int:
    names = set()
    for path in files:
        if not path.startswith(NATIVE_SRC) or not path.endswith(".rs"):
            continue
        parts = path[len(NATIVE_SRC) :].split("/")
        if len(parts) == 1 and parts[0] not in ("lib.rs", "main.rs"):
            names.add(parts[0][:-3])
        elif len(parts) == 2 and parts[1] == "mod.rs":
            names.add(parts[0])
    return len({n for n in names if f"{NATIVE_SRC}{n}/**" not in covers})


def _gone(root: Path, spec: str) -> bool:
    path, _, text = spec.partition("::")
    file = root / path
    return not file.is_file() or (bool(text) and text not in file.read_text(encoding="utf-8"))


def _debt_problems(root: Path, debt: dict) -> list[str]:
    problems = []
    for name, row in debt.items():
        missing = {"lines", "repaid_at", "due_when", "deletes", "covers"} - row.keys()
        if missing or not isinstance(row["lines"], int) or row["lines"] <= 0 or not row["deletes"]:
            problems.append(f"debt {name!r} needs lines > 0, repaid_at, due_when, deletes, covers (missing {sorted(missing)})")
            continue
        if not all(isinstance(g.get("base"), int) and isinstance(g.get("plus"), int) for g in row.get("grows", {}).values()):
            problems.append(f"debt {name!r}: every grows entry needs integer base and plus")
            continue
        if _gone(root, row["due_when"]):
            left = [d for d in row["deletes"] if not _gone(root, d)]
            problems.append(
                f"debt {name!r} (+{row['lines']}) is due, {row['due_when']!r} is gone: remove its row, "
                f"lower budget.toml, and delete what it promised ({row['repaid_at']}); still present: {left or 'none'}"
            )
        elif absent := [d for d in row["deletes"] if _gone(root, d)]:
            problems.append(f"debt {name!r} lists {absent}, which no longer exist: edit the row (the amount stays)")
    return problems


def _measured(debt: dict) -> dict:
    files = _product_files()
    sizes = {p: _lines(p) for p in files}
    rust = [p for p in files if p.endswith(".rs")]
    return {
        "sizes": sizes,
        "allow_count": sum(len(ALLOW.findall((ROOT / p).read_text(encoding="utf-8"))) for p in rust),
        "root_modules": _root_modules(files, [c for row in debt.values() for c in row["covers"]]),
    }


def _covered(path: str, patterns: list[str]) -> bool:
    return any(path.startswith(p[:-2]) if p.endswith("/**") else path == p for p in patterns)


def _size_problems(recorded: dict, sizes: dict[str, int]) -> list[str]:
    covered: set[str] = set()
    grows: dict[str, dict] = {}
    problems = []
    for name, row in recorded.get("debt", {}).items():
        mine = {p for p in sizes if _covered(p, row["covers"])}
        covered |= mine
        grows |= row.get("grows", {})
        used = sum(sizes[p] for p in mine)
        for path, g in row.get("grows", {}).items():
            n = sizes.get(path, 0)
            used += max(0, n - g["base"])
            if n > g["base"] + g["plus"]:
                problems.append(f"{path} grew {g['base']} -> {n}, debt {name!r} allows +{g['plus']}")
            elif n < g["base"]:
                problems.append(f"{path} dropped {g['base']} -> {n}: lower its base and product_lines together")
        if used > row["lines"]:
            problems.append(f"debt {name!r} covers {used} lines, +{row['lines']} recorded")
    outside = sum(n for p, n in sizes.items() if p not in covered and p not in grows)
    before = recorded["product_lines"] - sum(g["base"] for g in grows.values())
    if outside > before:
        problems.append(f"product lines outside the debt grew {before} -> {outside}")
    elif outside < before:
        problems.append(f"product lines outside the debt dropped {before} -> {outside}")
    big = {p: n for p, n in sizes.items() if n > BIG_FILE}
    for path, lines in big.items():
        was = recorded["big_files"].get(path)
        ceiling = None if was is None else was + grows.get(path, {}).get("plus", 0)
        if was is None:
            problems.append(f"{path} is a new file over {BIG_FILE} lines ({lines})")
        elif lines > ceiling:
            problems.append(f"{path} grew {was} -> {lines}")
        elif lines < was:
            problems.append(f"{path} dropped {was} -> {lines}")
    for path, was in recorded["big_files"].items():
        if path not in big:
            problems.append(f"{path} left the over-{BIG_FILE} list (was {was})")
    return problems


def test_product_budget_is_a_ratchet():
    recorded = tomllib.loads(BUDGET.read_text(encoding="utf-8"))
    measured = _measured(recorded.get("debt", {}))
    problems = _size_problems(recorded, measured["sizes"])
    for key in ("allow_count", "root_modules"):
        if measured[key] > recorded[key]:
            problems.append(f"{key} grew {recorded[key]} -> {measured[key]}")
        elif measured[key] < recorded[key]:
            problems.append(f"{key} dropped {recorded[key]} -> {measured[key]}")
    assert not problems, (
        "budget.toml no longer matches the tree: "
        + "; ".join(problems)
        + ". A drop: lower budget.toml in the same commit. Growth needs the owner's word, "
        "recorded as a `Budget-raise:` commit trailer quoting it (AGENTS.md R-budget)."
    )


def test_every_debt_is_open_and_names_what_repays_it():
    debt = tomllib.loads(BUDGET.read_text(encoding="utf-8")).get("debt", {})
    assert not (problems := _debt_problems(ROOT, debt)), "; ".join(problems)


def test_a_due_debt_is_red_until_its_row_and_deletes_are_gone(tmp_path):
    (tmp_path / "decide.rs").write_text("fn status() {}\nfn lost_namers() {}\n", encoding="utf-8")
    row = {
        "lines": 10,
        "covers": [],
        "repaid_at": "the switch",
        "due_when": "decide.rs::fn status(",
        "deletes": ["decide.rs::fn status(", "decide.rs::fn lost_namers("],
    }
    assert _debt_problems(tmp_path, {"d": row}) == []
    (tmp_path / "decide.rs").write_text("fn lost_namers() {}\n", encoding="utf-8")
    [problem] = _debt_problems(tmp_path, {"d": row})
    assert "is due" in problem and "still present: ['decide.rs::fn lost_namers(']" in problem
    (tmp_path / "decide.rs").write_text("", encoding="utf-8")
    [problem] = _debt_problems(tmp_path, {"d": row})
    assert "still present: none" in problem
    assert _debt_problems(tmp_path, {}) == []
    (tmp_path / "decide.rs").write_text("fn status() {}\n", encoding="utf-8")
    [problem] = _debt_problems(tmp_path, {"d": row})
    assert "no longer exist" in problem


def test_unrelated_growth_cannot_hide_under_a_debt():
    debt = {"lines": 60, "covers": ["new/**", "one.rs"], "grows": {"big.rs": {"base": 1600, "plus": 10}}}
    recorded = {"product_lines": 1601, "big_files": {"big.rs": 1600}, "debt": {"d": debt}}
    sizes = {"old.rs": 1, "big.rs": 1600, "new/a.rs": 40, "one.rs": 10}
    assert _size_problems(recorded, sizes) == []
    assert _size_problems(recorded, sizes | {"big.rs": 1610}) == []
    assert _size_problems(recorded, sizes | {"big.rs": 1611})[0] == "big.rs grew 1600 -> 1611, debt 'd' allows +10"
    assert _size_problems(recorded, sizes | {"old.rs": 2}) == ["product lines outside the debt grew 1 -> 2"]
    assert _size_problems(recorded, sizes | {"new/b.rs": 11}) == ["debt 'd' covers 61 lines, +60 recorded"]
    assert _size_problems(recorded, sizes | {"big.rs": 1599})[0].startswith("big.rs dropped 1600 -> 1599")
    assert _size_problems(recorded, {"old.rs": 1, "big.rs": 1600, "elsewhere.rs": 5}) == [
        "product lines outside the debt grew 1 -> 6"
    ]


def test_a_covered_new_root_module_counts_only_while_its_row_stands():
    files = [f"{NATIVE_SRC}{p}" for p in ("lib.rs", "a.rs", "occurrence/mod.rs", "occurrence/x.rs")]
    assert _root_modules(files) == 2
    assert _root_modules(files, [f"{NATIVE_SRC}occurrence/**"]) == 1
    assert _root_modules(files, [f"{NATIVE_SRC}other/**", f"{NATIVE_SRC}a.rs"]) == 2
