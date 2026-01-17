from __future__ import annotations

from pathlib import Path

import pytest

from treemapper.diffctx.graph import Graph, build_graph
from treemapper.diffctx.ppr import personalized_pagerank
from treemapper.diffctx.types import Fragment, FragmentId


def _make_fragment(
    path: str,
    start: int,
    end: int,
    kind: str = "function",
    content: str = "",
    identifiers: frozenset[str] | None = None,
) -> Fragment:
    return Fragment(
        id=FragmentId(Path(path), start, end),
        kind=kind,
        content=content or f"content {start}-{end}",
        identifiers=identifiers or frozenset(),
    )


def _make_simple_graph(nodes: list[FragmentId], edges: dict[tuple[FragmentId, FragmentId], float]) -> Graph:
    graph = Graph()
    for node in nodes:
        graph.add_node(node)
    for (src, dst), weight in edges.items():
        graph.add_edge(src, dst, weight)
    return graph


class TestPPRConvergence:
    def test_ppr_conv_001_converges_within_tolerance(self):
        frags = [
            _make_fragment("a.py", 1, 10, identifiers=frozenset(["func_a"])),
            _make_fragment("b.py", 1, 10, identifiers=frozenset(["func_b", "func_a"])),
            _make_fragment("c.py", 1, 10, identifiers=frozenset(["func_c", "func_b"])),
        ]

        graph = build_graph(frags)
        seeds = {frags[0].id}

        scores = personalized_pagerank(graph, seeds, alpha=0.6, tol=1e-4, max_iter=50)

        assert len(scores) == len(frags)
        for score in scores.values():
            assert 0 <= score <= 1

    def test_ppr_conv_002_max_iterations_respected(self):
        frags = [_make_fragment(f"mod{i}.py", 1, 10, identifiers=frozenset([f"func{i}"])) for i in range(10)]

        graph = build_graph(frags)
        seeds = {frags[0].id}

        scores = personalized_pagerank(graph, seeds, alpha=0.99, tol=1e-20, max_iter=5)

        assert len(scores) == len(frags)

    def test_ppr_conv_003_tight_tolerance(self):
        frags = [
            _make_fragment("a.py", 1, 10, identifiers=frozenset(["shared"])),
            _make_fragment("b.py", 1, 10, identifiers=frozenset(["shared"])),
        ]

        graph = build_graph(frags)
        seeds = {frags[0].id}

        scores = personalized_pagerank(graph, seeds, alpha=0.6, tol=1e-10, max_iter=100)

        assert len(scores) == len(frags)
        total = sum(scores.values())
        assert abs(total - 1.0) < 1e-6


class TestPPRSeeds:
    def test_ppr_seeds_001_valid_seeds_get_high_scores(self):
        frags = [
            _make_fragment("seed.py", 1, 10, identifiers=frozenset(["seed_func"])),
            _make_fragment("other.py", 1, 10, identifiers=frozenset(["other_func"])),
        ]

        graph = build_graph(frags)
        seeds = {frags[0].id}

        scores = personalized_pagerank(graph, seeds, alpha=0.6)

        assert scores[frags[0].id] >= scores.get(frags[1].id, 0)

    def test_ppr_seeds_002_invalid_seeds_filtered(self):
        frags = [
            _make_fragment("a.py", 1, 10, identifiers=frozenset(["func_a"])),
        ]

        graph = build_graph(frags)
        invalid_seed = FragmentId(Path("nonexistent.py"), 1, 10)
        seeds = {invalid_seed}

        scores = personalized_pagerank(graph, seeds, alpha=0.6)

        assert len(scores) == len(frags)
        for score in scores.values():
            assert score >= 0

    def test_ppr_seeds_003_multiple_seeds(self):
        frags = [
            _make_fragment("a.py", 1, 10, identifiers=frozenset(["func_a"])),
            _make_fragment("b.py", 1, 10, identifiers=frozenset(["func_b"])),
            _make_fragment("c.py", 1, 10, identifiers=frozenset(["func_c"])),
        ]

        graph = build_graph(frags)
        seeds = {frags[0].id, frags[1].id}

        scores = personalized_pagerank(graph, seeds, alpha=0.6)

        assert scores[frags[0].id] > 0
        assert scores[frags[1].id] > 0

    def test_ppr_seeds_004_empty_seeds_uniform(self):
        frags = [
            _make_fragment("a.py", 1, 10, identifiers=frozenset(["func_a"])),
            _make_fragment("b.py", 1, 10, identifiers=frozenset(["func_b"])),
        ]

        graph = build_graph(frags)
        seeds = set()

        scores = personalized_pagerank(graph, seeds, alpha=0.6)

        assert len(scores) == len(frags)
        for score in scores.values():
            assert abs(score - 1.0 / len(frags)) < 0.1


