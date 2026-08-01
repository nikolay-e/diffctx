from __future__ import annotations

import asyncio
import subprocess

import pytest

mcp = pytest.importorskip("mcp", reason="mcp package not installed")
from mcp.server.fastmcp.exceptions import ToolError  # noqa: E402

from tests.framework.pygit2_backend import Pygit2Repo  # noqa: E402
from tests.garbage_data import GARBAGE_FILES  # noqa: E402


@pytest.fixture
def mcp_repo(tmp_path):
    repo = Pygit2Repo(tmp_path / "mcp_test_repo")
    for rel_path, content in GARBAGE_FILES.items():
        repo.add_file(rel_path, content)
    repo.add_file("src/calc.py", "def add(a, b):\n    return a + b\n")
    repo.add_file("src/main.py", "from calc import add\n\ndef run():\n    return add(1, 2)\n")
    repo.commit("initial commit")
    repo.add_file("src/calc.py", "def add(a, b):\n    return a + b\n\ndef subtract(a, b):\n    return a - b\n")
    repo.add_file(
        "src/main.py",
        "from calc import add, subtract\n\ndef run():\n    return add(1, 2)\n\ndef run_sub():\n    return subtract(5, 3)\n",
    )
    repo.commit("add subtract function")
    return repo


@pytest.fixture
def server():
    from diffctx.mcp.server import mcp

    return mcp


@pytest.fixture
def legacy_tools(server):
    """The pre-v3 tree-map and glob-read tools, which #127 took off by default.

    Registration is idempotent-by-overwrite on the FastMCP registry, so a class
    that needs them can ask repeatedly. The *default-off* claim is deliberately
    NOT tested through this fixture — a test that registers the tools cannot
    also prove they are absent, so that assertion runs in a fresh process
    (`TestLegacyToolsAreOptIn`).
    """
    from diffctx.mcp.server import register_legacy_tools

    register_legacy_tools(server)


def _get_text(call_result: tuple) -> str:
    content_blocks = call_result[0]
    return content_blocks[0].text


def _run_with_argv(main, argv):
    import sys

    old_argv = sys.argv
    sys.argv = ["diffctx-mcp", *argv]
    try:
        main()
    finally:
        sys.argv = old_argv


class TestMcpMainEntrypoint:
    """Regression (#88): `diffctx-mcp --help` used to produce zero output on
    both stdout and stderr — main() never parsed sys.argv at all before
    falling through to the blocking run_server() call."""

    def test_help_prints_usage_and_exits_zero(self, capsys):
        from diffctx.mcp.__main__ import main

        with pytest.raises(SystemExit) as exc_info:
            _run_with_argv(main, ["--help"])
        assert exc_info.value.code == 0
        captured = capsys.readouterr()
        assert "usage:" in captured.out
        assert captured.err == ""

    def test_unrecognized_argument_errors_cleanly(self, capsys):
        from diffctx.mcp.__main__ import main

        with pytest.raises(SystemExit) as exc_info:
            _run_with_argv(main, ["--bogus"])
        assert exc_info.value.code == 2
        captured = capsys.readouterr()
        assert "unrecognized arguments" in captured.err


class TestMcpSubcommand:
    """MCP clients that resolve the executable from the published package name
    run `diffctx`, not `diffctx-mcp`. Without a subcommand that reaches the
    server, they start a tree-mapping run and flood the transport with the
    whole working directory."""

    def test_mcp_subcommand_reaches_the_server(self, monkeypatch):
        from diffctx import cli

        started = []
        monkeypatch.setattr("diffctx.mcp.server.main", lambda prog="diffctx-mcp": started.append(prog))
        monkeypatch.setattr(cli.sys, "argv", ["diffctx", "mcp"])
        cli.main()
        assert started, "mcp subcommand did not dispatch to the server"

    def test_bare_invocation_still_maps_the_tree(self, monkeypatch, tmp_path):
        from diffctx import cli

        ran = []
        monkeypatch.setattr("diffctx._app.run", lambda: ran.append(True))
        monkeypatch.setattr(cli.sys, "argv", ["diffctx", str(tmp_path)])
        cli.main()
        assert ran, "non-mcp invocation must still reach the tree/diff pipeline"


class TestServerIdentity:
    """The `initialize` response is how a client learns which diffctx it is
    talking to. FastMCP takes no version argument, so an unset version makes
    the SDK report its own — clients saw the mcp package version instead."""

    def test_reports_package_version_not_sdk_version(self, server):
        from diffctx.version import __version__

        assert server._mcp_server.version == __version__

    @pytest.mark.asyncio
    async def test_every_tool_is_annotated_read_only(self, server):
        """MCP defaults are pessimistic: an unannotated tool is advertised as
        destructive and open-world, which costs it auto-permission in clients.
        Every diffctx tool only reads."""
        tools = await server.list_tools()
        assert tools
        for tool in tools:
            assert tool.annotations is not None, f"{tool.name} has no annotations"
            assert tool.annotations.title, f"{tool.name} has no title"
            assert tool.annotations.readOnlyHint is True, f"{tool.name} is not read-only"
            assert tool.annotations.openWorldHint is False, f"{tool.name} is open-world"


