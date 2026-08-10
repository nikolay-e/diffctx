"""Read dcbench mode runs: paired deltas, and the union ceiling that bounds fusion.

Two things this does that a pooled mean cannot.

**Paired.** Modes are compared only on instances every mode produced. A mode that
times out on the hard instances would otherwise post a better mean by being
absent from them, and the hang class (#121) is concentrated in two repos, so the
unpaired comparison is dominated by which mode survived where.

**Ceiling.** `union(ego, bm25)` is what the two components already reach between
them on the same candidate universe. Fusion cannot exceed it. A fusion result at
the ceiling has nothing left to win from ranking — the remaining gap is
candidate supply (#130), not scoring (#125) — and a ceiling equal to EGO alone
says fusion was never able to add anything, whatever its ranking does.

CLI:
    python -m eval.analysis.dcbench_summary --runs results/dcbench_b8000
"""

from __future__ import annotations

import argparse
import json
import random
import statistics
from pathlib import Path

METRICS = [
    "recall_all",
    "recall_nontrivial",
    "recall_essential",
    "precision_labelled",
    "forbidden_rate",
    "content_tokens",
]


PROJECT_ROOT = Path(__file__).resolve().parents[2]
INSTANCES = PROJECT_ROOT / "datasets/dcbench/v1/instances"


def nontrivial_gold(instance_id: str) -> set[str]:
    import yaml

    ann = INSTANCES / instance_id / "annotation.yaml"
    if not ann.exists():
        return set()
    a = yaml.safe_load(ann.read_text())
    return {g["path"] for g in (a.get("gold") or []) if not g.get("in_diff")}


def _hit_rate(gold: set[str], selected: set[str]) -> float:
    # Same suffix match the runner scores with: gold is repo-relative, the
    # renderer emits worktree-relative paths.
    if not gold:
        return 0.0
    hits = sum(1 for g in gold if any(s == g or s.endswith("/" + g) for s in selected))
    return hits / len(gold)


def load_mode(d: Path) -> dict[str, dict]:
    rows: dict[str, dict] = {}
    for f in sorted(d.glob("*.jsonl")):
        for line in f.read_text().splitlines():
            if line.strip():
                r = json.loads(line)
                rows[r["instance_id"]] = r
    return rows


def _mean(vals: list[float | None]) -> float | None:
    got = [v for v in vals if v is not None]
    return round(statistics.mean(got), 4) if got else None


def paired_bootstrap(a: list[float], b: list[float], iters: int = 5000, seed: int = 42) -> tuple[float, float]:
    rng = random.Random(seed)
    n = len(a)
    diffs = []
    idx = range(n)
    for _ in range(iters):
        pick = [rng.choice(idx) for _ in range(n)]
        diffs.append(statistics.mean(a[i] for i in pick) - statistics.mean(b[i] for i in pick))
    diffs.sort()
    return round(diffs[int(0.025 * iters)], 4), round(diffs[int(0.975 * iters)], 4)


def _load_modes(root: Path, subset: str | None) -> dict[str, dict[str, dict]]:
    modes = {d.name: load_mode(d) for d in sorted(root.iterdir()) if d.is_dir()}
    if subset:
        want = {m.strip() for m in subset.split(",")}
        modes = {m: r for m, r in modes.items() if m in want}
    # A mode still being collected would empty the paired intersection and take
    # every other mode's numbers down with it.
    modes = {m: r for m, r in modes.items() if r}
    if not modes:
        raise SystemExit(f"no mode directories under {root}")
    return modes


def _print_coverage(modes: dict[str, dict[str, dict]]) -> None:
    print("## Coverage (rows written per mode)\n")
    print("| mode | rows | produced | hang | other |")
    print("|---|---:|---:|---:|---:|")
    for m, rows in modes.items():
        st = [r.get("status") for r in rows.values()]
        print(
            f"| {m} | {len(st)} | {st.count('produced')} | {st.count('hang')} | "
            f"{len(st) - st.count('produced') - st.count('hang')} |"
        )
    counts = {m: len(rows) for m, rows in modes.items()}
    if len(set(counts.values())) > 1:
        lo = min(counts, key=lambda m: counts[m])
        hi = max(counts, key=lambda m: counts[m])
        print(
            f"\n**WARNING: unequal row counts ({lo}={counts[lo]} vs {hi}={counts[hi]}).** "
            "A half-collected mode shrinks the paired set for every mode below; "
            "treat all paired numbers as provisional until the counts agree."
        )


def _paired_sets(modes: dict[str, dict[str, dict]]) -> tuple[set[str], list[str]]:
    shared = set.intersection(*[{i for i, r in rows.items() if r.get("status") == "produced"} for rows in modes.values()])
    print(f"\nPaired set: **{len(shared)}** instances produced by every mode.\n")

    # Retrieval is only measurable where there is something to retrieve. Read
    # the count from every mode, not an arbitrary one: a mode whose rows
    # predate the field would otherwise silently empty this set and with it
    # the whole statistical section.
    with_nt = sorted(i for i in shared if any((rows[i].get("n_nontrivial") or 0) > 0 for rows in modes.values()))
    if shared and not with_nt:
        print("**WARNING: no row in any mode carries n_nontrivial > 0** — bootstrap and ceiling sections are skipped.\n")
    print(f"Of those, **{len(with_nt)}** carry at least one nontrivial gold file.\n")
    return shared, with_nt


