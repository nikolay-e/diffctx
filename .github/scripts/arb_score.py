"""Score diffctx rankings with Agent Retrieval Bench's own evaluator.

Run with an interpreter that has the pinned evaluator installed and none of our
code: it imports nothing from `eval` or `diffctx`, only the standard library and
`agent_retrieval_bench`. Recall, MRR and the file metrics come from the
evaluator's `sample_metrics`; BCY from its `bcy_curve` packing over the release
corpus text. This file only turns our ranking rows into the evaluator's input
shape and summarizes what it returns.

    arb-venv/bin/python .github/scripts/arb_score.py \\
        --data-root DIR/data --rankings DIR/rankings.jsonl --out DIR/score \\
        --engine-ref SHA --evaluator-commit SHA
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from collections import defaultdict
from pathlib import Path
from typing import Any

from agent_retrieval_bench import baseline, bcy_curve
from agent_retrieval_bench.io import read_jsonl

RELEASE = "v2_edit2ripple"
BUDGETS = (4_000, 8_000, 16_000, 32_000)
TOP_K = 20
BASELINES = ("lexical", "bm25", "repomap")
CAVEATS = [
    "BCY packs with the evaluator's regex_code_tokenizer_v1, not o200k_base; the selection_8k block counts the "
    "engine's own o200k tokens, so the two budget columns are never mixed",
    "the selective (no-gold) track does not apply: diffctx has no abstention and none is invented",
    "the release stores anchor diffs that do not apply as shipped (a trimmed trailing context line, or a cut at a size limit "
    "marked ...[truncated]); their hunk counts are repaired and a seed_quality block separates untouched from repaired seeds, "
    "so a truncated seed is a partial change, never the whole one",
    "seeding is diff-only: the anchor diff is the whole input and the intent text is unused, so these numbers "
    "answer a different question than the published ones, which receive the intent",
    "gold is the files changed in the same fix: a recall oracle for co-change, weak evidence for precision",
    "candidates are the release corpus files (candidate filter all_files); files the corpus does not hold are dropped from the ranking",
    "BCY is computed from the stored top-20 file list, as the evaluator does for its own runs",
    "published baselines are the summaries the release ships for this subset; the evaluator's note says RepoMap "
    "scores there predate its deterministic-graph fix",
]


def merge_rankings(paths: list[Path]) -> dict[str, dict[str, Any]]:
    rows: dict[str, dict[str, Any]] = {}
    for path in paths:
        for row in read_jsonl(path):
            rows[str(row["sample_id"])] = row
    return rows


def detail_for(
    index: int, sample: dict[str, Any], ranking: dict[str, Any] | None, cache: bcy_curve.CorpusFileCache
) -> dict[str, Any]:
    gold_files = baseline.target_gold_files(sample)
    hard_negatives = baseline.hard_negative_files(sample)
    anchor = (sample.get("query") or {}).get("anchor_file")
    ok = bool(ranking) and ranking.get("status") == "ok"
    ranked_chunks: list[dict[str, Any]] = []
    selected: list[str] = []
    if ok:
        assert ranking is not None
        repo, base_commit = str(sample["repo"]), str(sample["base_commit"])
        ranked_chunks = [
            {"path": path, "kind": "file", "text": text}
            for path, _score in ranking["ranked"]
            if path != anchor and (text := cache.file_text(repo, base_commit, path)) is not None
        ]
        selected = [p for p in ranking["selected_files"] if p != anchor]
    top_files = baseline.unique_ranked_paths(ranked_chunks)[:TOP_K]
    gold = set(gold_files)
    return {
        "_sample_index": index,
        "sample_id": sample.get("id"),
        "task_type": sample.get("task_type"),
        "repo": sample.get("repo"),
        "base_commit": sample.get("base_commit"),
        "candidate_filter": "all_files",
        "ranker": "diffctx_locate",
        "seed_repair": (ranking or {}).get("seed_repair"),
        "status": ranking.get("status") if ranking else "missing",
        "gold_files": gold_files,
        "hard_negative_files": hard_negatives,
        "gold_ranks": baseline.gold_file_ranks(gold_files, ranked_chunks),
        "top_files": top_files,
        "ranked_files": baseline.unique_ranked_paths(ranked_chunks),
        "metrics": baseline.sample_metrics(gold_files, ranked_chunks, hard_negative_files=hard_negatives),
        "selection_8k": {
            "files": selected,
            "gold_recall": (len(gold & set(selected)) / len(gold)) if gold else 0.0,
            "precision": (len(gold & set(selected)) / len(selected)) if selected else 0.0,
        },
        "ranked_dropped_outside_corpus": (len(ranking["ranked"]) - len(ranked_chunks)) if ok else None,
    }


def mean(values: list[float]) -> float:
    return sum(values) / len(values) if values else 0.0


def selection_summary(details: list[dict[str, Any]]) -> dict[str, float]:
    return {
        "samples": len(details),
        "gold_recall": mean([d["selection_8k"]["gold_recall"] for d in details]),
        "precision": mean([d["selection_8k"]["precision"] for d in details]),
        "files": mean([float(len(d["selection_8k"]["files"])) for d in details]),
    }


def published_baselines(data_root: Path) -> dict[str, Any]:
    out: dict[str, Any] = {}
    for name in BASELINES:
        path = data_root / "eval" / RELEASE / f"{name}_summary.json"
        if path.exists():
            out[name] = json.loads(path.read_text())["metrics"]["overall"]
    return out


def score(args: argparse.Namespace) -> dict[str, Any]:
    # the corpus manifest stores chunk paths relative to the data root's parent, so
    # every path argument is made absolute before the working directory changes
    data_root = args.data_root.absolute()
    args.out = args.out.absolute()
    args.rankings = [p.absolute() for p in args.rankings]
    missing_files = [str(p) for p in args.rankings if not p.is_file()]
    if missing_files:
        raise SystemExit(f"rankings not found: {missing_files}")
    os.chdir(data_root.parent)  # the corpus manifest stores chunk paths as data/corpus/...
    samples = list(baseline.iter_samples([data_root / "benchmark" / RELEASE / "samples.jsonl"]))
    manifest = baseline.load_corpus_manifest(data_root / "corpus" / RELEASE)
    missing_corpus = sorted({f"{s['repo']}@{s['base_commit'][:12]}" for s in samples} - {f"{r}@{b[:12]}" for r, b in manifest})
    if missing_corpus:
        raise SystemExit(f"the release corpus is not readable from {data_root.parent} for: {missing_corpus[:5]}")
    rankings = merge_rankings(args.rankings)
    unknown = sorted(set(rankings) - {str(s["id"]) for s in samples})
    if unknown:
        raise SystemExit(f"rankings name samples the release does not hold: {unknown[:5]}")
    if args.require_all and len(rankings) != len(samples):
        raise SystemExit(f"{len(rankings)} rankings for {len(samples)} samples")

    cache = bcy_curve.CorpusFileCache(manifest)
    details: list[dict[str, Any]] = []
    for index, sample in enumerate(samples):
        ranking = rankings.get(str(sample["id"]))
        if ranking is None and not args.require_all:
            continue
        details.append(detail_for(index, sample, ranking, cache))
    for detail in details:
        detail.pop("_sample_index")

    args.out.mkdir(parents=True, exist_ok=True)
    details_path = args.out / "diffctx_details.jsonl"
    details_path.write_text("".join(json.dumps(d, ensure_ascii=False, sort_keys=True) + "\n" for d in details), encoding="utf-8")

    ok = [d for d in details if d["status"] == "ok"]
    statuses: dict[str, int] = defaultdict(int)
    for d in details:
        statuses[d["status"]] += 1

    thresholds = bcy_curve.DEFAULT_COVERAGE_THRESHOLDS
    bcy_rows = [bcy_curve.evaluate_sample(d, d["gold_files"], cache, BUDGETS, thresholds) for d in details]
    ok_ids = {d["sample_id"] for d in ok}
    bcy_ok_rows = [row for row in bcy_rows if row["sample_id"] in ok_ids]

    by_repo: dict[str, dict[str, Any]] = {}
    for repo in sorted({d["repo"] for d in details}):
        group = [d for d in ok if d["repo"] == repo]
        by_repo[repo] = {
            "samples": sum(1 for d in details if d["repo"] == repo),
            "ok": len(group),
            "Recall@20": mean([d["metrics"]["Recall@20"] for d in group]),
            "MRR": mean([d["metrics"]["MRR"] for d in group]),
            "selection_8k_recall": mean([d["selection_8k"]["gold_recall"] for d in group]),
        }

    def repaired(d: dict[str, Any]) -> bool:
        repair = d.get("seed_repair") or {}
        return bool(repair.get("truncated") or repair.get("recounted_hunks") or repair.get("dropped_changeless_hunks"))

    clean = [d for d in ok if not repaired(d)]
    summary = {
        "dataset": "arb_edit2ripple",
        "release": RELEASE,
        "seeding": "diff-only",
        "engine_ref": args.engine_ref,
        "engine_versions": sorted({r.get("engine_version") for r in rankings.values() if r.get("engine_version")}),
        "effective_config_hashes": sorted(
            {r.get("effective_config_hash") for r in rankings.values() if r.get("effective_config_hash")}
        ),
        "evaluator_commit": args.evaluator_commit,
        "tokenizer": bcy_curve.TOKENIZER_NAME,
        "samples": len(details),
        "ok": len(ok),
        "statuses": dict(sorted(statuses.items())),
        "metrics_ok": baseline.summarize_details(ok).get("overall", {}),
        "seed_quality": {
            "ok_untouched_seed": len(clean),
            "ok_repaired_seed": len(ok) - len(clean),
            "ok_truncated_seed": sum(1 for d in ok if (d.get("seed_repair") or {}).get("truncated")),
        },
        "metrics_ok_untouched_seed": baseline.summarize_details(clean).get("overall", {}),
        "metrics_all_failures_zero": baseline.summarize_details(details).get("overall", {}),
        "bcy_ok": bcy_curve.summarize_rows(bcy_ok_rows, BUDGETS, thresholds),
        "bcy_all_failures_zero": bcy_curve.summarize_rows(bcy_rows, BUDGETS, thresholds),
        "selection_8k_ok": selection_summary(ok),
        "ranked_files_per_sample_ok": mean([float(len(d["ranked_files"])) for d in ok]),
        "ranked_dropped_outside_corpus_ok": mean([float(d["ranked_dropped_outside_corpus"] or 0) for d in ok]),
        "by_repo": by_repo,
        "published_baselines": published_baselines(data_root),
        "caveats": CAVEATS,
    }
    (args.out / "summary.json").write_text(json.dumps(summary, indent=2, sort_keys=True, default=str) + "\n", encoding="utf-8")
    return summary


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--data-root", type=Path, required=True)
    parser.add_argument("--rankings", type=Path, nargs="+", required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--engine-ref", default="")
    parser.add_argument("--evaluator-commit", default="")
    parser.add_argument("--require-all", action="store_true", help="fail unless every release sample has a ranking row")
    args = parser.parse_args()
    summary = score(args)
    headline = {k: summary[k] for k in ("samples", "ok", "statuses")}
    print(json.dumps(headline))
    metrics = summary["metrics_ok"]
    print(
        "ok-only: "
        + " ".join(f"{k}={metrics.get(k, 0.0):.4f}" for k in ("Recall@5", "Recall@10", "Recall@20", "MRR"))
        + " "
        + " ".join(f"BCY@{b}={summary['bcy_ok'].get(f'BCY@{b}', 0.0):.4f}" for b in BUDGETS)
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
