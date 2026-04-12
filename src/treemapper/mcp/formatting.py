from __future__ import annotations

from typing import Any

from treemapper.writer import tree_to_string


def format_diff_context_as_markdown(result: dict[str, Any]) -> str:
    return tree_to_string(result, "md")
