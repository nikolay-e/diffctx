"""Markdown rendering the report and the cell-state table share."""

from __future__ import annotations

from model import UNCAPPED_BUDGET


def budget_label(budget: int | None) -> str:
    return "default" if budget is None else "uncapped" if budget >= UNCAPPED_BUDGET else str(budget)


def table(headers: list[str], rows: list[list]) -> str:
    def cell(value: object) -> str:
        return "n/a" if value is None else str(value)

    lines = ["| " + " | ".join(headers) + " |", "| " + " | ".join("---" for _ in headers) + " |"]
    lines += ["| " + " | ".join(cell(v) for v in row) + " |" for row in rows]
    return "\n".join(lines)
