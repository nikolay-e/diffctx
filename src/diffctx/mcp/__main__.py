from __future__ import annotations

import sys


def main(prog: str = "diffctx-mcp", extra: str = "diffctx[mcp]") -> None:
    try:
        from diffctx.mcp.server import run_server
    except ImportError:
        print(
            f"{prog}: missing optional dependencies for MCP server mode.\n" f"Install with: pip install '{extra}'",
            file=sys.stderr,
        )
        sys.exit(2)
    run_server()


if __name__ == "__main__":
    main()
