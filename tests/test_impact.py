from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

from tests.framework.pygit2_backend import Pygit2Repo

PROJECT_ROOT = Path(__file__).parent.parent
SRC_DIR = PROJECT_ROOT / "src"


@pytest.fixture
def impact_repo(tmp_path):
    repo = Pygit2Repo(tmp_path / "impact_repo")
    repo.add_file("shop/pricing.py", "def total(items):\n    return sum(i.price for i in items)\n")
    repo.add_file(
        "shop/checkout.py",
        "from shop.pricing import total\n\n\ndef charge(cart):\n    return cart.pay(total(cart.items))\n",
    )
    repo.add_file(
        "shop/report.py",
        "from shop.pricing import total\n\n\ndef summarize(orders):\n    return [total(o.items) for o in orders]\n",
    )
    repo.add_file(
        "tests/test_checkout.py",
        "from shop.checkout import charge\n\n\ndef test_charge_pays_the_total(cart):\n    assert charge(cart) == 30\n",
    )
    base = repo.commit("initial")
    repo.add_file(
        "shop/pricing.py",
        "def total(items):\n    return round(sum(i.price for i in items) * 1.19, 2)\n",
    )
    head = repo.commit("totals include VAT")
    return repo, f"{base}..{head}"


def _run(cwd: Path, args: list[str]) -> subprocess.CompletedProcess[str]:
    env = {**os.environ, "PYTHONPATH": str(SRC_DIR)}
    return subprocess.run(
        [sys.executable, "-m", "diffctx", *args],
        cwd=cwd,
        env=env,
        capture_output=True,
        text=True,
        encoding="utf-8",
        timeout=120,
    )


class TestImpactCli:
    def test_json_names_callers_outside_the_diff_and_their_guard(self, impact_repo):
        repo, diff_range = impact_repo
        result = _run(repo.path, [".", "--diff", diff_range, "--mode", "impact", "-q", "-f", "json"])
        assert result.returncode == 0, result.stderr
        doc = json.loads(result.stdout)
        assert doc["schema"] == "diffctx.impact.v1"
        assert doc["empty"] is False
        total = next(c for c in doc["changed"] if c["symbol"] == "total")
        callers = {c["symbol"]: c for c in total["callers"]}
        assert callers["charge"]["path"] == "shop/checkout.py"
        assert callers["charge"]["tested_by"] == "tests/test_checkout.py"
        assert callers["summarize"]["path"] == "shop/report.py"
        assert "tested_by" not in callers["summarize"]

    def test_the_default_text_is_what_the_hook_injects(self, impact_repo):
        repo, diff_range = impact_repo
        result = _run(repo.path, [".", "--diff", diff_range, "--mode", "impact", "-q"])
        assert result.returncode == 0, result.stderr
        assert "shop/checkout.py::charge" in result.stdout
        assert "reachable from tests: tests/test_checkout.py" in result.stdout
        assert "shop/report.py::summarize" in result.stdout
        assert "summarize (4-5) — no static test link found" in result.stdout
        assert len(result.stdout.splitlines()) <= 25

    def test_a_docs_only_change_is_empty_and_exits_zero(self, impact_repo):
        repo, _ = impact_repo
        repo.add_file("README.md", "# shop\n")
        head = repo.commit("docs")
        result = _run(repo.path, [".", "--diff", f"{head}~1..{head}", "--mode", "impact", "-q", "-f", "json"])
        assert result.returncode == 0, result.stderr
        doc = json.loads(result.stdout)
        assert doc["empty"] is True
        assert doc["changed"] == []

    def test_other_formats_are_refused(self, impact_repo):
        repo, diff_range = impact_repo
        result = _run(repo.path, [".", "--diff", diff_range, "--mode", "impact", "-f", "yaml"])
        assert result.returncode == 2
        assert "impact" in result.stderr
        # The format an output file's extension implies is refused the same way,
        # instead of JSON landing silently in out.yaml.
        result = _run(repo.path, [".", "--diff", diff_range, "--mode", "impact", "-o", str(repo.path / "out.yaml")])
        assert result.returncode == 2
        assert "impact" in result.stderr
        assert not (repo.path / "out.yaml").exists()

    def test_full_is_refused(self, impact_repo):
        repo, diff_range = impact_repo
        result = _run(repo.path, [".", "--diff", diff_range, "--mode", "impact", "--full"])
        assert result.returncode == 2


mcp = pytest.importorskip("mcp", reason="mcp package not installed")


@pytest.fixture
def server():
    from diffctx.mcp.server import mcp as server

    return server


def _get_text(call_result) -> str:
    return call_result.content[0].text


