"""One contract, three doors (#507): the native binary, the Python CLI and the
MCP server deliver the same artifact for the same range, budget and format,
each within its budget, because one fit measured on the delivered text cuts
all of them. The native binary serves JSON and YAML, the MCP server Markdown,
the Python CLI all of them. They used to fit in three places on three texts,
and the Python copy cut JSON and YAML by position: production before docs,
and the rewrite of a multi-fragment file before its one-line bumps (#438)."""

from __future__ import annotations

import asyncio
import json
import os
import re
import subprocess

import pytest
import yaml

from diffctx._diffctx import count_tokens
from tests.framework.pygit2_backend import Pygit2Repo

from .conftest import run_diffctx_subprocess

NATIVE_BIN = os.environ.get("DIFFCTX_NATIVE_BIN")
needs_native = pytest.mark.skipif(not NATIVE_BIN, reason="DIFFCTX_NATIVE_BIN names no native binary")

HELPERS = "HEAD~1..HEAD"
TIER = "HEAD~2..HEAD~1"
BOTH = "HEAD~2..HEAD"


def _engine(version: int) -> str:
    helpers = "".join(f"def helper_{i}(x):\n    return x + {i * version}\n\n\n" for i in range(5))
    steps = "".join(f"    step_{i} = helper_{i % 5}(rows[{i}]) * {version}\n" for i in range(60))
    return helpers + "def plan(rows):\n" + steps + "    return step_0\n"


@pytest.fixture(scope="module")
def repo(tmp_path_factory):
    repo = Pygit2Repo(tmp_path_factory.mktemp("contract") / "repo")
    repo.add_file("zsrc/x.py", "def compute(a, b):\n    return a + b\n\n\ndef helper(x):\n    return x * 2\n")
    repo.add_file("tests/test_x.py", "from zsrc.x import compute\n\n\ndef test_compute():\n    assert compute(1, 2) == 3\n")
    repo.add_file("README.md", "# Project\n\nCompute adds numbers.\n")
    repo.add_file("app/engine.py", _engine(1))
    repo.commit("initial")
    # #438's tier fixture: production sorts last (`zsrc/`), after the docs
    # and the tests it must outlast.
    body = "".join(f"    total += {i} * 0\n" for i in range(40))
    repo.add_file(
        "zsrc/x.py", f"def compute(a, b):\n    total = a + b\n{body}    return total\n\n\ndef helper(x):\n    return x * 2\n"
    )
    asserts = "".join(f"    assert compute({i}, 2) == {i}+2\n" for i in range(25))
    repo.add_file("tests/test_x.py", f"from zsrc.x import compute\n\n\ndef test_compute():\n{asserts}")
    docs = "".join(f"Line {i} of docs about compute.\n" for i in range(20))
    repo.add_file("README.md", f"# Project\n\nCompute adds numbers.\n\n{docs}")
    repo.commit("rewrite compute, its tests and docs")
    # The other half of #438: one-line helper bumps, then the rewrite of
    # `plan`, which comes after them in the file.
    repo.add_file("app/engine.py", _engine(2))
    repo.commit("bump the helpers, rewrite plan")
    return repo


def _native(repo, *args: str) -> str:
    out = subprocess.run([NATIVE_BIN, ".", *args, "-q"], cwd=repo.path, capture_output=True, text=True, encoding="utf-8")
    assert out.returncode == 0, out.stderr
    return out.stdout


def _cli(repo, *args: str) -> str:
    out = run_diffctx_subprocess([".", *args, "-q"], cwd=repo.path)
    assert out.returncode == 0, out.stderr
    return out.stdout


def _mcp(repo, **arguments) -> str:
    # The mcp extra is not installed everywhere (no cryptography wheel on
    # Windows on ARM); there the MCP door is skipped, the others still run.
    mcp = pytest.importorskip("diffctx.mcp.server", exc_type=ImportError).mcp

    result = asyncio.run(mcp.call_tool("diffctx_context", {"repo_path": str(repo.path), **arguments}))
    return result.content[0].text


_MD_HEADER = re.compile(r"^## (`+) ?(.+?) ?\1(.*)$")
_TXT_HEADER = re.compile(r"^  (\S.*?):(\d+-\d+)(?: \(.*\))?(?: \[.*\])?(?: <(\w+)>)?$")


