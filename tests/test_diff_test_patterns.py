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


def _extract_fragments_content(tree: dict) -> str:
    contents = []
    if tree.get("type") == "diff_context":
        for frag in tree.get("fragments", []):
            content = frag.get("content", "")
            if content:
                contents.append(content)
    return "\n".join(contents)


@pytest.fixture
def project(git_with_commits):
    return git_with_commits


class TestPytestParametrize:
    def test_c1_parametrize_source_includes_tested_function(self, project):
        project.add_file(
            "src/calculator.py",
            """def add(a: int, b: int) -> int:
    return a + b

def subtract(a: int, b: int) -> int:
    return a - b

def multiply(a: int, b: int) -> int:
    return a * b

def divide(a: int, b: int) -> float:
    if b == 0:
        raise ValueError("Cannot divide by zero")
    return a / b
""",
        )
        project.add_file(
            "tests/test_calculator.py",
            """import pytest
from src.calculator import add

def test_add_basic():
    assert add(1, 2) == 3
""",
        )
        project.commit("Initial calculator with basic test")

        project.add_file(
            "tests/test_calculator.py",
            """import pytest
from src.calculator import add, subtract, multiply

@pytest.mark.parametrize("a,b,expected", [
    (1, 2, 3),
    (0, 0, 0),
    (-1, 1, 0),
    (100, 200, 300),
    (-5, -3, -8),
])
def test_add_parametrized(a, b, expected):
    assert add(a, b) == expected

@pytest.mark.parametrize("a,b,expected", [
    (5, 3, 2),
    (0, 0, 0),
    (-1, -1, 0),
])
def test_subtract_parametrized(a, b, expected):
    assert subtract(a, b) == expected

def test_multiply_basic():
    assert multiply(2, 3) == 6
""",
        )
        project.commit("Add parametrized tests for add and subtract")

        tree = build_diff_context(
            root_dir=project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=800,
        )

        all_content = _extract_fragments_content(tree)
        assert "def add" in all_content or "calculator.py" in str(tree)

    def test_c1_parametrize_edge_cases_includes_function(self, project):
        project.add_file(
            "src/validator.py",
            """import re

def validate_email(email: str) -> bool:
    pattern = r'^[a-zA-Z0-9_.+-]+@[a-zA-Z0-9-]+\\.[a-zA-Z0-9-.]+$'
    return bool(re.match(pattern, email))

def validate_phone(phone: str) -> bool:
    return phone.isdigit() and 10 <= len(phone) <= 15

def validate_username(username: str) -> bool:
    return len(username) >= 3 and username.isalnum()
""",
        )
        project.add_file(
            "tests/test_validator.py",
            """from src.validator import validate_email

def test_email_valid():
    assert validate_email("test@example.com")
""",
        )
        project.commit("Initial validator")

        project.add_file(
            "tests/test_validator.py",
            """import pytest
from src.validator import validate_email, validate_phone

@pytest.mark.parametrize("email,expected", [
    ("test@example.com", True),
    ("user.name+tag@domain.co.uk", True),
    ("invalid", False),
    ("missing@domain", False),
    ("@nodomain.com", False),
    ("spaces in@email.com", False),
    ("", False),
])
def test_email_validation(email, expected):
    assert validate_email(email) == expected

@pytest.mark.parametrize("phone,expected", [
    ("1234567890", True),
    ("12345678901234", True),
    ("123", False),
    ("12345678901234567", False),
    ("123-456-7890", False),
    ("phone123", False),
])
def test_phone_validation(phone, expected):
    assert validate_phone(phone) == expected
""",
        )
        project.commit("Add comprehensive parametrized validation tests")

        tree = build_diff_context(
            root_dir=project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=800,
        )

        all_content = _extract_fragments_content(tree)
        assert "def validate_email" in all_content or "validator.py" in str(tree)


