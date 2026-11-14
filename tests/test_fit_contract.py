"""One contract, three doors (#507): the native binary, the Python CLI and the
MCP server deliver the same artifact for the same range, budget and format,
each within its budget, because one fit measured on the delivered text cuts
all of them. The native binary serves JSON and YAML, the MCP server Markdown,
the Python CLI all of them. They used to fit in three places on three texts,
and the Python copy cut JSON and YAML by position: production before docs,
and the rewrite of a multi-fragment file before its one-line bumps (#438).

The MCP server's `max_tokens` is a ceiling the engine fits the reply to, not a
check made after it: a reply over it is cut to it, never refused whole."""

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
# The job that builds the binary sets this: there a missing binary is a failure
# of the job, not a reason to skip the contract it exists to hold.
REQUIRE_NATIVE = os.environ.get("DIFFCTX_REQUIRE_NATIVE") == "1"


def _native_bin() -> str:
    if NATIVE_BIN and os.path.isfile(NATIVE_BIN):
        return NATIVE_BIN
    message = f"DIFFCTX_NATIVE_BIN names no native binary ({NATIVE_BIN!r})"
    if REQUIRE_NATIVE:
        pytest.fail(message)
    pytest.skip(message)


@pytest.fixture
def native_bin() -> str:
    return _native_bin()


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
    out = subprocess.run([_native_bin(), ".", *args, "-q"], cwd=repo.path, capture_output=True, text=True, encoding="utf-8")
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


# A JSON document carries its range, inventory and changes before any fragment, and the
# budget is charged for all of it: this fixture's empty document is 551 to 662 tokens, so
# below about 700 no fragment fits, and the door says so (exit 4) instead of delivering the
# cut it was asked for. 800 is the lowest budget that leaves room for one for every range.
_JSON_FRAMING_FLOOR = 800


def _json_leaves_room(fmt: str, budget: int, floor: int) -> None:
    if fmt == "json" and budget < floor:
        pytest.skip(f"a JSON document's own framing takes more than {budget} tokens here")


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
    _json_leaves_room(fmt, budget, _JSON_FRAMING_FLOOR)
    args = ("--diff", diff_range, "--budget", str(budget), "-f", fmt)
    doors = {"cli": _cli(repo, *args)}
    if fmt == "md":
        doors["mcp"] = _mcp(repo, diff_ref=diff_range, mode="pack", budget_tokens=budget)
    else:
        doors["native"] = _native(repo, *args)
    for door, text in doors.items():
        assert count_tokens(text) <= budget, f"{door} -f {fmt} delivered {count_tokens(text)} tokens"
    fragments = {door: _fragments(text, fmt) for door, text in doors.items()}
    assert fragments["cli"], "the fixture must leave something to compare"
    assert all(f == fragments["cli"] for f in fragments.values()), fragments
    assert all(text == doors["cli"] for text in doors.values()), "the doors deliver different documents"


def _says_budget_too_small(text: str, fmt: str) -> bool:
    """The least an impact answer can say may outgrow a budget; it then says so
    (a line in Markdown, `fit.least` in JSON) and cuts nothing more."""
    if fmt == "json":
        return "least" in json.loads(text).get("fit", {})
    return "(budget too small:" in text


@pytest.mark.parametrize("fmt", ["md", "json"])
@pytest.mark.parametrize("budget", [150, 400, 2000])
def test_impact_is_fitted_once_to_the_budget_asked(repo, native_bin, budget, fmt):
    args = ("--diff", BOTH, "--mode", "impact", "--budget", str(budget), "-f", fmt)
    doors = {"native": _native(repo, *args), "cli": _cli(repo, *args)}
    if fmt == "md":
        doors["mcp"] = _mcp(repo, diff_ref=BOTH, mode="impact", budget_tokens=budget).rstrip("\n") + "\n"
    for door, text in doors.items():
        assert count_tokens(text) <= budget or _says_budget_too_small(
            text, fmt
        ), f"{door} impact -f {fmt} delivered {count_tokens(text)} tokens without saying the budget is too small"
    assert all(text == doors["native"] for text in doors.values()), doors


