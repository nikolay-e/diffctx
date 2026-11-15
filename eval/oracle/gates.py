"""The thresholds of the verdict (`gates.toml`) and how a value is held against one.

Alone in its module so that `report` (which prints the verdict) and `cellstate` (which a verdict line is computed
from) read one definition and neither imports the other's side.
"""

from __future__ import annotations

import operator
import tomllib
from dataclasses import dataclass
from pathlib import Path

GATES_FILE = Path(__file__).with_name("gates.toml")
SUFFIX_OPERATORS = {"_max": "max", "_min": "min", "_below": "below"}
COMPARE = {"max": operator.le, "min": operator.ge, "below": operator.lt}
OPERATOR_TEXT = {"max": "<=", "min": ">=", "below": "<"}


@dataclass(frozen=True)
class Threshold:
    gate: str  # `section.name`: the key without its operator suffix
    op: str  # max, min, below, require or read
    limit: float | None


def load_gates(path: Path = GATES_FILE) -> dict[str, Threshold]:
    """Every threshold of the verdict by gate name: the file's `<name>_max|_min|_below` keys, `true` and "read"."""
    found: dict[str, Threshold] = {}
    for section, entries in tomllib.loads(path.read_text(encoding="utf-8")).items():
        for key, limit in entries.items():
            suffix = next((s for s in SUFFIX_OPERATORS if key.endswith(s)), None)
            if limit == "read":
                threshold = Threshold(f"{section}.{key}", "read", None)
            elif limit is True:
                threshold = Threshold(f"{section}.{key}", "require", None)
            elif suffix and isinstance(limit, int | float) and not isinstance(limit, bool):
                threshold = Threshold(f"{section}.{key.removesuffix(suffix)}", SUFFIX_OPERATORS[suffix], limit)
            else:
                raise ValueError(f"{path}: [{section}] {key} = {limit!r} is no threshold (_max, _min, _below, true or 'read')")
            found[threshold.gate] = threshold
    return found


def holds(gate: Threshold, value: object) -> bool:
    """Whether `value` meets the threshold; a `read` row has no limit to miss."""
    if gate.op == "read":
        return True
    if gate.op == "require":
        return bool(value)
    return COMPARE[gate.op](value, gate.limit)


def limit_text(gate: Threshold) -> str:
    if gate.op in ("read", "require"):
        return "every row read" if gate.op == "read" else "required"
    return f"{OPERATOR_TEXT[gate.op]} {gate.limit}"