@pytest.mark.timeout(30)
class TestGetDiffContext:
    @pytest.mark.asyncio
    async def test_returns_markdown(self, server, mcp_repo):
        result = await server.call_tool(
            "diffctx_context",
            {"repo_path": str(mcp_repo.path), "diff_ref": "HEAD~1..HEAD", "mode": "pack"},
        )
        text = _get_text(result)
        assert "```" in text
        assert "calc.py" in text

    @pytest.mark.asyncio
    async def test_returns_nonempty_for_real_diff(self, server, mcp_repo):
        result = await server.call_tool(
            "diffctx_context",
            {"repo_path": str(mcp_repo.path), "diff_ref": "HEAD~1..HEAD", "mode": "pack"},
        )
        text = _get_text(result)
        assert len(text) > 100
        assert "```" in text

    @pytest.mark.asyncio
    async def test_budget_is_respected(self, server, mcp_repo):
        result_small = await server.call_tool(
            "diffctx_context",
            {"repo_path": str(mcp_repo.path), "diff_ref": "HEAD~1..HEAD", "budget_tokens": 200},
        )
        result_large = await server.call_tool(
            "diffctx_context",
            {"repo_path": str(mcp_repo.path), "diff_ref": "HEAD~1..HEAD", "budget_tokens": 8000},
        )
        assert len(_get_text(result_small)) <= len(_get_text(result_large))

    @pytest.mark.asyncio
    async def test_include_raw_diff_embeds_patch_ahead_of_fragments(self, server, mcp_repo):
        result = await server.call_tool(
            "diffctx_context",
            {"repo_path": str(mcp_repo.path), "diff_ref": "HEAD~1..HEAD", "mode": "pack", "include_raw_diff": True},
        )
        text = _get_text(result)
        assert "## Raw diff" in text
        assert "+def subtract(a, b):" in text
        assert text.index("## Raw diff") < text.index("## `")

    @pytest.mark.asyncio
    async def test_raw_diff_is_additive_selection_unchanged(self, server, mcp_repo):
        args = {"repo_path": str(mcp_repo.path), "diff_ref": "HEAD~1..HEAD", "mode": "pack"}
        without = _get_text(await server.call_tool("diffctx_context", args))
        with_raw = _get_text(await server.call_tool("diffctx_context", {**args, "include_raw_diff": True}))
        assert "## Raw diff" not in without
        raw_start = with_raw.index("## Raw diff")
        fragments_after_raw = with_raw[with_raw.index("\n## ", raw_start + 1) :]
        assert fragments_after_raw in without

    @pytest.mark.asyncio
    async def test_locate_mode_returns_versioned_navigation_json(self, server, mcp_repo):
        import json

        result = await server.call_tool(
            "diffctx_context",
            {"repo_path": str(mcp_repo.path), "diff_ref": "HEAD~1..HEAD", "mode": "locate"},
        )
        doc = json.loads(_get_text(result))
        assert doc["schema"] == "diffctx.locate.v1"
        assert doc["items"]
        assert all(i["reasons"] for i in doc["items"])

    @pytest.mark.asyncio
    async def test_locate_rejects_include_raw_diff(self, server, mcp_repo):
        args = {
            "repo_path": str(mcp_repo.path),
            "diff_ref": "HEAD~1..HEAD",
            "mode": "locate",
            "include_raw_diff": True,
        }
        with pytest.raises(ToolError, match="pack"):
            await server.call_tool("diffctx_context", args)

    @pytest.mark.asyncio
    async def test_locate_clipboard_degrades_to_inline_json(self, server, mcp_repo, monkeypatch):
        monkeypatch.setattr("diffctx.clipboard.detect_clipboard_command", lambda: None)
        result = await server.call_tool(
            "diffctx_context",
            {"repo_path": str(mcp_repo.path), "diff_ref": "HEAD~1..HEAD", "mode": "locate", "clipboard": True},
        )
        text = _get_text(result)
        assert "clipboard unavailable" in text
        assert '"diffctx.locate.v1"' in text

    @pytest.mark.asyncio
    async def test_locate_respects_max_tokens_cap(self, server, mcp_repo):
        result = await server.call_tool(
            "diffctx_context",
            {"repo_path": str(mcp_repo.path), "diff_ref": "HEAD~1..HEAD", "mode": "locate", "max_tokens": 1},
        )
        text = _get_text(result)
        assert "exceeding max_tokens" in text
        assert "diffctx.locate.v1" not in text

    @pytest.mark.asyncio
    async def test_invalid_mode_is_rejected(self, server, mcp_repo):
        args = {"repo_path": str(mcp_repo.path), "mode": "navigate"}
        with pytest.raises(ToolError, match="mode"):
            await server.call_tool("diffctx_context", args)

    @pytest.mark.asyncio
    async def test_invalid_repo_path(self, server, tmp_path):
        args = {"repo_path": str(tmp_path / "nonexistent"), "diff_ref": "HEAD~1..HEAD"}
        with pytest.raises(ToolError, match="Not a directory"):
            await server.call_tool("diffctx_context", args)

    @pytest.mark.asyncio
    async def test_not_a_git_repo(self, server, tmp_path):
        plain_dir = tmp_path / "not_a_repo"
        plain_dir.mkdir()
        args = {"repo_path": str(plain_dir), "diff_ref": "HEAD~1..HEAD"}
        with pytest.raises(ToolError, match="Not a git repository"):
            await server.call_tool("diffctx_context", args)

    @pytest.mark.asyncio
    async def test_allowed_paths_enforcement(self, server, mcp_repo, monkeypatch):
        monkeypatch.setenv("DIFFCTX_ALLOWED_PATHS", "/some/other/path")
        args = {"repo_path": str(mcp_repo.path), "diff_ref": "HEAD~1..HEAD"}
        with pytest.raises(ToolError, match="not in allowed paths"):
            await server.call_tool("diffctx_context", args)

    @pytest.mark.asyncio
    async def test_invalid_diff_range(self, server, mcp_repo):
        args = {"repo_path": str(mcp_repo.path), "diff_ref": "nonexistent_ref..HEAD"}
        with pytest.raises(ToolError):
            await server.call_tool("diffctx_context", args)