def _fragments(text: str, fmt: str) -> list[tuple[str, str, str | None]]:
    if fmt in ("json", "yaml"):
        doc = json.loads(text) if fmt == "json" else yaml.safe_load(text)
        return [(f["path"], f["lines"], f.get("role")) for f in doc.get("fragments") or []]
    found = []
    for line in text.splitlines():
        if fmt == "md" and (m := _MD_HEADER.match(line)):
            path, _, lines = m.group(2).rpartition(":")
            found.append((path, lines, "changed" if m.group(3).endswith("— **changed**") else None))
        elif fmt == "txt" and (m := _TXT_HEADER.match(line)):
            found.append((m.group(1), m.group(2), m.group(3)))
    return found


@pytest.mark.parametrize("fmt", ["md", "json", "yaml"])
@pytest.mark.parametrize("budget", [700, 1100, 1500, 2600])
@pytest.mark.parametrize("diff_range", [HELPERS, TIER, BOTH])
def test_every_door_delivers_one_artifact_within_its_budget(repo, diff_range, budget, fmt):
    args = ("--diff", diff_range, "--budget", str(budget), "-f", fmt)
    doors = {"cli": _cli(repo, *args)}
    if fmt == "md":
        doors["mcp"] = _mcp(repo, diff_ref=diff_range, mode="pack", budget_tokens=budget)
    elif NATIVE_BIN:
        doors["native"] = _native(repo, *args)
    else:
        pytest.skip("DIFFCTX_NATIVE_BIN names no native binary")
    for door, text in doors.items():
        assert count_tokens(text) <= budget, f"{door} -f {fmt} delivered {count_tokens(text)} tokens"
    fragments = {door: _fragments(text, fmt) for door, text in doors.items()}
    assert fragments["cli"], "the fixture must leave something to compare"
    assert all(f == fragments["cli"] for f in fragments.values()), fragments
    assert all(text == doors["cli"] for text in doors.values()), "the doors deliver different documents"


@needs_native
@pytest.mark.parametrize("fmt", ["md", "json"])
@pytest.mark.parametrize("budget", [150, 400, 2000])
def test_impact_is_fitted_once_to_the_budget_asked(repo, budget, fmt):
    args = ("--diff", BOTH, "--mode", "impact", "--budget", str(budget), "-f", fmt)
    doors = {"native": _native(repo, *args), "cli": _cli(repo, *args)}
    if fmt == "md":
        doors["mcp"] = _mcp(repo, diff_ref=BOTH, mode="impact", budget_tokens=budget).rstrip("\n") + "\n"
    for door, text in doors.items():
        assert count_tokens(text) <= budget, f"{door} impact -f {fmt} delivered {count_tokens(text)} tokens"
    assert all(text == doors["native"] for text in doors.values()), doors


@pytest.mark.parametrize("fmt", ["md", "json", "yaml", "txt"])
@pytest.mark.parametrize("budget", [600, 700, 900, 1100, 1300])
def test_production_outlasts_its_tests_and_docs(repo, budget, fmt):
    shown = {path for path, _, _ in _fragments(_cli(repo, "--diff", TIER, "--budget", str(budget), "-f", fmt), fmt)}
    assert shown, "the fixture must leave something to cut"
    assert "zsrc/x.py" in shown, (fmt, shown)
    if "README.md" in shown:
        assert "tests/test_x.py" in shown, (fmt, shown)


@pytest.mark.parametrize("fmt", ["md", "json", "yaml", "txt"])
def test_a_larger_budget_never_hides_a_changed_hunk(repo, fmt):
    previous: set[str] = set()
    for budget in (900, 1100, 1300, 1500, 1700, 2000):
        text = _cli(repo, "--diff", HELPERS, "--budget", str(budget), "-f", fmt)
        heads = {lines.split("-")[0] for _, lines, role in _fragments(text, fmt) if role == "changed"}
        assert previous <= heads, f"at {budget} -f {fmt} the hunks starting at {sorted(previous - heads)} went missing"
        previous = heads
    assert "21" in previous, "plan's rewrite is the change the budget must keep"


@pytest.mark.parametrize("fmt", ["md", "json", "yaml", "txt"])
def test_the_raw_diff_is_not_charged_to_the_budget(repo, fmt):
    without = _fragments(_cli(repo, "--diff", BOTH, "--budget", "1500", "-f", fmt), fmt)
    with_patch = _fragments(_cli(repo, "--diff", BOTH, "--budget", "1500", "-f", fmt, "--with-raw-diff"), fmt)
    assert without
    assert with_patch == without
