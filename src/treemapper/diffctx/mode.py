from __future__ import annotations

from dataclasses import dataclass
from enum import Enum


class ScoringMode(Enum):
    HYBRID = "hybrid"
    PPR = "ppr"
    EGO = "ego"


@dataclass(frozen=True)
class PipelineConfig:
    discovery: str
    scoring: str
    low_relevance: bool
    bm25_top_k: int
    ego_depth: int
    ppr_alpha: float

    @classmethod
    def from_mode(cls, mode: ScoringMode, n_candidate_files: int = 0) -> PipelineConfig:
        if mode == ScoringMode.PPR:
            return cls(
                discovery="ensemble",
                scoring="ppr",
                low_relevance=False,
                bm25_top_k=1,
                ego_depth=1,
                ppr_alpha=0.60,
            )
        if mode == ScoringMode.EGO:
            return cls(
                discovery="ensemble",
                scoring="ego",
                low_relevance=False,
                bm25_top_k=1,
                ego_depth=2,
                ppr_alpha=0.60,
            )
        is_large = n_candidate_files > 50
        return PipelineConfig(
            discovery="ensemble" if is_large else "default",
            scoring="ego" if is_large else "ppr",
            low_relevance=not is_large,
            bm25_top_k=1 if is_large else 0,
            ego_depth=2 if is_large else 1,
            ppr_alpha=0.60,
        )
