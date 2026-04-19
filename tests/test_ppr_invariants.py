from __future__ import annotations

import math
from pathlib import Path

from hypothesis import given, settings
from hypothesis import strategies as st

from treemapper.diffctx.graph import Graph
from treemapper.diffctx.ppr import personalized_pagerank
from treemapper.diffctx.types import FragmentId


def _fid(name: str) -> FragmentId:
    return FragmentId(Path(name), 0, 0)


class TestPPRMathematicalInvariants:
    def _create_graph_with_edges(self, edges: list[tuple[str, str, float]]) -> Graph:
        graph = Graph()
        nodes = set()
        for src, dst, _ in edges:
            nodes.add(src)
            nodes.add(dst)
        for node in nodes:
            graph.add_node(_fid(node))
        for src, dst, weight in edges:
            graph.add_edge(_fid(src), _fid(dst), weight)
        return graph

    def test_ppr_normalization_sum_to_one(self) -> None:
        graph = self._create_graph_with_edges(
            [
                ("a", "b", 0.5),
                ("b", "c", 0.5),
                ("c", "a", 0.5),
                ("a", "d", 0.3),
            ]
        )
        seeds = {_fid("a")}
        scores = personalized_pagerank(graph, seeds, alpha=0.6)
        total = sum(scores.values())
        assert abs(total - 1.0) < 1e-6, f"PPR scores sum to {total}, expected 1.0"

    def test_ppr_all_scores_non_negative(self) -> None:
        graph = self._create_graph_with_edges(
            [
                ("a", "b", 0.9),
                ("b", "c", 0.8),
                ("c", "d", 0.7),
                ("d", "a", 0.6),
            ]
        )
        seeds = {_fid("a"), _fid("c")}
        scores = personalized_pagerank(graph, seeds, alpha=0.5)
        for node, score in scores.items():
            assert score >= 0, f"Node {node} has negative score: {score}"

    def test_ppr_seeds_have_higher_scores_than_average(self) -> None:
        graph = self._create_graph_with_edges(
            [
                ("seed1", "other1", 0.5),
                ("seed1", "other2", 0.5),
                ("seed2", "other3", 0.5),
                ("other1", "other4", 0.3),
                ("other2", "other5", 0.3),
            ]
        )
        seeds = {_fid("seed1"), _fid("seed2")}
        scores = personalized_pagerank(graph, seeds, alpha=0.6)
        seed_scores = [scores[s] for s in seeds if s in scores]
        non_seed_scores = [scores[n] for n in scores if n not in seeds]
        if seed_scores and non_seed_scores:
            avg_seed = sum(seed_scores) / len(seed_scores)
            avg_non_seed = sum(non_seed_scores) / len(non_seed_scores)
            assert avg_seed > avg_non_seed, f"Seeds avg ({avg_seed:.4f}) should be > non-seeds avg ({avg_non_seed:.4f})"

    def test_ppr_deterministic_output(self) -> None:
        graph = self._create_graph_with_edges(
            [
                ("a", "b", 0.5),
                ("b", "c", 0.5),
                ("c", "a", 0.5),
            ]
        )
        seeds = {_fid("a")}
        scores1 = personalized_pagerank(graph, seeds, alpha=0.6)
        scores2 = personalized_pagerank(graph, seeds, alpha=0.6)
        for node in scores1:
            assert (
                abs(scores1[node] - scores2[node]) < 1e-10
            ), f"Non-deterministic PPR: {node} has {scores1[node]} vs {scores2[node]}"

    def test_ppr_empty_graph_returns_empty(self) -> None:
        graph = Graph()
        scores = personalized_pagerank(graph, {_fid("a")}, alpha=0.6)
        assert scores == {}

    def test_ppr_invalid_seeds_return_valid_distribution(self) -> None:
        graph = self._create_graph_with_edges([("a", "b", 0.5)])
        scores = personalized_pagerank(graph, {_fid("nonexistent")}, alpha=0.6)
        if scores:
            assert abs(sum(scores.values()) - 1.0) < 1e-6, "Scores must sum to 1.0"
            for node, score in scores.items():
                assert score >= 0, f"Node {node} has negative score: {score}"

    def test_ppr_single_node_graph(self) -> None:
        graph = Graph()
        solo = _fid("solo")
        graph.add_node(solo)
        scores = personalized_pagerank(graph, {solo}, alpha=0.6)
        assert solo in scores
        assert abs(scores[solo] - 1.0) < 1e-6

    def test_ppr_disconnected_components(self) -> None:
        graph = self._create_graph_with_edges(
            [
                ("a1", "a2", 0.5),
                ("a2", "a1", 0.5),
                ("b1", "b2", 0.5),
                ("b2", "b1", 0.5),
            ]
        )
        seeds = {_fid("a1")}
        scores = personalized_pagerank(graph, seeds, alpha=0.6)
        assert abs(sum(scores.values()) - 1.0) < 1e-6

    @given(
        num_nodes=st.integers(min_value=2, max_value=20),
        num_edges=st.integers(min_value=1, max_value=50),
        alpha=st.floats(min_value=0.1, max_value=0.9, allow_nan=False, allow_infinity=False),
    )
    @settings(max_examples=50, deadline=None)
    def test_ppr_invariants_property_based(self, num_nodes: int, num_edges: int, alpha: float) -> None:
        node_ids = [_fid(f"node_{i}") for i in range(num_nodes)]
        graph = Graph()
        for node in node_ids:
            graph.add_node(node)
        for i in range(num_edges):
            src = node_ids[i % num_nodes]
            dst = node_ids[(i * 7 + 3) % num_nodes]
            if src != dst:
                weight = 0.1 + (i % 10) * 0.08
                graph.add_edge(src, dst, weight)
        seeds = {node_ids[0]}
        scores = personalized_pagerank(graph, seeds, alpha=alpha)
        if scores:
            total = sum(scores.values())
            assert abs(total - 1.0) < 1e-5, f"Sum={total}, expected 1.0"
            for node, score in scores.items():
                assert score >= 0, f"Negative score for {node}: {score}"
                assert math.isfinite(score), f"Non-finite score for {node}: {score}"

    def test_ppr_rank_decays_with_graph_distance(self) -> None:
        graph = self._create_graph_with_edges(
            [
                ("A", "B", 1.0),
                ("B", "C", 1.0),
                ("C", "D", 1.0),
            ]
        )
        seeds = {_fid("A")}
        scores = personalized_pagerank(graph, seeds)

        score_a = scores.get(_fid("A"), 0.0)
        score_b = scores.get(_fid("B"), 0.0)
        score_c = scores.get(_fid("C"), 0.0)
        score_d = scores.get(_fid("D"), 0.0)

        assert score_a > score_b, f"Seed {score_a:.4f} must exceed hop-1 {score_b:.4f}"
        assert score_b > score_c, f"Hop-1 {score_b:.4f} must exceed hop-2 {score_c:.4f}"
        assert score_c > score_d, f"Hop-2 {score_c:.4f} must exceed hop-3 {score_d:.4f}"
