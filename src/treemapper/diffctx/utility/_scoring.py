from __future__ import annotations

import math
from dataclasses import dataclass, field, replace
from pathlib import Path

from ..config.limits import UTILITY
from ..types import Fragment
from ._needs import InformationNeed, _match_strength_typed


@dataclass
class UtilityState:
    max_rel: dict[tuple[str, str], float] = field(default_factory=dict)
    priorities: dict[tuple[str, str], float] = field(default_factory=dict)
    structural_sum: float = 0.0
    eta: float = UTILITY.eta
    gamma: float = UTILITY.gamma
    r_cap: float = 1.0
    changed_dirs: frozenset[Path] = field(default_factory=frozenset)
    proximity_decay: float = UTILITY.proximity_decay
    file_importance: dict[Path, float] = field(default_factory=dict)

    def copy(self) -> UtilityState:
        return replace(
            self,
            max_rel=dict(self.max_rel),
            priorities=dict(self.priorities),
            file_importance=dict(self.file_importance),
        )


def _phi(x: float) -> float:
    return math.sqrt(x) if x > 0 else 0.0


_MIN_REL_FOR_BONUS = 0.03
_RELATEDNESS_BONUS = 0.25


def _augmented_score(m: float, rel_score: float, state: UtilityState) -> float:
    r_norm = min(rel_score / state.r_cap, 1.0) if state.r_cap > 0 else 0.0
    return m + state.eta * r_norm


def _needs_from_identifiers(frag: Fragment) -> tuple[InformationNeed, ...]:
    return tuple(InformationNeed("definition", c, None, 0.5) for c in frag.identifiers)


@dataclass
class _GainResult:
    gain: float = 0.0
    has_match: bool = False
    need_updates: list[tuple[tuple[str, str], float, float]] = field(default_factory=list)
    diversity_bonus: float = 0.0
    structural_bonus: float = 0.0


def _diversity_bonus(
    needs: tuple[InformationNeed, ...],
    rel_score: float,
    gain: float,
    state: UtilityState,
) -> float:
    if not needs or rel_score < _MIN_REL_FOR_BONUS:
        return 0.0
    if gain <= 0:
        return 0.0
    total_covered = sum(min(state.max_rel.get((n.need_type, n.symbol), 0.0), 1.0) for n in needs)
    unsatisfied = max(0.0, 1.0 - total_covered / max(1, len(needs)))
    return rel_score * _RELATEDNESS_BONUS * unsatisfied


def _compute_gain_core(
    frag: Fragment,
    rel_score: float,
    needs: tuple[InformationNeed, ...],
    state: UtilityState,
    use_state_priorities: bool = False,
) -> _GainResult:
    effective = needs if needs else _needs_from_identifiers(frag)
    result = _GainResult()
    if not effective:
        return result

    for need in effective:
        m = _match_strength_typed(frag, need)
        if m <= 0.0:
            continue
        if need.need_type == "impact" and state.file_importance:
            m *= state.file_importance.get(frag.path, 1.0)
        result.has_match = True
        a_fz = _augmented_score(m, rel_score, state)
        nkey = (need.need_type, need.symbol)
        old_max = state.max_rel.get(nkey, 0.0)
        new_max = max(old_max, a_fz)
        priority = state.priorities.get(nkey, need.priority) if use_state_priorities else need.priority
        result.gain += priority * (_phi(new_max) - _phi(old_max))
        result.need_updates.append((nkey, new_max, need.priority))

    result.diversity_bonus = _diversity_bonus(needs, rel_score, result.gain, state)

    if result.has_match:
        r_norm = min(rel_score / state.r_cap, 1.0) if state.r_cap > 0 else 0.0
        result.structural_bonus = state.gamma * r_norm

    return result


def marginal_gain(
    frag: Fragment,
    rel_score: float,
    needs: tuple[InformationNeed, ...],
    state: UtilityState,
) -> float:
    result = _compute_gain_core(frag, rel_score, needs, state)
    return result.gain + result.diversity_bonus + result.structural_bonus


def apply_fragment(
    frag: Fragment,
    rel_score: float,
    needs: tuple[InformationNeed, ...],
    state: UtilityState,
) -> None:
    result = _compute_gain_core(frag, rel_score, needs, state, use_state_priorities=True)
    for nkey, new_max, priority in result.need_updates:
        state.max_rel[nkey] = new_max
        state.priorities[nkey] = max(state.priorities.get(nkey, 0.0), priority)
    state.structural_sum += result.diversity_bonus + result.structural_bonus


def _dir_distance(d1: Path, d2: Path) -> int:
    p1 = d1.parts
    p2 = d2.parts
    common = 0
    for a, b in zip(p1, p2):
        if a == b:
            common += 1
        else:
            break
    return (len(p1) - common) + (len(p2) - common)


def _proximity_factor(frag_path: Path, changed_dirs: frozenset[Path], alpha: float) -> float:
    if not changed_dirs:
        return 1.0
    frag_dir = frag_path.parent
    min_dist = min(_dir_distance(frag_dir, d) for d in changed_dirs)
    if min_dist <= 0:
        return 1.0
    return 1.0 / (1.0 + alpha * min_dist)


def compute_density(frag: Fragment, rel_score: float, needs: tuple[InformationNeed, ...], state: UtilityState) -> float:
    if frag.token_count <= 0:
        return 0.0
    gain = marginal_gain(frag, rel_score, needs, state)
    pf = _proximity_factor(frag.path, state.changed_dirs, state.proximity_decay)
    return gain * pf / frag.token_count


def utility_value(state: UtilityState) -> float:
    u1 = sum(state.priorities.get(sym, 1.0) * _phi(v) for sym, v in state.max_rel.items())
    return u1 + state.structural_sum