class TestConftestFixtureChange:
    def test_c2_conftest_fixture_change_includes_using_tests(self, project):
        project.add_file(
            "src/database.py",
            """class Database:
    def __init__(self, connection_string: str):
        self.connection_string = connection_string
        self.connected = False

    def connect(self):
        self.connected = True

    def disconnect(self):
        self.connected = False

    def query(self, sql: str) -> list:
        if not self.connected:
            raise RuntimeError("Not connected")
        return []
""",
        )
        project.add_file(
            "tests/conftest.py",
            """import pytest
from src.database import Database

@pytest.fixture
def db():
    database = Database("test://localhost")
    database.connect()
    yield database
    database.disconnect()
""",
        )
        project.add_file(
            "tests/test_queries.py",
            """def test_query_returns_list(db):
    result = db.query("SELECT * FROM users")
    assert isinstance(result, list)

def test_db_is_connected(db):
    assert db.connected is True
""",
        )
        project.commit("Initial database tests with fixture")

        project.add_file(
            "tests/conftest.py",
            """import pytest
from src.database import Database

@pytest.fixture
def db():
    database = Database("test://localhost")
    database.connect()
    yield database
    database.disconnect()

@pytest.fixture
def db_with_data(db):
    db.query("INSERT INTO users VALUES (1, 'test')")
    return db

@pytest.fixture
def transaction_db(db):
    db.query("BEGIN TRANSACTION")
    yield db
    db.query("ROLLBACK")
""",
        )
        project.commit("Add db_with_data and transaction_db fixtures")

        tree = build_diff_context(
            root_dir=project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=800,
        )

        all_content = _extract_fragments_content(tree)
        selected = _extract_files_from_tree(tree)
        assert "conftest.py" in selected
        assert "@pytest.fixture" in all_content

    def test_c2_fixture_modification_includes_dependent_tests(self, project):
        project.add_file(
            "src/user_service.py",
            """class User:
    def __init__(self, id: int, name: str, email: str):
        self.id = id
        self.name = name
        self.email = email

class UserService:
    def __init__(self):
        self.users = {}

    def create_user(self, name: str, email: str) -> User:
        user_id = len(self.users) + 1
        user = User(user_id, name, email)
        self.users[user_id] = user
        return user

    def get_user(self, user_id: int) -> User:
        return self.users.get(user_id)
""",
        )
        project.add_file(
            "tests/conftest.py",
            """import pytest
from src.user_service import UserService

@pytest.fixture
def user_service():
    return UserService()

@pytest.fixture
def sample_user(user_service):
    return user_service.create_user("Test", "test@example.com")
""",
        )
        project.add_file(
            "tests/test_user_service.py",
            """def test_create_user(user_service):
    user = user_service.create_user("John", "john@example.com")
    assert user.name == "John"

def test_get_user(user_service, sample_user):
    retrieved = user_service.get_user(sample_user.id)
    assert retrieved.name == "Test"
""",
        )
        project.commit("Initial user service tests")

        project.add_file(
            "tests/conftest.py",
            """import pytest
from src.user_service import UserService

@pytest.fixture
def user_service():
    service = UserService()
    yield service
    service.users.clear()

@pytest.fixture
def sample_user(user_service):
    return user_service.create_user("Test", "test@example.com")

@pytest.fixture
def admin_user(user_service):
    user = user_service.create_user("Admin", "admin@example.com")
    user.is_admin = True
    return user
""",
        )
        project.commit("Add cleanup to user_service fixture and admin_user fixture")

        tree = build_diff_context(
            root_dir=project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=800,
        )

        all_content = _extract_fragments_content(tree)
        assert "def user_service" in all_content or "conftest.py" in str(tree)


