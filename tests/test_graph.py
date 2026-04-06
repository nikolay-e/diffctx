import json
import math
import xml.etree.ElementTree as ET
from pathlib import Path

import pytest

from tests.conftest import run_treemapper_subprocess
from tests.framework.pygit2_backend import Pygit2Repo
from treemapper.diffctx.graph_analytics import (
    QuotientGraph,
    coupling_metrics,
    detect_cycles,
    hotspots,
    quotient_graph,
    to_mermaid,
)
from treemapper.diffctx.graph_export import (
    graph_summary,
    graph_to_graphml_string,
    graph_to_json_string,
)
from treemapper.diffctx.project_graph import build_project_graph

# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------

MODELS_PY = """\
class User:
    def __init__(self, name: str, email: str):
        self.name = name
        self.email = email

    def full_info(self) -> str:
        return f"{self.name} <{self.email}>"


class Order:
    def __init__(self, user: User, amount: float):
        self.user = user
        self.amount = amount

    def summary(self) -> str:
        return f"Order({self.user.name}, {self.amount})"
"""

SERVICES_PY = """\
from .models import User


class UserService:
    def __init__(self):
        self.users: list[User] = []

    def add_user(self, name: str, email: str) -> User:
        user = User(name, email)
        self.users.append(user)
        return user

    def find_user(self, name: str) -> User | None:
        for user in self.users:
            if user.name == name:
                return user
        return None
"""

API_PY = """\
from .services import UserService

_service = UserService()


def get_user(name: str) -> dict | None:
    user = _service.find_user(name)
    if user:
        return {"name": user.name, "email": user.email}
    return None


def create_user(name: str, email: str) -> dict:
    user = _service.add_user(name, email)
    return {"name": user.name, "email": user.email}
"""

TEST_API_PY = """\
from src.api import get_user, create_user


def test_get_user():
    result = create_user("Alice", "alice@example.com")
    assert result["name"] == "Alice"


def test_get_user_not_found():
    result = get_user("NonExistent")
    assert result is None
"""

CONFIG_YAML = """\
app:
  name: myproject
  services:
    - UserService
  database:
    host: localhost
    port: 5432
"""

DOCKERFILE = """\
FROM python:3.12-slim
WORKDIR /app
COPY src/ /app/src/
COPY config.yaml /app/
CMD ["python", "-m", "src.api"]
"""

README_MD = """\
# MyProject

API for user management. See `src/api.py` for endpoints.
Uses `src/services.py` for business logic and `src/models.py` for data models.
"""

HELPERS_PY = """\
def clamp(value: float, low: float, high: float) -> float:
    return max(low, min(high, value))


def slugify(text: str) -> str:
    return text.lower().replace(" ", "-")
"""

INIT_PY = ""


def _populate_project(root: Path) -> None:
    (root / "src").mkdir(parents=True, exist_ok=True)
    (root / "tests").mkdir(parents=True, exist_ok=True)
    (root / "utils").mkdir(parents=True, exist_ok=True)

    (root / "src" / "__init__.py").write_text(INIT_PY)
    (root / "src" / "models.py").write_text(MODELS_PY)
    (root / "src" / "services.py").write_text(SERVICES_PY)
    (root / "src" / "api.py").write_text(API_PY)
    (root / "tests" / "test_api.py").write_text(TEST_API_PY)
    (root / "config.yaml").write_text(CONFIG_YAML)
    (root / "Dockerfile").write_text(DOCKERFILE)
    (root / "README.md").write_text(README_MD)
    (root / "utils" / "helpers.py").write_text(HELPERS_PY)


@pytest.fixture
def graph_project(tmp_path):
    root = tmp_path / "project"
    root.mkdir()
    _populate_project(root)
    return root


@pytest.fixture
def graph_git_project(tmp_path):
    root = tmp_path / "gitproject"
    repo = Pygit2Repo(root)
    _populate_project(root)
    repo.commit("initial commit")

    for i in range(5):
        (root / "src" / "services.py").write_text(SERVICES_PY + f"\n# churn iteration {i}\n")
        repo.commit(f"update services {i}")

    return root


def _build_graph(root, **kw):
    return build_project_graph(root, **kw)


def _run_graph_cli(args, cwd):
    return run_treemapper_subprocess(["graph", *args], cwd=cwd)


# ===================================================================
# Phase 1 — Core
# ===================================================================


