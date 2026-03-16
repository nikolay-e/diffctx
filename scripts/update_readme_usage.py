#!/usr/bin/env python3
"""Auto-generate the Usage section in README.md from canonical examples."""

from __future__ import annotations

import sys
from pathlib import Path

_USAGE_EXAMPLES: list[tuple[str, str]] = [
    ("treemapper", "current dir, YAML to stdout"),
    ("treemapper .", "YAML to stdout + token count"),
    ("treemapper . -o tree.yaml", "save to file"),
    ("treemapper . --save", "save to tree.yaml (default name)"),
    ("treemapper . -o -", "explicit stdout"),
    ("treemapper . -f json", "JSON format"),
    ("treemapper . -f txt", "plain text with indentation"),
    ("treemapper . -f md", "Markdown with fenced code blocks"),
    ("treemapper . --no-content", "structure only, no file contents"),
    ("treemapper . --max-depth 3", "limit depth (0=root only)"),
    ("treemapper . --max-file-bytes 10000", "skip files > 10KB (default: 10 MB)"),
    ("treemapper . --no-file-size-limit", "include all files regardless of size"),
    ("treemapper . -i custom.ignore", "custom ignore patterns"),
    ("treemapper . -w whitelist", "include-only filter"),
    ("treemapper . --no-default-ignores", "disable built-in ignore patterns"),
    ("treemapper . --log-level info", "log level (default: error)"),
    ("treemapper . -c", "copy to clipboard"),
    ("treemapper . -c -o tree.yaml", "clipboard + save to file"),
    ("treemapper -v", "show version"),
    # diff context mode
    ("", ""),
    ("# diff context mode (requires git repo):", ""),
    ("treemapper . --diff HEAD~1", "context for last commit"),
    ("treemapper . --diff main..feature", "context for feature branch"),
    ("treemapper . --diff HEAD~1 --budget 30000", "limit diff context to ~30k tokens"),
    ("treemapper . --diff HEAD~1 --full", "all changed code, no smart selection"),
    ("treemapper . --diff HEAD~1 -c", "diff context to clipboard"),
]

_BEGIN = "<!-- BEGIN USAGE -->"
_END = "<!-- END USAGE -->"


def _generate_block() -> str:
    real_cmds = [cmd for cmd, _ in _USAGE_EXAMPLES if cmd and not cmd.startswith("#")]
    max_cmd = max(len(cmd) for cmd in real_cmds)
    lines = []
    for cmd, comment in _USAGE_EXAMPLES:
        if not cmd and not comment:
            lines.append("")
        elif cmd.startswith("#"):
            lines.append(cmd)
        else:
            lines.append(f"{cmd}{' ' * (max_cmd - len(cmd) + 3)}# {comment}")
    return "```bash\n" + "\n".join(lines) + "\n```\n"


def main() -> int:
    readme = Path(__file__).parent.parent / "README.md"
    content = readme.read_text(encoding="utf-8")

    start = content.find(_BEGIN)
    end = content.find(_END)
    if start == -1 or end == -1:
        print(f"ERROR: markers {_BEGIN!r} / {_END!r} not found in README.md", file=sys.stderr)
        return 2

    new_section = f"{_BEGIN}\n{_generate_block()}{_END}"
    if content[start : end + len(_END)] == new_section:
        return 0

    readme.write_text(content[:start] + new_section + content[end + len(_END) :], encoding="utf-8")
    print("README.md: usage section updated", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main())
