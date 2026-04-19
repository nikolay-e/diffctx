import json
import xml.etree.ElementTree as ET
from pathlib import Path

import pytest

from tests.conftest import run_treemapper_subprocess
from tests.framework.pygit2_backend import Pygit2Repo
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
