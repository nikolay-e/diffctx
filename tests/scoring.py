import json
import time
from pathlib import Path

MINIMUM_AVERAGE_SCORE = 82.0


def extract_scores_from_reports(terminalreporter):
    results = []
    for report in terminalreporter.stats.get("passed", []) + terminalreporter.stats.get("failed", []):
        if not hasattr(report, "user_properties"):
            continue
        props = dict(report.user_properties)
        if "score" not in props:
            continue
        case_id = report.nodeid.split("[")[-1].rstrip("]") if "[" in report.nodeid else report.nodeid
        results.append((case_id, props))
    return results


def compute_score_stats(results, config):
    scores = [p["score"] for _, p in results]
    return {
        "scores": scores,
        "diff_fails": sum(1 for _, p in results if not p.get("diff_covered", True)),
        "perfect": sum(1 for s in scores if s >= 100.0),
        "above_90": sum(1 for s in scores if s >= 90.0),
        "above_70": sum(1 for s in scores if s >= 70.0),
        "below_50": sum(1 for s in scores if s < 50.0),
        "avg_score": getattr(config, "_diffctx_avg", sum(scores) / len(scores) if scores else 0),
    }


def compute_enrichment_stats(results):
    enrichments = [p["enrichment"] for _, p in results if p.get("enrichment", 0) > 0]
    total_diff_tok = sum(p.get("diff_tokens", 0) for _, p in results)
    total_ctx_tok = sum(p.get("context_tokens", 0) for _, p in results)
    return {
        "avg_enrichment": sum(enrichments) / len(enrichments) if enrichments else 0,
        "total_diff_tok": total_diff_tok,
        "total_ctx_tok": total_ctx_tok,
        "global_enrichment": (total_ctx_tok / total_diff_tok * 100) if total_diff_tok > 0 else 0,
    }


def format_entry(case_id, props):
    flags = []
    if not props.get("diff_covered", True):
        flags.append("DIFF_MISS")
    noise = props.get("noise_rate", 0)
    if noise > 0:
        flags.append(f"noise={noise}%")
    garbage = props.get("garbage_rate", 0)
    if garbage > 0:
        flags.append(f"garbage={garbage}%")
    recall = props.get("recall", 100)
    if recall < 100:
        flags.append(f"recall={recall}%")
    enrich = props.get("enrichment", 0)
    if enrich > 0:
        flags.append(f"ctx={enrich}%")
    flag_str = f" [{', '.join(flags)}]" if flags else ""
    return f"  {props['score']:5.1f}%  {case_id}{flag_str}"


def write_scores_report(results, stats, enrich):
    scores_dir = Path(".scores")
    scores_dir.mkdir(exist_ok=True)
    report = {
        "timestamp": time.time(),
        "total_cases": len(results),
        "average_score": round(stats["avg_score"], 2),
        "minimum_score": MINIMUM_AVERAGE_SCORE,
        "perfect_count": stats["perfect"],
        "diff_hard_fails": stats["diff_fails"],
        "avg_enrichment": round(enrich["avg_enrichment"]),
        "global_enrichment": round(enrich["global_enrichment"]),
        "total_diff_tokens": enrich["total_diff_tok"],
        "total_context_tokens": enrich["total_ctx_tok"],
        "cases": {
            case_id: {
                "score": p["score"],
                "recall": p.get("recall", 100),
                "noise_rate": p.get("noise_rate", 0),
                "garbage_rate": p.get("garbage_rate", 0),
                "diff_covered": p.get("diff_covered", True),
                "enrichment": p.get("enrichment", 0),
                "diff_tokens": p.get("diff_tokens", 0),
                "context_tokens": p.get("context_tokens", 0),
            }
            for case_id, p in results
        },
    }
    (scores_dir / "latest.json").write_text(json.dumps(report, indent=2))


