import json
import math
import xml.etree.ElementTree as ET
from pathlib import Path

import pytest

from treemapper.diffctx.graph_analytics import (
    QuotientGraph,
    quotient_graph,
    to_mermaid,
)
from treemapper.diffctx.graph_export import (
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


def _build_graph(root, **kw):
    return build_project_graph(root, **kw)


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
