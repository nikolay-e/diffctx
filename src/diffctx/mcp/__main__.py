from __future__ import annotations

import sys


def main() -> None:
    try:
        from diffctx.mcp.server import run_server
    except ImportError:
        print(
            "diffctx-mcp: missing optional dependencies for MCP server mode.\n" "Install with: pip install 'diffctx[mcp]'",
            file=sys.stderr,
        )
        sys.exit(2)
    run_server()


if __name__ == "__main__":
    main()
