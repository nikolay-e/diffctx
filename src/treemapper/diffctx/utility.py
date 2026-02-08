from __future__ import annotations

import math
import re
from dataclasses import dataclass, field

from .stopwords import CODE_STOPWORDS
from .tokenizer import extract_tokens
from .types import Fragment

_CONCEPT_RE = re.compile(r"[A-Za-z_]\w*")


def concepts_from_diff_text(diff_text: str, profile: str = "code", *, use_nlp: bool = False) -> frozenset[str]:
    diff_lines = []
    for line in diff_text.splitlines():
        is_added = line.startswith("+") and not line.startswith("+++")
        is_removed = line.startswith("-") and not line.startswith("---")
        if is_added or is_removed:
            diff_lines.append(line[1:])
    text = "\n".join(diff_lines)

    if use_nlp and profile != "code":
        return extract_tokens(text, profile=profile, use_nlp=True)

    raw = _CONCEPT_RE.findall(text)
    # Normalize to lowercase and filter stopwords to avoid matching common keywords
    return frozenset(ident.lower() for ident in raw if len(ident) >= 3 and ident.lower() not in CODE_STOPWORDS)


@dataclass
class UtilityState:
    max_rel: dict[str, float] = field(default_factory=dict)

    def copy(self) -> UtilityState:
        return UtilityState(max_rel=dict(self.max_rel))


def _phi(x: float) -> float:
    return math.sqrt(x) if x > 0 else 0.0


_MIN_REL_FOR_BONUS = 0.03
_RELATEDNESS_BONUS = 0.25
_NO_CONCEPTS_FALLBACK_FACTOR = 0.1


def marginal_gain(
    frag: Fragment,
    rel_score: float,
    concepts: frozenset[str],
    state: UtilityState,
) -> float:
    if not concepts:
        return rel_score * _NO_CONCEPTS_FALLBACK_FACTOR

    gain = 0.0
    covered = frag.identifiers & concepts

    for c in covered:
        old_max = state.max_rel.get(c, 0.0)
        new_max = max(old_max, rel_score)
        gain += _phi(new_max) - _phi(old_max)

    # Add minimum gain for high-relevance fragments with concept overlap
    # This ensures semantically related fragments are included even when
    # concepts are already covered by core fragments with higher scores
    if covered and rel_score >= _MIN_REL_FOR_BONUS:
        min_gain = rel_score * _RELATEDNESS_BONUS * min(len(covered), 5)
        gain = max(gain, min_gain)

    # Fallback: high PPR score should contribute even without concept overlap
    # This ensures structurally related fragments (via call graph) are included
    if rel_score >= _MIN_REL_FOR_BONUS:
        gain = max(gain, rel_score * _RELATEDNESS_BONUS)

    return gain


def apply_fragment(
    frag: Fragment,
    rel_score: float,
    concepts: frozenset[str],
    state: UtilityState,
) -> None:
    covered = frag.identifiers & concepts

    for c in covered:
        old_max = state.max_rel.get(c, 0.0)
        state.max_rel[c] = max(old_max, rel_score)


def compute_density(frag: Fragment, rel_score: float, concepts: frozenset[str], state: UtilityState) -> float:
    if frag.token_count <= 0:
        return 0.0
    gain = marginal_gain(frag, rel_score, concepts, state)
    return gain / frag.token_count


def utility_value(state: UtilityState) -> float:
    return sum(_phi(v) for v in state.max_rel.values())
