from __future__ import annotations

from abc import ABC, abstractmethod
from dataclasses import dataclass
from pathlib import Path

from .graph import Graph
from .types import DiffHunk, Fragment, FragmentId


@dataclass(frozen=True)
class DiscoveryContext:
    root_dir: Path
    changed_files: list[Path]
    all_candidate_files: list[Path]
    diff_text: str
    expansion_concepts: frozenset[str]
    file_cache: dict[Path, str] | None = None

    def read_file(self, path: Path) -> str | None:
        if self.file_cache is not None and path in self.file_cache:
            return self.file_cache[path]
        try:
            return path.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            return None


class DiscoveryStrategy(ABC):
    @abstractmethod
    def discover(self, ctx: DiscoveryContext) -> list[Path]: ...


class DefaultDiscovery(DiscoveryStrategy):
    def discover(self, ctx: DiscoveryContext) -> list[Path]:
        from .edges import discover_all_related_files
        from .universe import _expand_universe_by_rare_identifiers

        edge_discovered = discover_all_related_files(ctx.changed_files, ctx.all_candidate_files, ctx.root_dir)

        from ..ignore import get_ignore_specs

        combined_spec = get_ignore_specs(ctx.root_dir, None, False, None)
        expanded = _expand_universe_by_rare_identifiers(
            ctx.root_dir,
            ctx.expansion_concepts,
            ctx.changed_files + edge_discovered,
            combined_spec,
            candidate_files=ctx.all_candidate_files,
        )

        return list(dict.fromkeys(edge_discovered + expanded))


class BM25Discovery(DiscoveryStrategy):
    def __init__(self, top_k: int = 1) -> None:
        self.top_k = top_k

    def discover(self, ctx: DiscoveryContext) -> list[Path]:
        import math
        import re
        from collections import Counter

        token_re = re.compile(r"[A-Za-z_]\w{2,}")
        changed_set = set(ctx.changed_files)

        query_tokens = [m.group().lower() for m in token_re.finditer(ctx.diff_text)]
        if not query_tokens:
            return []

        corpus: list[list[str]] = []
        paths: list[Path] = []
        for f in ctx.all_candidate_files:
            if f in changed_set:
                continue
            content = ctx.read_file(f)
            if content is None:
                continue
            corpus.append([m.group().lower() for m in token_re.finditer(content)])
            paths.append(f)

        if not corpus:
            return []

        n_docs = len(corpus)
        avgdl = sum(len(d) for d in corpus) / n_docs
        df: Counter[str] = Counter()
        for doc in corpus:
            for term in set(doc):
                df[term] += 1

        query_set = set(query_tokens)
        idf = {t: math.log((n_docs - df.get(t, 0) + 0.5) / (df.get(t, 0) + 0.5) + 1.0) for t in query_set}

        scores: list[float] = []
        for doc in corpus:
            tf: Counter[str] = Counter(doc)
            dl = len(doc)
            s = 0.0
            for t in query_set:
                if t not in tf:
                    continue
                freq = tf[t]
                s += idf.get(t, 0) * (freq * 2.5) / (freq + 1.5 * (1 - 0.75 + 0.75 * dl / avgdl))
            scores.append(s)

        ranked = sorted(range(len(scores)), key=lambda i: -scores[i])
        return [paths[i] for i in ranked[: self.top_k] if scores[i] > 0]


class EnsembleDiscovery(DiscoveryStrategy):
    def __init__(self, strategies: list[DiscoveryStrategy] | None = None) -> None:
        self._strategies = strategies or [DefaultDiscovery(), BM25Discovery()]

    def discover(self, ctx: DiscoveryContext) -> list[Path]:
        seen: dict[Path, None] = {}
        for strategy in self._strategies:
            for path in strategy.discover(ctx):
                if path not in seen:
                    seen[path] = None
        return list(seen.keys())


@dataclass
class ScoringResult:
    rel_scores: dict[FragmentId, float]
    filtered_fragments: list[Fragment]
    graph: Graph


class ScoringStrategy(ABC):
    @abstractmethod
    def score_and_filter(
        self,
        all_fragments: list[Fragment],
        core_ids: set[FragmentId],
        hunks: list[DiffHunk],
        repo_root: Path | None = None,
        seed_weights: dict[FragmentId, float] | None = None,
        dump_scores_file: str | None = None,
    ) -> ScoringResult: ...


