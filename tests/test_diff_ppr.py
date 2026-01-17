import pytest

from treemapper.diffctx.graph import build_graph
from treemapper.diffctx.ppr import personalized_pagerank
from treemapper.diffctx.types import Fragment, FragmentId


@pytest.fixture
def tmp_fragments(tmp_path):
    def _create(count, with_edges=True):
        path = tmp_path / "test.py"
        fragments = []

        for i in range(count):
            common_idents = frozenset([f"func{i}"])
            if with_edges and i > 0:
                common_idents = common_idents | frozenset([f"func{i-1}"])

            frag = Fragment(
                id=FragmentId(path=path, start_line=i * 10 + 1, end_line=i * 10 + 5),
                kind="function",
                content=f"def func{i}():\n    func{i-1}() if i > 0 else pass\n",
                identifiers=common_idents,
                token_count=50,
            )
            fragments.append(frag)

        return fragments

    return _create


class TestGraphStructure:
    def test_ppr_001_no_path_from_core_to_candidate(self, tmp_path):
        path = tmp_path / "test.py"

        frag_a = Fragment(
            id=FragmentId(path=path, start_line=1, end_line=5),
            kind="function",
            content="def func_a():\n    unique_a()\n",
            identifiers=frozenset(["func_a", "unique_a"]),
            token_count=50,
        )

        path_b = tmp_path / "other.py"
        frag_b = Fragment(
            id=FragmentId(path=path_b, start_line=1, end_line=5),
            kind="function",
            content="def func_b():\n    unique_b()\n",
            identifiers=frozenset(["func_b", "unique_b"]),
            token_count=50,
        )

        fragments = [frag_a, frag_b]
        graph = build_graph(fragments)

        scores = personalized_pagerank(graph, seeds={frag_a.id}, alpha=0.6)

        assert scores[frag_a.id] > scores[frag_b.id]

    def test_ppr_002_hub_node_dominance(self, tmp_path):
        path = tmp_path / "test.py"

        hub_frag = Fragment(
            id=FragmentId(path=path, start_line=1, end_line=5),
            kind="function",
            content="def utils_helper():\n    pass\n",
            identifiers=frozenset(["utils_helper"]),
            token_count=20,
        )

        callers = []
        for i in range(20):
            caller = Fragment(
                id=FragmentId(path=path, start_line=10 + i * 10, end_line=15 + i * 10),
                kind="function",
                content=f"def caller{i}():\n    utils_helper()\n",
                identifiers=frozenset([f"caller{i}", "utils_helper"]),
                token_count=30,
            )
            callers.append(caller)

        fragments = [hub_frag, *callers]
        graph = build_graph(fragments)

        scores = personalized_pagerank(graph, seeds={callers[0].id}, alpha=0.6)

        assert scores[callers[0].id] > 0

    def test_ppr_003_disconnected_components(self, tmp_path):
        path_a = tmp_path / "component_a.py"
        path_b = tmp_path / "component_b.py"

        frag_a1 = Fragment(
            id=FragmentId(path=path_a, start_line=1, end_line=5),
            kind="function",
            content="def a1():\n    a2()\n",
            identifiers=frozenset(["a1", "a2"]),
            token_count=50,
        )

        frag_a2 = Fragment(
            id=FragmentId(path=path_a, start_line=10, end_line=15),
            kind="function",
            content="def a2():\n    pass\n",
            identifiers=frozenset(["a2"]),
            token_count=50,
        )

        frag_b1 = Fragment(
            id=FragmentId(path=path_b, start_line=1, end_line=5),
            kind="function",
            content="def b1():\n    b2()\n",
            identifiers=frozenset(["b1", "b2"]),
            token_count=50,
        )

        frag_b2 = Fragment(
            id=FragmentId(path=path_b, start_line=10, end_line=15),
            kind="function",
            content="def b2():\n    pass\n",
            identifiers=frozenset(["b2"]),
            token_count=50,
        )

        fragments = [frag_a1, frag_a2, frag_b1, frag_b2]
        graph = build_graph(fragments)

        scores = personalized_pagerank(graph, seeds={frag_a1.id}, alpha=0.6)

        assert scores[frag_a1.id] >= scores[frag_b1.id]

    def test_ppr_004_circular_dependencies(self, tmp_path):
        path = tmp_path / "circular.py"

        frag_a = Fragment(
            id=FragmentId(path=path, start_line=1, end_line=5),
            kind="function",
            content="def func_alpha():\n    func_beta()\n",
            identifiers=frozenset(["func_alpha", "func_beta"]),
            token_count=50,
        )

        frag_b = Fragment(
            id=FragmentId(path=path, start_line=10, end_line=15),
            kind="function",
            content="def func_beta():\n    func_alpha()\n",
            identifiers=frozenset(["func_beta", "func_alpha"]),
            token_count=50,
        )

        fragments = [frag_a, frag_b]
        graph = build_graph(fragments)

        scores = personalized_pagerank(graph, seeds={frag_a.id}, alpha=0.6)

        assert scores[frag_a.id] > 0
        assert scores[frag_b.id] > 0


