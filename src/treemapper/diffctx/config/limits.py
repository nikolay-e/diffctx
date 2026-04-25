from __future__ import annotations

import logging
import os
from dataclasses import dataclass, field


def _env_int(key: str, default: int) -> int:
    raw = os.environ.get(key)
    if raw is None:
        return default
    try:
        return int(raw)
    except ValueError:
        logging.warning("invalid integer value for %s=%r, using default %d", key, raw, default)
        return default


@dataclass(frozen=True)
class AlgorithmLimits:
    max_file_size: int = 100_000
    max_fragments: int = field(default_factory=lambda: _env_int("TREEMAPPER_MAX_FRAGMENTS", 200))
    max_generated_fragments: int = 5
    max_generated_lines: int = 30
    skip_expensive_threshold: int = 2000
    rare_identifier_threshold: int = 3
    overhead_per_fragment: int = 18


LIMITS = AlgorithmLimits()
