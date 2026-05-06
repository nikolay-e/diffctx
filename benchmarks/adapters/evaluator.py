from __future__ import annotations

from collections.abc import Iterable
from dataclasses import dataclass, field

from benchmarks.adapters.base import BenchmarkInstance, EvalResult, GoldenFragment


@dataclass(frozen=True)
class SelectionOutput:
    """What a context-selection method produced for one instance.

    `selected_files` is always populated. `selected_fragments` is optional —
    methods that only emit file lists (baselines) can leave it None and still
    receive file-level metrics.
    """

    selected_files: frozenset[str]
    selected_fragments: tuple[GoldenFragment, ...] | None = None
    used_tokens: int = 0
    elapsed_seconds: float = 0.0
    extra: dict[str, object] = field(default_factory=dict)


def _safe_div(num: int, denom: int) -> float:
    return num / denom if denom > 0 else 0.0


def _line_set(start: int | None, end: int | None) -> set[int]:
    if start is None or end is None:
        return set()
    if end < start:
        return set()
    return set(range(start, end + 1))


def _by_path(fragments: Iterable[GoldenFragment]) -> dict[str, list[GoldenFragment]]:
    out: dict[str, list[GoldenFragment]] = {}
    for f in fragments:
        out.setdefault(f.path, []).append(f)
    return out


def _file_metrics(selected: frozenset[str], gold: frozenset[str]) -> tuple[float, float]:
    overlap = len(selected & gold)
    return _safe_div(overlap, len(gold)), _safe_div(overlap, len(selected))


def _gold_fragment_hit(g: GoldenFragment, sel_by_path: dict[str, list[GoldenFragment]]) -> bool:
    if g.is_whole_file():
        return bool(sel_by_path.get(g.path))
    g_lines = _line_set(g.start_line, g.end_line)
    if not g_lines:
        return False
    for s in sel_by_path.get(g.path, []):
        if s.is_whole_file() or (g_lines & _line_set(s.start_line, s.end_line)):
            return True
    return False


def _sel_fragment_hit(s: GoldenFragment, gold_by_path: dict[str, list[GoldenFragment]]) -> bool:
    if s.is_whole_file():
        return bool(gold_by_path.get(s.path))
    s_lines = _line_set(s.start_line, s.end_line)
    if not s_lines:
        return False
    for g in gold_by_path.get(s.path, []):
        if g.is_whole_file() or (s_lines & _line_set(g.start_line, g.end_line)):
            return True
    return False


def _fragment_metrics(
    selected: tuple[GoldenFragment, ...],
    gold: tuple[GoldenFragment, ...],
) -> tuple[float, float]:
    sel_by_path = _by_path(selected)
    gold_by_path = _by_path(gold)
    gold_hits = sum(1 for g in gold if _gold_fragment_hit(g, sel_by_path))
    sel_hits = sum(1 for s in selected if _sel_fragment_hit(s, gold_by_path))
    return _safe_div(gold_hits, len(gold)), _safe_div(sel_hits, len(selected))


def _line_f1(
    selected: tuple[GoldenFragment, ...],
    gold: tuple[GoldenFragment, ...],
) -> float:
    """Per-file line set F1, averaged over files appearing in gold.

    Whole-file gold contributes nothing to line F1 (we lack a line count
    without reading the file). Whole-file selected fragments are likewise
    skipped — line-level F1 is only meaningful for hunk-level annotations.
    """
    sel_lines: dict[str, set[int]] = {}
    for s in selected:
        sel_lines.setdefault(s.path, set()).update(_line_set(s.start_line, s.end_line))
    gold_lines: dict[str, set[int]] = {}
    for g in gold:
        gold_lines.setdefault(g.path, set()).update(_line_set(g.start_line, g.end_line))

    paths = [p for p, ls in gold_lines.items() if ls]
    if not paths:
        return 0.0
    total = 0.0
    for path in paths:
        g = gold_lines[path]
        s = sel_lines.get(path, set())
        if not g and not s:
            continue
        tp = len(g & s)
        fp = len(s - g)
        fn = len(g - s)
        precision = _safe_div(tp, tp + fp)
        recall = _safe_div(tp, tp + fn)
        if precision + recall == 0:
            continue
        total += 2 * precision * recall / (precision + recall)
    return total / len(paths)


