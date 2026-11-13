"""W75-E: are the facts-based edges (cells) no worse than the legacy edge builders?

Two `run_final` directories of one binary, run with `DIFFCTX_CELL_EDGES=none` (legacy) and `=all` (cells), and the two
`bench.json` files of `eval/oracle/bench.py` run the same way:

    python -m eval.analysis.compare_runs --legacy A --cells B --bench-legacy a.json --bench-cells b.json

Exit 0 not worse, 1 worse, 2 invalid: the pair is no measurement, whatever its numbers say.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path

from eval.analysis.equivalence_gate import load_run

RUN_ENV = "run_env.json"
FILES_MARGIN = -0.05
LINES_MARGIN = -0.05
P95_RATIO = 1.10
BENCH_OPS = ("pack", "locate")
EDGE_ENV = ("DIFFCTX_CELL_EDGE", "DIFFCTX_DISABLE_BUILDERS")
METRICS = ("file_recall", "nontrivial_file_recall", "line_recall")
RULE = f"""## Decision rule (fixed before the run)

Over the manifest's instances, paired by id, cells minus legacy, a failed row counting as recall 0, the facts-based edges
are NOT WORSE when all hold:

- the lower 95% bound of the paired file-recall difference is >= {FILES_MARGIN}
- the lower 95% bound of the paired line-recall difference is >= {LINES_MARGIN}
- p95 wall time of pack and of locate is <= {P95_RATIO:.2f} x the legacy set's

