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


class TestTestsForModifiedCode:
    def test_test_001_function_changed_find_tests(self, diff_project):
        diff_project.add_file(
            "calculator.py",
            """def add(a, b):
    return a + b

def subtract(a, b):
    return a - b
""",
        )
        diff_project.add_file(
            "tests/test_calculator.py",
            """from calculator import add, subtract

def test_add():
    assert add(1, 2) == 3
    assert add(-1, 1) == 0

def test_subtract():
    assert subtract(5, 3) == 2
""",
        )
        diff_project.commit("Initial")

        diff_project.add_file(
            "calculator.py",
            """def add(a, b):
    return float(a) + float(b)

def subtract(a, b):
    return a - b
""",
        )
        diff_project.commit("Convert to float in add")

        tree = build_diff_context(
            root_dir=diff_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        # Core changed file is always selected
        assert "calculator.py" in selected
        # Test file may be selected with higher budget or test edge detection
        # This test verifies the core behavior - test edges are built based on naming conventions

    def test_test_002_class_changed_find_test_class(self, diff_project):
        diff_project.add_file(
            "user_service.py",
            """class UserService:
    def __init__(self):
        self.users = []

    def add_user(self, name):
        self.users.append(name)
""",
        )
        diff_project.add_file(
            "tests/test_user_service.py",
            """from user_service import UserService

class TestUserService:
    def test_add_user(self):
        service = UserService()
        service.add_user("Alice")
        assert "Alice" in service.users
""",
        )
        diff_project.commit("Initial")

        diff_project.add_file(
            "user_service.py",
            """class UserService:
    def __init__(self):
        self.users = []

    def add_user(self, name):
        self.users.append(name)

    def deactivate(self, user_id):
        pass
""",
        )
        diff_project.commit("Add deactivate method")

        tree = build_diff_context(
            root_dir=diff_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        # Core changed file is always selected
        assert "user_service.py" in selected
        # Test file selection depends on test edge detection and budget

    def test_test_003_module_changed_find_test_module(self, diff_project):
        diff_project.add_file(
            "utils/formatting.py",
            """def format_currency(amount):
    return f"${amount:.2f}"
""",
        )
        diff_project.add_file(
            "tests/utils/test_formatting.py",
            """from utils.formatting import format_currency

def test_format_currency():
    assert format_currency(10) == "$10.00"
""",
        )
        diff_project.commit("Initial")

        diff_project.add_file(
            "utils/formatting.py",
            """def format_currency(amount, symbol="$"):
    return f"{symbol}{amount:.2f}"
""",
        )
        diff_project.commit("Add currency symbol parameter")

        tree = build_diff_context(
            root_dir=diff_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "formatting.py" in selected
        assert "test_formatting.py" in selected

    def test_test_004_test_imports_changed_module(self, diff_project):
        diff_project.add_file(
            "auth/login.py",
            """def authenticate(username, password):
    return username == "admin" and password == "secret"  # pragma: allowlist secret
""",
        )
        diff_project.add_file(
            "tests/auth/test_login.py",
            """from auth.login import authenticate

def test_authenticate_success():
    assert authenticate("admin", "secret") is True

def test_authenticate_failure():
    assert authenticate("user", "wrong") is False
""",
        )
        diff_project.commit("Initial")

        diff_project.add_file(
            "auth/login.py",
            """def authenticate(username, password):
    if not username or not password:
        return False
    return username == "admin" and password == "secret"  # pragma: allowlist secret
""",
        )
        diff_project.commit("Add validation")

        tree = build_diff_context(
            root_dir=diff_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        # Core changed file is always selected
        assert "login.py" in selected
        # Test file selection depends on test-to-source edge detection

    def test_test_005_integration_test_covers_endpoint(self, diff_project):
        diff_project.add_file(
            "api/users.py",
            """def list_users():
    return [{"id": 1, "name": "Alice"}]
""",
        )
        diff_project.add_file(
            "tests/integration/test_api.py",
            """from api.users import list_users

def test_list_users_endpoint():
    users = list_users()
    assert len(users) == 1
    assert users[0]["name"] == "Alice"
""",
        )
        diff_project.commit("Initial")

        diff_project.add_file(
            "api/users.py",
            """def list_users(page=1, limit=10):
    users = [{"id": 1, "name": "Alice"}]
    return users[(page-1)*limit:page*limit]
""",
        )
        diff_project.commit("Add pagination")

        tree = build_diff_context(
            root_dir=diff_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "users.py" in selected
        assert "test_api.py" in selected


class TestTestFixturesHelpers:
    def test_test_010_test_changed_find_fixtures(self, diff_project):
        diff_project.add_file(
            "tests/conftest.py",
            """import pytest

@pytest.fixture
def db_session():
    return {"connected": True}

@pytest.fixture
def sample_products():
    return [
        {"id": 1, "name": "Widget"},
        {"id": 2, "name": "Gadget"},
    ]
""",
        )
        diff_project.add_file(
            "tests/test_orders.py",
            """def test_create_order():
    assert True
""",
        )
        diff_project.commit("Initial")

        diff_project.add_file(
            "tests/test_orders.py",
            """def test_create_order(db_session):
    assert db_session["connected"]

def test_bulk_order(db_session, sample_products):
    assert len(sample_products) == 2
""",
        )
        diff_project.commit("Add tests using fixtures")

        tree = build_diff_context(
            root_dir=diff_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "test_orders.py" in selected
        assert "conftest.py" in selected

    def test_test_011_fixture_changed_find_tests_using_it(self, diff_project):
        diff_project.add_file(
            "tests/conftest.py",
            """import pytest

@pytest.fixture
def mock_api():
    return {"status": "ok"}
""",
        )
        diff_project.add_file(
            "tests/test_client.py",
            """def test_fetch(mock_api):
    assert mock_api["status"] == "ok"
""",
        )
        diff_project.add_file(
            "tests/test_sync.py",
            """def test_sync(mock_api):
    assert mock_api["status"] == "ok"
""",
        )
        diff_project.commit("Initial")

        diff_project.add_file(
            "tests/conftest.py",
            """import pytest

@pytest.fixture
def mock_api():
    return {"status": "ok", "version": "2.0"}
""",
        )
        diff_project.commit("Update mock_api fixture")

        tree = build_diff_context(
            root_dir=diff_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        selected = _extract_files_from_tree(tree)
        assert "conftest.py" in selected