class TestPPREmptyGraph:
    def test_ppr_empty_001_empty_graph_returns_empty(self):
        graph = Graph()
        seeds = set()

        scores = personalized_pagerank(graph, seeds, alpha=0.6)

        assert scores == {}

    def test_ppr_empty_002_single_node_graph(self):
        frag = _make_fragment("single.py", 1, 10)
        graph = Graph()
        graph.add_node(frag.id)

        scores = personalized_pagerank(graph, {frag.id}, alpha=0.6)

        assert len(scores) == 1
        assert scores[frag.id] == pytest.approx(1.0, rel=1e-6)


class TestPPRDanglingNodes:
    def test_ppr_dangling_001_no_outgoing_edges(self):
        frag_hub = _make_fragment("hub.py", 1, 10, identifiers=frozenset(["hub"]))
        frag_leaf1 = _make_fragment("leaf1.py", 1, 10, identifiers=frozenset(["hub"]))
        frag_leaf2 = _make_fragment("leaf2.py", 1, 10, identifiers=frozenset(["hub"]))

        graph = Graph()
        graph.add_node(frag_hub.id)
        graph.add_node(frag_leaf1.id)
        graph.add_node(frag_leaf2.id)
        graph.add_edge(frag_hub.id, frag_leaf1.id, 1.0)
        graph.add_edge(frag_hub.id, frag_leaf2.id, 1.0)

        scores = personalized_pagerank(graph, {frag_hub.id}, alpha=0.6)

        assert len(scores) == 3
        total = sum(scores.values())
        assert abs(total - 1.0) < 1e-6

    def test_ppr_dangling_002_all_dangling(self):
        frags = [_make_fragment(f"isolated{i}.py", 1, 10) for i in range(3)]

        graph = Graph()
        for frag in frags:
            graph.add_node(frag.id)

        scores = personalized_pagerank(graph, {frags[0].id}, alpha=0.6)

        assert len(scores) == 3
        total = sum(scores.values())
        assert abs(total - 1.0) < 1e-6


class TestPPRAlphaParameter:
    def test_ppr_alpha_001_low_alpha_stays_near_seeds(self):
        frags = [
            _make_fragment("seed.py", 1, 10, identifiers=frozenset(["common"])),
            _make_fragment("far.py", 1, 10, identifiers=frozenset(["common"])),
        ]

        graph = build_graph(frags)
        seeds = {frags[0].id}

        scores_low = personalized_pagerank(graph, seeds, alpha=0.1)
        scores_high = personalized_pagerank(graph, seeds, alpha=0.9)

        seed_score_low = scores_low[frags[0].id]
        seed_score_high = scores_high[frags[0].id]

        assert seed_score_low >= seed_score_high * 0.5

    def test_ppr_alpha_002_alpha_zero_pure_personalization(self):
        frags = [
            _make_fragment("seed.py", 1, 10, identifiers=frozenset(["shared"])),
            _make_fragment("other.py", 1, 10, identifiers=frozenset(["shared"])),
        ]

        graph = build_graph(frags)
        seeds = {frags[0].id}

        scores = personalized_pagerank(graph, seeds, alpha=0.0)

        assert scores[frags[0].id] == pytest.approx(1.0, rel=1e-6)


class TestPPRNormalization:
    def test_ppr_norm_001_scores_sum_to_one(self):
        frags = [_make_fragment(f"mod{i}.py", 1, 10, identifiers=frozenset([f"func{i}"])) for i in range(5)]

        graph = build_graph(frags)
        seeds = {frags[0].id, frags[2].id}

        scores = personalized_pagerank(graph, seeds, alpha=0.6)

        total = sum(scores.values())
        assert abs(total - 1.0) < 1e-6

    def test_ppr_norm_002_all_scores_non_negative(self):
        frags = [_make_fragment(f"mod{i}.py", 1, 10, identifiers=frozenset([f"func{i % 3}"])) for i in range(10)]

        graph = build_graph(frags)
        seeds = {frags[0].id}

        scores = personalized_pagerank(graph, seeds, alpha=0.6)

        for score in scores.values():
            assert score >= 0