class TestGetTreeMap:
    pytestmark = pytest.mark.usefixtures("legacy_tools")

    @pytest.mark.asyncio
    async def test_returns_tree_for_repo(self, server, mcp_repo):
        result = await server.call_tool(
            "get_tree_map",
            {"repo_path": str(mcp_repo.path)},
        )
        text = _get_text(result)
        assert "calc.py" in text
        assert "main.py" in text

    @pytest.mark.asyncio
    async def test_subdirectory_scopes_output(self, server, mcp_repo):
        result = await server.call_tool(
            "get_tree_map",
            {"repo_path": str(mcp_repo.path), "subdirectory": "src"},
        )
        text = _get_text(result)
        assert "calc.py" in text

    @pytest.mark.asyncio
    async def test_no_content_omits_file_bodies(self, server, mcp_repo):
        result = await server.call_tool(
            "get_tree_map",
            {"repo_path": str(mcp_repo.path), "no_content": True},
        )
        text = _get_text(result)
        assert "calc.py" in text
        assert "def add" not in text

    @pytest.mark.asyncio
    async def test_unknown_output_format_falls_back_to_yaml(self, server, mcp_repo):
        result = await server.call_tool(
            "get_tree_map",
            {"repo_path": str(mcp_repo.path), "output_format": "not_a_format"},
        )
        text = _get_text(result)
        assert "calc.py" in text
        assert "name:" in text or "type:" in text


class TestGetFileContext:
    pytestmark = pytest.mark.usefixtures("legacy_tools")

    @pytest.mark.asyncio
    async def test_glob_returns_matched_file_contents(self, server, mcp_repo):
        result = await server.call_tool(
            "get_file_context",
            {"repo_path": str(mcp_repo.path), "patterns": ["src/*.py"]},
        )
        text = _get_text(result)
        assert "calc.py" in text
        assert "def add" in text

    @pytest.mark.asyncio
    async def test_dry_run_lists_without_reading(self, server, mcp_repo):
        result = await server.call_tool(
            "get_file_context",
            {"repo_path": str(mcp_repo.path), "patterns": ["src/*.py"], "dry_run": True},
        )
        text = _get_text(result)
        assert "Would match" in text
        assert "def add" not in text

    @pytest.mark.asyncio
    async def test_no_match_returns_explicit_message(self, server, mcp_repo):
        result = await server.call_tool(
            "get_file_context",
            {"repo_path": str(mcp_repo.path), "patterns": ["nonexistent_*.xyz"]},
        )
        text = _get_text(result)
        assert "No files matched" in text


class TestPathTraversalContainment:
    pytestmark = pytest.mark.usefixtures("legacy_tools")

    @pytest.mark.asyncio
    async def test_get_file_context_glob_traversal_returns_only_contained_paths(self, server, mcp_repo):
        outside_secret = mcp_repo.path.parent / "secret.txt"
        outside_secret.write_text("SHOULD_NOT_LEAK\n")
        result = await server.call_tool(
            "get_file_context",
            {"repo_path": str(mcp_repo.path), "patterns": ["../secret.txt"]},
        )
        text = _get_text(result)
        assert "SHOULD_NOT_LEAK" not in text, "glob traversal escaped repo_path; M2 regression"

    @pytest.mark.asyncio
    async def test_get_tree_map_subdirectory_traversal_rejected(self, server, mcp_repo):
        args = {"repo_path": str(mcp_repo.path), "subdirectory": "../"}
        with pytest.raises((ToolError, ValueError), match=r"escapes|outside|not.*directory"):
            await server.call_tool("get_tree_map", args)


class TestToolDeadline:
    pytestmark = pytest.mark.usefixtures("legacy_tools")
    """The engine caps each git subprocess but not the CPU-bound phases, and
    the MCP path has no CLI watchdog behind it — without a deadline in the tool
    itself one call wedges the server for the lifetime of the client."""

    @pytest.mark.asyncio
    async def test_tool_call_fails_when_the_deadline_passes(self, server, mcp_repo, monkeypatch):
        from diffctx.mcp import server as server_module

        monkeypatch.setattr(server_module, "_DEFAULT_TIMEOUT_SECONDS", 0)
        args = {"repo_path": str(mcp_repo.path)}
        with pytest.raises(ToolError, match="exceeded the 0s deadline"):
            await server.call_tool("get_tree_map", args)

    @pytest.mark.asyncio
    async def test_every_tool_warns_that_repo_content_is_untrusted(self, server):
        tools = await server.list_tools()
        assert tools
        for tool in tools:
            assert "untrusted" in (tool.description or ""), f"{tool.name} omits the untrusted-content warning"


