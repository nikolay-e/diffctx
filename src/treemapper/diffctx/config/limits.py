from __future__ import annotations

import os
from dataclasses import dataclass, field


def _env_int(key: str, default: int) -> int:
    raw = os.environ.get(key)
    if raw is None:
        return default
    try:
        return int(raw)
    except ValueError:
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


@dataclass(frozen=True)
class PPRConfig:
    alpha: float = 0.60


@dataclass(frozen=True)
class LexicalConfig:
    min_similarity: float = 0.30
    top_k_neighbors: int = 5
    max_df_ratio: float = 0.15
    min_idf: float = 2.0
    max_postings: int = 100
    weight_min: float = 0.05
    weight_max: float = 0.15
    backward_factor: float = 0.5


@dataclass(frozen=True)
class CochangeConfig:
    weight: float = 0.40
    min_count: int = 2
    max_files_per_commit: int = 30
    commits_limit: int = 500
    timeout_seconds: int = 10


@dataclass(frozen=True)
class SiblingConfig:
    max_files_per_dir: int = 20


@dataclass(frozen=True)
class UtilityConfig:
    eta: float = 0.20
    gamma: float = 0.10
    r_cap_sigma: float = 2.0
    proximity_decay: float = 0.30
    peripheral_cap: float = 0.15


LIMITS = AlgorithmLimits()
PPR = PPRConfig()
LEXICAL = LexicalConfig()
COCHANGE = CochangeConfig()
SIBLING = SiblingConfig()
UTILITY = UtilityConfig()
