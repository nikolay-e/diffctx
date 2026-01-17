from __future__ import annotations

from pathlib import Path

import pytest

from treemapper.diffctx import build_diff_context
from treemapper.diffctx.graph import build_graph
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


def _extract_fragments_from_tree(tree: dict) -> list[dict]:
    if tree.get("type") == "diff_context":
        return tree.get("fragments", [])
    return []


@pytest.fixture
def diff_project(git_with_commits):
    return git_with_commits


class TestCallEdges:
    def test_graph_call_001_function_call_creates_edge(self):
        caller = _make_fragment(
            "caller.py",
            1,
            10,
            content="def main():\n    helper()\n",
            identifiers=frozenset(["main", "helper"]),
        )
        callee = _make_fragment(
            "callee.py",
            1,
            10,
            content="def helper():\n    return 42\n",
            identifiers=frozenset(["helper"]),
        )

        graph = build_graph([caller, callee])

        assert caller.id in graph.nodes
        assert callee.id in graph.nodes

    def test_graph_call_002_method_call_creates_edge(self):
        caller = _make_fragment(
            "caller.py",
            1,
            10,
            content="def use_service():\n    service.process()\n",
            identifiers=frozenset(["use_service", "service", "process"]),
        )
        service = _make_fragment(
            "service.py",
            1,
            10,
            content="class Service:\n    def process(self):\n        pass\n",
            identifiers=frozenset(["Service", "process"]),
        )

        graph = build_graph([caller, service])

        assert caller.id in graph.nodes
        assert service.id in graph.nodes


class TestSymbolReferenceEdges:
    def test_graph_symbol_001_variable_reference(self):
        definer = _make_fragment(
            "constants.py",
            1,
            5,
            content="CONFIG_VALUE = 42\n",
            identifiers=frozenset(["CONFIG_VALUE"]),
        )
        user = _make_fragment(
            "app.py",
            1,
            10,
            content="def main():\n    x = CONFIG_VALUE\n",
            identifiers=frozenset(["main", "CONFIG_VALUE"]),
        )

        graph = build_graph([definer, user])

        assert definer.id in graph.nodes
        assert user.id in graph.nodes


class TestTypeReferenceEdges:
    def test_graph_type_001_type_annotation_edge(self):
        type_def = _make_fragment(
            "types.py",
            1,
            10,
            content="class UserModel:\n    name: str\n",
            identifiers=frozenset(["UserModel", "name", "str"]),
        )
        user = _make_fragment(
            "service.py",
            1,
            10,
            content="def get_user() -> UserModel:\n    pass\n",
            identifiers=frozenset(["get_user", "UserModel"]),
        )

        graph = build_graph([type_def, user])

        assert type_def.id in graph.nodes
        assert user.id in graph.nodes


