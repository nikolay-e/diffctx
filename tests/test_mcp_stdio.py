from __future__ import annotations

import json
import os
import shutil
import sys
from contextlib import asynccontextmanager
from pathlib import Path

import pytest

from diffctx.version import __version__
from tests.framework.pygit2_backend import Pygit2Repo

pytest.importorskip("mcp")
pytest.importorskip("anyio")

import anyio
from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client

_TOOL_SNAPSHOT = {
    "diffctx_context": (
        ["budget_tokens", "clipboard", "diff_ref", "fragment_ids", "include_raw_diff", "max_tokens", "mode", "repo_path"],
        ["repo_path"],
    ),
}
_LEGACY_TOOL_SNAPSHOT = {
    **_TOOL_SNAPSHOT,
    "get_tree_map": (
        ["clipboard", "max_depth", "max_file_bytes", "max_tokens", "no_content", "output_format", "repo_path", "subdirectory"],
        ["repo_path"],
    ),
    "get_file_context": (
        ["clipboard", "dry_run", "max_file_bytes", "max_files", "max_tokens", "patterns", "repo_path"],
        ["patterns", "repo_path"],
    ),
}
_CWD_WARNING = "'mcp' is the subcommand"


def _installed_script() -> str | None:
    return shutil.which("diffctx-mcp", path=str(Path(sys.executable).parent))


def _launchers() -> list:
    script = _installed_script()
    return [
        pytest.param([sys.executable, "-m", "diffctx", "mcp"], id="python-m-diffctx-mcp"),
        pytest.param(
            [script or "diffctx-mcp"],
            id="diffctx-mcp",
            marks=pytest.mark.skipif(script is None, reason="the diffctx-mcp script is not installed beside this interpreter"),
        ),
    ]


@pytest.fixture
def workspace(tmp_path):
    allowed = tmp_path / "allowed"
    repo = Pygit2Repo(allowed / "repo")
    repo.add_file("src/calc.py", "def add(a, b):\n    return a + b\n")
    repo.commit("base")
    repo.add_file("src/calc.py", "def add(a, b):\n    return a + b\n\ndef sub(a, b):\n    return a - b\n")
    repo.commit("change")
    outside = Pygit2Repo(tmp_path / "outside")
    outside.add_file("x.py", "X = 1\n")
    outside.commit("x")
    cwd = tmp_path / "cwd"
    (cwd / "mcp").mkdir(parents=True)
    return {
        "allowed": allowed,
        "repo": Path(repo.path),
        "outside": Path(outside.path),
        "cwd": cwd,
        "stderr": tmp_path / "stderr.txt",
    }


@asynccontextmanager
async def _session(argv: list[str], workspace: dict, legacy: bool = False):
    env = {k: v for k, v in os.environ.items() if k not in ("DIFFCTX_MCP_LEGACY_TOOLS", "DIFFCTX_ALLOWED_PATHS")}
    env["DIFFCTX_ALLOWED_PATHS"] = str(workspace["allowed"])
    if legacy:
        env["DIFFCTX_MCP_LEGACY_TOOLS"] = "1"
    params = StdioServerParameters(command=argv[0], args=argv[1:], env=env, cwd=str(workspace["cwd"]))
    with open(workspace["stderr"], "w", encoding="utf-8") as errlog:
        async with stdio_client(params, errlog=errlog) as (read, write):
            async with ClientSession(read, write) as session:
                init = await _step("initialize", session.initialize())
                yield session, init


_STEP_SECONDS = 45


async def _step(name: str, call):
    # A hung request fails here with its name; the global timeout would kill
    # the worker instead and say nothing about which request never answered.
    try:
        with anyio.fail_after(_STEP_SECONDS):
            return await call
    except TimeoutError as exc:
        raise AssertionError(f"MCP request '{name}' got no answer within {_STEP_SECONDS}s") from exc


def _snapshot(tools) -> dict:
    return {t.name: (sorted(t.inputSchema["properties"]), sorted(t.inputSchema.get("required", []))) for t in tools}


