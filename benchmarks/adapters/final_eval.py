from __future__ import annotations

from collections.abc import Iterable
from dataclasses import dataclass

from benchmarks.adapters.base import EvalResult


@dataclass(frozen=True)
class TestSetReport:
    """Aggregate metrics for one test manifest at the final evaluation stage."""

    name: str  # e.g. "swebench_verified"
    n: int
    file_recall: float
    file_precision: float
    fragment_recall: float | None
    fragment_precision: float | None
    line_f1: float | None
    used_tokens_mean: float


def aggregate_test_set(name: str, results: list[EvalResult]) -> TestSetReport:
    if not results:
        return TestSetReport(name, 0, 0.0, 0.0, None, None, None, 0.0)
    n = len(results)
    file_recall = sum(r.file_recall for r in results) / n
    file_precision = sum(r.file_precision for r in results) / n
    used_tokens_mean = sum(r.used_tokens for r in results) / n
    fr = [r.fragment_recall for r in results if r.fragment_recall is not None]
    fp = [r.fragment_precision for r in results if r.fragment_precision is not None]
    lf = [r.line_f1 for r in results if r.line_f1 is not None]
    return TestSetReport(
        name=name,
        n=n,
        file_recall=file_recall,
        file_precision=file_precision,
        fragment_recall=(sum(fr) / len(fr)) if fr else None,
        fragment_precision=(sum(fp) / len(fp)) if fp else None,
        line_f1=(sum(lf) / len(lf)) if lf else None,
        used_tokens_mean=used_tokens_mean,
    )


def aggregate_by_language(results: Iterable[EvalResult]) -> dict[str, dict[str, float]]:
    """Cross-benchmark per-language file_recall, the second paper table.

    The result of `UniversalEvaluator.aggregate_per_benchmark` does NOT carry
    language, so we use the raw results plus the original instance lookup.
    To keep this layer pure we accept a flat list and group by an opaque
    language tag stamped into `extra["language"]` by the runner.
    """
    by_lang: dict[str, list[EvalResult]] = {}
    for r in results:
        lang = str(r.extra.get("language", "unknown"))
        by_lang.setdefault(lang, []).append(r)
    out: dict[str, dict[str, float]] = {}
    for lang, rs in by_lang.items():
        n = len(rs)
        out[lang] = {
            "n": float(n),
            "file_recall": sum(r.file_recall for r in rs) / n,
            "file_precision": sum(r.file_precision for r in rs) / n,
        }
    return out


def render_paper_table(reports: list[TestSetReport]) -> str:
    """Markdown table for paper Section 5 — per-test-set numbers."""
    if not reports:
        return "(no test reports)\n"
    lines: list[str] = []
    lines.append("| Benchmark | n | File recall | File precision | Fragment recall | Line F1 | Tokens mean |")
    lines.append("|---|---|---|---|---|---|---|")
    total_n = 0
    weighted_recall = 0.0
    weighted_precision = 0.0
    for r in reports:
        frag_r = f"{r.fragment_recall:.3f}" if r.fragment_recall is not None else "—"
        line_f1 = f"{r.line_f1:.3f}" if r.line_f1 is not None else "—"
        lines.append(
            f"| {r.name} | {r.n} | {r.file_recall:.3f} | {r.file_precision:.3f} | "
            f"{frag_r} | {line_f1} | {r.used_tokens_mean:.0f} |"
        )
        total_n += r.n
        weighted_recall += r.file_recall * r.n
        weighted_precision += r.file_precision * r.n
    if total_n > 0:
        lines.append(
            f"| **All benchmarks** | **{total_n}** | "
            f"**{weighted_recall / total_n:.3f}** | "
            f"**{weighted_precision / total_n:.3f}** | — | — | — |"
        )
    return "\n".join(lines) + "\n"


def render_language_table(per_lang: dict[str, dict[str, float]]) -> str:
    """Per-language file recall, used as a paper table."""
    if not per_lang:
        return "(no per-language data)\n"
    lines: list[str] = []
    lines.append("| Language | n | File recall | File precision |")
    lines.append("|---|---|---|---|")
    for lang in sorted(per_lang, key=lambda k: -per_lang[k]["n"]):
        agg = per_lang[lang]
        lines.append(f"| {lang} | {int(agg['n'])} | {agg['file_recall']:.3f} | {agg['file_precision']:.3f} |")
    return "\n".join(lines) + "\n"
