#!/usr/bin/env python3
"""Render docs/product/cli.md from the argparse parsers, so the flag reference
cannot drift from `diffctx --help` (the README table did, twice)."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "src"))

from diffctx.cli import _build_graph_parser, _build_main_parser  # noqa: E402

TARGET = ROOT / "docs" / "product" / "cli.md"

INTRO = """# Command-line reference

Every flag `diffctx` accepts, with its default and one line of meaning. This
page is rendered from the parsers themselves by `scripts/update_cli_reference.py`
(`tests/test_cli_reference.py` fails when it differs from what the parsers
declare), so what you read here is what the installed version answers. Worked examples
live in the [README](../../README.md#usage); what `--budget` counts is in
[Token counting](token-budget.md).

The native binary (`cargo install diffctx`, `npx diffctx`, the Docker image)
takes the diff-mode subset of these flags and writes YAML or JSON; `--help`
there lists exactly which.
"""


def _default(action: argparse.Action) -> str:
    if action.default in (None, argparse.SUPPRESS) or isinstance(action, (argparse._HelpAction, argparse._VersionAction)):
        return "—"
    if isinstance(action.default, bool):
        return "off" if not action.default else "on"
    if not isinstance(action.default, (str, int, float)) or str(action.default).startswith("<"):
        return "—"
    return f"`{action.default}`"


def _flag(action: argparse.Action) -> str:
    if not action.option_strings:
        return f"`{action.metavar or action.dest}`"
    metavar = action.metavar or (action.dest.upper() if action.nargs != 0 else "")
    if action.choices and not action.metavar:
        metavar = "{" + ",".join(str(c) for c in action.choices) + "}"
    names = ", ".join(f"`{o}`" for o in action.option_strings)
    return f"{names} {metavar}".rstrip() if metavar else names


def _cell(text: str) -> str:
    return " ".join(text.split()).replace("|", "\\|")


def _table(parser: argparse.ArgumentParser) -> str:
    out = []
    for group in parser._action_groups:
        actions = [a for a in group._group_actions if a.help is not argparse.SUPPRESS]
        if not actions:
            continue
        out.append(f"### {group.title}\n")
        out.append("| Flag | Default | Meaning |\n|---|---|---|")
        for a in actions:
            out.append(f"| {_flag(a)} | {_default(a)} | {_cell(a.help or '')} |")
        out.append("")
    return "\n".join(out)


def _section(title: str, parser: argparse.ArgumentParser) -> str:
    body = f"## `{title}`\n\n```text\n{(parser.description or '').strip()}\n```\n\n{_table(parser)}"
    if parser.epilog:
        body += "\n```text\n" + parser.epilog.strip("\n") + "\n```\n"
    return body


def render() -> str:
    main = _build_main_parser(prog="diffctx", version="<version>")
    graph = _build_graph_parser(prog="diffctx graph")
    return (
        f"{INTRO}\n{_section('diffctx', main)}\n{_section('diffctx graph', graph)}\n"
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