@pytest.mark.timeout(120)
@pytest.mark.parametrize("argv", _launchers())
@pytest.mark.asyncio
async def test_the_default_surface_over_real_stdio(argv, workspace):
    async with _session(argv, workspace) as (session, init):
        assert init.serverInfo.version == __version__
        assert _snapshot((await _step("list_tools", session.list_tools())).tools) == _TOOL_SNAPSHOT

        outside = await _step(
            "diffctx_context", session.call_tool("diffctx_context", {"repo_path": str(workspace["outside"]), "mode": "locate"})
        )
        assert outside.isError
        assert "outside the roots" in outside.content[0].text

        bad_budget = await _step(
            "diffctx_context", session.call_tool("diffctx_context", {"repo_path": str(workspace["repo"]), "budget_tokens": "abc"})
        )
        assert bad_budget.isError

        legacy = await _step("get_tree_map", session.call_tool("get_tree_map", {"repo_path": str(workspace["repo"])}))
        assert legacy.isError
        assert "Unknown tool" in legacy.content[0].text

        located = await _step(
            "locate",
            session.call_tool(
                "diffctx_context", {"repo_path": str(workspace["repo"]), "diff_ref": "HEAD~1..HEAD", "mode": "locate"}
            ),
        )
        assert not located.isError, located.content[0].text
        # The document travels once, as text: a structured copy wrapped it in
        # {"result": "<escaped JSON>"} and clients rendered that instead.
        assert located.structuredContent is None
        doc = json.loads(located.content[0].text)
        assert doc["schema"] == "diffctx.locate.v1"
        assert any(item["path"] == "src/calc.py" for item in doc["items"])

    stderr = workspace["stderr"].read_text(encoding="utf-8")
    if argv[1:] == ["-m", "diffctx", "mcp"]:
        assert _CWD_WARNING in stderr
    else:
        assert _CWD_WARNING not in stderr


@pytest.mark.timeout(120)
@pytest.mark.parametrize("argv", _launchers())
@pytest.mark.asyncio
async def test_legacy_tools_appear_only_behind_the_env_gate(argv, workspace):
    async with _session(argv, workspace, legacy=True) as (session, _):
        assert _snapshot((await _step("list_tools", session.list_tools())).tools) == _LEGACY_TOOL_SNAPSHOT
        tree = await _step("get_tree_map", session.call_tool("get_tree_map", {"repo_path": str(workspace["repo"])}))
        assert not tree.isError, tree.content[0].text
        assert "calc.py" in tree.content[0].text


@pytest.mark.timeout(120)
@pytest.mark.asyncio
async def test_a_running_server_survives_its_package_being_replaced_on_disk(workspace, tmp_path):
    """#291: `uv tool install --force` swaps the package under a live server.
    Every module a call needs must already be loaded, or the first call after
    the upgrade dies with ModuleNotFoundError."""
    import diffctx

    site = tmp_path / "site"
    shutil.copytree(Path(diffctx.__file__).parent, site / "diffctx", ignore=shutil.ignore_patterns("__pycache__"))
    env = {k: v for k, v in os.environ.items() if k not in ("DIFFCTX_MCP_LEGACY_TOOLS", "DIFFCTX_ALLOWED_PATHS")}
    env["DIFFCTX_ALLOWED_PATHS"] = str(workspace["allowed"])
    env["PYTHONPATH"] = str(site)
    params = StdioServerParameters(command=sys.executable, args=["-m", "diffctx.mcp"], env=env, cwd=str(tmp_path))
    with open(workspace["stderr"], "w", encoding="utf-8") as errlog:
        async with stdio_client(params, errlog=errlog) as (read, write), ClientSession(read, write) as session:
            await _step("initialize", session.initialize())
            # The Python modules are what a lazy import would reach for; the
            # loaded native extension stays, as Windows cannot unlink it.
            for module in (site / "diffctx").rglob("*.py"):
                module.unlink()
            repo = str(workspace["repo"])
            located = await _step("locate", session.call_tool("diffctx_context", {"repo_path": repo, "diff_ref": "HEAD~1..HEAD"}))
            assert not located.isError, located.content[0].text
            ids = [f"{i['path']}:{i['lines']}" for i in json.loads(located.content[0].text)["items"]]
            fetched = await _step(
                "fetch",
                session.call_tool("diffctx_context", {"repo_path": repo, "diff_ref": "HEAD~1..HEAD", "fragment_ids": ids}),
            )
            assert not fetched.isError, fetched.content[0].text
            assert "def sub" in fetched.content[0].text