def _paired_pool(modes: dict[str, dict[str, dict]], ids: list[str], metric: str) -> list[str]:
    # A metric that is None in ONE mode (precision_labelled: nothing labelled
    # was selected) must drop the instance for EVERY mode, or the whiffing
    # mode is scored on an easier subset under a header that promises pairing.
    return [i for i in ids if all(rows[i].get(metric) is not None for rows in modes.values())]


def _print_paired_means(modes: dict[str, dict[str, dict]], shared: set[str], with_nt: list[str]) -> None:
    print("## Paired means\n")
    pools = {k: _paired_pool(modes, with_nt if k == "recall_nontrivial" else sorted(shared), k) for k in METRICS}
    print("per-metric paired n: " + ", ".join(f"{k}={len(pools[k])}" for k in METRICS) + "\n")
    print("| mode | " + " | ".join(METRICS) + " |")
    print("|---" * (len(METRICS) + 1) + "|")
    for m, rows in modes.items():
        cells = [str(_mean([rows[i].get(k) for i in pools[k]])) for k in METRICS]
        print(f"| {m} | " + " | ".join(cells) + " |")


def _print_bootstrap(modes: dict[str, dict[str, dict]], base: str, with_nt: list[str]) -> None:
    print(f"\n## Nontrivial recall vs {base} (paired bootstrap 95% CI)\n")
    print("| mode | delta | CI low | CI high | excludes zero |")
    print("|---|---:|---:|---:|---|")
    pool = _paired_pool(modes, with_nt, "recall_nontrivial")
    if len(pool) < len(with_nt):
        print(f"(n={len(pool)}; {len(with_nt) - len(pool)} instances dropped — recall_nontrivial missing in some mode)\n")
    b = [modes[base][i]["recall_nontrivial"] for i in pool]
    for m, rows in modes.items():
        if m == base:
            continue
        a = [rows[i]["recall_nontrivial"] for i in pool]
        lo, hi = paired_bootstrap(a, b)
        d = round(statistics.mean(a) - statistics.mean(b), 4)
        print(f"| {m} | {d:+.4f} | {lo:+.4f} | {hi:+.4f} | {'yes' if lo > 0 or hi < 0 else 'no'} |")


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--runs", required=True, help="directory holding one subdirectory per mode")
    ap.add_argument("--baseline", default="ego")
    ap.add_argument("--modes", help="comma-separated subset; default every subdirectory")
    args = ap.parse_args()

    root = Path(args.runs)
    modes = _load_modes(root, args.modes)

    print(f"# dcbench — {root}\n")
    _print_coverage(modes)
    shared, with_nt = _paired_sets(modes)
    _print_paired_means(modes, shared, with_nt)

    base = args.baseline
    if base in modes and with_nt:
        _print_bootstrap(modes, base, with_nt)

    if "ego" in modes and "bm25" in modes and with_nt:
        print("\n## Union ceiling (ego+bm25 union, same instances)\n")
        ego, lex = modes["ego"], modes["bm25"]
        # Recomputed from selected_files rather than from the recall columns: the
        # union of two recalls is not the recall of the union.
        ceil, e_only, l_only = [], [], []
        for i in with_nt:
            gold = nontrivial_gold(i)
            if not gold:
                continue
            e = set(ego[i].get("selected_files") or [])
            x = set(lex[i].get("selected_files") or [])
            e_only.append(_hit_rate(gold, e))
            l_only.append(_hit_rate(gold, x))
            ceil.append(_hit_rate(gold, e | x))
        if not ceil:
            print(
                "**Skipped: no annotation.yaml found for any paired instance** — "
                f"the ceiling reads gold from {INSTANCES}, which must be checked out."
            )
        else:
            print(f"- ego nontrivial recall:   {statistics.mean(e_only):.4f} (n={len(ceil)}, re-read from today's annotations)")
            print(f"- bm25 nontrivial recall:  {statistics.mean(l_only):.4f}")
            print(f"- union ceiling:           {statistics.mean(ceil):.4f}")
            print(f"- headroom over ego:       {statistics.mean(ceil) - statistics.mean(e_only):+.4f}")
            print(
                "\nThe ceiling is an oracle: it credits a gold file if either arm surfaced "
                "it, with no mechanism able to choose. Fusion cannot exceed it. Headroom "
                "at or near zero means fusion had nothing to win here and the remaining "
                "loss is candidate supply (#130), not ranking (#125)."
            )

    print("\n## Per-repo nontrivial recall\n")
    repos = sorted({modes[base][i]["repo"] for i in with_nt}) if base in modes else []
    print("| repo | n | " + " | ".join(modes) + " |")
    print("|---" * (len(modes) + 2) + "|")
    for rp in repos:
        ids = [i for i in with_nt if modes[base][i]["repo"] == rp]
        cells = [str(_mean([modes[m][i].get("recall_nontrivial") for i in ids])) for m in modes]
        print(f"| {rp} | {len(ids)} | " + " | ".join(cells) + " |")


if __name__ == "__main__":
    main()