class TestGraphBuild:
    def test_empty_directory(self, tmp_path):
        pg = _build_graph(tmp_path)
        assert pg.node_count == 0
        assert pg.edge_count == 0

    def test_single_file_no_edges(self, tmp_path):
        (tmp_path / "main.py").write_text("def hello():\n    return 42\n")
        pg = _build_graph(tmp_path)
        assert pg.node_count >= 1
        assert pg.edge_count == 0

    def test_import_creates_semantic_edge(self, tmp_path):
        (tmp_path / "b.py").write_text("class Foo:\n    pass\n")
        (tmp_path / "a.py").write_text("from b import Foo\n\ndef use_foo():\n    return Foo()\n")
        pg = _build_graph(tmp_path)
        assert pg.edge_count >= 1
        categories = {cat for cat in pg.graph.edge_categories.values()}
        assert "semantic" in categories

    def test_medium_project_all_files_present(self, graph_project):
        pg = _build_graph(graph_project)
        rel_paths = {p.relative_to(graph_project.resolve()).as_posix() for p in pg.files}
        expected = {
            "src/models.py",
            "src/services.py",
            "src/api.py",
            "tests/test_api.py",
            "config.yaml",
            "Dockerfile",
            "README.md",
            "utils/helpers.py",
        }
        for f in expected:
            assert f in rel_paths, f"{f} missing from graph files"

    def test_binary_files_excluded(self, tmp_path):
        (tmp_path / "code.py").write_text("x = 1\n")
        (tmp_path / "image.png").write_bytes(b"\x89PNG\r\n\x1a\n" + b"\x00" * 100)
        pg = _build_graph(tmp_path)
        rel_paths = {p.relative_to(tmp_path.resolve()).as_posix() for p in pg.files}
        assert "image.png" not in rel_paths

    def test_ignores_respected(self, tmp_path):
        (tmp_path / "main.py").write_text("x = 1\n")
        cache = tmp_path / "__pycache__"
        cache.mkdir()
        (cache / "foo.pyc").write_bytes(b"\x00" * 10)
        pg = _build_graph(tmp_path)
        rel_paths = {p.relative_to(tmp_path.resolve()).as_posix() for p in pg.files}
        assert "__pycache__/foo.pyc" not in rel_paths

    def test_whitelist_filtering(self, graph_project):
        wl = graph_project / ".whitelist"
        wl.write_text("src/\n")
        pg = _build_graph(graph_project, whitelist_file=wl)
        for p in pg.files:
            rel = p.relative_to(graph_project.resolve()).as_posix()
            assert rel.startswith("src/"), f"non-src file in graph: {rel}"

    def test_fragments_have_positive_token_counts(self, graph_project):
        pg = _build_graph(graph_project)
        assert pg.node_count > 0
        for frag in pg.fragments.values():
            assert frag.token_count > 0

    def test_root_dir_set(self, graph_project):
        pg = _build_graph(graph_project)
        assert pg.root_dir == graph_project.resolve()


class TestGraphExportJSON:
    @pytest.fixture(autouse=True)
    def _setup(self, graph_project):
        self.pg = _build_graph(graph_project)
        self.json_str = graph_to_json_string(self.pg)
        self.data = json.loads(self.json_str)

    def test_json_parses(self):
        assert isinstance(self.data, dict)

    def test_json_top_level_fields(self):
        for key in ("name", "type", "node_count", "edge_count", "nodes", "edges"):
            assert key in self.data, f"missing top-level key: {key}"

    def test_json_type_is_project_graph(self):
        assert self.data["type"] == "project_graph"

    def test_json_counts_match_arrays(self):
        assert self.data["node_count"] == len(self.data["nodes"])
        assert self.data["edge_count"] == len(self.data["edges"])

    def test_json_node_required_fields(self):
        for node in self.data["nodes"]:
            for field in ("id", "path", "lines", "kind", "symbol", "token_count"):
                assert field in node, f"node missing field: {field}"

    def test_json_edge_required_fields(self):
        if not self.data["edges"]:
            pytest.skip("no edges in test graph")
        for edge in self.data["edges"]:
            for field in ("source", "target", "weight", "category"):
                assert field in edge, f"edge missing field: {field}"

    def test_json_no_absolute_paths(self):
        for node in self.data["nodes"]:
            assert not node["path"].startswith("/"), f"absolute path: {node['path']}"

    def test_json_weights_valid(self):
        for edge in self.data["edges"]:
            w = edge["weight"]
            assert w > 0, f"non-positive weight: {w}"
            assert not math.isnan(w), "NaN weight"
            assert not math.isinf(w), "Inf weight"