class TestSymlinkJail:
    pytestmark = pytest.mark.usefixtures("legacy_tools")
    """The allow-list is only a jail if containment is decided on the real path:
    a symlink sitting inside an allowed directory otherwise hands the server
    read access to whatever it points at."""

    @pytest.mark.asyncio
    async def test_repo_path_symlink_out_of_allowed_dir_is_rejected(self, server, tmp_path, monkeypatch):
        allowed = tmp_path / "allowed"
        allowed.mkdir()
        outside = Pygit2Repo(tmp_path / "outside_repo")
        outside.add_file("src/calc.py", "def add(a, b):\n    return a + b\n")
        outside.commit("initial commit")
        link = allowed / "repo_link"
        link.symlink_to(outside.path, target_is_directory=True)

        monkeypatch.setenv("DIFFCTX_ALLOWED_PATHS", str(allowed))
        args = {"repo_path": str(link), "diff_ref": "HEAD"}
        with pytest.raises(ToolError, match="not in allowed paths"):
            await server.call_tool("diffctx_context", args)

    @pytest.mark.asyncio
    async def test_get_file_context_does_not_follow_symlink_out_of_repo(self, server, mcp_repo):
        outside_secret = mcp_repo.path.parent / "outside_secret.txt"
        outside_secret.write_text("SHOULD_NOT_LEAK\n")
        (mcp_repo.path / "innocent.txt").symlink_to(outside_secret)

        result = await server.call_tool(
            "get_file_context",
            {"repo_path": str(mcp_repo.path), "patterns": ["*.txt"]},
        )
        assert "SHOULD_NOT_LEAK" not in _get_text(result)

    @pytest.mark.asyncio
    async def test_get_tree_map_subdirectory_symlink_out_of_repo_is_rejected(self, server, mcp_repo, tmp_path):
        outside_dir = tmp_path / "outside_dir"
        outside_dir.mkdir()
        (outside_dir / "secret.txt").write_text("SHOULD_NOT_LEAK\n")
        (mcp_repo.path / "escape").symlink_to(outside_dir, target_is_directory=True)

        args = {"repo_path": str(mcp_repo.path), "subdirectory": "escape"}
        with pytest.raises(ToolError, match="escapes repo_path"):
            await server.call_tool("get_tree_map", args)


class TestBudgetTokensValidation:
    """budget_tokens is the only diffctx_context parameter with no guard: a
    negative value below the -1 unlimited sentinel used to sail straight
    into the native pipeline, and 0 (a legitimate strict-zero floor) had no
    documented meaning at this surface."""

    @pytest.mark.asyncio
    async def test_value_below_unlimited_sentinel_is_rejected(self, server, mcp_repo):
        args = {"repo_path": str(mcp_repo.path), "diff_ref": "HEAD~1..HEAD", "budget_tokens": -2}
        with pytest.raises(ToolError, match=r"budget_tokens must be >= -1"):
            await server.call_tool("diffctx_context", args)

    @pytest.mark.asyncio
    async def test_strict_zero_floor_is_accepted(self, server, mcp_repo):
        result = await server.call_tool(
            "diffctx_context",
            {"repo_path": str(mcp_repo.path), "diff_ref": "HEAD~1..HEAD", "budget_tokens": 0},
        )
        assert isinstance(_get_text(result), str)

    @pytest.mark.asyncio
    async def test_unlimited_budget_is_still_capped_by_max_tokens(self, server, mcp_repo):
        result = await server.call_tool(
            "diffctx_context",
            {
                "repo_path": str(mcp_repo.path),
                "diff_ref": "HEAD~1..HEAD",
                "budget_tokens": -1,
                "max_tokens": 50,
            },
        )
        text = _get_text(result)
        assert "exceeding max_tokens=50" in text
        assert "Nothing was returned" in text


@pytest.mark.timeout(120)
class TestTokenBudgetGuardExecutes:
    """The over-budget branch on get_tree_map (:214) and get_file_context
    (:324) decides between returning real content and a refusal notice.
    Every other fixture in this suite stays far under the 25k default, so
    the branch was dead code outside these tests.

    The class-level timeout overrides the global 10s: the diff-context legs
    run the full parse+graph+selection pipeline over a 4000-function file at
    an unlimited budget, which fits 10s locally but not on a loaded CI
    runner sharing the box with xdist siblings."""

    pytestmark = pytest.mark.usefixtures("legacy_tools")

    @pytest.fixture
    def big_file_repo(self, tmp_path):
        repo = Pygit2Repo(tmp_path / "big_file_repo")
        repo.add_file("small.py", "def noop():\n    return None\n")
        repo.commit("initial commit")
        big_content = "\n".join(f"def func_{i}():\n    return {i}  # marker_{i}" for i in range(4000))
        repo.add_file("big.py", big_content)
        repo.commit("add big file")
        return repo

    @pytest.mark.asyncio
    async def test_get_tree_map_over_budget_returns_notice(self, server, big_file_repo):
        result = await server.call_tool(
            "get_tree_map",
            {"repo_path": str(big_file_repo.path), "max_tokens": 100},
        )
        text = _get_text(result)
        assert "exceeding max_tokens=100" in text
        assert "Nothing was returned" in text

    @pytest.mark.asyncio
    async def test_get_tree_map_under_generous_budget_returns_content(self, server, big_file_repo):
        result = await server.call_tool(
            "get_tree_map",
            {"repo_path": str(big_file_repo.path), "max_tokens": 200_000},
        )
        assert "func_0" in _get_text(result)

    @pytest.mark.asyncio
    async def test_get_file_context_over_budget_returns_notice(self, server, big_file_repo):
        result = await server.call_tool(
            "get_file_context",
            {"repo_path": str(big_file_repo.path), "patterns": ["big.py"], "max_tokens": 100},
        )
        text = _get_text(result)
        assert "exceeding max_tokens=100" in text
        assert "Nothing was returned" in text

    @pytest.mark.asyncio
    async def test_get_file_context_under_generous_budget_returns_content(self, server, big_file_repo):
        result = await server.call_tool(
            "get_file_context",
            {"repo_path": str(big_file_repo.path), "patterns": ["big.py"], "max_tokens": 200_000},
        )
        assert "func_0" in _get_text(result)

    @pytest.mark.asyncio
    async def test_diffctx_context_over_budget_returns_notice(self, server, big_file_repo):
        result = await server.call_tool(
            "diffctx_context",
            {
                "repo_path": str(big_file_repo.path),
                "diff_ref": "HEAD~1..HEAD",
                "budget_tokens": -1,
                "max_tokens": 100,
            },
        )
        text = _get_text(result)
        assert "exceeding max_tokens=100" in text

    @pytest.mark.asyncio
    async def test_diffctx_context_under_generous_budget_returns_content(self, server, big_file_repo):
        result = await server.call_tool(
            "diffctx_context",
            {
                "repo_path": str(big_file_repo.path),
                "diff_ref": "HEAD~1..HEAD",
                "budget_tokens": -1,
                "max_tokens": 200_000,
            },
        )
        assert "func_0" in _get_text(result)