class PPRScoring(ScoringStrategy):
    def __init__(self, alpha: float = 0.60) -> None:
        self.alpha = alpha

    def score_and_filter(
        self,
        all_fragments: list[Fragment],
        core_ids: set[FragmentId],
        hunks: list[DiffHunk],
        repo_root: Path | None = None,
        seed_weights: dict[FragmentId, float] | None = None,
        dump_scores_file: str | None = None,
    ) -> ScoringResult:
        from .filtering import (
            _apply_hunk_proximity_bonus,
            _cap_context_fragments,
            _filter_low_relevance_fragments,
            _filter_unrelated_fragments,
        )
        from .graph import build_graph
        from .ppr import personalized_pagerank

        graph = build_graph(all_fragments, repo_root=repo_root)
        rel_scores = personalized_pagerank(graph, core_ids, alpha=self.alpha, seed_weights=seed_weights)
        _apply_hunk_proximity_bonus(rel_scores, core_ids, all_fragments, hunks)

        if dump_scores_file and repo_root:
            self._dump_scores(
                dump_scores_file,
                all_fragments,
                core_ids,
                rel_scores,
                graph,
                repo_root,
            )

        filtered = _filter_unrelated_fragments(all_fragments, core_ids, graph)
        filtered = _filter_low_relevance_fragments(filtered, core_ids, rel_scores)
        filtered = _cap_context_fragments(filtered, core_ids, rel_scores)

        return ScoringResult(rel_scores=rel_scores, filtered_fragments=filtered, graph=graph)

    def _dump_scores(
        self,
        scores_file: str,
        all_fragments: list[Fragment],
        core_ids: set[FragmentId],
        rel_scores: dict[FragmentId, float],
        graph: Graph,
        repo_root: Path,
    ) -> None:
        import json as _json

        from .filtering import (
            _cap_context_fragments,
            _filter_low_relevance_fragments,
            _filter_unrelated_fragments,
        )

        filtered_fragments = _filter_unrelated_fragments(all_fragments, core_ids, graph)
        post_unrelated_ids = {f.id for f in filtered_fragments}
        filtered_fragments = _filter_low_relevance_fragments(filtered_fragments, core_ids, rel_scores)
        post_lowrel_ids = {f.id for f in filtered_fragments}
        filtered_fragments = _cap_context_fragments(filtered_fragments, core_ids, rel_scores)
        post_cap_ids = {f.id for f in filtered_fragments}

        with open(scores_file, "w") as _sf:
            for f in all_fragments:
                if f.id in core_ids:
                    continue
                try:
                    rel_path = str(f.path.relative_to(repo_root))
                except ValueError:
                    rel_path = str(f.path)
                score = rel_scores.get(f.id, 0.0)
                if f.id not in post_unrelated_ids:
                    reason = "filtered_unrelated"
                elif f.id not in post_lowrel_ids:
                    reason = f"filtered_low_relevance (threshold={0.02 * max(1.0, f.token_count / 100) ** 0.5:.4f})"
                elif f.id not in post_cap_ids:
                    reason = "filtered_cap_per_file"
                else:
                    reason = "candidate_for_greedy"
                _sf.write(
                    _json.dumps(
                        {
                            "path": rel_path,
                            "lines": f"{f.start_line}-{f.end_line}",
                            "kind": f.kind,
                            "ppr_score": round(score, 6),
                            "token_count": f.token_count,
                            "status": reason,
                        }
                    )
                    + "\n"
                )


class EgoGraphScoring(ScoringStrategy):
    def __init__(self, max_depth: int = 2) -> None:
        self.max_depth = max_depth

    def score_and_filter(
        self,
        all_fragments: list[Fragment],
        core_ids: set[FragmentId],
        hunks: list[DiffHunk],
        repo_root: Path | None = None,
        seed_weights: dict[FragmentId, float] | None = None,
        dump_scores_file: str | None = None,
    ) -> ScoringResult:
        from .filtering import _cap_context_fragments
        from .graph import build_graph

        graph = build_graph(all_fragments, repo_root=repo_root)
        rel_scores = graph.ego_graph(core_ids, radius=self.max_depth)

        filtered = [f for f in all_fragments if f.id in core_ids or rel_scores.get(f.id, 0.0) > 0]
        filtered = _cap_context_fragments(filtered, core_ids, rel_scores)

        return ScoringResult(rel_scores=rel_scores, filtered_fragments=filtered, graph=graph)