Otherwise they are WORSE. Two runs that are no measurement (other instances than the manifest's, parameters beyond the
edge environment, one edge environment, identical selections, a bench without samples) are INVALID."""


def load(d: Path) -> tuple[dict[str, dict], dict | None]:
    rows: dict[str, dict] = {}
    for row in load_run(d).values():
        if row["instance_id"] in rows:
            raise SystemExit(f"{d}: {row['instance_id']} appears twice (several budgets or checkpoints in one run)")
        rows[row["instance_id"]] = row
    env_file = d / RUN_ENV
    return rows, json.loads(env_file.read_text()) if env_file.is_file() else None


def value(row: dict, metric: str) -> float | None:
    v = row[metric]
    return 0.0 if v is None and row["status"] != "ok" else v


def pairs(legacy: dict, cells: dict, ids: set[str], metric: str) -> list[tuple[str, float, float]]:
    out = []
    for i in sorted(ids):
        a, b = value(legacy[i], metric), value(cells[i], metric)
        if a is not None and b is not None:
            out.append((legacy[i]["language"] or cells[i]["language"] or "unknown", a, b))
    return out


def differences(env_legacy: dict, env_cells: dict) -> list[str]:
    is_edge = lambda k: k.startswith(EDGE_ENV)  # noqa: E731
    why = []
    for key in sorted((env_legacy.keys() | env_cells.keys()) - {"extra_env"}):
        if env_legacy.get(key) != env_cells.get(key):
            why.append(f"the runs differ in {key}: {env_legacy.get(key)} vs {env_cells.get(key)}")
    legacy, cells = env_legacy["extra_env"], env_cells["extra_env"]
    if {k: v for k, v in legacy.items() if is_edge(k)} == {k: v for k, v in cells.items() if is_edge(k)}:
        why.append("the runs have the same edge environment")
    other = sorted(k for k in legacy.keys() | cells.keys() if not is_edge(k) and legacy.get(k) != cells.get(k))
    return why + ([f"the runs differ in extra env beyond the edge set: {', '.join(other)}"] if other else [])


def invalid_reasons(legacy, cells, ids, envs, benches) -> list[str]:
    why = []
    for name, rows in (("legacy", legacy), ("cells", cells)):
        if ids - rows.keys() or rows.keys() - ids:
            why.append(
                f"{name}: {len(ids - rows.keys())} manifest instances missing, {len(rows.keys() - ids)} not in the manifest"
            )
    why += differences(*envs) if all(envs) else [f"a run directory has no {RUN_ENV}"]
    versions = {r["engine_version"] for rows in (legacy, cells) for r in rows.values()} - {None}
    if len(versions) > 1:
        why.append(f"engine versions differ: {sorted(versions)}")
    if not why and all(legacy[i]["selected_files"] == cells[i]["selected_files"] for i in ids):
        why.append("the runs selected the same files on every instance: the environment had no effect")
    for m in ("file_recall", "line_recall"):
        if not why and not pairs(legacy, cells, ids, m):
            why.append(f"no instance has a {m} in both runs")
    bench_legacy, bench_cells = benches
    if bench_legacy.get("diffctx") != bench_cells.get("diffctx"):
        why.append(f"bench binaries differ: {bench_legacy.get('diffctx')} vs {bench_cells.get('diffctx')}")
    for op in BENCH_OPS:
        legacy_op, cells_op = bench_legacy["all"].get(op, {}), bench_cells["all"].get(op, {})
        if not legacy_op.get("n") or not cells_op.get("n") or legacy_op.get("errors") != cells_op.get("errors"):
            why.append(
                f"bench {op}: samples/errors legacy {legacy_op.get('n')}/{legacy_op.get('errors')}, cells {cells_op.get('n')}/{cells_op.get('errors')}"
            )
    return why


def decide(files_lo: float, lines_lo: float, p95: dict[str, tuple[float, float]]) -> list[str]:
    """The reasons the facts-based edges are worse; none means not worse."""
    worse = []
    if files_lo < FILES_MARGIN:
        worse.append(f"file recall: lower bound {files_lo:+.3f} < {FILES_MARGIN}")
    if lines_lo < LINES_MARGIN:
        worse.append(f"line recall: lower bound {lines_lo:+.3f} < {LINES_MARGIN}")
    for op, (legacy_p95, cells_p95) in p95.items():
        if cells_p95 > P95_RATIO * legacy_p95:
            worse.append(f"{op} p95 {cells_p95:.3f}s > {P95_RATIO:.2f} x {legacy_p95:.3f}s")
    return worse


def interval(group: list[tuple[str, float, float]]) -> dict:
    from eval.harness.stats import paired_bootstrap_delta

    return paired_bootstrap_delta([a for _, a, _ in group], [b for _, _, b in group])


def mean(group: list[tuple[str, float, float]], side: int) -> float:
    return sum(g[side] for g in group) / len(group) if group else float("nan")


def cell(group: list[tuple[str, float, float]]) -> str:
    if not group:
        return "n/a"
    d = interval(group)
    return f"{d['delta_mean']:+.3f} [{d['ci_lo']:+.3f}, {d['ci_hi']:+.3f}]"


def report(legacy: dict, cells: dict, ids: set[str], benches: tuple[dict, dict]) -> list[str]:
    by_metric = {m: pairs(legacy, cells, ids, m) for m in METRICS}
    print(
        "## Paired recall, cells minus legacy\n\n| metric | n | legacy | cells | delta [95% interval] |\n|---|---:|---:|---:|---|"
    )
    for m, group in by_metric.items():
        print(f"| {m} | {len(group)} | {mean(group, 1):.3f} | {mean(group, 2):.3f} | {cell(group)} |")
    print(
        "\n## Per language\n\n| language | n | files legacy | files cells | files delta | lines n | lines delta |\n|---|---:|---:|---:|---|---:|---|"
    )
    for lang in sorted({g[0] for g in by_metric["file_recall"]}):
        files = [g for g in by_metric["file_recall"] if g[0] == lang]
        lines = [g for g in by_metric["line_recall"] if g[0] == lang]
        print(
            f"| {lang} | {len(files)} | {mean(files, 1):.3f} | {mean(files, 2):.3f} | {cell(files)} | {len(lines)} | {cell(lines)} |"
        )
    moved = sum(legacy[i]["selected_files"] != cells[i]["selected_files"] for i in ids)
    failed = [sum(r["status"] != "ok" for r in rows.values()) for rows in (legacy, cells)]
    print(f"\nselection differs on {moved}/{len(ids)} instances; non-ok rows: legacy {failed[0]}, cells {failed[1]}")
    p95 = {op: (benches[0]["all"][op]["p95"], benches[1]["all"][op]["p95"]) for op in BENCH_OPS}
    print("\n" + " ".join(f"{op} p95 legacy {a:.3f}s cells {b:.3f}s." for op, (a, b) in p95.items()))
    lower = [interval(by_metric[m])["ci_lo"] for m in ("file_recall", "line_recall")]
    return decide(lower[0], lower[1], p95)


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--legacy", type=Path, required=True)
    p.add_argument("--cells", type=Path, required=True)
    p.add_argument("--bench-legacy", type=Path, required=True)
    p.add_argument("--bench-cells", type=Path, required=True)
    p.add_argument("--manifest-dir", type=Path, default=Path(__file__).resolve().parents[1] / "manifests" / "equiv")
    args = p.parse_args()
    ids = {line.strip() for f in args.manifest_dir.glob("test_*.txt") for line in f.read_text().splitlines() if line.strip()}
    (legacy, env_legacy), (cells, env_cells) = load(args.legacy), load(args.cells)
    benches = (json.loads(args.bench_legacy.read_text()), json.loads(args.bench_cells.read_text()))
    print(RULE + "\n")
    invalid = invalid_reasons(legacy, cells, ids, (env_legacy, env_cells), benches)
    if invalid:
        print("## Verdict: INVALID\n\n" + "\n".join(f"- {why}" for why in invalid))
        return 2
    worse = report(legacy, cells, ids, benches)
    print("\n## Verdict: " + ("WORSE\n\n" + "\n".join(f"- {why}" for why in worse) if worse else "NOT WORSE"))
    return 1 if worse else 0


if __name__ == "__main__":
    raise SystemExit(main())