class TestFileContextTruncationAndDedup:
    """With 400 matches the old loop silently stopped at max_files and
    reported '# 50 files matched' with no truncation marker — the agent had
    no way to tell a full read from a partial one. Overlapping glob patterns
    (a natural LLM-authored pair) also burned slots twice for the same
    file."""

    pytestmark = pytest.mark.usefixtures("legacy_tools")

    @pytest.fixture
    def many_files_repo(self, tmp_path):
        repo = Pygit2Repo(tmp_path / "many_files_repo")
        for i in range(60):
            repo.add_file(f"src/mod_{i:03d}.py", f"def fn_{i}():\n    return {i}\n")
        repo.commit("many files")
        return repo

    @pytest.mark.asyncio
    async def test_truncation_beyond_max_files_is_disclosed(self, server, many_files_repo):
        result = await server.call_tool(
            "get_file_context",
            {"repo_path": str(many_files_repo.path), "patterns": ["src/*.py"], "max_files": 10},
        )
        text = _get_text(result)
        assert "10 files matched" in text
        assert "60 total" in text
        assert "TRUNCATED" in text

    @pytest.mark.asyncio
    async def test_dry_run_truncation_is_disclosed(self, server, many_files_repo):
        result = await server.call_tool(
            "get_file_context",
            {
                "repo_path": str(many_files_repo.path),
                "patterns": ["src/*.py"],
                "max_files": 10,
                "dry_run": True,
            },
        )
        text = _get_text(result)
        assert "Would match 60 files" in text
        assert "TRUNCATED" in text

    @pytest.mark.asyncio
    async def test_no_truncation_when_matches_fit(self, server, mcp_repo):
        result = await server.call_tool(
            "get_file_context",
            {"repo_path": str(mcp_repo.path), "patterns": ["src/*.py"]},
        )
        assert "TRUNCATED" not in _get_text(result)

    @pytest.mark.asyncio
    async def test_absolute_pattern_reaching_the_repo_through_a_symlink_is_reported(self, server, mcp_repo, tmp_path):
        """Containment is checked on the resolved path while the report keyed off
        the raw glob match, so a file accepted as inside the repo could still be
        inexpressible relative to it — surfacing as an opaque ValueError instead
        of the file's contents."""
        link = tmp_path / "repo_link"
        link.symlink_to(mcp_repo.path, target_is_directory=True)

        result = await server.call_tool(
            "get_file_context",
            {"repo_path": str(mcp_repo.path), "patterns": [str(link / "src" / "calc.py")]},
        )
        text = _get_text(result)
        assert "## src/calc.py" in text
        assert "def add" in text

    @pytest.mark.asyncio
    async def test_overlapping_glob_patterns_do_not_duplicate_files(self, server, mcp_repo):
        result = await server.call_tool(
            "get_file_context",
            {"repo_path": str(mcp_repo.path), "patterns": ["src/*.py", "src/**/*.py"]},
        )
        text = _get_text(result)
        assert text.count("## src/calc.py") == 1
        assert text.count("## src/main.py") == 1
        assert "2 files matched" in text


class TestClipboardDegradation:
    """copy_to_clipboard raises ClipboardError with no DISPLAY/WAYLAND_DISPLAY
    or pbcopy — the default state of a headless MCP server. The CLI degrades
    to stdout in that case; the MCP tools must not throw away already-computed
    content and raise a bare ToolError instead."""

    pytestmark = pytest.mark.usefixtures("legacy_tools")

    @pytest.mark.asyncio
    async def test_diffctx_context_degrades_instead_of_raising(self, server, mcp_repo, monkeypatch):
        monkeypatch.setattr("diffctx.clipboard.detect_clipboard_command", lambda: None)
        result = await server.call_tool(
            "diffctx_context",
            {"repo_path": str(mcp_repo.path), "diff_ref": "HEAD~1..HEAD", "clipboard": True},
        )
        text = _get_text(result)
        assert "clipboard unavailable" in text
        assert "calc.py" in text

    @pytest.mark.asyncio
    async def test_get_tree_map_degrades_instead_of_raising(self, server, mcp_repo, monkeypatch):
        monkeypatch.setattr("diffctx.clipboard.detect_clipboard_command", lambda: None)
        result = await server.call_tool(
            "get_tree_map",
            {"repo_path": str(mcp_repo.path), "clipboard": True},
        )
        text = _get_text(result)
        assert "clipboard unavailable" in text
        assert "calc.py" in text

    @pytest.mark.asyncio
    async def test_get_file_context_degrades_instead_of_raising(self, server, mcp_repo, monkeypatch):
        monkeypatch.setattr("diffctx.clipboard.detect_clipboard_command", lambda: None)
        result = await server.call_tool(
            "get_file_context",
            {"repo_path": str(mcp_repo.path), "patterns": ["src/*.py"], "clipboard": True},
        )
        text = _get_text(result)
        assert "clipboard unavailable" in text
        assert "def add" in text