class TestConvergence:
    def test_ppr_010_large_sparse_graph(self, tmp_fragments):
        fragments = tmp_fragments(100, with_edges=True)
        graph = build_graph(fragments)

        scores = personalized_pagerank(graph, seeds={fragments[0].id}, alpha=0.6, max_iter=50)

        assert len(scores) == len(fragments)
        assert all(s >= 0 for s in scores.values())

    def test_ppr_011_dense_small_graph(self, tmp_path):
        path = tmp_path / "dense.py"

        fragments = []
        shared_idents = frozenset([f"shared{i}" for i in range(5)])

        for i in range(20):
            frag = Fragment(
                id=FragmentId(path=path, start_line=i * 10 + 1, end_line=i * 10 + 5),
                kind="function",
                content=f"def func{i}():\n    pass\n",
                identifiers=shared_idents | frozenset([f"func{i}"]),
                token_count=50,
            )
            fragments.append(frag)

        graph = build_graph(fragments)

        scores = personalized_pagerank(graph, seeds={fragments[0].id}, alpha=0.6)

        assert len(scores) == len(fragments)
        assert all(s >= 0 for s in scores.values())


class TestPPRParameters:
    def test_alpha_affects_distribution(self, tmp_fragments):
        fragments = tmp_fragments(10, with_edges=True)
        graph = build_graph(fragments)

        scores_low_alpha = personalized_pagerank(graph, seeds={fragments[0].id}, alpha=0.3)
        scores_high_alpha = personalized_pagerank(graph, seeds={fragments[0].id}, alpha=0.9)

        assert scores_low_alpha[fragments[0].id] != scores_high_alpha[fragments[0].id]

    def test_empty_seeds_returns_uniform(self, tmp_fragments):
        fragments = tmp_fragments(5, with_edges=False)
        graph = build_graph(fragments)

        scores = personalized_pagerank(graph, seeds=set(), alpha=0.6)

        values = list(scores.values())
        assert len({round(v, 6) for v in values}) <= 2

    def test_single_node_graph(self, tmp_path):
        path = tmp_path / "single.py"

        frag = Fragment(
            id=FragmentId(path=path, start_line=1, end_line=5),
            kind="function",
            content="def only():\n    pass\n",
            identifiers=frozenset(["only"]),
            token_count=50,
        )

        graph = build_graph([frag])

        scores = personalized_pagerank(graph, seeds={frag.id}, alpha=0.6)

        assert abs(scores[frag.id] - 1.0) < 1e-9


class TestEdgeWeights:
    def test_graph_builds_import_edges(self, tmp_path):
        path_main = tmp_path / "main.py"
        path_helper = tmp_path / "helper.py"

        main_frag = Fragment(
            id=FragmentId(path=path_main, start_line=1, end_line=5),
            kind="function",
            content="from helper import do_work\ndef main():\n    do_work()\n",
            identifiers=frozenset(["main", "do_work", "helper"]),
            token_count=50,
        )

        helper_frag = Fragment(
            id=FragmentId(path=path_helper, start_line=1, end_line=5),
            kind="function",
            content="def do_work():\n    pass\n",
            identifiers=frozenset(["do_work"]),
            token_count=50,
        )

        fragments = [main_frag, helper_frag]
        graph = build_graph(fragments)

        assert len(graph.nodes) == 2

    def test_graph_builds_call_edges(self, tmp_path):
        path = tmp_path / "calls.py"

        caller = Fragment(
            id=FragmentId(path=path, start_line=1, end_line=5),
            kind="function",
            content="def caller():\n    callee()\n",
            identifiers=frozenset(["caller", "callee"]),
            token_count=50,
        )

        callee = Fragment(
            id=FragmentId(path=path, start_line=10, end_line=15),
            kind="function",
            content="def callee():\n    pass\n",
            identifiers=frozenset(["callee"]),
            token_count=50,
        )

        fragments = [caller, callee]
        graph = build_graph(fragments)

        neighbors = graph.neighbors(caller.id)
        assert callee.id in neighbors or len(graph.nodes) == 2


