from __future__ import annotations

import logging
import sys
from functools import partial

import anyio
from mcp.server.fastmcp import FastMCP

from treemapper.diffctx import GitError, build_diff_context

from .formatting import format_diff_context_as_markdown
from .security import validate_repo_path

logger = logging.getLogger(__name__)

mcp = FastMCP("treemapper")

_TOOL_DESCRIPTION = (
    "Get the most relevant code fragments for understanding a git diff. "
    "Returns ranked function/class definitions, type references, and "
    "cross-file dependencies needed to understand a change WITHOUT "
    "dumping entire files.\n\n"
    "Use this tool when:\n"
    "- Reviewing a pull request or commit\n"
    "- Explaining what a change does\n"
    "- Analyzing the impact of a refactor\n"
    "- Investigating why tests broke after a commit\n\n"
    "Returns Markdown-formatted context with file paths, line ranges, "
    "and source code, ranked by relevance. Stays within the specified "
    "token budget using submodular optimization.\n\n"
    "Supports 50+ languages including Python, JavaScript, TypeScript, "
    "Go, Rust, Java, C/C++, Ruby, PHP, and more."
)


@mcp.tool(description=_TOOL_DESCRIPTION)
async def get_diff_context(
    repo_path: str,
    diff_range: str = "HEAD~1..HEAD",
    budget_tokens: int = 8000,
) -> str:
    validated_path = validate_repo_path(repo_path)
    try:
        result = await anyio.to_thread.run_sync(
            partial(
                build_diff_context,
                root_dir=validated_path,
                diff_range=diff_range,
                budget_tokens=budget_tokens,
            )
        )
    except GitError as e:
        msg = str(e)
        if "unknown revision" in msg or "bad revision" in msg:
            raise ValueError(
                f"Invalid diff range '{diff_range}'. "
                "Try 'HEAD~1..HEAD' for the last commit, "
                "'main..feature' for a branch comparison, "
                "or check that both refs exist with 'git log --oneline'."
            ) from e
        raise ValueError(f"Git error: {e}") from e
    return format_diff_context_as_markdown(result)


def run_server() -> None:
    logging.basicConfig(
        stream=sys.stderr,
        level=logging.WARNING,
        format="%(name)s: %(message)s",
    )
    mcp.run(transport="stdio")
