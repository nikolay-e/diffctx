#!/usr/bin/env python3
"""Render docs/product/cli.md from the argparse parsers, so the flag reference
cannot drift from `diffctx --help` (the README table did, twice)."""

from __future__ import annotations

import os
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "src"))

from diffctx.cli import _build_graph_parser, _build_main_parser  # noqa: E402

TARGET = ROOT / "docs" / "product" / "cli.md"

INTRO = """# Command-line reference

Every flag `diffctx` accepts, with its default and one line of meaning. This
page is rendered from the parsers themselves by `scripts/update_cli_reference.py`
(`tests/test_cli_reference.py` fails when it differs from `diffctx --help`), so
what you read here is what the installed version answers. Worked examples
live in the [README](../../README.md#usage); what `--budget` counts is in
[Token counting](token-budget.md).

The native binary (`cargo install diffctx`, `npx diffctx`, the Docker image)
takes the diff-mode subset of these flags and writes YAML or JSON; `--help`
there lists exactly which.
"""


def _help(parser) -> str:
    return parser.format_help().rstrip("\n") + "\n"


def render() -> str:
    os.environ["COLUMNS"] = "80"
    main = _build_main_parser(prog="diffctx", version="<version>")
    graph = _build_graph_parser(prog="diffctx graph")
    return (
        f"{INTRO}\n## `diffctx`\n\n```text\n{_help(main)}```\n\n"
        f"## `diffctx graph`\n\n```text\n{_help(graph)}```\n\n"
        "## `diffctx mcp`\n\n"
        "Runs the MCP server over stdio — the same entry point as `diffctx-mcp` —\n"
        "and takes no flags. It needs the `mcp` extra (`pip install 'diffctx[mcp]'`);\n"
        "the tool it exposes, its arguments and its read-only guarantees are\n"
        "described in the [security policy](../../SECURITY.md) and the README's\n"
        "MCP section.\n"
    )


def main() -> int:
    rendered = render()
    current = TARGET.read_text(encoding="utf-8") if TARGET.exists() else ""
    if current == rendered:
        return 0
    TARGET.write_text(rendered, encoding="utf-8")
    print(f"{TARGET.relative_to(ROOT)}: regenerated from the parsers", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main())