class TestRepoPathWalksUpToRoot:
    """diffctx_context(repo_path='/repo/src') used to fail with 'Not a git
    repository' even though it plainly is inside one. repo_path only locates
    the .git directory (diff_range still addresses the whole repo), so
    walking up to the repo root is safe and turns a dead end into a working
    call. Bare repos and worktree checkouts were rejected the same way even
    at their own root."""

    @staticmethod
    def _run_git(*args, cwd):
        subprocess.run(["git", *args], cwd=cwd, check=True, capture_output=True, text=True)

    @pytest.mark.asyncio
    async def test_subdirectory_of_a_normal_repo_is_accepted(self, server, mcp_repo):
        result = await server.call_tool(
            "diffctx_context",
            {"repo_path": str(mcp_repo.path / "src"), "diff_ref": "HEAD~1..HEAD"},
        )
        assert "calc.py" in _get_text(result)

    @pytest.mark.asyncio
    async def test_subdirectory_of_a_worktree_checkout_is_accepted(self, server, mcp_repo, tmp_path):
        worktree_path = tmp_path / "wt"
        self._run_git("worktree", "add", str(worktree_path), "-b", "wt-branch", cwd=mcp_repo.path)
        result = await server.call_tool(
            "diffctx_context",
            {"repo_path": str(worktree_path / "src"), "diff_ref": "HEAD~1..HEAD"},
        )
        assert "calc.py" in _get_text(result)

    @pytest.mark.asyncio
    async def test_a_bare_clone_is_accepted(self, server, mcp_repo, tmp_path):
        bare_path = tmp_path / "bare.git"
        self._run_git("clone", "--bare", str(mcp_repo.path), str(bare_path), cwd=tmp_path)
        result = await server.call_tool(
            "diffctx_context",
            {"repo_path": str(bare_path), "diff_ref": "HEAD~1..HEAD"},
        )
        assert "calc.py" in _get_text(result)

    @pytest.mark.asyncio
    async def test_a_directory_with_no_git_repo_anywhere_above_it_is_still_rejected(self, server, tmp_path):
        plain_dir = tmp_path / "not_a_repo"
        plain_dir.mkdir()
        args = {"repo_path": str(plain_dir), "diff_ref": "HEAD~1..HEAD"}
        with pytest.raises(ToolError, match="Not a git repository"):
            await server.call_tool("diffctx_context", args)


class TestConcurrencyAndTimeoutRecovery:
    """abandon_on_cancel=True exists so a timed-out call fails fast without
    wedging the server. The only prior timeout test never actually asserted
    that goal — it just checked the error string. These tests exercise the
    stated goal directly: a following call must still succeed, and two real
    calls must be able to run concurrently."""

    pytestmark = pytest.mark.usefixtures("legacy_tools")

    @pytest.mark.asyncio
    async def test_server_stays_responsive_after_a_timed_out_call(self, server, mcp_repo, monkeypatch):
        from diffctx.mcp import server as server_module

        monkeypatch.setattr(server_module, "_DEFAULT_TIMEOUT_SECONDS", 0)
        args = {"repo_path": str(mcp_repo.path)}
        with pytest.raises(ToolError, match="exceeded the 0s deadline"):
            await server.call_tool("get_tree_map", args)

        monkeypatch.setattr(server_module, "_DEFAULT_TIMEOUT_SECONDS", 300)
        result = await server.call_tool("get_tree_map", {"repo_path": str(mcp_repo.path)})
        assert "calc.py" in _get_text(result)

    @pytest.mark.asyncio
    async def test_concurrent_tool_calls_on_a_real_repo_both_succeed(self, server, mcp_repo):
        diff_result, tree_result = await asyncio.gather(
            server.call_tool(
                "diffctx_context",
                {"repo_path": str(mcp_repo.path), "diff_ref": "HEAD~1..HEAD"},
            ),
            server.call_tool("get_tree_map", {"repo_path": str(mcp_repo.path)}),
        )
        assert "calc.py" in _get_text(diff_result)
        assert "calc.py" in _get_text(tree_result)


class TestGitRefInjection:
    """A diff range reaches a `git` argv. Anything option-shaped must be
    refused before git can interpret it — `--ext-diff`/`--textconv` would
    re-enable repository-configured filter commands that diffctx disables."""

    @pytest.mark.parametrize(
        "diff_range",
        ["--ext-diff", "--textconv", "-p", "--upload-pack=touch /tmp/pwn", "-u"],
    )
    @pytest.mark.asyncio
    async def test_option_shaped_range_is_refused_before_git_sees_it(self, server, mcp_repo, diff_range):
        args = {"repo_path": str(mcp_repo.path), "diff_ref": diff_range}
        with pytest.raises(ToolError, match="invalid diff range:"):
            await server.call_tool("diffctx_context", args)

    @pytest.mark.asyncio
    async def test_dashes_inside_a_ref_name_still_reach_git(self, server, mcp_repo):
        """The syntax gate rejects a leading dash only. A branch named
        `no-such-branch` must still be resolved by git, not refused as
        malformed."""
        args = {"repo_path": str(mcp_repo.path), "diff_ref": "no-such-branch..HEAD"}
        with pytest.raises(ToolError, match="git log --oneline"):
            await server.call_tool("diffctx_context", args)

    @pytest.mark.asyncio
    async def test_ordinary_range_is_unaffected(self, server, mcp_repo):
        result = await server.call_tool(
            "diffctx_context",
            {"repo_path": str(mcp_repo.path), "diff_ref": "HEAD~1..HEAD"},
        )
        assert "calc.py" in _get_text(result)