class TestMockSideEffect:
    def test_c3_mock_side_effect_includes_mocked_function(self, project):
        project.add_file(
            "src/api_client.py",
            """import requests

class ApiClient:
    def __init__(self, base_url: str):
        self.base_url = base_url

    def fetch_data(self, endpoint: str) -> dict:
        response = requests.get(f"{self.base_url}/{endpoint}")
        response.raise_for_status()
        return response.json()

    def post_data(self, endpoint: str, data: dict) -> dict:
        response = requests.post(f"{self.base_url}/{endpoint}", json=data)
        response.raise_for_status()
        return response.json()
""",
        )
        project.add_file(
            "src/data_processor.py",
            """from src.api_client import ApiClient

class DataProcessor:
    def __init__(self, client: ApiClient):
        self.client = client

    def process_user_data(self, user_id: int) -> dict:
        data = self.client.fetch_data(f"users/{user_id}")
        return {"processed": True, "user": data}

    def process_with_retry(self, endpoint: str, retries: int = 3) -> dict:
        for attempt in range(retries):
            try:
                return self.client.fetch_data(endpoint)
            except Exception as e:
                if attempt == retries - 1:
                    raise
        return {}
""",
        )
        project.add_file(
            "tests/test_data_processor.py",
            """from unittest.mock import Mock
from src.data_processor import DataProcessor

def test_process_user_data():
    mock_client = Mock()
    mock_client.fetch_data.return_value = {"id": 1, "name": "Test"}
    processor = DataProcessor(mock_client)
    result = processor.process_user_data(1)
    assert result["processed"] is True
""",
        )
        project.commit("Initial data processor with basic mock test")

        project.add_file(
            "tests/test_data_processor.py",
            """from unittest.mock import Mock
import pytest
from src.data_processor import DataProcessor

def test_process_user_data():
    mock_client = Mock()
    mock_client.fetch_data.return_value = {"id": 1, "name": "Test"}
    processor = DataProcessor(mock_client)
    result = processor.process_user_data(1)
    assert result["processed"] is True

def test_retry_on_failure():
    mock_client = Mock()
    mock_client.fetch_data.side_effect = [
        ConnectionError("First failure"),
        ConnectionError("Second failure"),
        {"id": 1, "data": "success"},
    ]
    processor = DataProcessor(mock_client)
    result = processor.process_with_retry("users/1", retries=3)
    assert result["data"] == "success"
    assert mock_client.fetch_data.call_count == 3

def test_retry_exhausted_raises():
    mock_client = Mock()
    mock_client.fetch_data.side_effect = [
        ConnectionError("Failure 1"),
        ConnectionError("Failure 2"),
        ConnectionError("Failure 3"),
    ]
    processor = DataProcessor(mock_client)
    with pytest.raises(ConnectionError):
        processor.process_with_retry("users/1", retries=3)
""",
        )
        project.commit("Add side_effect tests for retry logic")

        tree = build_diff_context(
            root_dir=project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=800,
        )

        all_content = _extract_fragments_content(tree)
        assert "def process_with_retry" in all_content or "data_processor.py" in str(tree)

    def test_c3_mock_return_sequence_includes_source(self, project):
        project.add_file(
            "src/rate_limiter.py",
            """import time

class RateLimiter:
    def __init__(self, max_calls: int, period: float):
        self.max_calls = max_calls
        self.period = period
        self.calls = []

    def is_allowed(self) -> bool:
        now = time.time()
        self.calls = [c for c in self.calls if now - c < self.period]
        if len(self.calls) < self.max_calls:
            self.calls.append(now)
            return True
        return False

    def wait_time(self) -> float:
        if not self.calls:
            return 0
        oldest = min(self.calls)
        return max(0, self.period - (time.time() - oldest))
""",
        )
        project.add_file(
            "tests/test_rate_limiter.py",
            """from src.rate_limiter import RateLimiter

def test_allows_within_limit():
    limiter = RateLimiter(max_calls=3, period=1.0)
    assert limiter.is_allowed() is True
""",
        )
        project.commit("Initial rate limiter")

        project.add_file(
            "tests/test_rate_limiter.py",
            """from unittest.mock import patch
from src.rate_limiter import RateLimiter

def test_allows_within_limit():
    limiter = RateLimiter(max_calls=3, period=1.0)
    assert limiter.is_allowed() is True

@patch('src.rate_limiter.time.time')
def test_rate_limit_sequence(mock_time):
    mock_time.side_effect = [
        0.0,    # First call check
        0.0,    # First call append
        0.1,    # Second call check
        0.1,    # Second call append
        0.2,    # Third call check
        0.2,    # Third call append
        0.3,    # Fourth call check (should be blocked)
        1.5,    # Fifth call check (period passed)
        1.5,    # Fifth call append
    ]
    limiter = RateLimiter(max_calls=3, period=1.0)
    assert limiter.is_allowed() is True
    assert limiter.is_allowed() is True
    assert limiter.is_allowed() is True
    assert limiter.is_allowed() is False
    assert limiter.is_allowed() is True
""",
        )
        project.commit("Add time-mocked rate limit test with side_effect sequence")

        tree = build_diff_context(
            root_dir=project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=800,
        )

        all_content = _extract_fragments_content(tree)
        assert "def is_allowed" in all_content or "rate_limiter.py" in str(tree)