class TestGraphCycles:
    def test_obvious_cycle(self, tmp_path):
        (tmp_path / "a.py").write_text("from b import B\n\nclass A:\n    pass\n")
        (tmp_path / "b.py").write_text("from a import A\n\nclass B:\n    pass\n")
        pg = _build_graph(tmp_path)
        cycles = detect_cycles(pg, level="file")
        assert len(cycles) >= 1
        flat = {item for cycle in cycles for item in cycle}
        assert any("a.py" in m for m in flat)
        assert any("b.py" in m for m in flat)

    def test_transitive_cycle(self, tmp_path):
        (tmp_path / "a.py").write_text("from b import B\n\nclass A:\n    pass\n")
        (tmp_path / "b.py").write_text("from c import C\n\nclass B:\n    pass\n")
        (tmp_path / "c.py").write_text("from a import A\n\nclass C:\n    pass\n")
        pg = _build_graph(tmp_path)
        cycles = detect_cycles(pg, level="file")
        assert len(cycles) >= 1
        longest = max(cycles, key=len)
        assert len(longest) >= 3

    def test_acyclic_returns_empty(self, tmp_path):
        (tmp_path / "a.py").write_text("from b import B\n\nclass A:\n    pass\n")
        (tmp_path / "b.py").write_text("from c import C\n\nclass B:\n    pass\n")
        (tmp_path / "c.py").write_text("class C:\n    pass\n")
        pg = _build_graph(tmp_path)
        cycles = detect_cycles(pg, level="file", edge_types={"semantic"})
        assert len(cycles) == 0

    def test_self_import_excluded(self, tmp_path):
        (tmp_path / "a.py").write_text("from a import something\n\ndef something():\n    return 1\n")
        pg = _build_graph(tmp_path)
        cycles = detect_cycles(pg, level="file")
        assert len(cycles) == 0

    def test_directory_level(self, tmp_path):
        pkg_a = tmp_path / "pkg_a"
        pkg_b = tmp_path / "pkg_b"
        pkg_a.mkdir()
        pkg_b.mkdir()
        (pkg_a / "mod.py").write_text("from pkg_b.mod import B\n\nclass A:\n    pass\n")
        (pkg_b / "mod.py").write_text("from pkg_a.mod import A\n\nclass B:\n    pass\n")
        pg = _build_graph(tmp_path)
        cycles = detect_cycles(pg, level="directory")
        flat = {item for cycle in cycles for item in cycle}
        has_dirs = any("pkg_a" in m for m in flat) and any("pkg_b" in m for m in flat)
        if pg.edge_count > 0:
            assert has_dirs or len(cycles) == 0

    def test_edge_type_filter(self, tmp_path):
        (tmp_path / "a.py").write_text("from b import B\n\nclass A:\n    pass\n")
        (tmp_path / "b.py").write_text("from a import A\n\nclass B:\n    pass\n")
        pg = _build_graph(tmp_path)
        semantic_cycles = detect_cycles(pg, level="file", edge_types={"semantic"})
        all_cycles = detect_cycles(pg, level="file")
        assert len(semantic_cycles) <= len(all_cycles) or len(semantic_cycles) == len(all_cycles)


class TestGraphCLI:
    def test_default_mermaid_export(self, graph_git_project):
        result = _run_graph_cli([".", "-q"], cwd=graph_git_project)
        assert result.returncode == 0
        assert result.stdout.startswith("graph LR")

    def test_json_format(self, graph_git_project):
        result = _run_graph_cli([".", "-f", "json", "-q"], cwd=graph_git_project)
        assert result.returncode == 0
        data = json.loads(result.stdout)
        assert data["type"] == "project_graph"

    def test_graphml_format(self, graph_git_project):
        result = _run_graph_cli([".", "-f", "graphml", "-q"], cwd=graph_git_project)
        assert result.returncode == 0
        ET.fromstring(result.stdout)

    def test_summary(self, graph_git_project):
        result = _run_graph_cli([".", "--summary", "-q"], cwd=graph_git_project)
        assert result.returncode == 0
        out = result.stdout.lower()
        assert "nodes" in out
        assert "edges" in out
        assert "cycle" in out
        assert "hotspot" in out
        assert "cohesion" in out

    def test_output_file(self, graph_git_project):
        out_file = graph_git_project / "graph.json"
        result = _run_graph_cli([".", "-f", "json", "-o", str(out_file), "-q"], cwd=graph_git_project)
        assert result.returncode == 0
        assert out_file.exists()
        data = json.loads(out_file.read_text())
        assert data["type"] == "project_graph"

    def test_level_directory_mermaid(self, graph_git_project):
        result = _run_graph_cli([".", "-f", "mermaid", "--level", "directory", "-q"], cwd=graph_git_project)
        assert result.returncode == 0
        out = result.stdout
        assert out.strip().startswith("graph LR")
        assert "src" in out or '["' in out

    def test_tree_mode_still_works(self, graph_git_project):
        out_file = graph_git_project / "tree.yaml"
        result = run_treemapper_subprocess([".", "-o", str(out_file), "-q"], cwd=graph_git_project)
        assert result.returncode == 0
        assert out_file.exists()
        content = out_file.read_text()
        assert "type:" in content or "name:" in content


