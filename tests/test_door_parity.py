"""One question, three doors: the native binary, the Python CLI and the MCP
server answer the same range, symbol and budget with the same claims.

The doors share the engine, so every difference is a door adding, dropping or
re-fitting something of its own. What must be identical is what the answers
say, not how they are wrapped:

- impact and symbol answers: the same text from every door, and the same
  relation set from the two JSON doors;
- pack: the same fragments (path and lines) from every door and format;
- locate: the same ranked ids in the same order;
- a refused symbol: the same refusal;
- what no answer says: the budget a door was not asked for, the empty-diff exit
  and its hint, the token summary and the redaction line are the engine's
  (`diffctx::delivery`), so every door observes the same one.

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


def _native(repo: Path, *args: str, quiet: bool = True) -> subprocess.CompletedProcess[str]:
    binary = _native_bin()
    if binary is None:
        pytest.skip("no native binary: set DIFFCTX_NATIVE_BIN")
    return subprocess.run(
        [binary, ".", *args, *(["-q"] if quiet else [])], cwd=repo, capture_output=True, text=True, encoding="utf-8"
    )


def _cli(repo: Path, *args: str, quiet: bool = True) -> subprocess.CompletedProcess[str]:
    return run_diffctx_subprocess([".", *args, *(["-q"] if quiet else [])], cwd=repo)


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
    """Rows, the test reaching each, the possible rows' gaps, stale references
    and the listed definitions — no line numbers, no weights."""
    found: set[str] = set()
    for definition in doc.get("definitions", []):
        target = f"{definition['path']}::{definition.get('symbol')} ({definition.get('kind')})"
        found.add(target)
        for row in definition.get("rows", []):
            who = f"{row['path']}::{row.get('symbol')}"
            found.add(f"{target} <- {who} [{row['class']}, {row['standing']}]")
            guard = row["guard"]
            if guard["state"] == "linked":
                found.add(f"{who} is tested by {guard['test']}")
        if definition.get("gaps"):
            found.add(f"{target} has gaps: {definition['gaps']}")
    found.update(f"stale {row['path']}" for row in doc.get("stale", {}).get("items", []))
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
    # A JSON document's own framing can take the tightest budget whole.
    assert as_json["native"] or budget == min(PACK_BUDGETS), "the fixture must leave something to compare"
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


# ---- the engine's policies, observed through every door ----


@pytest.mark.parametrize("ref", RANGES.values(), ids=RANGES.keys())
def test_impact_without_a_budget_is_one_text(repo, ref):
    """No door is asked for a budget, so none decides one: the engine's default
    for the mode applies everywhere, the MCP's included (it used to say 8000
    while the CLI and the docs said 2000)."""
    doors = {
        "native": _text(_ok(_native(repo, "--diff", ref, "--mode", "impact", "-f", "md"))),
        "cli": _text(_ok(_cli(repo, "--diff", ref, "--mode", "impact", "-f", "md"))),
    }
    if _mcp_server() is not None:
        text, refusal = _mcp(repo, diff_ref=ref, mode="impact")
        assert text is not None, refusal
        doors["mcp"] = _text(text)
    _identical(f"impact {ref} without a budget", doors)


@pytest.mark.parametrize("symbol", SYMBOLS)
def test_symbol_without_a_budget_is_one_text(repo, symbol):
    doors = {
        "native": _text(_ok(_native(repo, "--symbol", symbol, "-f", "md"))),
        "cli": _text(_ok(_cli(repo, "--symbol", symbol, "-f", "md"))),
    }
    if _mcp_server() is not None:
        text, refusal = _mcp(repo, symbol=symbol)
        assert text is not None, refusal
        doors["mcp"] = _text(text)
    _identical(f"symbol {symbol} without a budget", doors)


@pytest.fixture(scope="module")
def quiet_repo(tmp_path_factory) -> Path:
    """One commit that changed a function, and a clean working tree after it."""
    root = tmp_path_factory.mktemp("quiet") / "repo"
    root.mkdir()
    _git(root, "init", "-q", "-b", "main", ".")
    _git(root, "config", "user.email", "test@example.com")
    _git(root, "config", "user.name", "door parity")
    _write(root, "pricing.py", "def total(items):\n    return sum(items)\n")
    _git(root, "add", "-A")
    _git(root, "commit", "-q", "-m", "initial")
    _write(root, "pricing.py", "def total(items):\n    return sum(items) + 1\n")
    _git(root, "add", "-A")
    _git(root, "commit", "-q", "-m", "total adds one")
    return root


def _said(done: subprocess.CompletedProcess[str]) -> list[str]:
    """What a door warned on stderr, without its own name."""
    return [line.split(": ", 1)[1] for line in done.stderr.splitlines() if line.startswith("diffctx: ")]


@pytest.mark.parametrize(
    ("mode", "args", "hint"),
    [
        pytest.param("pack", ("--diff", "HEAD"), "the working tree matches HEAD", id="clean-tree"),
        pytest.param("locate", ("--diff", "HEAD"), "the working tree matches HEAD", id="clean-tree-locate"),
        pytest.param("pack", ("--diff", "HEAD~1..HEAD", "--budget", "0"), "--budget 0 emits no fragments", id="budget-zero"),
        pytest.param("pack", ("--diff", "staged"), "git diff --cached --stat", id="nothing-staged"),
    ],
)
def test_an_empty_diff_is_one_exit_and_one_hint(quiet_repo, mode, args, hint):
    """The engine decides what an empty diff is, says and exits with (4); the
    Python door used to word it its own way, and say none of it for locate."""
    flags = (*args, "--mode", mode)
    native, cli = _native(quiet_repo, *flags), _cli(quiet_repo, *flags)
    assert native.returncode == cli.returncode == 4, (native.stderr, cli.stderr)
    assert _said(native) == _said(cli), (native.stderr, cli.stderr)
    assert any(hint in line for line in _said(native)), native.stderr


def test_an_empty_impact_is_an_answer_not_a_failure(quiet_repo):
    flags = ("--diff", "HEAD", "--mode", "impact", "-f", "md")
    assert _native(quiet_repo, *flags).returncode == 0
    assert _cli(quiet_repo, *flags).returncode == 0
    if _mcp_server() is not None:
        text, refusal = _mcp(quiet_repo, diff_ref="HEAD", mode="impact")
        assert text is not None, refusal


@pytest.mark.parametrize(
    "args",
    [
        pytest.param(("--diff", "HEAD~1..HEAD", "--mode", "impact", "-f", "md"), id="impact"),
        pytest.param(("--diff", "HEAD~1..HEAD", "--mode", "locate"), id="locate"),
        pytest.param(("--diff", "HEAD~1..HEAD", "-f", "json", "--budget", "8000"), id="pack"),
    ],
)
def test_the_token_summary_is_one_line(quiet_repo, args):
    native, cli = _native(quiet_repo, *args, quiet=False), _cli(quiet_repo, *args, quiet=False)
    assert native.returncode == cli.returncode == 0, (native.stderr, cli.stderr)
    assert native.stdout == cli.stdout
    assert re.fullmatch(r"[\d,]+ tokens \(o200k_base\), [\d.]+ (B|KB|MB)\n?", native.stderr.strip() + "\n"), native.stderr
    assert native.stderr.strip() == cli.stderr.strip()


def test_a_redacted_secret_is_said_by_every_door(tmp_path):
    aws = "AKIA" + "IOSFODNN7EXAMPLE"  # assembled so the repository's own scanners do not trip
    root = tmp_path / "repo"
    root.mkdir()
    _git(root, "init", "-q", "-b", "main", ".")
    _git(root, "config", "user.email", "test@example.com")
    _git(root, "config", "user.name", "door parity")
    _write(root, "settings.py", "REGION = 'eu-central-1'\n\n\ndef client():\n    return REGION\n")
    _git(root, "add", "-A")
    _git(root, "commit", "-q", "-m", "initial")
    _write(root, "settings.py", f"REGION = 'eu-central-1'\nACCESS_KEY = '{aws}'\n\n\ndef client():\n    return REGION\n")
    _git(root, "add", "-A")
    _git(root, "commit", "-q", "-m", "wire the client")

    args = ("--diff", "HEAD~1..HEAD", "-f", "json")
    native, cli = _native(root, *args, quiet=False), _cli(root, *args, quiet=False)
    for door in (native, cli):
        assert aws not in door.stdout
        assert door.returncode == 0, door.stderr
    said = {
        door: [line for line in out.stderr.splitlines() if "credential-shaped" in line]
        for door, out in (("native", native), ("cli", cli))
    }
    assert said["native"] and said["native"] == said["cli"], said


@pytest.mark.parametrize("ref", RANGES.values(), ids=RANGES.keys())
def test_the_raw_diff_is_one_patch(repo, ref):
    """The native and Python CLIs ask the engine for the same request, so they
    bundle the same patch, say its share the same way, and withhold the same
    sections (the file `.diffctx/ignore` names is not in it)."""
    args = ("--diff", ref, "--budget", "8000", "-f", "json", "--with-raw-diff")
    native, cli = _native(repo, *args, quiet=False), _cli(repo, *args, quiet=False)
    assert native.returncode == cli.returncode == 0, (native.stderr, cli.stderr)
    patch = {door: json.loads(done.stdout).get("raw_diff") for door, done in (("native", native), ("cli", cli))}
    assert patch["native"], "the fixture's range must leave a patch to bundle"
    assert "vault.py" not in patch["native"]
    _identical(f"raw diff {ref}", patch)
    assert native.stdout == cli.stdout
    share = {
        door: [line for line in done.stderr.splitlines() if "are the raw diff" in line]
        for door, done in (("native", native), ("cli", cli))
    }
    assert share["native"], native.stderr
    _identical(f"raw diff share {ref}", share)


@pytest.mark.parametrize("mode", ["impact", "locate"])
def test_a_raw_diff_is_refused_where_there_is_no_source(repo, mode):
    args = ("--diff", "HEAD~1..HEAD", "--mode", mode, "--with-raw-diff")
    native, cli = _native(repo, *args), _cli(repo, *args)
    assert native.returncode == cli.returncode == 2, (native.stderr, cli.stderr)
    for done in (native, cli):
        assert f"--mode {mode} emits no source" in done.stderr, done.stderr