@pytest.mark.parametrize("fmt", ["md", "json", "yaml", "txt"])
@pytest.mark.parametrize("budget", [600, 700, 900, 1100, 1300])
def test_production_outlasts_its_tests_and_docs(repo, budget, fmt):
    _json_leaves_room(fmt, budget, 700)
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


def _says_how_little_fits(repo) -> int:
    """The least an impact answer for BOTH can say, as the engine declares it."""
    text = _cli(repo, "--diff", BOTH, "--mode", "impact", "-f", "md", "--budget", "1")
    match = re.search(r"takes (\d+) tokens", text)
    assert match, text
    return int(match.group(1))


def _refused(reply: str) -> bool:
    return "exceeding max_tokens" in reply


def test_the_mcp_ceiling_fits_an_impact_answer_and_holds_below_the_least(repo):
    least = _says_how_little_fits(repo)
    full = _mcp(repo, diff_ref=BOTH, mode="impact")
    assert count_tokens(full) > least, "the fixture's answer is already the least it can say"
    for ceiling in (least, (least + count_tokens(full)) // 2):
        reply = _mcp(repo, diff_ref=BOTH, mode="impact", max_tokens=ceiling)
        assert not _refused(reply), f"max_tokens={ceiling} refused an answer that fits it:\n{reply}"
        assert count_tokens(reply) <= ceiling, f"max_tokens={ceiling}: {count_tokens(reply)} tokens"
    # Below the least the answer cannot be cut to the ceiling: it is not sent.
    assert _refused(_mcp(repo, diff_ref=BOTH, mode="impact", max_tokens=least - 1))


def test_the_mcp_ceiling_fits_a_pack_answer(repo):
    full = _mcp(repo, diff_ref=BOTH, mode="pack")
    ceiling = max(700, count_tokens(full) // 2)
    assert count_tokens(full) > ceiling, "the fixture's answer fits the ceiling already"
    reply = _mcp(repo, diff_ref=BOTH, mode="pack", max_tokens=ceiling)
    assert not _refused(reply), reply
    assert count_tokens(reply) <= ceiling, f"{count_tokens(reply)} tokens over max_tokens={ceiling}"
    kept = {path for path, _, _ in _fragments(reply, "md")}
    assert kept, "the reply keeps what the change touched"
    assert kept <= {path for path, _, _ in _fragments(full, "md")}


def test_the_mcp_ceiling_fits_a_locate_answer_and_keeps_it_json(repo):
    full = _mcp(repo, diff_ref=BOTH, mode="locate")
    ceiling = count_tokens(full) - 1
    reply = _mcp(repo, diff_ref=BOTH, mode="locate", max_tokens=ceiling)
    assert not _refused(reply), reply
    assert count_tokens(reply) <= ceiling
    items = {(i["path"], i["lines"]) for i in json.loads(reply)["items"]}
    assert items and items <= {(i["path"], i["lines"]) for i in json.loads(full)["items"]}
    assert (
        not json.loads(reply).get("overflow", {}).get("items")
    ), "the MCP lists the candidates a budget left behind only when asked"


def test_a_budget_above_the_ceiling_is_lowered_to_it_and_said(repo):
    ceiling = 900
    reply = _mcp(repo, diff_ref=BOTH, mode="pack", budget_tokens=ceiling * 10, max_tokens=ceiling)
    assert reply.startswith(f"Note: budget_tokens={ceiling * 10:,} is above max_tokens={ceiling:,}"), reply
    assert count_tokens(reply) <= ceiling, f"{count_tokens(reply)} tokens over max_tokens={ceiling}"


def test_the_raw_diff_counts_against_the_ceiling_though_not_against_the_budget(repo):
    """`--budget` does not charge the patch; the MCP's `max_tokens` protects the
    reader's context, which the patch fills like anything else."""
    without = _mcp(repo, diff_ref=BOTH, mode="pack", max_tokens=2000)
    with_patch = _mcp(repo, diff_ref=BOTH, mode="pack", max_tokens=2000, include_raw_diff=True)
    assert _refused(with_patch) or ("## Raw diff" in with_patch and count_tokens(with_patch) <= 2000), count_tokens(with_patch)
    assert not _refused(without)
