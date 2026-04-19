from __future__ import annotations

import math
from pathlib import Path
from typing import Any

from tests.framework.pygit2_backend import Pygit2Repo
from treemapper.diffctx import build_diff_context
from treemapper.diffctx.graph import Graph
from treemapper.diffctx.ppr import personalized_pagerank
from treemapper.diffctx.types import FragmentId


def _fid(name: str) -> FragmentId:
    return FragmentId(Path(name), 0, 0)


def _extract_content(context: dict[str, Any]) -> str:
    parts = []
    for frag in context.get("fragments", []):
        if "content" in frag:
            parts.append(frag["content"])
        if "path" in frag:
            parts.append(frag["path"])
    return "\n".join(parts)


class TestGraphBuildingIntegrity:
    def test_graph_self_loops_dont_break_ppr(self) -> None:
        a, b = _fid("a"), _fid("b")
        graph = Graph()
        graph.add_node(a)
        graph.add_node(b)
        graph.add_edge(a, a, 0.5)
        graph.add_edge(a, b, 0.5)
        scores = personalized_pagerank(graph, {a}, alpha=0.6)
        assert abs(sum(scores.values()) - 1.0) < 1e-6, "Self-loops should not break normalization"
        assert all(s >= 0 for s in scores.values()), "Self-loops should not produce negative scores"

    def test_graph_infinite_weight_edge_filtered(self) -> None:
        a, b = _fid("a"), _fid("b")
        graph = Graph()
        graph.add_node(a)
        graph.add_node(b)
        graph.add_edge(a, b, float("inf"))
        neighbors = graph.neighbors(a)
        for weight in neighbors.values():
            assert math.isfinite(weight), f"Infinite weight should be filtered, got {weight}"

    def test_graph_nan_weight_edge_filtered(self) -> None:
        a, b = _fid("a"), _fid("b")
        graph = Graph()
        graph.add_node(a)
        graph.add_node(b)
        graph.add_edge(a, b, float("nan"))
        neighbors = graph.neighbors(a)
        for weight in neighbors.values():
            assert math.isfinite(weight), f"NaN weight should be filtered, got {weight}"

    def test_graph_zero_weight_edge_filtered(self) -> None:
        a, b = _fid("a"), _fid("b")
        graph = Graph()
        graph.add_node(a)
        graph.add_node(b)
        graph.add_edge(a, b, 0.0)
        neighbors = graph.neighbors(a)
        assert b not in neighbors, "Zero-weight edge should be filtered"

    def test_graph_negative_weight_edge_filtered(self) -> None:
        a, b = _fid("a"), _fid("b")
        graph = Graph()
        graph.add_node(a)
        graph.add_node(b)
        graph.add_edge(a, b, -0.5)
        neighbors = graph.neighbors(a)
        assert b not in neighbors, "Negative-weight edge should be filtered"

    def test_hub_node_edges_are_dampened(self) -> None:
        import math as _math

        from treemapper.diffctx.graph import _apply_hub_suppression

        hub = _fid("hub")
        non_hub = _fid("non_hub")
        sources = [_fid(f"src_{i}") for i in range(8)]

        raw_weight = 0.9
        edges: dict[tuple[FragmentId, FragmentId], float] = {}
        for src in sources:
            edges[(src, hub)] = raw_weight
        edges[(sources[0], non_hub)] = raw_weight

        categories: dict[tuple[FragmentId, FragmentId], str] = dict.fromkeys(edges, "generic")

        suppressed = _apply_hub_suppression(edges, categories)

        hub_weight = suppressed[(sources[0], hub)]
        non_hub_weight = suppressed[(sources[0], non_hub)]

        assert hub_weight < non_hub_weight, (
            f"Hub node edge weight ({hub_weight:.4f}) should be less than "
            f"non-hub edge weight ({non_hub_weight:.4f}) after dampening"
        )

        hub_in_degree = 8
        expected_dampened = raw_weight / _math.log(1 + hub_in_degree)
        assert (
            abs(hub_weight - expected_dampened) < 1e-9
        ), f"Hub weight {hub_weight:.6f} does not match expected dampened value {expected_dampened:.6f}"