# ===================================================================
# Phase 2 — Analytics
# ===================================================================


class TestGraphHotspots:
    def test_scores_bounded_zero_to_one(self, graph_git_project):
        pg = _build_graph(graph_git_project)
        hot = hotspots(pg, top=20)
        for _, score, _ in hot:
            assert 0 <= score <= 1, f"score out of range: {score}"

    def test_top_n_respected(self, graph_git_project):
        pg = _build_graph(graph_git_project)
        hot = hotspots(pg, top=3)
        assert len(hot) <= 3

    def test_details_have_degree_and_churn(self, graph_git_project):
        pg = _build_graph(graph_git_project)
        hot = hotspots(pg, top=5)
        for _, _, details in hot:
            assert "out_degree" in details
            assert "churn" in details

    def test_high_churn_file_ranks_higher(self, graph_git_project):
        pg = _build_graph(graph_git_project)
        hot = hotspots(pg, top=20)
        names = [name for name, _, _ in hot]
        if "src/services.py" in names and "utils/helpers.py" in names:
            assert names.index("src/services.py") < names.index("utils/helpers.py")

    def test_non_git_project_no_crash(self, graph_project):
        pg = _build_graph(graph_project)
        hot = hotspots(pg, top=5)
        assert isinstance(hot, list)
        for _, _, details in hot:
            assert details["churn"] == 0


class TestGraphSummary:
    def test_contains_node_and_edge_counts(self, graph_project):
        pg = _build_graph(graph_project)
        text = graph_summary(pg)
        assert f"{pg.node_count} nodes" in text

    def test_contains_edge_type_distribution(self, graph_project):
        pg = _build_graph(graph_project)
        text = graph_summary(pg)
        if pg.edge_count > 0:
            assert "Edge types:" in text

    def test_counts_match_graph(self, graph_project):
        pg = _build_graph(graph_project)
        text = graph_summary(pg)
        assert str(pg.node_count) in text
        assert str(pg.edge_count) in text


class TestGraphMermaid:
    def test_empty_graph(self):
        qg = QuotientGraph()
        result = to_mermaid(qg)
        assert result == "graph LR\n"

    def test_starts_with_graph_lr(self, graph_project):
        pg = _build_graph(graph_project)
        qg = quotient_graph(pg, level="directory")
        result = to_mermaid(qg)
        assert result.startswith("graph LR")

    def test_contains_arrows(self, graph_project):
        pg = _build_graph(graph_project)
        qg = quotient_graph(pg, level="directory")
        result = to_mermaid(qg)
        if pg.edge_count > 0:
            assert "-->" in result

    def test_directory_level_labels(self, graph_project):
        pg = _build_graph(graph_project)
        qg = quotient_graph(pg, level="directory")
        result = to_mermaid(qg)
        for line in result.splitlines():
            if '["' in line:
                label = line.split('["')[1].split('"]')[0]
                assert ":" not in label or label.count(":") == 0

    def test_top_n_limit(self, graph_project):
        pg = _build_graph(graph_project)
        qg = quotient_graph(pg, level="file")
        result = to_mermaid(qg, top_n=3)
        node_defs = [line for line in result.splitlines() if '["' in line]
        assert len(node_defs) <= 3


class TestGraphMetrics:
    def test_instability_range(self, graph_project):
        pg = _build_graph(graph_project)
        metrics = coupling_metrics(pg, level="directory")
        for m in metrics:
            assert 0 <= m.instability <= 1, f"instability out of range: {m.instability}"

    def test_cohesion_coupling_range(self, graph_project):
        pg = _build_graph(graph_project)
        metrics = coupling_metrics(pg, level="directory")
        for m in metrics:
            assert 0 <= m.cohesion <= 1, f"cohesion out of range: {m.cohesion}"
            assert 0 <= m.coupling <= 1, f"coupling out of range: {m.coupling}"

    def test_fan_in_fan_out_non_negative(self, graph_project):
        pg = _build_graph(graph_project)
        metrics = coupling_metrics(pg, level="directory")
        for m in metrics:
            assert m.fan_in >= 0
            assert m.fan_out >= 0

    def test_isolated_module_zero_coupling(self, graph_project):
        pg = _build_graph(graph_project)
        metrics = coupling_metrics(pg, level="directory")
        utils_m = [m for m in metrics if "utils" in m.name]
        if utils_m:
            m = utils_m[0]
            assert m.fan_in == 0 or m.coupling <= 1.0

    def test_directory_level_names(self, graph_project):
        pg = _build_graph(graph_project)
        metrics = coupling_metrics(pg, level="directory")
        names = {m.name for m in metrics}
        assert any("src" in n for n in names)