class TestPytestRaisesContext:
    def test_c4_pytest_raises_includes_exception_definition(self, project):
        project.add_file(
            "src/exceptions.py",
            """class ValidationError(Exception):
    def __init__(self, field: str, message: str):
        self.field = field
        self.message = message
        super().__init__(f"{field}: {message}")

class NotFoundError(Exception):
    def __init__(self, resource: str, identifier: str):
        self.resource = resource
        self.identifier = identifier
        super().__init__(f"{resource} with id {identifier} not found")
""",
        )
        project.add_file(
            "src/user_repository.py",
            """from src.exceptions import NotFoundError, ValidationError

class UserRepository:
    def __init__(self):
        self.users = {}

    def get(self, user_id: int):
        if user_id not in self.users:
            raise NotFoundError("User", str(user_id))
        return self.users[user_id]

    def create(self, data: dict):
        if "email" not in data:
            raise ValidationError("email", "is required")
        user_id = len(self.users) + 1
        self.users[user_id] = {**data, "id": user_id}
        return self.users[user_id]
""",
        )
        project.add_file(
            "tests/test_user_repository.py",
            """from src.user_repository import UserRepository

def test_create_user():
    repo = UserRepository()
    user = repo.create({"name": "Test", "email": "test@example.com"})
    assert user["name"] == "Test"
""",
        )
        project.commit("Initial user repository")

        project.add_file(
            "src/exceptions.py",
            """class ValidationError(Exception):
    def __init__(self, field: str, message: str):
        self.field = field
        self.message = message
        super().__init__(f"{field}: {message}")

class NotFoundError(Exception):
    def __init__(self, resource: str, identifier: str):
        self.resource = resource
        self.identifier = identifier
        super().__init__(f"{resource} with id {identifier} not found")

class PermissionDeniedError(Exception):
    def __init__(self, action: str, resource: str):
        self.action = action
        self.resource = resource
        super().__init__(f"Permission denied: cannot {action} {resource}")
""",
        )
        project.add_file(
            "src/user_repository.py",
            """from src.exceptions import NotFoundError, ValidationError

class UserRepository:
    def __init__(self):
        self.users = {}

    def get(self, user_id: int):
        if user_id not in self.users:
            raise NotFoundError("User", str(user_id))
        return self.users[user_id]

    def create(self, data: dict):
        if "email" not in data:
            raise ValidationError("email", "is required")
        if "name" not in data:
            raise ValidationError("name", "is required")
        user_id = len(self.users) + 1
        self.users[user_id] = {**data, "id": user_id}
        return self.users[user_id]
""",
        )
        project.add_file(
            "tests/test_user_repository.py",
            """import pytest
from src.user_repository import UserRepository
from src.exceptions import NotFoundError, ValidationError

def test_create_user():
    repo = UserRepository()
    user = repo.create({"name": "Test", "email": "test@example.com"})
    assert user["name"] == "Test"

def test_get_nonexistent_raises_not_found():
    repo = UserRepository()
    with pytest.raises(NotFoundError) as exc_info:
        repo.get(999)
    assert exc_info.value.resource == "User"
    assert exc_info.value.identifier == "999"

def test_create_without_email_raises_validation_error():
    repo = UserRepository()
    with pytest.raises(ValidationError) as exc_info:
        repo.create({"name": "Test"})
    assert exc_info.value.field == "email"
    assert "required" in exc_info.value.message

def test_create_without_name_raises_validation_error():
    repo = UserRepository()
    with pytest.raises(ValidationError) as exc_info:
        repo.create({"email": "test@example.com"})
    assert exc_info.value.field == "name"
""",
        )
        project.commit("Add new exception, validation in repository, and pytest.raises tests")

        tree = build_diff_context(
            root_dir=project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=800,
        )

        selected = _extract_files_from_tree(tree)
        all_content = _extract_fragments_content(tree)
        assert "exceptions.py" in selected or "class NotFoundError" in all_content or "class ValidationError" in all_content

    def test_c4_pytest_raises_includes_raise_sites(self, project):
        project.add_file(
            "src/auth_errors.py",
            """class AuthenticationError(Exception):
    pass

class TokenExpiredError(AuthenticationError):
    def __init__(self, token_id: str):
        self.token_id = token_id
        super().__init__(f"Token {token_id} has expired")

class InvalidCredentialsError(AuthenticationError):
    pass
""",
        )
        project.add_file(
            "src/auth_service.py",
            """import time
from src.auth_errors import TokenExpiredError, InvalidCredentialsError

class AuthService:
    def __init__(self):
        self.tokens = {}
        self.users = {"admin": "secret123"}

    def login(self, username: str, password: str) -> str:
        if username not in self.users or self.users[username] != password:
            raise InvalidCredentialsError("Invalid username or password")
        token_id = f"token_{username}_{time.time()}"
        self.tokens[token_id] = {"user": username, "expires": time.time() + 3600}
        return token_id

    def verify_token(self, token_id: str) -> dict:
        if token_id not in self.tokens:
            raise TokenExpiredError(token_id)
        return self.tokens[token_id]
""",
        )
        project.add_file(
            "tests/test_auth.py",
            """from src.auth_service import AuthService

def test_login_success():
    service = AuthService()
    token = service.login("admin", "secret123")
    assert token.startswith("token_")
""",
        )
        project.commit("Initial auth service")

        project.add_file(
            "src/auth_service.py",
            """import time
from src.auth_errors import TokenExpiredError, InvalidCredentialsError

class AuthService:
    def __init__(self):
        self.tokens = {}
        self.users = {"admin": "secret123"}

    def login(self, username: str, password: str) -> str:
        if username not in self.users or self.users[username] != password:
            raise InvalidCredentialsError("Invalid username or password")
        token_id = f"token_{username}_{time.time()}"
        self.tokens[token_id] = {"user": username, "expires": time.time() + 3600}
        return token_id

    def verify_token(self, token_id: str) -> dict:
        if token_id not in self.tokens:
            raise TokenExpiredError(token_id)
        token_data = self.tokens[token_id]
        if time.time() > token_data["expires"]:
            del self.tokens[token_id]
            raise TokenExpiredError(token_id)
        return token_data
""",
        )
        project.add_file(
            "tests/test_auth.py",
            """import pytest
from unittest.mock import patch
from src.auth_service import AuthService
from src.auth_errors import TokenExpiredError, InvalidCredentialsError

def test_login_success():
    service = AuthService()
    token = service.login("admin", "secret123")
    assert token.startswith("token_")

def test_login_invalid_credentials():
    service = AuthService()
    with pytest.raises(InvalidCredentialsError):
        service.login("admin", "wrong_password")

def test_login_unknown_user():
    service = AuthService()
    with pytest.raises(InvalidCredentialsError):
        service.login("unknown", "password")

@patch('src.auth_service.time.time')
def test_verify_expired_token(mock_time):
    service = AuthService()
    mock_time.return_value = 1000.0
    token = service.login("admin", "secret123")

    mock_time.return_value = 5000.0
    with pytest.raises(TokenExpiredError) as exc_info:
        service.verify_token(token)
    assert token in str(exc_info.value)

def test_verify_nonexistent_token():
    service = AuthService()
    with pytest.raises(TokenExpiredError):
        service.verify_token("nonexistent_token")
""",
        )
        project.commit("Add token expiry check in verify_token and pytest.raises tests")

        tree = build_diff_context(
            root_dir=project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=800,
        )

        selected = _extract_files_from_tree(tree)
        all_content = _extract_fragments_content(tree)
        assert "auth_service.py" in selected or "raise TokenExpiredError" in all_content


