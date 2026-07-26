"""Enrich existing checkpoint.jsonl files with per-instance descriptors.

Old sweep artifacts predate the evaluator stamping `n_gold`, `gold_to_changed_ratio`,
`is_*_file_gold`, `n_changed_files`, etc. into `EvalResult.extra`. Without those
fields `cell_metrics.py` cannot compute the stratification or gold-characterization
sections. This script walks each checkpoint row, looks up its `BenchmarkInstance`
by id, recomputes the missing fields, and writes them back.

CLI:
    python -m eval backfill-checkpoints --cells-dir /tmp/sweep-25402041321
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

from eval.datasets.build_splits import default_calibration_pool_adapters, default_test_adapters
from eval.harness.adapters.base import BenchmarkAdapter, BenchmarkInstance
from eval.harness.common import patch_size_metrics


def _load_instance_index(adapters: list[BenchmarkAdapter]) -> dict[str, BenchmarkInstance]:
    out: dict[str, BenchmarkInstance] = {}
    for adapter in adapters:
        sys.stderr.write(f"Loading {adapter.name}...\n")
        for inst in adapter.load():
            out[inst.instance_id] = inst
    return out


def _gold_lines_total(inst: BenchmarkInstance) -> int:
    if inst.gold_fragments is None:
        return 0
    total = 0
    for g in inst.gold_fragments:
        if g.is_whole_file() or g.start_line is None or g.end_line is None:
            continue
        total += g.end_line - g.start_line + 1
    return total


def _enrich_extra(extra: dict, inst: BenchmarkInstance, n_selected_proxy: int | None) -> dict:
    """Add fields the new evaluator stamps; preserve everything else."""
    n_gold = len(inst.gold_files)
    extra.setdefault("n_gold", n_gold)
    if n_selected_proxy is not None:
        extra.setdefault("n_selected", n_selected_proxy)
        extra.setdefault("selected_to_gold_ratio", n_selected_proxy / n_gold if n_gold > 0 else 0.0)

    patch_stats = patch_size_metrics(inst.gold_patch)
    for k, v in patch_stats.items():
        extra.setdefault(k, v)
    n_changed = patch_stats["n_changed_files"]
    extra.setdefault("gold_to_changed_ratio", n_gold / n_changed if n_changed > 0 else 0.0)
    extra.setdefault("is_single_file_gold", n_gold == 1)
    extra.setdefault("is_multi_file_gold", n_gold >= 2)
    if inst.gold_fragments is not None:
        n_whole = sum(1 for g in inst.gold_fragments if g.is_whole_file())
        n_hunk = len(inst.gold_fragments) - n_whole
        extra.setdefault("n_gold_fragments_total", len(inst.gold_fragments))
        extra.setdefault("n_gold_fragments_whole_file", n_whole)
        extra.setdefault("n_gold_fragments_hunk", n_hunk)
        extra.setdefault("n_gold_lines", _gold_lines_total(inst))
        extra.setdefault("is_whole_file_gold", n_whole > 0 and n_hunk == 0)
    return extra


def backfill_checkpoint(path: Path, index: dict[str, BenchmarkInstance]) -> tuple[int, int]:
    """Rewrite checkpoint.jsonl atomically (tmp + rename). Returns (rows_total, rows_enriched).

    The checkpoint is the single source of truth for collected sweep data;
    an in-place truncate-and-rewrite would destroy it on any crash mid-write.
    Corrupt lines (torn tail after a kill) are preserved verbatim, matching
    the tolerant readers in runner.py / aggregate_sweep.py.
    """
    rows: list[dict | str] = []
    with path.open() as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                rows.append(json.loads(line))
            except ValueError:
                rows.append(line)

    enriched = 0
    for r in rows:
        if not isinstance(r, dict):
            continue
        inst = index.get(r.get("instance_id", ""))
        if inst is None:
            continue
        extra = r.setdefault("extra", {})
        # Use existing fragment_count as n_selected proxy when n_selected absent
        # (legacy diffctx checkpoints stamped fragment_count, not file count, so this
        # is approximate but better than dropping cardinality entirely).
        n_sel_proxy = extra.get("fragment_count")
        _enrich_extra(extra, inst, n_sel_proxy)
        enriched += 1

    tmp_path = path.with_name(path.name + ".tmp")
    with tmp_path.open("w") as f:
        for r in rows:
            f.write((json.dumps(r, default=str) if isinstance(r, dict) else r) + "\n")
    tmp_path.replace(path)
    return len(rows), enriched


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--cells-dir", type=Path, required=True, help="root containing cell-* artifact directories")
    p.add_argument("--include-calibration", action="store_true", help="also load calibration adapters (Lite/PolyBench/Multi-SWE)")
    args = p.parse_args()

    adapters: list[BenchmarkAdapter] = list(default_test_adapters())
    if args.include_calibration:
        adapters.extend(default_calibration_pool_adapters())

    index = _load_instance_index(adapters)
    sys.stderr.write(f"Loaded {len(index)} instances across {len(adapters)} adapters\n")

    cell_dirs = sorted(p for p in args.cells_dir.iterdir() if p.is_dir() and p.name.startswith("cell-"))
    total_rows = 0
    total_enriched = 0
    for cell in cell_dirs:
        ckpts = sorted(cell.glob("*.checkpoint.jsonl"))
        if not ckpts:
            continue
        for ckpt in ckpts:
            n, m = backfill_checkpoint(ckpt, index)
            total_rows += n
            total_enriched += m
    sys.stderr.write(f"Backfilled {total_enriched}/{total_rows} rows in {len(cell_dirs)} cells\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
