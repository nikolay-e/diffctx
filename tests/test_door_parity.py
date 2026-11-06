"""One question, three doors: the native binary, the Python CLI and the MCP
server answer the same range, symbol and budget with the same claims.

The doors share the engine, so every difference is a door adding, dropping or
re-fitting something of its own. What must be identical is what the answers
say, not how they are wrapped:

- impact and symbol answers: the same text from every door, and the same
  relation set from the two JSON doors;
- pack: the same fragments (path and lines) from every door and format;
- locate: the same ranked ids in the same order;
- a refused symbol: the same refusal.

A difference that exists today is an `xfail(strict=True)` entry in
KNOWN_DIFFERENCES, keyed by the test id and carrying its tracker reference
(`gh#N` or `fj#N`): strict, so the entry has to go when the door is fixed.

The native binary is `DIFFCTX_NATIVE_BIN`, or `target/release/diffctx` when
that exists; without one the native door is left out and the other two still
compare. The MCP door is left out where the `mcp` extra is not installed.
"""

from __future__ import annotations

import asyncio
import importlib
import json
import os
import re
import subprocess
from pathlib import Path
from typing import Any

import pytest

from .conftest import run_diffctx_subprocess

PROJECT_ROOT = Path(__file__).resolve().parent.parent

# test id (`test_name[case]`) -> "gh#N" / "fj#N". Filled from a run: an entry is
# a door that differs from the others today, not a verdict on which one is right.
KNOWN_DIFFERENCES: dict[str, str] = {}

RANGES = {"committed": "HEAD~1..HEAD", "staged": "staged", "worktree": "HEAD"}
BUDGETS = [2000, 150]
# Pack has to leave a fragment: 150 tokens is an error in every door.
PACK_BUDGETS = [8000, 700]
SYMBOLS = ["total", "shop/pricing.py:total", "charge"]
MISSING = "nosuchname"


@pytest.fixture(autouse=True)
def _known_difference(request):
    reason = KNOWN_DIFFERENCES.get(request.node.name)
    if reason:
        request.node.add_marker(pytest.mark.xfail(strict=True, reason=reason))


def _native_bin() -> str | None:
    named = os.environ.get("DIFFCTX_NATIVE_BIN")
    if named:
        return named
    built = PROJECT_ROOT / "target" / "release" / "diffctx"
    return str(built) if built.exists() else None


def _git(repo: Path, *args: str) -> None:
    env = {**os.environ, "GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_NOSYSTEM": "1"}
    for var in ("GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"):
        env.pop(var, None)
    subprocess.run(["git", *args], cwd=repo, env=env, check=True, capture_output=True)


def _write(repo: Path, rel: str, content: str) -> None:
    path = repo / rel
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8", newline="")


@pytest.fixture(scope="module")
def repo(tmp_path_factory) -> Path:
    """A change with every kind of claim impact makes — callers with and
    without a test, a removed definition's caller, a line naming a deleted
    script — plus a pure rename, a withheld file, a non-ASCII path and a second
    language, and uncommitted work on top, so the three kinds of range differ."""
    root = tmp_path_factory.mktemp("doors") / "repo"
    root.mkdir()
    _git(root, "init", "-q", "-b", "main", ".")
    _git(root, "config", "user.email", "test@example.com")
    _git(root, "config", "user.name", "door parity")
    _write(root, "shop/pricing.py", "def total(items):\n    return sum(items)\n\n\ndef legacy(items):\n    return list(items)\n")
    _write(root, "shop/checkout.py", "from shop.pricing import total\n\n\ndef charge(cart):\n    return total(cart)\n")
    _write(root, "shop/report.py", "from shop.pricing import total\n\n\ndef summarize(orders):\n    return total(orders)\n")
    _write(root, "shop/old.py", "from shop.pricing import legacy\n\n\ndef use_legacy(cart):\n    return legacy(cart)\n")
    _write(
        root, "tests/test_checkout.py", "from shop.checkout import charge\n\n\ndef test_charge():\n    assert charge([1]) == 1\n"
    )
    _write(root, "shop/\u00fcbung.py", "from shop.pricing import total\n\n\ndef \u00fcbung(cart):\n    return total(cart)\n")
    _write(root, "shop/helpers_old.py", "def helper(x):\n    return x\n")
    _write(root, "web/price.ts", "export function price(x: number) {\n  return x;\n}\n")
    _write(root, "web/view.ts", "import { price } from './price';\n\nexport function show() {\n  return price(1);\n}\n")
    _write(root, ".diffctx/ignore", "vault.py\n")
    _write(root, "vault.py", "TOKEN = 1\n")
    _write(root, "scripts/rotate.sh", "echo rotate\n")
    _write(root, "ops.md", "Run scripts/rotate.sh weekly.\n")
    _git(root, "add", "-A")
    _git(root, "commit", "-q", "-m", "initial")
    _write(root, "shop/pricing.py", "def total(items, tax=0):\n    return sum(items) + tax\n")
    _write(root, "web/price.ts", "export function price(x: number, rate = 1) {\n  return x * rate;\n}\n")
    _write(root, "vault.py", "TOKEN = 2\n")
    _git(root, "mv", "shop/helpers_old.py", "shop/helpers.py")
    _git(root, "rm", "-q", "scripts/rotate.sh")
    _git(root, "add", "-A")
    _git(root, "commit", "-q", "-m", "total takes a tax; legacy and the script go")
    _write(root, "shop/report.py", "from shop.pricing import total\n\n\ndef summarize(orders):\n    return total(orders, 1)\n")
    _git(root, "add", "shop/report.py")
    _write(root, "shop/checkout.py", "from shop.pricing import total\n\n\ndef charge(cart):\n    return total(cart, 2)\n")
    _write(root, "shop/refund.py", "from shop.pricing import total\n\n\ndef refund(cart):\n    return -total(cart)\n")
    return root


