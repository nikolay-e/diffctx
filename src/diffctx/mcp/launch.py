from __future__ import annotations

import argparse
import sys

from diffctx.version import __version__

# The base package installs the diffctx-mcp script too (a console script cannot
# depend on an extra), so this module parses arguments and reports a missing
# extra before importing anything the extra provides (#333).
_EXTRA_MODULES = frozenset({"anyio", "mcp"})


def main(prog: str = "diffctx-mcp") -> None:
    parser = argparse.ArgumentParser(
        prog=prog,
        description="Run the diffctx MCP server (stdio transport) for editor/agent integration.",
    )
    parser.add_argument("-v", "--version", action="version", version=f"%(prog)s {__version__}")
    parser.parse_args()
    try:
        from .server import run_server
    except ModuleNotFoundError as e:
        if (e.name or "").partition(".")[0] not in _EXTRA_MODULES:
            raise
        print(
            f"{prog}: error: the MCP server needs the 'mcp' extra ({e.name} is not installed): "
            "pip install 'diffctx[mcp]', or run uvx --from 'diffctx[mcp]' diffctx-mcp",
            file=sys.stderr,
        )
        sys.exit(3)
    run_server()