class TestFileContextHonoursTheIgnoreContract:
    """`get_file_context` accepts `**/*`, so it is the widest read surface the
    MCP server exposes. It used to glob without the ignore specs that
    `get_tree_map` and diff mode both apply, which made `.diffctx/ignore` — a
    security contract per QA.md — advisory for anyone who asked directly."""

    pytestmark = pytest.mark.usefixtures("legacy_tools")

    @staticmethod
    def _repo(tmp_path):
        root = tmp_path / "ignored_repo"
        (root / ".diffctx").mkdir(parents=True)
        (root / ".diffctx" / "ignore").write_text("secrets/\n*.key\n")
        (root / "secrets").mkdir()
        (root / "secrets" / "prod.env").write_text("API_TOKEN=must-not-surface\n")
        (root / "deploy.key").write_text("PRIVATE-KEY-must-not-surface\n")
        (root / "app.py").write_text("def run():\n    return 1\n")
        return root

    @pytest.mark.asyncio
    async def test_a_declared_exclusion_is_not_readable_through_a_glob(self, server, tmp_path):
        root = self._repo(tmp_path)
        result = await server.call_tool(
            "get_file_context",
            {"repo_path": str(root), "patterns": ["**/*"]},
        )
        text = _get_text(result)

        assert "must-not-surface" not in text, "an ignored file's contents were returned"
        assert "prod.env" not in text
        assert "deploy.key" not in text
        assert "app.py" in text, "the fix must not stop ordinary files being read"

    @pytest.mark.asyncio
    async def test_an_excluded_file_is_not_counted_in_the_truncation_notice(self, server, tmp_path):
        """Reporting "N more files" for withheld files would leak that they
        exist, which is most of what the exclusion is protecting."""
        root = self._repo(tmp_path)
        result = await server.call_tool(
            "get_file_context",
            {"repo_path": str(root), "patterns": ["**/*"], "dry_run": True},
        )
        text = _get_text(result)

        assert "secrets" not in text
        assert "deploy.key" not in text


@pytest.mark.timeout(60)
class TestProgressiveDisclosure:
    """The two-call shape #127 collapsed the server onto: rank, then read.

    The acceptance criterion is behavioural, not cosmetic — a diff question has
    to be answerable in two calls, with the second one addressed entirely by
    fields the first returned.
    """

    @pytest.mark.asyncio
    async def test_a_diff_question_is_answered_in_two_calls(self, server, mcp_repo):
        import json

        ranking = json.loads(
            _get_text(
                await server.call_tool(
                    "diffctx_context",
                    {"repo_path": str(mcp_repo.path), "diff_ref": "HEAD~1..HEAD"},
                )
            )
        )
        # Ids are built from the ranking's own fields, with no side table: that
        # is what makes the second call derivable from the first.
        ids = [f"{i['path']}:{i['lines']}" for i in ranking["items"][:3]]
        assert ids

        bodies = _get_text(
            await server.call_tool(
                "diffctx_context",
                {"repo_path": str(mcp_repo.path), "diff_ref": "HEAD~1..HEAD", "fragment_ids": ids},
            )
        )
        assert "def subtract(a, b):" in bodies
        assert "```" in bodies

    @pytest.mark.asyncio
    async def test_fragment_ids_win_over_mode(self, server, mcp_repo):
        """Passing ids means "read these"; it must not silently return a ranking
        because `mode` still holds its default."""
        text = _get_text(
            await server.call_tool(
                "diffctx_context",
                {
                    "repo_path": str(mcp_repo.path),
                    "diff_ref": "HEAD~1..HEAD",
                    "mode": "locate",
                    "fragment_ids": ["src/calc.py:1-2"],
                },
            )
        )
        assert "diffctx.locate.v1" not in text
        assert "def add(a, b):" in text

    @pytest.mark.asyncio
    async def test_bodies_come_from_the_revision_the_ranking_used(self, server, tmp_path):
        """The line numbers in a ranking belong to the diff's end revision, so
        the bodies must be read there too.

        Reading the working tree instead would mis-slice every fragment on any
        historical range — and still look like source, which is why this is
        pinned rather than left to review.
        """
        repo = Pygit2Repo(tmp_path / "revision_repo")
        repo.add_file("mod.py", "def one():\n    return 1\n")
        repo.commit("first")
        repo.add_file("mod.py", "def one():\n    return 1\n\n\ndef two():\n    return 2\n")
        repo.commit("second")
        # A third commit moves the working tree past the range under test.
        repo.add_file("mod.py", "def one():\n    return 1\n\n\ndef LATER():\n    return 99\n")
        repo.commit("third")

        text = _get_text(
            await server.call_tool(
                "diffctx_context",
                {"repo_path": str(repo.path), "diff_ref": "HEAD~2..HEAD~1", "fragment_ids": ["mod.py:5-6"]},
            )
        )
        assert "def two():" in text
        assert "LATER" not in text

    @pytest.mark.asyncio
    async def test_a_declared_exclusion_is_not_readable_through_fragment_ids(self, server, tmp_path):
        """`.diffctx/ignore` is a security contract per QA.md. fragment_ids is a
        new read surface, and an id can arrive from anything the model read, so
        the contract is enforced here or it is advisory again."""
        root = tmp_path / "jail_repo"
        (root / ".diffctx").mkdir(parents=True)
        (root / ".diffctx" / "ignore").write_text("secrets/\n*.key\n")
        (root / "secrets").mkdir()
        (root / "secrets" / "prod.env").write_text("API_TOKEN=must-not-surface\n")
        (root / "deploy.key").write_text("PRIVATE-KEY-must-not-surface\n")
        repo = Pygit2Repo(root)
        repo.add_file("app.py", "def run():\n    return 1\n")
        repo.commit("initial")

        text = _get_text(
            await server.call_tool(
                "diffctx_context",
                {
                    "repo_path": str(root),
                    "diff_ref": "HEAD",
                    "fragment_ids": ["secrets/prod.env", "deploy.key"],
                },
            )
        )
        assert "must-not-surface" not in text
        assert "API_TOKEN" not in text

    @pytest.mark.asyncio
    async def test_an_id_escaping_the_repo_is_refused(self, server, mcp_repo, tmp_path):
        outside = tmp_path / "outside.txt"
        outside.write_text("OUTSIDE-must-not-surface\n")
        text = _get_text(
            await server.call_tool(
                "diffctx_context",
                {
                    "repo_path": str(mcp_repo.path),
                    "diff_ref": "HEAD~1..HEAD",
                    "fragment_ids": ["../outside.txt", str(outside)],
                },
            )
        )
        assert "OUTSIDE-must-not-surface" not in text

    @pytest.mark.asyncio
    async def test_one_malformed_id_costs_that_id_not_the_call(self, server, mcp_repo):
        text = _get_text(
            await server.call_tool(
                "diffctx_context",
                {
                    "repo_path": str(mcp_repo.path),
                    "diff_ref": "HEAD~1..HEAD",
                    "fragment_ids": ["src/calc.py:1-2", "src/calc.py:9-3", "no/such/file.py:1-2"],
                },
            )
        )
        assert "def add(a, b):" in text
        # Every id is accounted for: a dropped one would read as "empty
        # fragment" rather than "refused fragment".
        assert "Unparseable" in text
        assert "Not found" in text

    @pytest.mark.asyncio
    async def test_a_batch_larger_than_the_limit_is_refused(self, server, mcp_repo):
        from diffctx.mcp.fetch import MAX_FETCH_IDS

        args = {
            "repo_path": str(mcp_repo.path),
            "diff_ref": "HEAD~1..HEAD",
            "fragment_ids": [f"src/calc.py:{i}-{i}" for i in range(MAX_FETCH_IDS + 1)],
        }
        with pytest.raises(ToolError, match="pack"):
            await server.call_tool("diffctx_context", args)