def write_score_histogram(terminalreporter, results):
    scores = [p["score"] for _, p in results]
    if not scores:
        return

    bucket_labels = ["0-10", "10-20", "20-30", "30-40", "40-50", "50-60", "60-70", "70-80", "80-90", "90-100", "100"]
    counts = dict.fromkeys(bucket_labels, 0)
    for s in scores:
        if s >= 100.0:
            counts["100"] += 1
        else:
            lo = int(s // 10) * 10
            counts[f"{lo}-{lo + 10}"] += 1

    total = len(scores)
    max_count = max(counts.values()) if counts else 1

    terminalreporter.write_line("")
    terminalreporter.write_line("  Score distribution:")
    for label in bucket_labels:
        count = counts[label]
        bar_len = int(40 * count / max_count) if max_count > 0 else 0
        bar = "\u2588" * bar_len
        pct = 100 * count / total
        if label == "100":
            terminalreporter.write_line(f"     {label}% \u2502 {bar:<40} {count:>4} ({pct:>5.1f}%)")
        else:
            terminalreporter.write_line(f"  {label:>6}% \u2502 {bar:<40} {count:>4} ({pct:>5.1f}%)")
    terminalreporter.write_line("")

    for thresh in [50, 70, 90, 100]:
        above = sum(1 for s in scores if s >= thresh)
        op = "=" if thresh == 100 else "\u2265"
        terminalreporter.write_line(f"  {op}{thresh:>3}%: {above:>5} / {total} ({100 * above / total:.1f}%)")

    below_50 = sum(1 for s in scores if s < 50)
    terminalreporter.write_line(f"   <50%: {below_50:>5} / {total} ({100 * below_50 / total:.1f}%)")
    terminalreporter.write_line("")


def handle_session_finish(session, exitstatus):
    tr = session.config.pluginmanager.get_plugin("terminalreporter")
    if tr is None:
        return
    results = extract_scores_from_reports(tr)
    if not results:
        return
    scores = [p["score"] for _, p in results]
    avg = sum(scores) / len(scores)
    session.config._diffctx_avg = avg
    session.config._diffctx_results = results
    if avg < MINIMUM_AVERAGE_SCORE:
        session.exitstatus = 1


def handle_terminal_summary(terminalreporter, exitstatus, config):
    results = getattr(config, "_diffctx_results", None)
    if results is None:
        results = extract_scores_from_reports(terminalreporter)
    if not results:
        return

    stats = compute_score_stats(results, config)
    enrich = compute_enrichment_stats(results)

    terminalreporter.write_sep("=", "DIFFCTX QUALITY SCORES")
    terminalreporter.write_line(f"  Cases scored:    {len(results)}")
    terminalreporter.write_line(f"  Average score:   {stats['avg_score']:.1f}% (min: {MINIMUM_AVERAGE_SCORE}%)")
    terminalreporter.write_line(f"  Perfect (100%):  {stats['perfect']}")
    terminalreporter.write_line(f"  Above 90%:       {stats['above_90']}")
    terminalreporter.write_line(f"  Above 70%:       {stats['above_70']}")
    terminalreporter.write_line(f"  Below 50%:       {stats['below_50']}")
    terminalreporter.write_line(f"  Diff hard fails: {stats['diff_fails']}")
    terminalreporter.write_line("")
    terminalreporter.write_line(f"  Context enrichment (avg per-case):  {enrich['avg_enrichment']:.0f}%")
    terminalreporter.write_line(f"  Context enrichment (global):        {enrich['global_enrichment']:.0f}%")
    terminalreporter.write_line(f"  Total diff tokens:    {enrich['total_diff_tok']:,}")
    terminalreporter.write_line(f"  Total context tokens: {enrich['total_ctx_tok']:,}")

    write_score_histogram(terminalreporter, results)

    if stats["avg_score"] < MINIMUM_AVERAGE_SCORE:
        terminalreporter.write_sep("!", "SCORE REGRESSION")
        terminalreporter.write_line(f"  Average score {stats['avg_score']:.1f}% dropped below minimum {MINIMUM_AVERAGE_SCORE}%")

    sorted_results = sorted(results, key=lambda r: r[1]["score"])

    worst = [r for r in sorted_results if r[1]["score"] < 100.0][:300]
    if worst:
        terminalreporter.write_sep("-", f"LOWEST SCORES ({len(worst)})")
        for case_id, props in worst:
            terminalreporter.write_line(format_entry(case_id, props))

    best = [r for r in reversed(sorted_results) if r[1]["score"] > 0.0][:300]
    if best:
        terminalreporter.write_sep("-", f"HIGHEST SCORES ({len(best)})")
        for case_id, props in best:
            terminalreporter.write_line(format_entry(case_id, props))

    write_scores_report(results, stats, enrich)