class TestPPRDeterminism:
    def test_ppr_det_001_same_input_same_output(self):
        frags = [
            _make_fragment("a.py", 1, 10, identifiers=frozenset(["shared", "func_a"])),
            _make_fragment("b.py", 1, 10, identifiers=frozenset(["shared", "func_b"])),
            _make_fragment("c.py", 1, 10, identifiers=frozenset(["func_c"])),
        ]

        graph = build_graph(frags)
        seeds = {frags[0].id}

        results = []
        for _ in range(5):
            scores = personalized_pagerank(graph, seeds, alpha=0.6, tol=1e-6)
            results.append(tuple(sorted(scores.items())))

        for result in results[1:]:
            assert result == results[0]


class TestPPRCircularDependencies:
    def test_ppr_circular_001_cycle_handled(self):
        frag_a = _make_fragment("a.py", 1, 10, identifiers=frozenset(["func_b"]))
        frag_b = _make_fragment("b.py", 1, 10, identifiers=frozenset(["func_c"]))
        frag_c = _make_fragment("c.py", 1, 10, identifiers=frozenset(["func_a"]))

        graph = Graph()
        graph.add_node(frag_a.id)
        graph.add_node(frag_b.id)
        graph.add_node(frag_c.id)
        graph.add_edge(frag_a.id, frag_b.id, 0.5)
        graph.add_edge(frag_b.id, frag_c.id, 0.5)
        graph.add_edge(frag_c.id, frag_a.id, 0.5)

        scores = personalized_pagerank(graph, {frag_a.id}, alpha=0.6)

        assert len(scores) == 3
        total = sum(scores.values())
        assert abs(total - 1.0) < 1e-6


class TestPPRDisconnectedComponents:
    def test_ppr_disconnect_001_separate_components(self):
        comp1 = [
            _make_fragment("comp1_a.py", 1, 10, identifiers=frozenset(["shared1"])),
            _make_fragment("comp1_b.py", 1, 10, identifiers=frozenset(["shared1"])),
        ]
        comp2 = [
            _make_fragment("comp2_a.py", 1, 10, identifiers=frozenset(["shared2"])),
            _make_fragment("comp2_b.py", 1, 10, identifiers=frozenset(["shared2"])),
        ]

        all_frags = comp1 + comp2
        graph = build_graph(all_frags)
        seeds = {comp1[0].id}

        scores = personalized_pagerank(graph, seeds, alpha=0.6)

        assert len(scores) == len(all_frags)
        comp1_total = sum(scores.get(f.id, 0) for f in comp1)
        comp2_total = sum(scores.get(f.id, 0) for f in comp2)
        assert comp1_total >= comp2_total


class TestPPRWeightedEdges:
    def test_ppr_weight_001_higher_weight_more_flow(self):
        frag_src = _make_fragment("src.py", 1, 10)
        frag_high = _make_fragment("high.py", 1, 10)
        frag_low = _make_fragment("low.py", 1, 10)

        graph = Graph()
        graph.add_node(frag_src.id)
        graph.add_node(frag_high.id)
        graph.add_node(frag_low.id)
        graph.add_edge(frag_src.id, frag_high.id, 0.9)
        graph.add_edge(frag_src.id, frag_low.id, 0.1)

        scores = personalized_pagerank(graph, {frag_src.id}, alpha=0.8)

        assert scores[frag_high.id] > scores[frag_low.id]


class TestPPRLargeGraph:
    def test_ppr_large_001_scalability(self):
        n = 100
        frags = [_make_fragment(f"mod{i}.py", 1, 10, identifiers=frozenset([f"func{i % 10}"])) for i in range(n)]

        graph = build_graph(frags)
        seeds = {frags[0].id, frags[50].id}

        scores = personalized_pagerank(graph, seeds, alpha=0.6, max_iter=50)

        assert len(scores) == n
        total = sum(scores.values())
        assert abs(total - 1.0) < 1e-5
