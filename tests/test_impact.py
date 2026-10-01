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
        assert "tested by tests/test_checkout.py" in result.stdout
        assert "shop/report.py::summarize" in result.stdout
        assert "UNTESTED" in result.stdout
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
    return call_result[0].text


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
        assert "UNTESTED" in text

    @pytest.mark.asyncio
    async def test_mode_impact_refuses_the_raw_diff(self, server, impact_repo):
        from mcp.server.fastmcp.exceptions import ToolError

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