class TestImpactMcp:
    @pytest.mark.asyncio
    async def test_mode_impact_returns_the_text_form(self, server, impact_repo):
        repo, diff_range = impact_repo
        result = await server.call_tool(
            "diffctx_context",
            {"repo_path": str(repo.path), "diff_ref": diff_range, "mode": "impact"},
        )
        text = _get_text(result)
        assert text.startswith("diffctx impact for")
        assert "shop/checkout.py::charge" in text
        assert "summarize (4-5) — no static test link found" in text

    @pytest.mark.asyncio
    async def test_mode_impact_refuses_the_raw_diff(self, server, impact_repo):
        from mcp.server.mcpserver.exceptions import ToolError

        repo, diff_range = impact_repo
        with pytest.raises(ToolError, match="impact"):
            await server.call_tool(
                "diffctx_context",
                {
                    "repo_path": str(repo.path),
                    "diff_ref": diff_range,
                    "mode": "impact",
                    "include_raw_diff": True,
                },
            )


def test_machine_read_output_is_utf8_under_a_cp1252_console(tmp_path):
    # Windows' default stdout encoding wrote the impact's dashes as cp1252
    # bytes and crashed on a path cp1252 cannot encode.
    repo = Pygit2Repo(tmp_path / "repo")
    repo.add_file("shop/pricing.py", "def total(items):\n    return sum(items)\n")
    repo.add_file("shop/checkout.py", "from shop.pricing import total\n\n\ndef charge(cart):\n    return total(cart)\n")
    repo.add_file("shop/цены.py", "RATE = 1\n")
    base = repo.commit("initial")
    repo.add_file("shop/pricing.py", "def total(items):\n    return sum(items) * 2\n")
    repo.add_file("shop/цены.py", "RATE = 2\n")
    head = repo.commit("double")
    env = {**os.environ, "PYTHONPATH": str(SRC_DIR), "PYTHONIOENCODING": "cp1252", "DIFFCTX_NO_MARKER": "1"}
    for mode, fmt, expected in (("impact", "md", "charge (4-5) \u2014"), ("locate", "json", "цены.py")):
        result = subprocess.run(
            [sys.executable, "-m", "diffctx", ".", "--diff", f"{base}..{head}", "--mode", mode, "-f", fmt, "-q"],
            cwd=repo.path,
            capture_output=True,
            env=env,
            timeout=120,
        )
        assert result.returncode == 0, (mode, result.stderr.decode("utf-8", "replace")[-400:])
        text = result.stdout.decode("utf-8")
        assert expected in text, (mode, text[:400])