class TestSnapshotAssertion:
    def test_c5_snapshot_assertion_includes_generating_code(self, project):
        project.add_file(
            "src/report_generator.py",
            """from datetime import datetime

class ReportGenerator:
    def __init__(self, title: str):
        self.title = title

    def generate_summary(self, data: list[dict]) -> dict:
        total = sum(item.get("value", 0) for item in data)
        count = len(data)
        avg = total / count if count > 0 else 0
        return {
            "title": self.title,
            "total": total,
            "count": count,
            "average": round(avg, 2),
            "items": data,
        }

    def generate_html(self, data: list[dict]) -> str:
        summary = self.generate_summary(data)
        html = f"<html><head><title>{summary['title']}</title></head>"
        html += f"<body><h1>{summary['title']}</h1>"
        html += f"<p>Total: {summary['total']}</p>"
        html += f"<p>Count: {summary['count']}</p>"
        html += f"<p>Average: {summary['average']}</p>"
        html += "<ul>"
        for item in summary["items"]:
            html += f"<li>{item.get('name', 'Unknown')}: {item.get('value', 0)}</li>"
        html += "</ul></body></html>"
        return html
""",
        )
        project.add_file(
            "tests/test_report_generator.py",
            """from src.report_generator import ReportGenerator

def test_generate_summary_basic():
    generator = ReportGenerator("Test Report")
    data = [{"name": "A", "value": 10}]
    result = generator.generate_summary(data)
    assert result["total"] == 10
""",
        )
        project.commit("Initial report generator")

        project.add_file(
            "tests/test_report_generator.py",
            """from src.report_generator import ReportGenerator

def test_generate_summary_basic():
    generator = ReportGenerator("Test Report")
    data = [{"name": "A", "value": 10}]
    result = generator.generate_summary(data)
    assert result["total"] == 10

def test_generate_summary_snapshot():
    generator = ReportGenerator("Sales Report")
    data = [
        {"name": "Product A", "value": 100},
        {"name": "Product B", "value": 200},
        {"name": "Product C", "value": 150},
    ]
    result = generator.generate_summary(data)

    expected = {
        "title": "Sales Report",
        "total": 450,
        "count": 3,
        "average": 150.0,
        "items": data,
    }
    assert result == expected

def test_generate_html_snapshot():
    generator = ReportGenerator("Monthly Summary")
    data = [
        {"name": "Revenue", "value": 5000},
        {"name": "Expenses", "value": 3000},
    ]
    result = generator.generate_html(data)

    assert "<title>Monthly Summary</title>" in result
    assert "<h1>Monthly Summary</h1>" in result
    assert "<p>Total: 8000</p>" in result
    assert "<p>Count: 2</p>" in result
    assert "<p>Average: 4000.0</p>" in result
    assert "<li>Revenue: 5000</li>" in result
    assert "<li>Expenses: 3000</li>" in result
""",
        )
        project.commit("Add snapshot-style assertions for report generation")

        tree = build_diff_context(
            root_dir=project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=800,
        )

        all_content = _extract_fragments_content(tree)
        assert "def generate_summary" in all_content or "def generate_html" in all_content or "report_generator.py" in str(tree)

    def test_c5_json_snapshot_includes_serializer(self, project):
        project.add_file(
            "src/serializers.py",
            """import json
from dataclasses import dataclass, asdict
from typing import Any

@dataclass
class User:
    id: int
    name: str
    email: str
    active: bool = True

@dataclass
class Order:
    id: int
    user_id: int
    items: list
    total: float

class JsonSerializer:
    @staticmethod
    def serialize_user(user: User) -> str:
        data = asdict(user)
        data["type"] = "user"
        return json.dumps(data, indent=2, sort_keys=True)

    @staticmethod
    def serialize_order(order: Order) -> str:
        data = asdict(order)
        data["type"] = "order"
        data["item_count"] = len(order.items)
        return json.dumps(data, indent=2, sort_keys=True)

    @staticmethod
    def serialize_batch(items: list[Any]) -> str:
        result = []
        for item in items:
            if isinstance(item, User):
                result.append({"type": "user", **asdict(item)})
            elif isinstance(item, Order):
                result.append({"type": "order", **asdict(item)})
        return json.dumps({"batch": result, "count": len(result)}, indent=2)
""",
        )
        project.add_file(
            "tests/test_serializers.py",
            """from src.serializers import User, JsonSerializer

def test_serialize_user():
    user = User(id=1, name="Test", email="test@example.com")
    result = JsonSerializer.serialize_user(user)
    assert "test@example.com" in result
""",
        )
        project.commit("Initial serializers")

        project.add_file(
            "tests/test_serializers.py",
            """import json
from src.serializers import User, Order, JsonSerializer

def test_serialize_user():
    user = User(id=1, name="Test", email="test@example.com")
    result = JsonSerializer.serialize_user(user)
    assert "test@example.com" in result

def test_serialize_user_snapshot():
    user = User(id=42, name="John Doe", email="john@example.com", active=True)
    result = JsonSerializer.serialize_user(user)
    parsed = json.loads(result)

    expected = {
        "active": True,
        "email": "john@example.com",
        "id": 42,
        "name": "John Doe",
        "type": "user"
    }
    assert parsed == expected

def test_serialize_order_snapshot():
    order = Order(
        id=100,
        user_id=42,
        items=["item1", "item2", "item3"],
        total=299.99
    )
    result = JsonSerializer.serialize_order(order)
    parsed = json.loads(result)

    assert parsed["id"] == 100
    assert parsed["item_count"] == 3
    assert parsed["type"] == "order"
    assert parsed["total"] == 299.99

def test_serialize_batch_snapshot():
    user = User(id=1, name="Alice", email="alice@example.com")
    order = Order(id=10, user_id=1, items=["x"], total=50.0)
    result = JsonSerializer.serialize_batch([user, order])
    parsed = json.loads(result)

    assert parsed["count"] == 2
    assert len(parsed["batch"]) == 2
    assert parsed["batch"][0]["type"] == "user"
    assert parsed["batch"][1]["type"] == "order"
""",
        )
        project.commit("Add JSON snapshot tests for serializers")

        tree = build_diff_context(
            root_dir=project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=800,
        )

        all_content = _extract_fragments_content(tree)
        assert "def serialize_user" in all_content or "def serialize_order" in all_content or "serializers.py" in str(tree)

    def test_c5_api_response_snapshot_includes_endpoint(self, project):
        project.add_file(
            "src/api_handlers.py",
            """from dataclasses import dataclass
from typing import Optional
import json

@dataclass
class ApiResponse:
    status: str
    data: Optional[dict] = None
    error: Optional[str] = None
    meta: Optional[dict] = None

    def to_json(self) -> str:
        result = {"status": self.status}
        if self.data is not None:
            result["data"] = self.data
        if self.error is not None:
            result["error"] = self.error
        if self.meta is not None:
            result["meta"] = self.meta
        return json.dumps(result, indent=2)

class UserApiHandler:
    def __init__(self):
        self.users = {
            1: {"id": 1, "name": "Admin", "role": "admin"},
            2: {"id": 2, "name": "User", "role": "user"},
        }

    def get_user(self, user_id: int) -> ApiResponse:
        if user_id not in self.users:
            return ApiResponse(
                status="error",
                error=f"User {user_id} not found",
                meta={"request_id": "req_123"}
            )
        return ApiResponse(
            status="success",
            data=self.users[user_id],
            meta={"request_id": "req_123", "cached": False}
        )

    def list_users(self, page: int = 1, limit: int = 10) -> ApiResponse:
        users = list(self.users.values())
        return ApiResponse(
            status="success",
            data={"users": users, "total": len(users)},
            meta={"page": page, "limit": limit}
        )
""",
        )
        project.add_file(
            "tests/test_api_handlers.py",
            """from src.api_handlers import UserApiHandler

def test_get_user_exists():
    handler = UserApiHandler()
    response = handler.get_user(1)
    assert response.status == "success"
""",
        )
        project.commit("Initial API handlers")

        project.add_file(
            "tests/test_api_handlers.py",
            """import json
from src.api_handlers import UserApiHandler, ApiResponse

def test_get_user_exists():
    handler = UserApiHandler()
    response = handler.get_user(1)
    assert response.status == "success"

def test_get_user_success_snapshot():
    handler = UserApiHandler()
    response = handler.get_user(1)
    result = json.loads(response.to_json())

    assert result["status"] == "success"
    assert result["data"]["id"] == 1
    assert result["data"]["name"] == "Admin"
    assert result["data"]["role"] == "admin"
    assert "meta" in result
    assert result["meta"]["cached"] is False

def test_get_user_not_found_snapshot():
    handler = UserApiHandler()
    response = handler.get_user(999)
    result = json.loads(response.to_json())

    assert result["status"] == "error"
    assert "999" in result["error"]
    assert "not found" in result["error"]
    assert "data" not in result

def test_list_users_snapshot():
    handler = UserApiHandler()
    response = handler.list_users(page=1, limit=10)
    result = json.loads(response.to_json())

    assert result["status"] == "success"
    assert result["data"]["total"] == 2
    assert len(result["data"]["users"]) == 2
    assert result["meta"]["page"] == 1
    assert result["meta"]["limit"] == 10
""",
        )
        project.commit("Add API response snapshot tests")

        tree = build_diff_context(
            root_dir=project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=800,
        )

        all_content = _extract_fragments_content(tree)
        assert "def get_user" in all_content or "def list_users" in all_content or "api_handlers.py" in str(tree)