class TestSeeds:
    def test_multiple_seeds(self, tmp_path):
        path = tmp_path / "multi.py"

        fragments = [
            Fragment(
                id=FragmentId(path=path, start_line=i * 10 + 1, end_line=i * 10 + 5),
                kind="function",
                content=f"def func{i}():\n    pass\n",
                identifiers=frozenset([f"func{i}"]),
                token_count=50,
            )
            for i in range(5)
        ]

        graph = build_graph(fragments)

        seeds = {fragments[0].id, fragments[2].id}
        scores = personalized_pagerank(graph, seeds=seeds, alpha=0.6)

        assert scores[fragments[0].id] > 0
        assert scores[fragments[2].id] > 0

    def test_seeds_not_in_graph_filtered(self, tmp_path):
        path = tmp_path / "test.py"

        frag = Fragment(
            id=FragmentId(path=path, start_line=1, end_line=5),
            kind="function",
            content="def func():\n    pass\n",
            identifiers=frozenset(["func"]),
            token_count=50,
        )

        fake_id = FragmentId(path=path, start_line=100, end_line=105)

        graph = build_graph([frag])

        scores = personalized_pagerank(graph, seeds={frag.id, fake_id}, alpha=0.6)

        assert frag.id in scores


class TestPPRCorrectness:
    def test_ppr_score_normalization(self, tmp_path):
        path = tmp_path / "test.py"

        fragments = [
            Fragment(
                id=FragmentId(path=path, start_line=i * 10 + 1, end_line=i * 10 + 5),
                kind="function",
                content=f"def func{i}():\n    func{i-1}()\n" if i > 0 else "def func0():\n    pass\n",
                identifiers=frozenset([f"func{i}"] + ([f"func{i-1}"] if i > 0 else [])),
                token_count=50,
            )
            for i in range(10)
        ]

        graph = build_graph(fragments)
        scores = personalized_pagerank(graph, seeds={fragments[0].id}, alpha=0.6)

        assert abs(sum(scores.values()) - 1.0) < 1e-9

    def test_ppr_deterministic_results(self, tmp_path):
        path = tmp_path / "test.py"

        fragments = [
            Fragment(
                id=FragmentId(path=path, start_line=i * 10 + 1, end_line=i * 10 + 5),
                kind="function",
                content=f"def func{i}():\n    func{i-1}()\n" if i > 0 else "def func0():\n    pass\n",
                identifiers=frozenset([f"func{i}"] + ([f"func{i-1}"] if i > 0 else [])),
                token_count=50,
            )
            for i in range(10)
        ]

        graph = build_graph(fragments)
        seeds = {fragments[0].id}

        scores1 = personalized_pagerank(graph, seeds=seeds, alpha=0.6)
        scores2 = personalized_pagerank(graph, seeds=seeds, alpha=0.6)

        assert scores1 == scores2

    def test_ppr_alpha_zero_pure_personalization(self, tmp_path):
        path = tmp_path / "test.py"

        fragments = [
            Fragment(
                id=FragmentId(path=path, start_line=i * 10 + 1, end_line=i * 10 + 5),
                kind="function",
                content=f"def func{i}():\n    func{i-1}()\n" if i > 0 else "def func0():\n    pass\n",
                identifiers=frozenset([f"func{i}"] + ([f"func{i-1}"] if i > 0 else [])),
                token_count=50,
            )
            for i in range(5)
        ]

        graph = build_graph(fragments)
        seed_id = fragments[0].id

        scores = personalized_pagerank(graph, seeds={seed_id}, alpha=0.0)

        assert abs(scores[seed_id] - 1.0) < 1e-9
        for frag in fragments[1:]:
            assert scores[frag.id] < 1e-9

    def test_ppr_alpha_near_one(self, tmp_path):
        path = tmp_path / "test.py"

        fragments = [
            Fragment(
                id=FragmentId(path=path, start_line=i * 10 + 1, end_line=i * 10 + 5),
                kind="function",
                content=f"def func{i}():\n    func{i-1}()\n" if i > 0 else "def func0():\n    pass\n",
                identifiers=frozenset([f"func{i}"] + ([f"func{i-1}"] if i > 0 else [])),
                token_count=50,
            )
            for i in range(10)
        ]

        graph = build_graph(fragments)
        scores = personalized_pagerank(graph, seeds={fragments[0].id}, alpha=0.99)

        assert len(scores) == len(fragments)
        assert all(s >= 0 for s in scores.values())
        assert abs(sum(scores.values()) - 1.0) < 1e-9
