"""Render diffctx-vs-baseline comparison tables from per-benchmark JSON
result files produced by `benchmarks.run_final_eval`.

Example::

    python -m benchmarks.render_comparison \\
        --diffctx-dir results/final/v1 \\
        --baseline-dir results/final/v1/baselines/bm25 \\
        --baseline-name BM25 \\
        --out results/final/v1/COMPARISON_BM25.md
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path

from benchmarks.adapters.base import EvalResult
from benchmarks.adapters.final_eval import render_comparison_table


def _load_dir(path: Path) -> list[EvalResult]:
    out: list[EvalResult] = []
    for jf in sorted(path.glob("*.json")):
        if jf.name.startswith("."):
            continue
        for row in json.loads(jf.read_text()):
            out.append(
                EvalResult(
                    instance_id=row["instance_id"],
                    source_benchmark=row["source_benchmark"],
                    file_recall=float(row.get("file_recall", 0.0)),
                    file_precision=float(row.get("file_precision", 0.0)),
                    fragment_recall=row.get("fragment_recall"),
                    fragment_precision=row.get("fragment_precision"),
                    line_f1=row.get("line_f1"),
                    used_tokens=int(row.get("used_tokens", 0)),
                    budget=int(row.get("budget", 0)),
                    elapsed_seconds=float(row.get("elapsed_seconds", 0.0)),
                    extra=row.get("extra", {}) or {},
                )
            )
    return out


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--diffctx-dir", type=Path, required=True)
    p.add_argument("--baseline-dir", type=Path, required=True)
    p.add_argument("--baseline-name", type=str, required=True)
    p.add_argument("--out", type=Path, required=True)
    args = p.parse_args()

    d = _load_dir(args.diffctx_dir)
    b = _load_dir(args.baseline_dir)
    if not d or not b:
        print(f"empty: diffctx={len(d)} baseline={len(b)}")
        return 1

    sections: list[str] = [f"# diffctx vs {args.baseline_name}\n"]
    for metric, label in [
        ("file_recall", "File recall"),
        ("file_precision", "File precision"),
    ]:
        sections.append(f"## {label}\n")
        sections.append(render_comparison_table(d, b, args.baseline_name, metric=metric))

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text("\n".join(sections))
    print(f"wrote {args.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