class TestCICDSeparatorAwareMatching:
    def test_cicd_does_not_create_spurious_edges_from_greedy_prefix(self, tmp_path: Path) -> None:
        g = Pygit2Repo(tmp_path / "cicd_repo")

        g.add_file(
            ".github/workflows/ci.yml",
            "name: CI\non:\n  push:\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - run: python test.py\n",
        )
        g.add_file("test.py", "def run_tests(): pass\n")
        g.add_file("testing_utils.py", "TESTING_UTILS_MARKER_XYZZY = True\ndef utility(): pass\n")
        g.add_file("app.py", "APP_MARKER_QWERTY = True\ndef main(): pass\n")
        g.commit("init")

        g.add_file(
            ".github/workflows/ci.yml",
            "name: CI\non:\n  push:\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - run: python test.py --verbose\n",
        )
        g.commit("update ci")

        context = build_diff_context(
            root_dir=g.path,
            diff_range="HEAD~1..HEAD",
            budget_tokens=5000,
        )

        all_paths = {frag["path"] for frag in context.get("fragments", []) if "path" in frag}
        all_content = "\n".join(frag.get("content", "") + frag.get("path", "") for frag in context.get("fragments", []))

        assert any(
            "test.py" == Path(p).name for p in all_paths
        ), "test.py should be in context — exact match for CI/CD script reference"
        assert (
            "TESTING_UTILS_MARKER_XYZZY" not in all_content
        ), "testing_utils.py should NOT be in context — 'test' prefix without separator must not match"
        assert "APP_MARKER_QWERTY" not in all_content, "app.py should NOT be in context — completely unrelated file"


class TestJVMInheritanceEdges:
    def test_kotlin_inheritance_pulls_derived_class(self, tmp_path: Path) -> None:
        g = Pygit2Repo(tmp_path / "kotlin_repo")

        g.add_file("Base.kt", 'open class BaseService {\n    fun serve() = "base"\n}\n')
        g.add_file("Derived.kt", 'class DerivedService : BaseService {\n    override fun serve() = "derived"\n}\n')
        g.commit("init")

        g.add_file("Base.kt", 'abstract class BaseService {\n    fun serve() = "base_v2"\n}\n')
        g.commit("change base")

        context = build_diff_context(
            root_dir=g.path,
            diff_range="HEAD~1..HEAD",
            budget_tokens=50000,
        )
        all_content = _extract_content(context)
        assert "BaseService" in all_content, "Changed base class must be included"
        assert "DerivedService" in all_content, "Kotlin derived class using ':' inheritance must appear in context"

    def test_scala_extends_with_pulls_mixing_trait(self, tmp_path: Path) -> None:
        g = Pygit2Repo(tmp_path / "scala_repo")

        g.add_file("Base.scala", "trait Loggable {\n  def log(msg: String): Unit\n}\n")
        g.add_file("Service.scala", 'class Service extends Serializable with Loggable {\n  def run() = "running"\n}\n')
        g.commit("init")

        g.add_file("Base.scala", "sealed trait Loggable {\n  def log(msg: String): Unit\n  def debug(msg: String): Unit\n}\n")
        g.commit("change trait")

        context = build_diff_context(
            root_dir=g.path,
            diff_range="HEAD~1..HEAD",
            budget_tokens=50000,
        )
        all_content = _extract_content(context)
        assert "Loggable" in all_content, "Changed trait must be included"
        assert "Service" in all_content, "Scala class mixing in changed trait via 'with' must appear in context"

    def test_java_extends_implements_still_works(self, tmp_path: Path) -> None:
        g = Pygit2Repo(tmp_path / "java_repo")

        g.add_file("Animal.java", "public abstract class Animal {\n    public abstract String speak();\n}\n")
        g.add_file("Runnable.java", "public interface Runnable {\n    void run();\n}\n")
        g.add_file(
            "Dog.java",
            "public class Dog extends Animal implements Runnable {\n"
            '    public String speak() { return "woof"; }\n'
            "    public void run() { }\n"
            "}\n",
        )
        g.commit("init")

        g.add_file(
            "Animal.java",
            "public abstract class Animal {\n"
            "    public abstract String speak();\n"
            '    public String type() { return "animal"; }\n'
            "}\n",
        )
        g.commit("add method to base")

        context = build_diff_context(
            root_dir=g.path,
            diff_range="HEAD~1..HEAD",
            budget_tokens=50000,
        )
        all_content = _extract_content(context)
        assert "Animal" in all_content, "Changed abstract class must be included"
        assert "Dog" in all_content, "Java subclass using 'extends' must appear in context after refactor"
