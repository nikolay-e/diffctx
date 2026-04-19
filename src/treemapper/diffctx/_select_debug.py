from __future__ import annotations

from .types import Fragment, FragmentId
from .utility import InformationNeed, UtilityState, compute_density, marginal_gain


def _collect_greedy_densities(
    candidates: list[Fragment],
    rel: dict[FragmentId, float],
    needs: tuple[InformationNeed, ...],
    utility_state: UtilityState,
) -> list[tuple[str, int, int, float, float, float]]:
    result: list[tuple[str, int, int, float, float, float]] = []
    for frag in candidates:
        if frag.token_count > 0:
            density = compute_density(frag, rel.get(frag.id, 0.0), needs, utility_state)
            gain = marginal_gain(frag, rel.get(frag.id, 0.0), needs, utility_state)
            result.append((str(frag.path), frag.start_line, frag.token_count, rel.get(frag.id, 0.0), gain, density))
    return result


def _write_greedy_dump(
    path: str,
    tau: float,
    threshold: float,
    baseline_k: int,
    n_candidates: int,
    n_selected: int,
    remaining_budget: int,
    densities: list[tuple[str, int, int, float, float, float]],
) -> None:
    import json as _json

    with open(path, "w") as f:
        f.write(
            _json.dumps(
                {
                    "tau": tau,
                    "threshold": threshold,
                    "baseline_k": baseline_k,
                    "n_candidates": n_candidates,
                    "n_selected_noncore": n_selected,
                    "remaining_budget": remaining_budget,
                }
            )
            + "\n"
        )
        for fpath, start, tokens, ppr, gain, density in sorted(densities, key=lambda x: -x[5]):
            f.write(
                _json.dumps(
                    {
                        "path": fpath,
                        "start": start,
                        "tokens": tokens,
                        "ppr": round(ppr, 6),
                        "gain": round(gain, 4),
                        "density": round(density, 6),
                    }
                )
                + "\n"
            )