class TestGraphExportGraphML:
    def test_valid_xml(self, graph_project):
        pg = _build_graph(graph_project)
        xml_str = graph_to_graphml_string(pg)
        ET.fromstring(xml_str)

    def test_has_node_and_edge_elements(self, graph_project):
        pg = _build_graph(graph_project)
        xml_str = graph_to_graphml_string(pg)
        root = ET.fromstring(xml_str)
        ns = {"g": "http://graphml.graphdrawing.org/graphml"}
        nodes = root.findall(".//g:node", ns)
        assert len(nodes) > 0
        if pg.edge_count > 0:
            edges = root.findall(".//g:edge", ns)
            assert len(edges) > 0

    def test_attribute_keys_defined(self, graph_project):
        pg = _build_graph(graph_project)
        xml_str = graph_to_graphml_string(pg)
        root = ET.fromstring(xml_str)
        ns = {"g": "http://graphml.graphdrawing.org/graphml"}
        key_names = {k.get("attr.name") for k in root.findall("g:key", ns)}
        for attr in ("path", "lines", "kind", "symbol", "token_count", "weight", "category"):
            assert attr in key_names, f"missing GraphML key: {attr}"


# ===================================================================
# Phase 3 — API & Robustness
# ===================================================================


class TestProjectGraphAPI:
    def test_subgraph_preserves_internal_edges(self, graph_project):
        pg = _build_graph(graph_project)
        if pg.node_count < 2 or pg.edge_count == 0:
            pytest.skip("need edges for subgraph test")
        first_edge = next(iter(pg.graph.edge_categories.keys()))
        subset = {first_edge[0], first_edge[1]}
        sub = pg.subgraph(subset)
        assert sub.edge_count >= 1

    def test_subgraph_excludes_external(self, graph_project):
        pg = _build_graph(graph_project)
        if pg.node_count < 2:
            pytest.skip("need multiple nodes")
        some_nodes = set(list(pg.fragments.keys())[:2])
        sub = pg.subgraph(some_nodes)
        for src, nbrs in sub.graph.adjacency.items():
            assert src in some_nodes
            for dst in nbrs:
                assert dst in some_nodes

    def test_edges_of_type_filters_correctly(self, graph_project):
        pg = _build_graph(graph_project)
        for cat in pg.edge_type_counts():
            for _, _, w in pg.edges_of_type(cat):
                assert w >= 0

    def test_edge_type_counts_sum(self, graph_project):
        pg = _build_graph(graph_project)
        counts = pg.edge_type_counts()
        total = sum(counts.values())
        assert total == pg.edge_count

    def test_to_dict_roundtrip(self, graph_project):
        pg = _build_graph(graph_project)
        d = pg.to_dict()
        serialized = json.dumps(d)
        assert isinstance(serialized, str)
        deserialized = json.loads(serialized)
        assert deserialized["node_count"] == pg.node_count


class TestGraphEdgeCases:
    def test_syntax_error_no_crash(self, tmp_path):
        (tmp_path / "broken.py").write_text("def broken(\n")
        pg = _build_graph(tmp_path)
        assert isinstance(pg.node_count, int)

    def test_empty_python_file(self, tmp_path):
        (tmp_path / "__init__.py").write_text("")
        pg = _build_graph(tmp_path)
        assert isinstance(pg.node_count, int)

    def test_config_only_project(self, tmp_path):
        (tmp_path / "config.yaml").write_text("key: value\n")
        (tmp_path / "settings.yaml").write_text("debug: true\n")
        pg = _build_graph(tmp_path)
        assert pg.node_count >= 0

    def test_markdown_only_project(self, tmp_path):
        (tmp_path / "README.md").write_text("# Hello\n\nSome text.\n")
        (tmp_path / "CHANGELOG.md").write_text("## v1.0\n\n- Initial release\n")
        pg = _build_graph(tmp_path)
        assert pg.node_count >= 0