class TestSymbolQuery:
    """The questions an agent asks before it has changed anything — who calls
    this, where is it defined, which tests reach it — answered by the impact
    walk anchored on the name's definitions instead of a diff (#336)."""

    @pytest.fixture
    def clean_repo(self, impact_repo):
        repo, _ = impact_repo
        repo.add_file("legacy/tally.py", "def total(rows):\n    return len(rows)\n")
        repo.add_file(
            "legacy/invoice.py",
            "from legacy.tally import total\n\n\ndef bill(rows):\n    return total(rows)\n",
        )
        repo.add_file(
            "shop/discount.py",
            "def rebate(cart):\n    return cart.base * 0.1\n\n\ndef apply(cart):\n    return cart.base - rebate(cart)\n",
        )
        repo.commit("legacy pricing, discounts")
        return repo

    def test_names_definition_callers_and_their_tests(self, clean_repo):
        result = _run(clean_repo.path, [".", "--symbol", "shop/pricing.py:total", "-q", "-f", "md"])
        assert result.returncode == 0, result.stderr
        assert result.stdout.startswith("diffctx impact for symbol shop/pricing.py:total: 1 definition(s)")
        assert "- shop/pricing.py::total" in result.stdout
        assert "called from shop/checkout.py::charge" in result.stdout
        assert "reachable from tests: tests/test_checkout.py" in result.stdout
        assert "called from shop/report.py::summarize" in result.stdout
        assert "legacy/" not in result.stdout

    def test_a_bare_name_answers_for_every_definition(self, clean_repo):
        result = _run(clean_repo.path, [".", "--symbol", "total", "-q", "-f", "json"])
        assert result.returncode == 0, result.stderr
        doc = json.loads(result.stdout)
        assert doc["symbol"] == "total"
        callers = {d["path"]: {c["path"] for c in d["callers"]} for d in doc["changed"]}
        assert callers["legacy/tally.py"] == {"legacy/invoice.py"}
        assert {"shop/checkout.py", "shop/report.py"} <= callers["shop/pricing.py"]

    def test_a_caller_in_the_defining_file_counts(self, clean_repo):
        result = _run(clean_repo.path, [".", "--symbol", "rebate", "-q", "-f", "md"])
        assert result.returncode == 0, result.stderr
        assert "called from shop/discount.py::apply" in result.stdout

    def test_a_symbol_with_no_callers_still_says_where_it_is(self, clean_repo):
        result = _run(clean_repo.path, [".", "--symbol", "apply", "-q", "-f", "md"])
        assert result.returncode == 0, result.stderr
        assert "1 definition(s), 0 caller(s)" in result.stdout
        assert "- shop/discount.py::apply" in result.stdout

    @pytest.mark.parametrize(("query", "named"), [("no_such_function", "no_such_function"), ("Ledger::total", "Ledger.total")])
    def test_an_unknown_name_is_an_error_naming_it(self, clean_repo, query, named):
        result = _run(clean_repo.path, [".", "--symbol", query, "-q"])
        assert result.returncode == 1, result.stderr
        assert f'no definition of "{named}" found' in result.stderr
        assert "internal error" not in result.stderr

    def test_a_common_name_keeps_its_definition_past_the_file_cap(self, tmp_path):
        repo = Pygit2Repo(tmp_path / "repo")
        for i in range(300):
            repo.add_file(f"app/m{i:03}.py", f"from zz.core import settle\n\n\ndef use_{i}():\n    return settle()\n")
        repo.add_file("zz/core.py", "def settle():\n    return 1\n")
        repo.commit("many callers")
        result = _run(repo.path, [".", "--symbol", "settle", "-q", "-f", "json"])
        assert result.returncode == 0, result.stderr
        assert [d["path"] for d in json.loads(result.stdout)["changed"]] == ["zz/core.py"]

    def test_a_name_qualified_by_its_module_answers_for_that_definition(self, clean_repo):
        result = _run(clean_repo.path, [".", "--symbol", "pricing.total", "-q", "-f", "json"])
        assert result.returncode == 0, result.stderr
        assert [d["path"] for d in json.loads(result.stdout)["changed"]] == ["shop/pricing.py"]

    @pytest.mark.parametrize(
        "args",
        [
            ["--symbol", "total", "--diff", "HEAD~1"],
            ["--symbol", "total", "--mode", "locate"],
            ["--symbol", "a b"],
            ["--symbol", "shop..total"],
            ["--symbol", "9lives"],
        ],
    )
    def test_conflicting_or_malformed_requests_are_usage_errors(self, clean_repo, args):
        result = _run(clean_repo.path, [".", *args, "-q"])
        assert result.returncode == 2, result.stderr
        assert result.stdout == ""

    @pytest.mark.asyncio
    @pytest.mark.parametrize(
        ("arguments", "refusal"),
        [
            ({"symbol": "total", "diff_ref": "HEAD~1"}, "symbol answers in mode impact"),
            ({"symbol": "no_such_function"}, 'no definition of "no_such_function" found'),
            ({"symbol": ""}, "--symbol takes NAME"),
        ],
    )
    async def test_mcp_symbol_refusals_say_what_to_correct(self, server, clean_repo, arguments, refusal):
        from mcp.server.mcpserver.exceptions import ToolError

        with pytest.raises(ToolError) as refused:
            await server.call_tool("diffctx_context", {"repo_path": str(clean_repo.path), **arguments})
        assert refusal in str(refused.value)
        assert not isinstance(refused.value.__cause__, RuntimeError)

    @pytest.mark.asyncio
    async def test_mcp_symbol_parameter(self, server, clean_repo):
        result = await server.call_tool("diffctx_context", {"repo_path": str(clean_repo.path), "symbol": "shop/pricing.py:total"})
        text = _get_text(result)
        assert text.startswith("diffctx impact for symbol shop/pricing.py:total")
        assert "shop/checkout.py::charge" in text


class TestStagedImpact:
    """`staged` is the index captured once (#354): the disk's unstaged edits
    are not part of the answer, through the CLI and the MCP tool alike."""

    @pytest.fixture
    def staged_repo(self, impact_repo):
        repo, _ = impact_repo
        repo.add_file("shop/pricing.py", "def total(items):\n    return sum(i.price for i in items) * 2\n")
        subprocess.run(["git", "add", "shop/pricing.py"], cwd=repo.path, check=True)
        (repo.path / "shop" / "report.py").write_text("def summarize(orders):\n    return len(orders)\n", encoding="utf-8")
        return repo

    def test_cli_and_mcp_answer_for_the_index(self, staged_repo):
        result = _run(staged_repo.path, [".", "--diff", "staged", "--mode", "impact", "-q", "-f", "json"])
        assert result.returncode == 0, result.stderr
        doc = json.loads(result.stdout)
        assert doc["changed_files"] == ["shop/pricing.py"]
        assert doc["index_tree"]
        callers = {c["path"] for s in doc["changed"] for c in s["callers"]}
        assert callers == {"shop/checkout.py", "shop/report.py"}, "report.py's unstaged edit is not the snapshot"

    @pytest.mark.asyncio
    async def test_mcp_staged_matches_the_cli(self, server, staged_repo):
        result = await server.call_tool(
            "diffctx_context", {"repo_path": str(staged_repo.path), "diff_ref": "staged", "mode": "impact"}
        )
        text = _get_text(result)
        assert text.startswith("diffctx impact for staged changes (index tree ")
        assert "shop/report.py::summarize" in text
