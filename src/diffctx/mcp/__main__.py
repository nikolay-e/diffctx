from __future__ import annotations

import argparse
import sys


def main(prog: str = "diffctx-mcp", extra: str = "diffctx[mcp]") -> None:
    parser = argparse.ArgumentParser(
        prog=prog,
        description="Run the diffctx MCP server (stdio transport) for editor/agent integration.",
    )
    parser.parse_args()

    try:
        from diffctx.mcp.server import run_server
    except ImportError:
        print(
            f"{prog}: missing optional dependencies for MCP server mode.\nInstall with: pip install '{extra}'",
            file=sys.stderr,
        )
        sys.exit(2)
    run_server()


if __name__ == "__main__":
    main()