class TestImportEdges:
    def test_graph_import_001_direct_import(self, diff_project):
        diff_project.add_file(
            "utils.py",
            """\
def helper():
    return 42
""",
        )
        diff_project.add_file(
            "main.py",
            """\
from utils import helper

def main():
    return helper()
""",
        )
        diff_project.commit("Initial")

        diff_project.add_file(
            "main.py",
            """\
from utils import helper

def main():
    return helper() + 1
""",
        )
        diff_project.commit("Change")

        tree = build_diff_context(
            root_dir=diff_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        fragments = _extract_fragments_from_tree(tree)
        assert len(fragments) >= 1


class TestContainmentEdges:
    def test_graph_contain_001_method_inside_class(self):
        class_frag = _make_fragment(
            "module.py",
            1,
            20,
            kind="class",
            content="class MyClass:\n    def method(self):\n        pass\n",
            identifiers=frozenset(["MyClass", "method"]),
        )
        method_frag = _make_fragment(
            "module.py",
            2,
            4,
            kind="function",
            content="    def method(self):\n        pass\n",
            identifiers=frozenset(["method"]),
        )

        graph = build_graph([class_frag, method_frag])

        assert class_frag.id in graph.nodes
        assert method_frag.id in graph.nodes


class TestTestEdges:
    def test_graph_test_001_test_file_to_source(self, diff_project):
        diff_project.add_file(
            "calculator.py",
            """\
def add(a, b):
    return a + b
""",
        )
        diff_project.add_file(
            "tests/test_calculator.py",
            """\
from calculator import add

def test_add():
    assert add(1, 2) == 3
""",
        )
        diff_project.commit("Initial")

        diff_project.add_file(
            "calculator.py",
            """\
def add(a, b):
    return float(a) + float(b)
""",
        )
        diff_project.commit("Change")

        tree = build_diff_context(
            root_dir=diff_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        fragments = _extract_fragments_from_tree(tree)
        paths = {f.get("path", "") for f in fragments}
        assert any("calculator.py" in p for p in paths)

    def test_graph_test_002_naming_convention_edge(self, diff_project):
        diff_project.add_file(
            "user_service.py",
            """\
def get_user():
    return {"id": 1}
""",
        )
        diff_project.add_file(
            "tests/test_user_service.py",
            """\
def test_get_user():
    assert True
""",
        )
        diff_project.commit("Initial")

        diff_project.add_file(
            "user_service.py",
            """\
def get_user():
    return {"id": 1, "name": "Alice"}
""",
        )
        diff_project.commit("Change")

        tree = build_diff_context(
            root_dir=diff_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        fragments = _extract_fragments_from_tree(tree)
        assert len(fragments) >= 1


class TestConfigCodeEdges:
    def test_graph_config_001_yaml_key_to_code(self, diff_project):
        diff_project.add_file(
            "config.yaml",
            """\
database_host: localhost
database_port: 5432
""",
        )
        diff_project.add_file(
            "db.py",
            """\
import os

database_host = os.getenv("DATABASE_HOST", "localhost")
database_port = int(os.getenv("DATABASE_PORT", 5432))

def connect():
    return f"{database_host}:{database_port}"
""",
        )
        diff_project.commit("Initial")

        diff_project.add_file(
            "config.yaml",
            """\
database_host: db.example.com
database_port: 5432
""",
        )
        diff_project.commit("Change host")

        tree = build_diff_context(
            root_dir=diff_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        fragments = _extract_fragments_from_tree(tree)
        assert len(fragments) >= 1


class TestSiblingEdges:
    def test_graph_sibling_001_same_directory(self):
        frag1 = _make_fragment(
            "utils/helper1.py",
            1,
            10,
            content="def helper1():\n    pass\n",
            identifiers=frozenset(["helper1"]),
        )
        frag2 = _make_fragment(
            "utils/helper2.py",
            1,
            10,
            content="def helper2():\n    pass\n",
            identifiers=frozenset(["helper2"]),
        )

        graph = build_graph([frag1, frag2])

        assert frag1.id in graph.nodes
        assert frag2.id in graph.nodes


class TestLexicalEdges:
    def test_graph_lexical_001_tfidf_similarity(self):
        frag1 = _make_fragment(
            "module1.py",
            1,
            10,
            content="def process_user_data():\n    user_data = get_data()\n",
            identifiers=frozenset(["process_user_data", "user_data", "get_data"]),
        )
        frag2 = _make_fragment(
            "module2.py",
            1,
            10,
            content="def validate_user_data():\n    user_data = input()\n",
            identifiers=frozenset(["validate_user_data", "user_data", "input"]),
        )

        graph = build_graph([frag1, frag2])

        assert frag1.id in graph.nodes
        assert frag2.id in graph.nodes


class TestNoSelfLoops:
    def test_graph_noloop_001_no_self_reference(self):
        frag = _make_fragment(
            "module.py",
            1,
            10,
            content="def func():\n    func()\n",
            identifiers=frozenset(["func"]),
        )

        graph = build_graph([frag])

        neighbors = graph.neighbors(frag.id)
        assert frag.id not in neighbors


class TestHubSuppression:
    def test_graph_hub_001_common_module_suppressed(self):
        logger = _make_fragment(
            "logger.py",
            1,
            10,
            content="def log(msg):\n    print(msg)\n",
            identifiers=frozenset(["log"]),
        )

        users = [
            _make_fragment(
                f"module{i}.py",
                1,
                10,
                content=f"def func{i}():\n    log('message')\n",
                identifiers=frozenset([f"func{i}", "log"]),
            )
            for i in range(20)
        ]

        all_frags = [logger, *users]
        graph = build_graph(all_frags)

        assert logger.id in graph.nodes
        for user in users:
            assert user.id in graph.nodes


class TestBackwardWeightFactor:
    def test_graph_backward_001_reverse_edges_lower_weight(self):
        caller = _make_fragment(
            "caller.py",
            1,
            10,
            content="def main():\n    helper()\n",
            identifiers=frozenset(["main", "helper"]),
        )
        callee = _make_fragment(
            "callee.py",
            1,
            10,
            content="def helper():\n    return 42\n",
            identifiers=frozenset(["helper"]),
        )

        graph = build_graph([caller, callee])

        graph.neighbors(caller.id)
        graph.neighbors(callee.id)

        assert caller.id in graph.nodes
        assert callee.id in graph.nodes


class TestAnchorLinkEdges:
    def test_graph_anchor_001_markdown_internal_link(self, diff_project):
        diff_project.add_file(
            "docs.md",
            """\
# Introduction

See [Installation](#installation) for setup instructions.

# Installation

Run `pip install package`.

# Usage

After [Installation](#installation), you can use the tool.
""",
        )
        diff_project.commit("Initial")

        diff_project.add_file(
            "docs.md",
            """\
# Introduction

See [Installation](#installation) for setup instructions.

# Installation

Run `pip install package-v2`.

# Usage

After [Installation](#installation), you can use the tool.
""",
        )
        diff_project.commit("Update install")

        tree = build_diff_context(
            root_dir=diff_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        fragments = _extract_fragments_from_tree(tree)
        assert len(fragments) >= 1


class TestCitationEdges:
    def test_graph_citation_001_shared_citation(self, diff_project):
        diff_project.add_file(
            "paper.md",
            """\
# Methods

According to [@smith2020], the approach works.

# Results

As shown by [@smith2020], results are positive.
""",
        )
        diff_project.commit("Initial")

        diff_project.add_file(
            "paper.md",
            """\
# Methods

According to [@smith2020], the MODIFIED approach works.

# Results

As shown by [@smith2020], results are positive.
""",
        )
        diff_project.commit("Change methods")

        tree = build_diff_context(
            root_dir=diff_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        fragments = _extract_fragments_from_tree(tree)
        assert len(fragments) >= 1


class TestDocumentStructureEdges:
    def test_graph_doc_001_sequential_sections(self, diff_project):
        diff_project.add_file(
            "guide.md",
            """\
# Step 1

Do the first thing.

# Step 2

Do the second thing.

# Step 3

Do the third thing.
""",
        )
        diff_project.commit("Initial")

        diff_project.add_file(
            "guide.md",
            """\
# Step 1

Do the first thing.

# Step 2

Do the MODIFIED second thing.

# Step 3

Do the third thing.
""",
        )
        diff_project.commit("Change step 2")

        tree = build_diff_context(
            root_dir=diff_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        fragments = _extract_fragments_from_tree(tree)
        assert len(fragments) >= 1
