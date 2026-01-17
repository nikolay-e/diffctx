import pytest

from treemapper.diffctx import build_diff_context


def _extract_files_from_tree(tree: dict) -> set[str]:
    files = set()
    if tree.get("type") == "diff_context":
        for frag in tree.get("fragments", []):
            path = frag.get("path", "")
            if path:
                files.add(path.split("/")[-1])
        return files

    def traverse(node):
        if node.get("type") == "file":
            files.add(node["name"])
        for child in node.get("children", []):
            traverse(child)

    traverse(tree)
    return files


@pytest.fixture
def diff_project(git_with_commits):
    return git_with_commits


class TestConfigKeyUsage:
    def test_cfg_001_yaml_config_changed_find_code_using_it(self, diff_project):
        diff_project.add_file(
            "config.yaml",
            """database:
  pool_size: 5
  timeout: 30
""",
        )
        diff_project.add_file(
            "database/connection.py",
            """import yaml

def load_config():
    with open("config.yaml") as f:
        config = yaml.safe_load(f)
    return config

def get_pool():
    config = load_config()
    pool_size = config["database"]["pool_size"]
    return create_pool(pool_size)

def create_pool(size):
    return {"size": size}
""",
        )
        diff_project.commit("Initial")

        diff_project.add_file(
            "config.yaml",
            """database:
  pool_size: 10
  timeout: 30
""",
        )
        diff_project.commit("Increase pool size")

        tree = build_diff_context(
            root_dir=diff_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "config.yaml" in selected

    def test_cfg_002_new_config_key_added(self, diff_project):
        diff_project.add_file(
            "config.yaml",
            """app:
  name: MyApp
  debug: false
""",
        )
        diff_project.add_file(
            "checkout/views.py",
            """def checkout():
    return "Standard checkout"
""",
        )
        diff_project.commit("Initial")

        diff_project.add_file(
            "config.yaml",
            """app:
  name: MyApp
  debug: false
feature_flags:
  new_checkout: true
""",
        )
        diff_project.commit("Add feature flag")

        tree = build_diff_context(
            root_dir=diff_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "config.yaml" in selected

    def test_cfg_003_env_variable_changed(self, diff_project):
        diff_project.add_file(
            ".env.example",
            """API_TIMEOUT=30
API_KEY=your_key_here
""",
        )
        diff_project.add_file(
            "client.py",
            """import os

def get_timeout():
    return int(os.getenv("API_TIMEOUT", 30))

def make_request():
    timeout = get_timeout()
    return {"timeout": timeout}
""",
        )
        diff_project.add_file(
            "settings.py",
            """import os

API_TIMEOUT = os.environ.get("API_TIMEOUT", "30")
""",
        )
        diff_project.commit("Initial")

        diff_project.add_file(
            ".env.example",
            """API_TIMEOUT=60
API_KEY=your_key_here
""",
        )
        diff_project.commit("Increase API timeout")

        tree = build_diff_context(
            root_dir=diff_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert ".env.example" in selected

    def test_cfg_004_json_config_changed(self, diff_project):
        diff_project.add_file(
            "settings.json",
            """{
  "maxConnections": 100,
  "timeout": 30
}
""",
        )
        diff_project.add_file(
            "server.py",
            """import json

def load_settings():
    with open("settings.json") as f:
        return json.load(f)

def start_server():
    settings = load_settings()
    max_conn = settings["maxConnections"]
    return {"max_connections": max_conn}
""",
        )
        diff_project.commit("Initial")

        diff_project.add_file(
            "settings.json",
            """{
  "maxConnections": 200,
  "timeout": 30
}
""",
        )
        diff_project.commit("Increase max connections")

        tree = build_diff_context(
            root_dir=diff_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "settings.json" in selected

    def test_cfg_005_toml_config_changed(self, diff_project):
        diff_project.add_file(
            "pyproject.toml",
            """[tool.pytest]
timeout = 30
filterwarnings = "ignore"
""",
        )
        diff_project.add_file(
            "tests/test_slow.py",
            """import time

def test_slow_operation():
    time.sleep(1)
    assert True
""",
        )
        diff_project.commit("Initial")

        diff_project.add_file(
            "pyproject.toml",
            """[tool.pytest]
timeout = 60
filterwarnings = "ignore"
""",
        )
        diff_project.commit("Increase test timeout")

        tree = build_diff_context(
            root_dir=diff_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "pyproject.toml" in selected