class TestLegacyToolsAreOptIn:
    """#127's cost claim only holds if the wide tools are genuinely absent by
    default, so this runs in a fresh process: the registration is read from the
    environment at import, and a test that registers them in-process cannot
    also prove they are missing.
    """

    @staticmethod
    def _tool_names(env_extra: dict[str, str]) -> list[str]:
        import json
        import os
        import sys

        script = (
            "import asyncio, json\n"
            "from diffctx.mcp.server import mcp\n"
            "print(json.dumps([t.name for t in asyncio.run(mcp.list_tools())]))\n"
        )
        proc = subprocess.run(
            [sys.executable, "-c", script],
            capture_output=True,
            text=True,
            timeout=120,
            env={**os.environ, **env_extra},
        )
        assert proc.returncode == 0, proc.stderr
        names: list[str] = json.loads(proc.stdout.strip().splitlines()[-1])
        return names

    def test_a_default_server_exposes_only_the_one_tool(self):
        assert self._tool_names({"DIFFCTX_MCP_LEGACY_TOOLS": ""}) == ["diffctx_context"]

    def test_the_flag_restores_the_pre_v3_surface(self):
        names = self._tool_names({"DIFFCTX_MCP_LEGACY_TOOLS": "1"})
        assert set(names) == {"diffctx_context", "get_tree_map", "get_file_context"}


class TestToolDefinitionBudget:
    """#127's gate, pinned so it cannot decay by prose creep.

    Definitions are sent on every request of every session before any work
    happens, so this cost is paid by every window the server is installed in
    whether or not the tool is ever called. The three-tool surface measured 1063
    tokens; the ceilings below leave room to edit but not to drift back.
    """

    @staticmethod
    def _definition_tokens(tool) -> int:
        import json

        import tiktoken

        enc = tiktoken.get_encoding("o200k_base")
        payload = {"name": tool.name, "description": tool.description, "inputSchema": tool.inputSchema}
        return len(enc.encode(json.dumps(payload, separators=(",", ":"))))

    @pytest.mark.asyncio
    async def test_the_default_surface_stays_under_the_budget(self, server):
        tools = [t for t in await server.list_tools() if t.name == "diffctx_context"]
        assert len(tools) == 1
        total = self._definition_tokens(tools[0])
        # 1063 * 0.4 = 425: the acceptance criterion was a >=60% cut.
        assert total <= 425, f"tool definition grew to {total} tokens, above the #127 budget"

    @pytest.mark.asyncio
    async def test_the_description_stays_under_the_budget(self, server):
        import tiktoken

        tool = next(t for t in await server.list_tools() if t.name == "diffctx_context")
        enc = tiktoken.get_encoding("o200k_base")
        n = len(enc.encode(tool.description))
        assert n <= 80, f"description grew to {n} tokens, above the #127 budget of 80"
        # The safety boundary is part of that budget, not an exemption from it:
        # trimming prose must not be achieved by trimming this.
        assert "untrusted" in tool.description