class UniversalEvaluator:
    """One evaluator across heterogeneous benchmarks.

    File-level metrics always run. Fragment-level + line-F1 are only computed
    when the source benchmark provides `gold_fragments` (ContextBench,
    PolyBench). For Lite / Verified / Multi-SWE-bench the fragment fields
    stay None and the result still carries file-level numbers.
    """

    def evaluate(
        self,
        instance: BenchmarkInstance,
        output: SelectionOutput,
        budget: int,
    ) -> EvalResult:
        from benchmarks.common import patch_size_metrics

        file_recall, file_precision = _file_metrics(output.selected_files, instance.gold_files)
        result = EvalResult(
            instance_id=instance.instance_id,
            source_benchmark=instance.source_benchmark,
            file_recall=file_recall,
            file_precision=file_precision,
            used_tokens=output.used_tokens,
            budget=budget,
            elapsed_seconds=output.elapsed_seconds,
        )
        n_gold = len(instance.gold_files)
        result.extra["n_selected"] = len(output.selected_files)
        result.extra["n_gold"] = n_gold
        result.extra["selected_to_gold_ratio"] = len(output.selected_files) / n_gold if n_gold > 0 else 0.0
        patch_stats = patch_size_metrics(instance.gold_patch)
        result.extra.update(patch_stats)
        n_changed = patch_stats["n_changed_files"]
        # Difficulty proxy: |gold| / |changed_files|. Ratio==1 means gold == diff
        # (trivial, no context discovery needed). Ratio>1 means real retrieval is required.
        result.extra["gold_to_changed_ratio"] = n_gold / n_changed if n_changed > 0 else 0.0
        result.extra["is_single_file_gold"] = n_gold == 1
        result.extra["is_multi_file_gold"] = n_gold >= 2
        if instance.gold_fragments is not None:
            n_whole = sum(1 for g in instance.gold_fragments if g.is_whole_file())
            n_hunk = len(instance.gold_fragments) - n_whole
            n_gold_lines = sum(
                ((g.end_line or 0) - (g.start_line or 0) + 1) if not g.is_whole_file() else 0 for g in instance.gold_fragments
            )
            result.extra["n_gold_fragments_total"] = len(instance.gold_fragments)
            result.extra["n_gold_fragments_whole_file"] = n_whole
            result.extra["n_gold_fragments_hunk"] = n_hunk
            result.extra["n_gold_lines"] = n_gold_lines
            result.extra["is_whole_file_gold"] = n_whole > 0 and n_hunk == 0
        if instance.gold_fragments is not None and output.selected_fragments is not None:
            frag_r, frag_p = _fragment_metrics(output.selected_fragments, instance.gold_fragments)
            result.fragment_recall = frag_r
            result.fragment_precision = frag_p
            result.line_f1 = _line_f1(output.selected_fragments, instance.gold_fragments)
            result.extra["n_selected_fragments"] = len(output.selected_fragments)
            result.extra["n_gold_fragments"] = len(instance.gold_fragments)
        return result

    def aggregate_per_benchmark(self, results: Iterable[EvalResult]) -> dict[str, dict[str, float]]:
        """Mean per-benchmark per-metric. Calibration objective uses min over
        per-benchmark recalls (generalization-friendly)."""
        groups: dict[str, list[EvalResult]] = {}
        for r in results:
            groups.setdefault(r.source_benchmark, []).append(r)
        out: dict[str, dict[str, float]] = {}
        for name, rs in groups.items():
            n = len(rs)
            if n == 0:
                continue
            agg = {
                "n": float(n),
                "file_recall": sum(r.file_recall for r in rs) / n,
                "file_precision": sum(r.file_precision for r in rs) / n,
                "used_tokens_mean": sum(r.used_tokens for r in rs) / n,
            }
            frs = [r.fragment_recall for r in rs if r.fragment_recall is not None]
            if frs:
                agg["fragment_recall"] = sum(frs) / len(frs)
            fps = [r.fragment_precision for r in rs if r.fragment_precision is not None]
            if fps:
                agg["fragment_precision"] = sum(fps) / len(fps)
            lfs = [r.line_f1 for r in rs if r.line_f1 is not None]
            if lfs:
                agg["line_f1"] = sum(lfs) / len(lfs)
            out[name] = agg
        return out