# ---- the doors ----


def _native(repo: Path, *args: str) -> subprocess.CompletedProcess[str]:
    binary = _native_bin()
    if binary is None:
        pytest.skip("no native binary: set DIFFCTX_NATIVE_BIN")
    return subprocess.run([binary, ".", *args, "-q"], cwd=repo, capture_output=True, text=True, encoding="utf-8")


def _cli(repo: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return run_diffctx_subprocess([".", *args, "-q"], cwd=repo)


def _mcp_server() -> Any | None:
    try:
        return importlib.import_module("diffctx.mcp.server").mcp
    except ImportError:
        return None


def _mcp(repo: Path, **arguments: Any) -> tuple[str | None, str]:
    """(text, refusal): a refused call is the tool's error, not an exception."""
    server = _mcp_server()
    if server is None:
        pytest.skip("the mcp extra is not installed")
    try:
        result = asyncio.run(server.call_tool("diffctx_context", {"repo_path": str(repo), **arguments}))
    except Exception as refused:  # the SDK's ToolError, whatever module it lives in
        return None, str(refused)
    return result.content[0].text, ""


def _ok(done: subprocess.CompletedProcess[str]) -> str:
    assert done.returncode == 0, done.stderr
    return done.stdout


def _text(value: str) -> str:
    return value.rstrip("\n") + "\n"


# ---- what an answer claims ----


def _relations(doc: dict[str, Any]) -> set[str]:
    """Callers, the test reaching each, unresolved callers, stale references
    and the listed definitions — no line numbers, no weights."""
    found: set[str] = set()
    for changed in doc.get("changed", []):
        target = f"{changed['path']}::{changed.get('symbol')} ({changed.get('kind')})"
        found.add(target)
        for caller in changed.get("callers", []):
            who = f"{caller['path']}::{caller.get('symbol')}"
            found.add(f"{target} <- {who} [{caller.get('relation', 'call')}, {caller.get('evidence')}]")
            if caller.get("tested_by"):
                found.add(f"{who} is tested by {caller['tested_by']}")
        if changed.get("unresolved"):
            found.add(f"{target} has unresolved callers: {changed['unresolved']}")
    found.update(f"stale {row['path']}" for row in doc.get("stale_references", []))
    return found


_MD_FRAGMENT = re.compile(r"^## (`+) ?(.+?) ?\1(.*)$")


def _fragments(text: str, fmt: str) -> set[str]:
    if fmt == "json":
        return {f"{f['path']}:{f['lines']}" for f in json.loads(text).get("fragments", [])}
    found = set()
    for line in text.splitlines():
        if match := _MD_FRAGMENT.match(line):
            found.add(match.group(2))
    return found


def _ids(text: str) -> list[str]:
    return [f"{item['path']}:{item['lines']}" for item in json.loads(text)["items"]]


def _identical(label: str, by_door: dict[str, Any]) -> None:
    assert len(by_door) > 1, "no second door to compare against"
    first_door, first = next(iter(by_door.items()))
    for door, value in by_door.items():
        assert value == first, f"{label}: {door} differs from {first_door}\n{first_door}: {first!r}\n{door}: {value!r}"


# ---- the parity cases ----


def _range_cases(budgets: list[int] = BUDGETS) -> list[Any]:
    return [pytest.param(ref, budget, id=f"{name}-{budget}") for name, ref in RANGES.items() for budget in budgets]


@pytest.mark.parametrize(("ref", "budget"), _range_cases())
def test_impact_text_is_one_text(repo, ref, budget):
    doors = {
        "native": _text(_ok(_native(repo, "--diff", ref, "--mode", "impact", "-f", "md", "--budget", str(budget)))),
        "cli": _text(_ok(_cli(repo, "--diff", ref, "--mode", "impact", "-f", "md", "--budget", str(budget)))),
    }
    if _mcp_server() is not None:
        text, refusal = _mcp(repo, diff_ref=ref, mode="impact", budget_tokens=budget)
        assert text is not None, refusal
        doors["mcp"] = _text(text)
    _identical(f"impact {ref} at {budget}", doors)


@pytest.mark.parametrize(("ref", "budget"), _range_cases())
def test_impact_relations_are_one_set(repo, ref, budget):
    args = ("--diff", ref, "--mode", "impact", "-f", "json", "--budget", str(budget))
    doors = {
        "native": _relations(json.loads(_ok(_native(repo, *args)))),
        "cli": _relations(json.loads(_ok(_cli(repo, *args)))),
    }
    _identical(f"impact relations {ref} at {budget}", doors)


@pytest.mark.parametrize("symbol", SYMBOLS)
@pytest.mark.parametrize("budget", BUDGETS)
def test_symbol_text_is_one_text(repo, symbol, budget):
    doors = {
        "native": _text(_ok(_native(repo, "--symbol", symbol, "-f", "md", "--budget", str(budget)))),
        "cli": _text(_ok(_cli(repo, "--symbol", symbol, "-f", "md", "--budget", str(budget)))),
    }
    if _mcp_server() is not None:
        text, refusal = _mcp(repo, symbol=symbol, budget_tokens=budget)
        assert text is not None, refusal
        doors["mcp"] = _text(text)
    _identical(f"symbol {symbol} at {budget}", doors)


@pytest.mark.parametrize("symbol", SYMBOLS)
def test_symbol_relations_are_one_set(repo, symbol):
    args = ("--symbol", symbol, "-f", "json")
    doors = {
        "native": _relations(json.loads(_ok(_native(repo, *args)))),
        "cli": _relations(json.loads(_ok(_cli(repo, *args)))),
    }
    _identical(f"symbol relations {symbol}", doors)


def test_a_refused_symbol_is_one_refusal(repo):
    native = _native(repo, "--symbol", MISSING)
    cli = _cli(repo, "--symbol", MISSING)
    assert native.returncode == cli.returncode != 0, (native.stderr, cli.stderr)
    named = f'no definition of "{MISSING}" found'
    assert named in native.stderr and named in cli.stderr, (native.stderr, cli.stderr)
    if _mcp_server() is not None:
        text, refusal = _mcp(repo, symbol=MISSING)
        assert text is None and named in refusal, (text, refusal)


@pytest.mark.parametrize(("ref", "budget"), _range_cases(PACK_BUDGETS))
def test_pack_fragments_are_one_set(repo, ref, budget):
    # A budget is spent on the rendered text, so the formats may select
    # differently from one another; every door of one format must not.
    args = ("--diff", ref, "--budget", str(budget))
    as_json = {
        "native": _fragments(_ok(_native(repo, *args, "-f", "json")), "json"),
        "cli": _fragments(_ok(_cli(repo, *args, "-f", "json")), "json"),
    }
    assert as_json["native"], "the fixture must leave something to compare"
    _identical(f"pack json fragments {ref} at {budget}", as_json)
    as_md = {"cli": _fragments(_ok(_cli(repo, *args, "-f", "md")), "md")}
    if _mcp_server() is not None:
        text, refusal = _mcp(repo, diff_ref=ref, mode="pack", budget_tokens=budget)
        assert text is not None, refusal
        as_md["mcp"] = _fragments(text, "md")
    _identical(f"pack md fragments {ref} at {budget}", as_md)


@pytest.mark.parametrize(("ref", "budget"), _range_cases(PACK_BUDGETS))
def test_locate_ids_are_one_list(repo, ref, budget):
    args = ("--diff", ref, "--mode", "locate", "--budget", str(budget))
    doors = {
        "native": _ids(_ok(_native(repo, *args))),
        "cli": _ids(_ok(_cli(repo, *args))),
    }
    if _mcp_server() is not None:
        text, refusal = _mcp(repo, diff_ref=ref, mode="locate", budget_tokens=budget)
        assert text is not None, refusal
        doors["mcp"] = _ids(text)
    assert doors["native"], "the fixture must leave something to compare"
    _identical(f"locate ids {ref} at {budget}", doors)
