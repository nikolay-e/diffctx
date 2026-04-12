from __future__ import annotations

import pytest
from mcp.server.fastmcp.exceptions import ToolError

from tests.conftest import GARBAGE_FILES
from tests.framework.pygit2_backend import Pygit2Repo


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
    from treemapper.mcp.server import mcp

    return mcp


def _get_text(call_result: tuple) -> str:
    content_blocks = call_result[0]
    return content_blocks[0].text


@pytest.mark.timeout(30)
class TestGetDiffContext:
    @pytest.mark.asyncio
    async def test_returns_markdown(self, server, mcp_repo):
        result = await server.call_tool(
            "get_diff_context",
            {"repo_path": str(mcp_repo.path), "diff_range": "HEAD~1..HEAD"},
        )
        text = _get_text(result)
        assert "```" in text
        assert "calc.py" in text

    @pytest.mark.asyncio
    async def test_returns_nonempty_for_real_diff(self, server, mcp_repo):
        result = await server.call_tool(
            "get_diff_context",
            {"repo_path": str(mcp_repo.path), "diff_range": "HEAD~1..HEAD"},
        )
        text = _get_text(result)
        assert len(text) > 100
        assert "```" in text

    @pytest.mark.asyncio
    async def test_budget_is_respected(self, server, mcp_repo):
        result_small = await server.call_tool(
            "get_diff_context",
            {"repo_path": str(mcp_repo.path), "diff_range": "HEAD~1..HEAD", "budget_tokens": 200},
        )
        result_large = await server.call_tool(
            "get_diff_context",
            {"repo_path": str(mcp_repo.path), "diff_range": "HEAD~1..HEAD", "budget_tokens": 8000},
        )
        assert len(_get_text(result_small)) <= len(_get_text(result_large))

    @pytest.mark.asyncio
    async def test_invalid_repo_path(self, server, tmp_path):
        with pytest.raises(ToolError, match="Not a directory"):
            await server.call_tool(
                "get_diff_context",
                {"repo_path": str(tmp_path / "nonexistent"), "diff_range": "HEAD~1..HEAD"},
            )

    @pytest.mark.asyncio
    async def test_not_a_git_repo(self, server, tmp_path):
        plain_dir = tmp_path / "not_a_repo"
        plain_dir.mkdir()
        with pytest.raises(ToolError, match="Not a git repository"):
            await server.call_tool(
                "get_diff_context",
                {"repo_path": str(plain_dir), "diff_range": "HEAD~1..HEAD"},
            )

    @pytest.mark.asyncio
    async def test_allowed_paths_enforcement(self, server, mcp_repo, monkeypatch):
        monkeypatch.setenv("TREEMAPPER_ALLOWED_PATHS", "/some/other/path")
        with pytest.raises(ToolError, match="not in allowed paths"):
            await server.call_tool(
                "get_diff_context",
                {"repo_path": str(mcp_repo.path), "diff_range": "HEAD~1..HEAD"},
            )

    @pytest.mark.asyncio
    async def test_invalid_diff_range(self, server, mcp_repo):
        with pytest.raises(ToolError):
            await server.call_tool(
                "get_diff_context",
                {"repo_path": str(mcp_repo.path), "diff_range": "nonexistent_ref..HEAD"},
            )
