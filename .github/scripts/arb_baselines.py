"""The benchmark's own lexical, BM25 and RepoMap baselines, under both seedings.

Run with the interpreter that holds the pinned evaluator and nothing of ours,
like `arb_score.py`. The published baselines read the intent text; diffctx reads
only the diff. This reruns the evaluator's rankers on the same samples with the
query as shipped (intent plus anchor diff) and with the diff alone, so the two
seedings sit side by side. The rankers are the evaluator's own functions; this
file only loops them, once with the anchor file among the candidates (the
evaluator's protocol) and once without it (what diffctx's ranking does).

    arb-venv/bin/python .github/scripts/arb_baselines.py \\
        --data-root DIR/data --inputs DIR/inputs --out DIR/baselines.json
"""

from __future__ import annotations

import argparse
import json
import os
from collections import Counter, defaultdict
from pathlib import Path
from typing import Any

from agent_retrieval_bench import baseline, bcy_curve, repomap_eval
from agent_retrieval_bench.io import read_jsonl

RELEASE = "v2_edit2ripple"
BUDGETS = (4_000, 8_000, 16_000, 32_000)
SEEDINGS = ("full", "diffonly")
ANCHORS = ("kept", "dropped")
RANKERS = ("lexical", "bm25", "repomap")
TOP_K = 20
KEYS = ("Recall@5", "Recall@10", "Recall@20", "MRR")


def rank(
    ranker: str, sample: dict[str, Any], query_text: str, chunks: list[dict[str, Any]], index: Any, anchor: str | None
) -> list[dict[str, Any]]:
    if ranker == "repomap":
        candidates = repomap_eval.candidate_paths_for_filter(index.nodes, "all_files") - ({anchor} if anchor else set())
        ranked = repomap_eval.rank_files_for_sample(
            sample=sample,
            query_text=query_text,
            index=index,
            candidate_paths=candidates,
            query_weight=0.65,
            pagerank_weight=0.25,
            affinity_weight=0.10,
        )
        return repomap_eval.ranked_file_chunks(ranked, index.nodes) if ranked else []
    pool = [c for c in chunks if c.get("path") != anchor] if anchor else chunks
    return baseline.rank_chunks_for_ranker(query_text, pool, ranker)


def detail(sample: dict[str, Any], gold: list[str], ranked: list[dict[str, Any]]) -> dict[str, Any]:
    return {
        "sample_id": sample["id"],
        "task_type": sample.get("task_type"),
        "repo": sample["repo"],
        "base_commit": sample["base_commit"],
        "gold_files": gold,
        "top_files": baseline.unique_ranked_paths(ranked)[:TOP_K],
        "metrics": baseline.sample_metrics(gold, ranked, hard_negative_files=baseline.hard_negative_files(sample)),
    }


def mean(values: list[float]) -> float:
    return sum(values) / len(values) if values else 0.0


def summarize(details: list[dict[str, Any]], cache: bcy_curve.CorpusFileCache) -> dict[str, Any]:
    thresholds = bcy_curve.DEFAULT_COVERAGE_THRESHOLDS
    rows = [bcy_curve.evaluate_sample(d, d["gold_files"], cache, BUDGETS, thresholds) for d in details]
    bcy = bcy_curve.summarize_rows(rows, BUDGETS, thresholds)
    out: dict[str, Any] = {"samples": len(details)}
    out.update({k: mean([d["metrics"][k] for d in details]) for k in KEYS})
    out.update({f"BCY@{b}": bcy[f"BCY@{b}"] for b in BUDGETS})
    return out


def run(args: argparse.Namespace) -> dict[str, Any]:
    data_root = args.data_root.absolute()
    inputs = args.inputs.absolute()
    out_path = args.out.absolute()
    os.chdir(data_root.parent)  # the corpus manifest stores chunk paths relative to the data root's parent
    manifest = baseline.load_corpus_manifest(data_root / "corpus" / RELEASE)
    cache = bcy_curve.CorpusFileCache(manifest)
    ok_ids = set(json.loads((inputs / "ok_ids.json").read_text()))
    samples = {seeding: read_jsonl(inputs / f"samples_{seeding}.jsonl") for seeding in SEEDINGS}

    by_snapshot: dict[Path, list[int]] = defaultdict(list)
    for position, sample in enumerate(samples["full"][: args.limit or None]):
        chunks_path = manifest.get((sample["repo"], sample["base_commit"]))
        if chunks_path is None:
            raise SystemExit(f"no corpus for {sample['repo']}@{sample['base_commit'][:12]}")
        by_snapshot[chunks_path].append(position)

    cells: dict[tuple[str, str, str], list[dict[str, Any]]] = defaultdict(list)
    skipped: Counter[tuple[str, str, str]] = Counter()
    for chunks_path, positions in by_snapshot.items():
        chunks = read_jsonl(chunks_path)
        index = repomap_eval.build_repomap_index(chunks)
        for position in positions:
            for seeding in SEEDINGS:
                sample = samples[seeding][position]
                gold = baseline.target_gold_files(sample)
                query_text = baseline.query_text_for_eval(sample)
                anchor_file = (sample.get("query") or {}).get("anchor_file")
                for anchor_policy in ANCHORS:
                    for ranker in RANKERS:
                        cell = (seeding, anchor_policy, ranker)
                        if not gold or baseline.query_has_leakage(sample, query_text):
                            skipped[cell] += 1
                            continue
                        dropped = anchor_file if anchor_policy == "dropped" else None
                        ranked = rank(ranker, sample, query_text, chunks, index, dropped)
                        cells[cell].append(detail(sample, gold, ranked))
        print(f"done {chunks_path.name}", flush=True)

    result: dict[str, Any] = {"samples_ok_for_diffctx": len(ok_ids), "protocols": {}, "reproduction_check": {}}
    for (seeding, anchor_policy, ranker), details in sorted(cells.items()):
        subset = [d for d in details if d["sample_id"] in ok_ids]
        result["protocols"].setdefault(seeding, {}).setdefault(anchor_policy, {})[ranker] = {
            "same_samples_as_diffctx": summarize(subset, cache),
            "all_evaluated": summarize(details, cache),
            "skipped_as_leaky_or_goldless": skipped[(seeding, anchor_policy, ranker)],
        }
    # The shipped summaries cover all 58 samples with the query as shipped and the anchor among the candidates; the
    # same rankers must land on the same numbers or this rerun is not the evaluator's.
    for ranker in RANKERS:
        shipped = data_root / "eval" / RELEASE / f"{ranker}_summary.json"
        if shipped.exists():
            published = json.loads(shipped.read_text())["metrics"]["overall"]
            ours = result["protocols"]["full"]["kept"][ranker]["all_evaluated"]
            result["reproduction_check"][ranker] = {
                k: {"shipped": published[k], "rerun": ours[k], "abs_diff": abs(published[k] - ours[k])} for k in KEYS
            }
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return result


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--data-root", type=Path, required=True)
    parser.add_argument("--inputs", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--limit", type=int, default=0, help="first N samples only, for a local smoke")
    result = run(parser.parse_args())
    for seeding, anchors in result["protocols"].items():
        for anchor_policy, rankers in anchors.items():
            for ranker, cell in rankers.items():
                m = cell["same_samples_as_diffctx"]
                print(seeding, anchor_policy, ranker, " ".join(f"{k}={m[k]:.4f}" for k in KEYS), f"n={m['samples']}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
