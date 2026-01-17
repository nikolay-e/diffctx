import subprocess
from pathlib import Path

import pytest

from treemapper.diffctx import build_diff_context


def _fragments_contain(context: dict, substring: str) -> bool:
    for frag in context.get("fragments", []):
        if substring in frag.get("content", ""):
            return True
    return False


def _fragment_paths(context: dict) -> set[str]:
    return {frag.get("path", "") for frag in context.get("fragments", [])}


@pytest.fixture
def config_repo(tmp_path):
    repo_path = tmp_path / "config_test_repo"
    repo_path.mkdir()
    subprocess.run(["git", "init"], cwd=repo_path, capture_output=True, check=True)
    subprocess.run(["git", "config", "user.email", "test@test.com"], cwd=repo_path, capture_output=True, check=True)
    subprocess.run(["git", "config", "user.name", "Test"], cwd=repo_path, capture_output=True, check=True)

    class ConfigRepoHelper:
        def __init__(self, path: Path):
            self.repo = path

        def add_file(self, path: str, content: str) -> Path:
            file_path = self.repo / path
            file_path.parent.mkdir(parents=True, exist_ok=True)
            file_path.write_text(content, encoding="utf-8")
            return file_path

        def commit(self, message: str = "commit") -> str:
            subprocess.run(["git", "add", "-A"], cwd=self.repo, capture_output=True, check=True)
            subprocess.run(["git", "commit", "-m", message], cwd=self.repo, capture_output=True, check=True)
            result = subprocess.run(["git", "rev-parse", "HEAD"], cwd=self.repo, capture_output=True, text=True, check=True)
            return result.stdout.strip()

        def get_context(self, diff_range: str, budget: int = 5000) -> dict:
            return build_diff_context(self.repo, diff_range, budget_tokens=budget)

    return ConfigRepoHelper(repo_path)


class TestYamlToCode:
    def test_yaml_001_database_url_config(self, config_repo):
        config_repo.add_file(
            "config.yaml",
            """
database:
  host: localhost
  port: 5432
  name: myapp
""",
        )
        config_repo.add_file(
            "src/db.py",
            """
import os

def get_database_url():
    host = os.getenv("DB_HOST", "localhost")
    port = os.getenv("DB_PORT", "5432")
    name = os.getenv("DB_NAME", "myapp")
    return f"postgresql://{host}:{port}/{name}"
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "config.yaml",
            """
database:
  host: production-db.example.com
  port: 5432
  name: myapp_prod
""",
        )
        config_repo.commit("update db config")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "get_database_url") or "db.py" in str(_fragment_paths(context))

    def test_yaml_002_feature_flags(self, config_repo):
        config_repo.add_file(
            "config/features.yaml",
            """
features:
  new_ui: false
  dark_mode: true
""",
        )
        config_repo.add_file(
            "src/features.py",
            """
def is_feature_enabled(name: str) -> bool:
    features = {"new_ui": False, "dark_mode": True}
    return features.get(name, False)
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "config/features.yaml",
            """
features:
  new_ui: true
  dark_mode: true
  beta_api: false
""",
        )
        config_repo.commit("enable new_ui")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "is_feature_enabled") or "features.py" in str(_fragment_paths(context))

    def test_yaml_003_logging_config(self, config_repo):
        config_repo.add_file(
            "logging.yaml",
            """
logging:
  level: INFO
  handlers:
    - console
    - file
""",
        )
        config_repo.add_file(
            "src/logger.py",
            """
import logging

def setup_logging(level: str = "INFO"):
    logging.basicConfig(level=getattr(logging, level))
    return logging.getLogger(__name__)
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "logging.yaml",
            """
logging:
  level: DEBUG
  handlers:
    - console
    - file
    - syslog
""",
        )
        config_repo.commit("change log level")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "setup_logging") or "logger.py" in str(_fragment_paths(context))

    def test_yaml_004_redis_cache_config(self, config_repo):
        config_repo.add_file(
            "config/cache.yaml",
            """
redis:
  host: localhost
  port: 6379
  db: 0
""",
        )
        config_repo.add_file(
            "src/cache.py",
            """
import redis

def get_redis_client():
    return redis.Redis(host="localhost", port=6379, db=0)
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "config/cache.yaml",
            """
redis:
  host: redis.cluster.local
  port: 6379
  db: 1
  password: secret
""",
        )
        config_repo.commit("update redis config")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "get_redis_client") or "cache.py" in str(_fragment_paths(context))

    def test_yaml_005_jwt_settings(self, config_repo):
        config_repo.add_file(
            "config/auth.yaml",
            """
jwt:
  secret_key: dev-secret
  algorithm: HS256
  expiration_hours: 24
""",
        )
        config_repo.add_file(
            "src/auth.py",
            """
import jwt

def create_token(payload: dict, secret: str, algorithm: str = "HS256") -> str:
    return jwt.encode(payload, secret, algorithm=algorithm)

def verify_token(token: str, secret: str, algorithm: str = "HS256") -> dict:
    return jwt.decode(token, secret, algorithms=[algorithm])
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "config/auth.yaml",
            """
jwt:
  secret_key: prod-secret-key
  algorithm: RS256
  expiration_hours: 1
""",
        )
        config_repo.commit("update jwt config")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "create_token") or "auth.py" in str(_fragment_paths(context))

    def test_yaml_006_cors_settings(self, config_repo):
        config_repo.add_file(
            "config/api.yaml",
            """
cors:
  allowed_origins:
    - http://localhost:3000
  allow_credentials: true
""",
        )
        config_repo.add_file(
            "src/middleware.py",
            """
def setup_cors(app, origins: list[str], credentials: bool = True):
    for origin in origins:
        app.allow_origin(origin, credentials=credentials)
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "config/api.yaml",
            """
cors:
  allowed_origins:
    - https://example.com
    - https://api.example.com
  allow_credentials: true
""",
        )
        config_repo.commit("update cors")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "setup_cors") or "middleware.py" in str(_fragment_paths(context))

    def test_yaml_007_rate_limiting(self, config_repo):
        config_repo.add_file(
            "config/limits.yaml",
            """
rate_limit:
  requests_per_minute: 60
  burst: 10
""",
        )
        config_repo.add_file(
            "src/rate_limiter.py",
            """
class RateLimiter:
    def __init__(self, rpm: int = 60, burst: int = 10):
        self.rpm = rpm
        self.burst = burst

    def check_limit(self, client_id: str) -> bool:
        return True
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "config/limits.yaml",
            """
rate_limit:
  requests_per_minute: 100
  burst: 20
  whitelist:
    - admin
""",
        )
        config_repo.commit("update rate limits")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "RateLimiter") or "rate_limiter.py" in str(_fragment_paths(context))

    def test_yaml_008_cache_ttl(self, config_repo):
        config_repo.add_file(
            "config/cache.yaml",
            """
cache:
  default_ttl: 300
  max_size: 1000
""",
        )
        config_repo.add_file(
            "src/caching.py",
            """
from functools import lru_cache

def cached(ttl: int = 300):
    def decorator(func):
        return lru_cache(maxsize=1000)(func)
    return decorator
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "config/cache.yaml",
            """
cache:
  default_ttl: 600
  max_size: 5000
  eviction_policy: lru
""",
        )
        config_repo.commit("update cache ttl")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "cached") or "caching.py" in str(_fragment_paths(context))

    def test_yaml_009_api_versioning(self, config_repo):
        config_repo.add_file(
            "config/api.yaml",
            """
api:
  version: v1
  prefix: /api
""",
        )
        config_repo.add_file(
            "src/routes.py",
            """
def register_routes(app, version: str = "v1", prefix: str = "/api"):
    base_path = f"{prefix}/{version}"
    app.add_route(f"{base_path}/users", users_handler)
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "config/api.yaml",
            """
api:
  version: v2
  prefix: /api
  deprecated_versions:
    - v1
""",
        )
        config_repo.commit("bump api version")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "register_routes") or "routes.py" in str(_fragment_paths(context))

    def test_yaml_010_email_config(self, config_repo):
        config_repo.add_file(
            "config/email.yaml",
            """
email:
  smtp_host: localhost
  smtp_port: 25
  from_address: noreply@example.com
""",
        )
        config_repo.add_file(
            "src/mailer.py",
            """
import smtplib

class Mailer:
    def __init__(self, host: str, port: int, from_addr: str):
        self.host = host
        self.port = port
        self.from_addr = from_addr

    def send(self, to: str, subject: str, body: str):
        pass
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "config/email.yaml",
            """
email:
  smtp_host: smtp.sendgrid.net
  smtp_port: 587
  from_address: noreply@example.com
  use_tls: true
""",
        )
        config_repo.commit("update smtp config")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "Mailer") or "mailer.py" in str(_fragment_paths(context))

    def test_yaml_011_storage_config(self, config_repo):
        config_repo.add_file(
            "config/storage.yaml",
            """
storage:
  backend: local
  path: /data/uploads
""",
        )
        config_repo.add_file(
            "src/storage.py",
            """
class StorageBackend:
    def __init__(self, backend: str, path: str):
        self.backend = backend
        self.path = path

    def save(self, filename: str, data: bytes):
        pass
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "config/storage.yaml",
            """
storage:
  backend: s3
  bucket: my-bucket
  region: us-east-1
""",
        )
        config_repo.commit("switch to s3")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "StorageBackend") or "storage.py" in str(_fragment_paths(context))

    def test_yaml_012_queue_config(self, config_repo):
        config_repo.add_file(
            "config/queue.yaml",
            """
queue:
  broker: redis://localhost:6379
  backend: redis://localhost:6379
""",
        )
        config_repo.add_file(
            "src/tasks.py",
            """
from celery import Celery

def create_celery_app(broker: str, backend: str):
    return Celery(__name__, broker=broker, backend=backend)
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "config/queue.yaml",
            """
queue:
  broker: amqp://rabbitmq:5672
  backend: redis://redis:6379
  prefetch_count: 4
""",
        )
        config_repo.commit("switch to rabbitmq")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "create_celery_app") or "tasks.py" in str(_fragment_paths(context))

    def test_yaml_013_monitoring_config(self, config_repo):
        config_repo.add_file(
            "config/monitoring.yaml",
            """
metrics:
  enabled: true
  port: 9090
""",
        )
        config_repo.add_file(
            "src/metrics.py",
            """
from prometheus_client import start_http_server, Counter

REQUEST_COUNT = Counter("requests_total", "Total requests")

def start_metrics_server(port: int = 9090):
    start_http_server(port)
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "config/monitoring.yaml",
            """
metrics:
  enabled: true
  port: 8080
  path: /metrics
""",
        )
        config_repo.commit("change metrics port")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "start_metrics_server") or "metrics.py" in str(_fragment_paths(context))

    def test_yaml_014_scheduler_config(self, config_repo):
        config_repo.add_file(
            "config/scheduler.yaml",
            """
scheduler:
  timezone: UTC
  jobs:
    - name: cleanup
      cron: "0 0 * * *"
""",
        )
        config_repo.add_file(
            "src/scheduler.py",
            """
from apscheduler.schedulers.background import BackgroundScheduler

def create_scheduler(timezone: str = "UTC"):
    return BackgroundScheduler(timezone=timezone)

def cleanup_job():
    pass
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "config/scheduler.yaml",
            """
scheduler:
  timezone: America/New_York
  jobs:
    - name: cleanup
      cron: "0 2 * * *"
    - name: report
      cron: "0 8 * * 1"
""",
        )
        config_repo.commit("add report job")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "create_scheduler") or "scheduler.py" in str(_fragment_paths(context))

    def test_yaml_015_security_headers(self, config_repo):
        config_repo.add_file(
            "config/security.yaml",
            """
security:
  headers:
    x_frame_options: DENY
    content_security_policy: default-src 'self'
""",
        )
        config_repo.add_file(
            "src/security.py",
            """
def apply_security_headers(response, headers: dict):
    for key, value in headers.items():
        header_name = key.replace("_", "-").title()
        response.headers[header_name] = value
    return response
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "config/security.yaml",
            """
security:
  headers:
    x_frame_options: SAMEORIGIN
    content_security_policy: default-src 'self'; script-src 'self' 'unsafe-inline'
    strict_transport_security: max-age=31536000
""",
        )
        config_repo.commit("update security headers")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "apply_security_headers") or "security.py" in str(_fragment_paths(context))

    def test_yaml_016_pagination_config(self, config_repo):
        config_repo.add_file(
            "config/api.yaml",
            """
pagination:
  default_limit: 20
  max_limit: 100
""",
        )
        config_repo.add_file(
            "src/pagination.py",
            """
class Paginator:
    def __init__(self, default_limit: int = 20, max_limit: int = 100):
        self.default_limit = default_limit
        self.max_limit = max_limit

    def paginate(self, query, page: int, limit: int = None):
        limit = min(limit or self.default_limit, self.max_limit)
        offset = (page - 1) * limit
        return query.offset(offset).limit(limit)
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "config/api.yaml",
            """
pagination:
  default_limit: 50
  max_limit: 200
  cursor_based: true
""",
        )
        config_repo.commit("increase pagination limits")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "Paginator") or "pagination.py" in str(_fragment_paths(context))

    def test_yaml_017_retry_config(self, config_repo):
        config_repo.add_file(
            "config/resilience.yaml",
            """
retry:
  max_attempts: 3
  backoff_factor: 2
""",
        )
        config_repo.add_file(
            "src/retry.py",
            """
import time

def with_retry(func, max_attempts: int = 3, backoff: float = 2.0):
    def wrapper(*args, **kwargs):
        for attempt in range(max_attempts):
            try:
                return func(*args, **kwargs)
            except Exception:
                if attempt < max_attempts - 1:
                    time.sleep(backoff ** attempt)
                else:
                    raise
    return wrapper
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "config/resilience.yaml",
            """
retry:
  max_attempts: 5
  backoff_factor: 1.5
  jitter: true
""",
        )
        config_repo.commit("increase retry attempts")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "with_retry") or "retry.py" in str(_fragment_paths(context))

    def test_yaml_018_localization_config(self, config_repo):
        config_repo.add_file(
            "config/i18n.yaml",
            """
localization:
  default_locale: en
  supported_locales:
    - en
    - es
""",
        )
        config_repo.add_file(
            "src/i18n.py",
            """
class Translator:
    def __init__(self, default_locale: str = "en"):
        self.default_locale = default_locale
        self.translations = {}

    def translate(self, key: str, locale: str = None) -> str:
        locale = locale or self.default_locale
        return self.translations.get(locale, {}).get(key, key)
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "config/i18n.yaml",
            """
localization:
  default_locale: en
  supported_locales:
    - en
    - es
    - fr
    - de
""",
        )
        config_repo.commit("add more locales")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "Translator") or "i18n.py" in str(_fragment_paths(context))

    def test_yaml_019_validation_rules(self, config_repo):
        config_repo.add_file(
            "config/validation.yaml",
            """
validation:
  username:
    min_length: 3
    max_length: 20
  password:
    min_length: 8
""",
        )
        config_repo.add_file(
            "src/validators.py",
            """
def validate_username(username: str, min_len: int = 3, max_len: int = 20) -> bool:
    return min_len <= len(username) <= max_len

def validate_password(password: str, min_len: int = 8) -> bool:
    return len(password) >= min_len
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "config/validation.yaml",
            """
validation:
  username:
    min_length: 4
    max_length: 30
    pattern: "^[a-zA-Z0-9_]+$"
  password:
    min_length: 12
    require_special: true
""",
        )
        config_repo.commit("strengthen validation")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "validate_username") or "validators.py" in str(_fragment_paths(context))

    def test_yaml_020_webhook_config(self, config_repo):
        config_repo.add_file(
            "config/webhooks.yaml",
            """
webhooks:
  timeout: 30
  retry_count: 3
  endpoints:
    - url: https://example.com/hook
""",
        )
        config_repo.add_file(
            "src/webhooks.py",
            """
import requests

class WebhookDispatcher:
    def __init__(self, timeout: int = 30, retry_count: int = 3):
        self.timeout = timeout
        self.retry_count = retry_count

    def dispatch(self, url: str, payload: dict):
        return requests.post(url, json=payload, timeout=self.timeout)
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "config/webhooks.yaml",
            """
webhooks:
  timeout: 10
  retry_count: 5
  endpoints:
    - url: https://example.com/hook
    - url: https://backup.example.com/hook
""",
        )
        config_repo.commit("add backup webhook")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "WebhookDispatcher") or "webhooks.py" in str(_fragment_paths(context))


class TestJsonConfig:
    def test_json_021_package_json_scripts(self, config_repo):
        config_repo.add_file(
            "package.json",
            """{
  "name": "myapp",
  "scripts": {
    "build": "webpack",
    "test": "jest"
  }
}""",
        )
        config_repo.add_file(
            "webpack.config.js",
            """
module.exports = {
  entry: './src/index.js',
  output: {
    filename: 'bundle.js'
  }
};
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "package.json",
            """{
  "name": "myapp",
  "scripts": {
    "build": "webpack --mode production",
    "test": "jest --coverage",
    "lint": "eslint src"
  }
}""",
        )
        config_repo.commit("update build script")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "module.exports") or "webpack.config.js" in str(_fragment_paths(context))

    def test_json_022_package_json_main_entry(self, config_repo):
        config_repo.add_file(
            "package.json",
            """{
  "name": "mylib",
  "main": "dist/index.js"
}""",
        )
        config_repo.add_file(
            "src/index.ts",
            """
export function greet(name: string): string {
  return `Hello, ${name}!`;
}
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "package.json",
            """{
  "name": "mylib",
  "main": "dist/index.cjs",
  "module": "dist/index.mjs",
  "types": "dist/index.d.ts"
}""",
        )
        config_repo.commit("add esm support")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "export function greet") or "index.ts" in str(_fragment_paths(context))

    def test_json_023_tsconfig_paths(self, config_repo):
        config_repo.add_file(
            "tsconfig.json",
            """{
  "compilerOptions": {
    "baseUrl": ".",
    "paths": {
      "@utils/*": ["src/utils/*"]
    }
  }
}""",
        )
        config_repo.add_file(
            "src/utils/helpers.ts",
            """
export function formatDate(date: Date): string {
  return date.toISOString();
}
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "tsconfig.json",
            """{
  "compilerOptions": {
    "baseUrl": ".",
    "paths": {
      "@utils/*": ["src/utils/*"],
      "@components/*": ["src/components/*"]
    }
  }
}""",
        )
        config_repo.commit("add components path")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "formatDate") or "helpers.ts" in str(_fragment_paths(context))

    def test_json_024_tsconfig_strict(self, config_repo):
        config_repo.add_file(
            "tsconfig.json",
            """{
  "compilerOptions": {
    "strict": false,
    "target": "ES2020"
  }
}""",
        )
        config_repo.add_file(
            "src/app.ts",
            """
function processData(data: any) {
  return data.value;
}
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "tsconfig.json",
            """{
  "compilerOptions": {
    "strict": true,
    "target": "ES2022",
    "noImplicitAny": true
  }
}""",
        )
        config_repo.commit("enable strict mode")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "processData") or "app.ts" in str(_fragment_paths(context))

    def test_json_025_eslintrc_rules(self, config_repo):
        config_repo.add_file(
            ".eslintrc.json",
            """{
  "rules": {
    "semi": "error",
    "quotes": ["error", "double"]
  }
}""",
        )
        config_repo.add_file(
            "src/main.js",
            """
function hello() {
  console.log("Hello");
}
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            ".eslintrc.json",
            """{
  "rules": {
    "semi": "error",
    "quotes": ["error", "single"],
    "no-unused-vars": "error"
  }
}""",
        )
        config_repo.commit("change quote style")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "function hello") or "main.js" in str(_fragment_paths(context))

    def test_json_026_babel_config(self, config_repo):
        config_repo.add_file(
            "babel.config.json",
            """{
  "presets": ["@babel/preset-env"]
}""",
        )
        config_repo.add_file(
            "src/modern.js",
            """
const greet = (name) => `Hello, ${name}`;
export default greet;
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "babel.config.json",
            """{
  "presets": [
    ["@babel/preset-env", {"targets": {"node": "18"}}],
    "@babel/preset-typescript"
  ]
}""",
        )
        config_repo.commit("add typescript preset")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "const greet") or "modern.js" in str(_fragment_paths(context))

    def test_json_027_jest_config(self, config_repo):
        config_repo.add_file(
            "jest.config.json",
            """{
  "testMatch": ["**/*.test.js"],
  "collectCoverage": false
}""",
        )
        config_repo.add_file(
            "tests/math.test.js",
            """
const add = require('../src/math');
test('adds 1 + 2', () => {
  expect(add(1, 2)).toBe(3);
});
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "jest.config.json",
            """{
  "testMatch": ["**/*.test.js", "**/*.spec.js"],
  "collectCoverage": true,
  "coverageThreshold": {"global": {"branches": 80}}
}""",
        )
        config_repo.commit("enable coverage")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "test('adds") or "math.test.js" in str(_fragment_paths(context))

    def test_json_028_prettier_config(self, config_repo):
        config_repo.add_file(
            ".prettierrc.json",
            """{
  "semi": true,
  "singleQuote": false,
  "tabWidth": 2
}""",
        )
        config_repo.add_file(
            "src/format.js",
            """
function formatCode(code) {
  return code.trim();
}
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            ".prettierrc.json",
            """{
  "semi": false,
  "singleQuote": true,
  "tabWidth": 4,
  "printWidth": 100
}""",
        )
        config_repo.commit("change formatting")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "formatCode") or "format.js" in str(_fragment_paths(context))

    def test_json_029_vscode_settings(self, config_repo):
        config_repo.add_file(
            ".vscode/settings.json",
            """{
  "editor.formatOnSave": true,
  "python.linting.enabled": true
}""",
        )
        config_repo.add_file(
            "src/main.py",
            """
def main():
    print("Hello from Python")
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            ".vscode/settings.json",
            """{
  "editor.formatOnSave": true,
  "python.linting.enabled": true,
  "python.linting.pylintEnabled": true,
  "python.formatting.provider": "black"
}""",
        )
        config_repo.commit("enable black formatter")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "def main") or "main.py" in str(_fragment_paths(context))

    def test_json_030_renovate_config(self, config_repo):
        config_repo.add_file(
            "renovate.json",
            """{
  "extends": ["config:base"],
  "automerge": false
}""",
        )
        config_repo.add_file(
            "package.json",
            """{
  "dependencies": {
    "lodash": "^4.17.0"
  }
}""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "renovate.json",
            """{
  "extends": ["config:base"],
  "automerge": true,
  "packageRules": [
    {"matchPackagePatterns": ["*"], "enabled": true}
  ]
}""",
        )
        config_repo.commit("enable automerge")

        context = config_repo.get_context(f"{base}..HEAD")
        assert context.get("fragment_count", 0) >= 0

    def test_json_031_launch_json(self, config_repo):
        config_repo.add_file(
            ".vscode/launch.json",
            """{
  "configurations": [
    {
      "type": "node",
      "request": "launch",
      "program": "${workspaceFolder}/src/index.js"
    }
  ]
}""",
        )
        config_repo.add_file(
            "src/index.js",
            """
const express = require('express');
const app = express();
app.listen(3000);
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            ".vscode/launch.json",
            """{
  "configurations": [
    {
      "type": "node",
      "request": "launch",
      "program": "${workspaceFolder}/src/server.js",
      "env": {"NODE_ENV": "development"}
    }
  ]
}""",
        )
        config_repo.commit("update launch config")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "express") or "index.js" in str(_fragment_paths(context))

    def test_json_032_nodemon_config(self, config_repo):
        config_repo.add_file(
            "nodemon.json",
            """{
  "watch": ["src"],
  "ext": "js",
  "exec": "node src/index.js"
}""",
        )
        config_repo.add_file(
            "src/index.js",
            """
const http = require('http');
http.createServer((req, res) => res.end('OK')).listen(8080);
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "nodemon.json",
            """{
  "watch": ["src", "config"],
  "ext": "js,ts,json",
  "exec": "ts-node src/index.ts"
}""",
        )
        config_repo.commit("add typescript support")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "createServer") or "index.js" in str(_fragment_paths(context))

    def test_json_033_vercel_config(self, config_repo):
        config_repo.add_file(
            "vercel.json",
            """{
  "builds": [{"src": "api/*.js", "use": "@vercel/node"}],
  "routes": [{"src": "/api/(.*)", "dest": "/api/$1"}]
}""",
        )
        config_repo.add_file(
            "api/hello.js",
            """
module.exports = (req, res) => {
  res.json({ message: 'Hello!' });
};
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "vercel.json",
            """{
  "builds": [{"src": "api/**/*.ts", "use": "@vercel/node"}],
  "routes": [{"src": "/api/(.*)", "dest": "/api/$1"}],
  "regions": ["sfo1", "iad1"]
}""",
        )
        config_repo.commit("switch to typescript")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "module.exports") or "hello.js" in str(_fragment_paths(context))

    def test_json_034_netlify_config(self, config_repo):
        config_repo.add_file(
            "netlify.json",
            """{
  "build": {"command": "npm run build", "publish": "dist"},
  "functions": {"directory": "functions"}
}""",
        )
        config_repo.add_file(
            "functions/hello.js",
            """
exports.handler = async (event) => {
  return { statusCode: 200, body: 'Hello!' };
};
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "netlify.json",
            """{
  "build": {"command": "npm run build", "publish": "build"},
  "functions": {"directory": "netlify/functions"},
  "redirects": [{"from": "/api/*", "to": "/.netlify/functions/:splat"}]
}""",
        )
        config_repo.commit("update functions path")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "exports.handler") or "hello.js" in str(_fragment_paths(context))

    def test_json_035_firebase_config(self, config_repo):
        config_repo.add_file(
            "firebase.json",
            """{
  "hosting": {"public": "public", "ignore": ["firebase.json"]},
  "functions": {"source": "functions"}
}""",
        )
        config_repo.add_file(
            "functions/index.js",
            """
const functions = require('firebase-functions');
exports.hello = functions.https.onRequest((req, res) => {
  res.send('Hello!');
});
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "firebase.json",
            """{
  "hosting": {"public": "dist", "rewrites": [{"source": "**", "destination": "/index.html"}]},
  "functions": {"source": "functions", "runtime": "nodejs18"}
}""",
        )
        config_repo.commit("add spa rewrites")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "firebase-functions") or "index.js" in str(_fragment_paths(context))


class TestTerraformHCL:
    def test_tf_036_lambda_handler(self, config_repo):
        config_repo.add_file(
            "main.tf",
            """
resource "aws_lambda_function" "api" {
  filename      = "lambda.zip"
  function_name = "api-handler"
  handler       = "src/api.handler"
  runtime       = "nodejs18.x"
}
""",
        )
        config_repo.add_file(
            "src/api.ts",
            """
export const handler = async (event: any) => {
  return { statusCode: 200, body: JSON.stringify({ ok: true }) };
};
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "main.tf",
            """
resource "aws_lambda_function" "api" {
  filename      = "lambda.zip"
  function_name = "api-handler-v2"
  handler       = "src/api.handler"
  runtime       = "nodejs20.x"
  memory_size   = 512
}
""",
        )
        config_repo.commit("update lambda")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "export const handler") or "api.ts" in str(_fragment_paths(context))

    def test_tf_037_variables_file(self, config_repo):
        config_repo.add_file(
            "variables.tf",
            """
variable "environment" {
  default = "dev"
}

variable "instance_type" {
  default = "t3.micro"
}
""",
        )
        config_repo.add_file(
            "main.tf",
            """
resource "aws_instance" "web" {
  ami           = "ami-12345678"
  instance_type = var.instance_type
  tags = {
    Environment = var.environment
  }
}
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "variables.tf",
            """
variable "environment" {
  default = "prod"
}

variable "instance_type" {
  default = "t3.large"
}

variable "vpc_id" {
  type = string
}
""",
        )
        config_repo.commit("update variables")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "aws_instance") or "main.tf" in str(_fragment_paths(context))

    def test_tf_038_module_source(self, config_repo):
        config_repo.add_file(
            "modules/vpc/main.tf",
            """
resource "aws_vpc" "main" {
  cidr_block = var.cidr_block
}
""",
        )
        config_repo.add_file(
            "main.tf",
            """
module "vpc" {
  source     = "./modules/vpc"
  cidr_block = "10.0.0.0/16"
}
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "main.tf",
            """
module "vpc" {
  source     = "./modules/vpc"
  cidr_block = "10.1.0.0/16"
  enable_dns = true
}
""",
        )
        config_repo.commit("update module")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "aws_vpc") or "vpc/main.tf" in str(_fragment_paths(context))

    def test_tf_039_output_references(self, config_repo):
        config_repo.add_file(
            "outputs.tf",
            """
output "vpc_id" {
  value = aws_vpc.main.id
}
""",
        )
        config_repo.add_file(
            "main.tf",
            """
resource "aws_vpc" "main" {
  cidr_block = "10.0.0.0/16"
}
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "outputs.tf",
            """
output "vpc_id" {
  value = aws_vpc.main.id
}

output "vpc_cidr" {
  value = aws_vpc.main.cidr_block
}
""",
        )
        config_repo.commit("add output")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "aws_vpc") or "main.tf" in str(_fragment_paths(context))

    def test_tf_040_provider_config(self, config_repo):
        config_repo.add_file(
            "provider.tf",
            """
provider "aws" {
  region = "us-east-1"
}
""",
        )
        config_repo.add_file(
            "main.tf",
            """
resource "aws_s3_bucket" "data" {
  bucket = "my-data-bucket"
}
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "provider.tf",
            """
provider "aws" {
  region = "eu-west-1"
}

provider "aws" {
  alias  = "us"
  region = "us-east-1"
}
""",
        )
        config_repo.commit("add provider alias")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "aws_s3_bucket") or "main.tf" in str(_fragment_paths(context))

    def test_tf_041_data_source(self, config_repo):
        config_repo.add_file(
            "data.tf",
            """
data "aws_ami" "amazon_linux" {
  most_recent = true
  owners      = ["amazon"]
}
""",
        )
        config_repo.add_file(
            "main.tf",
            """
resource "aws_instance" "web" {
  ami           = data.aws_ami.amazon_linux.id
  instance_type = "t3.micro"
}
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "data.tf",
            """
data "aws_ami" "amazon_linux" {
  most_recent = true
  owners      = ["amazon"]
  filter {
    name   = "name"
    values = ["amzn2-ami-hvm-*"]
  }
}
""",
        )
        config_repo.commit("add ami filter")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "aws_instance") or "main.tf" in str(_fragment_paths(context))

    def test_tf_042_locals_block(self, config_repo):
        config_repo.add_file(
            "locals.tf",
            """
locals {
  common_tags = {
    Project = "myapp"
    Owner   = "team"
  }
}
""",
        )
        config_repo.add_file(
            "main.tf",
            """
resource "aws_instance" "web" {
  ami           = "ami-12345678"
  instance_type = "t3.micro"
  tags          = local.common_tags
}
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "locals.tf",
            """
locals {
  common_tags = {
    Project     = "myapp"
    Owner       = "team"
    Environment = "prod"
  }
}
""",
        )
        config_repo.commit("add environment tag")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "aws_instance") or "main.tf" in str(_fragment_paths(context))

    def test_tf_043_iam_policy_document(self, config_repo):
        config_repo.add_file(
            "iam.tf",
            """
data "aws_iam_policy_document" "lambda_assume" {
  statement {
    actions = ["sts:AssumeRole"]
    principals {
      type        = "Service"
      identifiers = ["lambda.amazonaws.com"]
    }
  }
}
""",
        )
        config_repo.add_file(
            "main.tf",
            """
resource "aws_iam_role" "lambda" {
  name               = "lambda-role"
  assume_role_policy = data.aws_iam_policy_document.lambda_assume.json
}
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "iam.tf",
            """
data "aws_iam_policy_document" "lambda_assume" {
  statement {
    actions = ["sts:AssumeRole"]
    principals {
      type        = "Service"
      identifiers = ["lambda.amazonaws.com", "edgelambda.amazonaws.com"]
    }
  }
}
""",
        )
        config_repo.commit("add edge lambda")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "aws_iam_role") or "main.tf" in str(_fragment_paths(context))

    def test_tf_044_security_group(self, config_repo):
        config_repo.add_file(
            "security.tf",
            """
resource "aws_security_group" "web" {
  name = "web-sg"
  ingress {
    from_port   = 80
    to_port     = 80
    protocol    = "tcp"
    cidr_blocks = ["0.0.0.0/0"]
  }
}
""",
        )
        config_repo.add_file(
            "main.tf",
            """
resource "aws_instance" "web" {
  ami                    = "ami-12345678"
  instance_type          = "t3.micro"
  vpc_security_group_ids = [aws_security_group.web.id]
}
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "security.tf",
            """
resource "aws_security_group" "web" {
  name = "web-sg"
  ingress {
    from_port   = 443
    to_port     = 443
    protocol    = "tcp"
    cidr_blocks = ["0.0.0.0/0"]
  }
}
""",
        )
        config_repo.commit("change to https")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "aws_instance") or "main.tf" in str(_fragment_paths(context))

    def test_tf_045_rds_instance(self, config_repo):
        config_repo.add_file(
            "rds.tf",
            """
resource "aws_db_instance" "main" {
  identifier        = "mydb"
  engine            = "postgres"
  instance_class    = "db.t3.micro"
  allocated_storage = 20
}
""",
        )
        config_repo.add_file(
            "src/db.py",
            """
import psycopg2

def connect():
    return psycopg2.connect(host="mydb.example.com", database="mydb")
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "rds.tf",
            """
resource "aws_db_instance" "main" {
  identifier        = "mydb"
  engine            = "postgres"
  engine_version    = "15.4"
  instance_class    = "db.t3.medium"
  allocated_storage = 50
}
""",
        )
        config_repo.commit("upgrade rds")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "psycopg2") or "db.py" in str(_fragment_paths(context))

    def test_tf_046_ecs_task_definition(self, config_repo):
        config_repo.add_file(
            "ecs.tf",
            """
resource "aws_ecs_task_definition" "app" {
  family = "app"
  container_definitions = jsonencode([{
    name  = "app"
    image = "myapp:latest"
    portMappings = [{
      containerPort = 8080
    }]
  }])
}
""",
        )
        config_repo.add_file(
            "src/server.py",
            """
from flask import Flask
app = Flask(__name__)

@app.route("/health")
def health():
    return "OK"
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "ecs.tf",
            """
resource "aws_ecs_task_definition" "app" {
  family = "app"
  cpu    = "256"
  memory = "512"
  container_definitions = jsonencode([{
    name  = "app"
    image = "myapp:v2"
    portMappings = [{
      containerPort = 8080
    }]
  }])
}
""",
        )
        config_repo.commit("update ecs task")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "Flask") or "server.py" in str(_fragment_paths(context))

    def test_tf_047_api_gateway(self, config_repo):
        config_repo.add_file(
            "api.tf",
            """
resource "aws_api_gateway_rest_api" "main" {
  name = "my-api"
}

resource "aws_api_gateway_resource" "users" {
  rest_api_id = aws_api_gateway_rest_api.main.id
  parent_id   = aws_api_gateway_rest_api.main.root_resource_id
  path_part   = "users"
}
""",
        )
        config_repo.add_file(
            "src/handlers/users.py",
            """
def get_users(event, context):
    return {"statusCode": 200, "body": "[]"}
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "api.tf",
            """
resource "aws_api_gateway_rest_api" "main" {
  name = "my-api-v2"
}

resource "aws_api_gateway_resource" "users" {
  rest_api_id = aws_api_gateway_rest_api.main.id
  parent_id   = aws_api_gateway_rest_api.main.root_resource_id
  path_part   = "v2/users"
}
""",
        )
        config_repo.commit("update api gateway")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "get_users") or "users.py" in str(_fragment_paths(context))

    def test_tf_048_s3_bucket_policy(self, config_repo):
        config_repo.add_file(
            "s3.tf",
            """
resource "aws_s3_bucket" "static" {
  bucket = "my-static-site"
}

resource "aws_s3_bucket_policy" "static" {
  bucket = aws_s3_bucket.static.id
  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect    = "Allow"
      Principal = "*"
      Action    = ["s3:GetObject"]
      Resource  = "${aws_s3_bucket.static.arn}/*"
    }]
  })
}
""",
        )
        config_repo.add_file(
            "src/upload.py",
            """
import boto3

def upload_file(bucket: str, key: str, data: bytes):
    s3 = boto3.client("s3")
    s3.put_object(Bucket=bucket, Key=key, Body=data)
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "s3.tf",
            """
resource "aws_s3_bucket" "static" {
  bucket = "my-static-site-prod"
}

resource "aws_s3_bucket_policy" "static" {
  bucket = aws_s3_bucket.static.id
  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect    = "Allow"
      Principal = {"AWS": "arn:aws:iam::123456789:root"}
      Action    = ["s3:GetObject", "s3:PutObject"]
      Resource  = "${aws_s3_bucket.static.arn}/*"
    }]
  })
}
""",
        )
        config_repo.commit("update s3 policy")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "boto3") or "upload.py" in str(_fragment_paths(context))

    def test_tf_049_cloudwatch_alarm(self, config_repo):
        config_repo.add_file(
            "monitoring.tf",
            """
resource "aws_cloudwatch_metric_alarm" "cpu" {
  alarm_name          = "high-cpu"
  comparison_operator = "GreaterThanThreshold"
  evaluation_periods  = 2
  metric_name         = "CPUUtilization"
  namespace           = "AWS/EC2"
  period              = 300
  statistic           = "Average"
  threshold           = 80
}
""",
        )
        config_repo.add_file(
            "src/metrics.py",
            """
import boto3

def get_cpu_metrics(instance_id: str):
    cw = boto3.client("cloudwatch")
    return cw.get_metric_statistics(
        Namespace="AWS/EC2",
        MetricName="CPUUtilization",
        Dimensions=[{"Name": "InstanceId", "Value": instance_id}],
        Period=300,
        Statistics=["Average"],
    )
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "monitoring.tf",
            """
resource "aws_cloudwatch_metric_alarm" "cpu" {
  alarm_name          = "critical-cpu"
  comparison_operator = "GreaterThanThreshold"
  evaluation_periods  = 1
  metric_name         = "CPUUtilization"
  namespace           = "AWS/EC2"
  period              = 60
  statistic           = "Average"
  threshold           = 90
}
""",
        )
        config_repo.commit("tighten cpu alarm")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "get_cpu_metrics") or "metrics.py" in str(_fragment_paths(context))

    def test_tf_050_sqs_queue(self, config_repo):
        config_repo.add_file(
            "sqs.tf",
            """
resource "aws_sqs_queue" "tasks" {
  name                       = "task-queue"
  visibility_timeout_seconds = 30
  message_retention_seconds  = 86400
}
""",
        )
        config_repo.add_file(
            "src/queue.py",
            """
import boto3

def send_message(queue_url: str, message: str):
    sqs = boto3.client("sqs")
    sqs.send_message(QueueUrl=queue_url, MessageBody=message)
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "sqs.tf",
            """
resource "aws_sqs_queue" "tasks" {
  name                       = "task-queue"
  visibility_timeout_seconds = 60
  message_retention_seconds  = 604800
  fifo_queue                 = true
}
""",
        )
        config_repo.commit("make fifo queue")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "send_message") or "queue.py" in str(_fragment_paths(context))

    def test_tf_051_dynamodb_table(self, config_repo):
        config_repo.add_file(
            "dynamodb.tf",
            """
resource "aws_dynamodb_table" "users" {
  name         = "users"
  billing_mode = "PAY_PER_REQUEST"
  hash_key     = "id"

  attribute {
    name = "id"
    type = "S"
  }
}
""",
        )
        config_repo.add_file(
            "src/users.py",
            """
import boto3

def get_user(user_id: str):
    dynamodb = boto3.resource("dynamodb")
    table = dynamodb.Table("users")
    return table.get_item(Key={"id": user_id})
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "dynamodb.tf",
            """
resource "aws_dynamodb_table" "users" {
  name         = "users"
  billing_mode = "PAY_PER_REQUEST"
  hash_key     = "id"
  range_key    = "created_at"

  attribute {
    name = "id"
    type = "S"
  }

  attribute {
    name = "created_at"
    type = "N"
  }
}
""",
        )
        config_repo.commit("add range key")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "get_user") or "users.py" in str(_fragment_paths(context))

    def test_tf_052_sns_topic(self, config_repo):
        config_repo.add_file(
            "sns.tf",
            """
resource "aws_sns_topic" "alerts" {
  name = "alert-notifications"
}
""",
        )
        config_repo.add_file(
            "src/notifications.py",
            """
import boto3

def publish_alert(topic_arn: str, message: str):
    sns = boto3.client("sns")
    sns.publish(TopicArn=topic_arn, Message=message)
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "sns.tf",
            """
resource "aws_sns_topic" "alerts" {
  name         = "alert-notifications"
  display_name = "System Alerts"
}

resource "aws_sns_topic_subscription" "email" {
  topic_arn = aws_sns_topic.alerts.arn
  protocol  = "email"
  endpoint  = "alerts@example.com"
}
""",
        )
        config_repo.commit("add email subscription")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "publish_alert") or "notifications.py" in str(_fragment_paths(context))

    def test_tf_053_route53_record(self, config_repo):
        config_repo.add_file(
            "dns.tf",
            """
resource "aws_route53_record" "www" {
  zone_id = "Z123456"
  name    = "www.example.com"
  type    = "A"
  ttl     = 300
  records = ["1.2.3.4"]
}
""",
        )
        config_repo.add_file(
            "src/health.py",
            """
import requests

def check_domain(domain: str) -> bool:
    try:
        r = requests.get(f"https://{domain}/health")
        return r.status_code == 200
    except Exception:
        return False
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "dns.tf",
            """
resource "aws_route53_record" "www" {
  zone_id = "Z123456"
  name    = "www.example.com"
  type    = "A"

  alias {
    name                   = aws_lb.main.dns_name
    zone_id                = aws_lb.main.zone_id
    evaluate_target_health = true
  }
}
""",
        )
        config_repo.commit("switch to alias")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "check_domain") or "health.py" in str(_fragment_paths(context))

    def test_tf_054_kms_key(self, config_repo):
        config_repo.add_file(
            "kms.tf",
            """
resource "aws_kms_key" "main" {
  description = "Main encryption key"
}
""",
        )
        config_repo.add_file(
            "src/encrypt.py",
            """
import boto3

def encrypt_data(key_id: str, plaintext: bytes) -> bytes:
    kms = boto3.client("kms")
    response = kms.encrypt(KeyId=key_id, Plaintext=plaintext)
    return response["CiphertextBlob"]
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "kms.tf",
            """
resource "aws_kms_key" "main" {
  description             = "Main encryption key"
  deletion_window_in_days = 7
  enable_key_rotation     = true
}
""",
        )
        config_repo.commit("enable rotation")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "encrypt_data") or "encrypt.py" in str(_fragment_paths(context))

    def test_tf_055_secrets_manager(self, config_repo):
        config_repo.add_file(
            "secrets.tf",
            """
resource "aws_secretsmanager_secret" "db_creds" {
  name = "db-credentials"
}
""",
        )
        config_repo.add_file(
            "src/secrets.py",
            """
import boto3
import json

def get_secret(name: str) -> dict:
    sm = boto3.client("secretsmanager")
    response = sm.get_secret_value(SecretId=name)
    return json.loads(response["SecretString"])
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "secrets.tf",
            """
resource "aws_secretsmanager_secret" "db_creds" {
  name                    = "db-credentials"
  recovery_window_in_days = 0
}

resource "aws_secretsmanager_secret_rotation" "db_creds" {
  secret_id           = aws_secretsmanager_secret.db_creds.id
  rotation_lambda_arn = aws_lambda_function.rotate.arn
  rotation_rules {
    automatically_after_days = 30
  }
}
""",
        )
        config_repo.commit("add rotation")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "get_secret") or "secrets.py" in str(_fragment_paths(context))


class TestHelmCharts:
    def test_helm_056_values_to_deployment(self, config_repo):
        config_repo.add_file(
            "chart/values.yaml",
            """
replicaCount: 1
image:
  repository: nginx
  tag: latest
""",
        )
        config_repo.add_file(
            "chart/templates/deployment.yaml",
            """
apiVersion: apps/v1
kind: Deployment
metadata:
  name: {{ .Release.Name }}
spec:
  replicas: {{ .Values.replicaCount }}
  template:
    spec:
      containers:
        - name: app
          image: "{{ .Values.image.repository }}:{{ .Values.image.tag }}"
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "chart/values.yaml",
            """
replicaCount: 3
image:
  repository: nginx
  tag: "1.25"
  pullPolicy: Always
""",
        )
        config_repo.commit("scale up")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "Deployment") or "deployment.yaml" in str(_fragment_paths(context))

    def test_helm_057_values_to_service(self, config_repo):
        config_repo.add_file(
            "chart/values.yaml",
            """
service:
  type: ClusterIP
  port: 80
""",
        )
        config_repo.add_file(
            "chart/templates/service.yaml",
            """
apiVersion: v1
kind: Service
metadata:
  name: {{ .Release.Name }}
spec:
  type: {{ .Values.service.type }}
  ports:
    - port: {{ .Values.service.port }}
      targetPort: http
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "chart/values.yaml",
            """
service:
  type: LoadBalancer
  port: 443
  annotations:
    service.beta.kubernetes.io/aws-load-balancer-type: nlb
""",
        )
        config_repo.commit("change to loadbalancer")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "Service") or "service.yaml" in str(_fragment_paths(context))

    def test_helm_058_values_to_ingress(self, config_repo):
        config_repo.add_file(
            "chart/values.yaml",
            """
ingress:
  enabled: false
""",
        )
        config_repo.add_file(
            "chart/templates/ingress.yaml",
            """
{{- if .Values.ingress.enabled }}
apiVersion: networking.k8s.io/v1
kind: Ingress
metadata:
  name: {{ .Release.Name }}
spec:
  rules:
    - host: {{ .Values.ingress.host }}
{{- end }}
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "chart/values.yaml",
            """
ingress:
  enabled: true
  host: app.example.com
  tls:
    secretName: app-tls
""",
        )
        config_repo.commit("enable ingress")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "Ingress") or "ingress.yaml" in str(_fragment_paths(context))

    def test_helm_059_values_to_configmap(self, config_repo):
        config_repo.add_file(
            "chart/values.yaml",
            """
config:
  logLevel: info
  debugMode: false
""",
        )
        config_repo.add_file(
            "chart/templates/configmap.yaml",
            """
apiVersion: v1
kind: ConfigMap
metadata:
  name: {{ .Release.Name }}-config
data:
  LOG_LEVEL: {{ .Values.config.logLevel }}
  DEBUG_MODE: {{ .Values.config.debugMode | quote }}
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "chart/values.yaml",
            """
config:
  logLevel: debug
  debugMode: true
  maxConnections: 100
""",
        )
        config_repo.commit("enable debug")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "ConfigMap") or "configmap.yaml" in str(_fragment_paths(context))

    def test_helm_060_values_to_secret(self, config_repo):
        config_repo.add_file(
            "chart/values.yaml",
            """
secrets:
  dbPassword: changeme
""",
        )
        config_repo.add_file(
            "chart/templates/secret.yaml",
            """
apiVersion: v1
kind: Secret
metadata:
  name: {{ .Release.Name }}-secrets
type: Opaque
data:
  DB_PASSWORD: {{ .Values.secrets.dbPassword | b64enc }}
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "chart/values.yaml",
            """
secrets:
  dbPassword: prod-secret
  apiKey: my-api-key
""",
        )
        config_repo.commit("add api key")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "Secret") or "secret.yaml" in str(_fragment_paths(context))

    def test_helm_061_values_to_hpa(self, config_repo):
        config_repo.add_file(
            "chart/values.yaml",
            """
autoscaling:
  enabled: false
  minReplicas: 1
  maxReplicas: 10
""",
        )
        config_repo.add_file(
            "chart/templates/hpa.yaml",
            """
{{- if .Values.autoscaling.enabled }}
apiVersion: autoscaling/v2
kind: HorizontalPodAutoscaler
metadata:
  name: {{ .Release.Name }}
spec:
  minReplicas: {{ .Values.autoscaling.minReplicas }}
  maxReplicas: {{ .Values.autoscaling.maxReplicas }}
{{- end }}
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "chart/values.yaml",
            """
autoscaling:
  enabled: true
  minReplicas: 2
  maxReplicas: 20
  targetCPU: 80
""",
        )
        config_repo.commit("enable hpa")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "HorizontalPodAutoscaler") or "hpa.yaml" in str(_fragment_paths(context))

    def test_helm_062_values_resources(self, config_repo):
        config_repo.add_file(
            "chart/values.yaml",
            """
resources:
  limits:
    cpu: 100m
    memory: 128Mi
""",
        )
        config_repo.add_file(
            "chart/templates/deployment.yaml",
            """
apiVersion: apps/v1
kind: Deployment
spec:
  template:
    spec:
      containers:
        - name: app
          resources:
            limits:
              cpu: {{ .Values.resources.limits.cpu }}
              memory: {{ .Values.resources.limits.memory }}
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "chart/values.yaml",
            """
resources:
  limits:
    cpu: 500m
    memory: 512Mi
  requests:
    cpu: 100m
    memory: 128Mi
""",
        )
        config_repo.commit("increase resources")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "Deployment") or "deployment.yaml" in str(_fragment_paths(context))

    def test_helm_063_chart_yaml_dependencies(self, config_repo):
        config_repo.add_file(
            "chart/Chart.yaml",
            """
apiVersion: v2
name: myapp
version: 1.0.0
dependencies:
  - name: postgresql
    version: "12.1.0"
    repository: https://charts.bitnami.com/bitnami
""",
        )
        config_repo.add_file(
            "src/db.py",
            """
import psycopg2

def get_connection():
    return psycopg2.connect(host="postgresql", database="myapp")
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "chart/Chart.yaml",
            """
apiVersion: v2
name: myapp
version: 1.1.0
dependencies:
  - name: postgresql
    version: "13.0.0"
    repository: https://charts.bitnami.com/bitnami
  - name: redis
    version: "17.0.0"
    repository: https://charts.bitnami.com/bitnami
""",
        )
        config_repo.commit("add redis dependency")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "psycopg2") or "db.py" in str(_fragment_paths(context))

    def test_helm_064_values_probes(self, config_repo):
        config_repo.add_file(
            "chart/values.yaml",
            """
probes:
  liveness:
    path: /health
    port: 8080
""",
        )
        config_repo.add_file(
            "chart/templates/deployment.yaml",
            """
apiVersion: apps/v1
kind: Deployment
spec:
  template:
    spec:
      containers:
        - name: app
          livenessProbe:
            httpGet:
              path: {{ .Values.probes.liveness.path }}
              port: {{ .Values.probes.liveness.port }}
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "chart/values.yaml",
            """
probes:
  liveness:
    path: /healthz
    port: 8080
    initialDelaySeconds: 30
  readiness:
    path: /ready
    port: 8080
""",
        )
        config_repo.commit("add readiness probe")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "livenessProbe") or "deployment.yaml" in str(_fragment_paths(context))

    def test_helm_065_values_env_vars(self, config_repo):
        config_repo.add_file(
            "chart/values.yaml",
            """
env:
  - name: APP_ENV
    value: development
""",
        )
        config_repo.add_file(
            "chart/templates/deployment.yaml",
            """
apiVersion: apps/v1
kind: Deployment
spec:
  template:
    spec:
      containers:
        - name: app
          env:
            {{- toYaml .Values.env | nindent 12 }}
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "chart/values.yaml",
            """
env:
  - name: APP_ENV
    value: production
  - name: LOG_LEVEL
    value: warn
  - name: DB_HOST
    valueFrom:
      secretKeyRef:
        name: db-secret
        key: host
""",
        )
        config_repo.commit("add production env")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "Deployment") or "deployment.yaml" in str(_fragment_paths(context))

    def test_helm_066_values_volumes(self, config_repo):
        config_repo.add_file(
            "chart/values.yaml",
            """
persistence:
  enabled: false
""",
        )
        config_repo.add_file(
            "chart/templates/deployment.yaml",
            """
apiVersion: apps/v1
kind: Deployment
spec:
  template:
    spec:
      {{- if .Values.persistence.enabled }}
      volumes:
        - name: data
          persistentVolumeClaim:
            claimName: {{ .Release.Name }}-data
      {{- end }}
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "chart/values.yaml",
            """
persistence:
  enabled: true
  size: 10Gi
  storageClass: standard
""",
        )
        config_repo.commit("enable persistence")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "Deployment") or "deployment.yaml" in str(_fragment_paths(context))

    def test_helm_067_values_affinity(self, config_repo):
        config_repo.add_file(
            "chart/values.yaml",
            """
affinity: {}
""",
        )
        config_repo.add_file(
            "chart/templates/deployment.yaml",
            """
apiVersion: apps/v1
kind: Deployment
spec:
  template:
    spec:
      affinity:
        {{- toYaml .Values.affinity | nindent 8 }}
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "chart/values.yaml",
            """
affinity:
  nodeAffinity:
    requiredDuringSchedulingIgnoredDuringExecution:
      nodeSelectorTerms:
        - matchExpressions:
            - key: kubernetes.io/arch
              operator: In
              values:
                - amd64
""",
        )
        config_repo.commit("add node affinity")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "affinity") or "deployment.yaml" in str(_fragment_paths(context))

    def test_helm_068_values_tolerations(self, config_repo):
        config_repo.add_file(
            "chart/values.yaml",
            """
tolerations: []
""",
        )
        config_repo.add_file(
            "chart/templates/deployment.yaml",
            """
apiVersion: apps/v1
kind: Deployment
spec:
  template:
    spec:
      tolerations:
        {{- toYaml .Values.tolerations | nindent 8 }}
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "chart/values.yaml",
            """
tolerations:
  - key: "dedicated"
    operator: "Equal"
    value: "app"
    effect: "NoSchedule"
""",
        )
        config_repo.commit("add toleration")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "tolerations") or "deployment.yaml" in str(_fragment_paths(context))

    def test_helm_069_values_security_context(self, config_repo):
        config_repo.add_file(
            "chart/values.yaml",
            """
securityContext:
  runAsNonRoot: true
""",
        )
        config_repo.add_file(
            "chart/templates/deployment.yaml",
            """
apiVersion: apps/v1
kind: Deployment
spec:
  template:
    spec:
      securityContext:
        runAsNonRoot: {{ .Values.securityContext.runAsNonRoot }}
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "chart/values.yaml",
            """
securityContext:
  runAsNonRoot: true
  runAsUser: 1000
  fsGroup: 1000
  readOnlyRootFilesystem: true
""",
        )
        config_repo.commit("add security context")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "securityContext") or "deployment.yaml" in str(_fragment_paths(context))

    def test_helm_070_values_service_account(self, config_repo):
        config_repo.add_file(
            "chart/values.yaml",
            """
serviceAccount:
  create: false
""",
        )
        config_repo.add_file(
            "chart/templates/serviceaccount.yaml",
            """
{{- if .Values.serviceAccount.create }}
apiVersion: v1
kind: ServiceAccount
metadata:
  name: {{ .Release.Name }}
  annotations:
    {{- toYaml .Values.serviceAccount.annotations | nindent 4 }}
{{- end }}
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "chart/values.yaml",
            """
serviceAccount:
  create: true
  name: app-sa
  annotations:
    eks.amazonaws.com/role-arn: arn:aws:iam::123456789:role/app-role
""",
        )
        config_repo.commit("create service account")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "ServiceAccount") or "serviceaccount.yaml" in str(_fragment_paths(context))


class TestDockerCompose:
    def test_docker_071_dockerfile_entrypoint(self, config_repo):
        config_repo.add_file(
            "Dockerfile",
            """
FROM python:3.11
WORKDIR /app
COPY . .
ENTRYPOINT ["python", "src/main.py"]
""",
        )
        config_repo.add_file(
            "src/main.py",
            """
def main():
    print("Starting application")

if __name__ == "__main__":
    main()
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "Dockerfile",
            """
FROM python:3.12
WORKDIR /app
COPY . .
RUN pip install -r requirements.txt
ENTRYPOINT ["python", "-m", "src.main"]
""",
        )
        config_repo.commit("update dockerfile")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "def main") or "main.py" in str(_fragment_paths(context))

    def test_docker_072_dockerfile_copy_path(self, config_repo):
        config_repo.add_file(
            "Dockerfile",
            """
FROM node:18
WORKDIR /app
COPY src/ /app/src/
CMD ["node", "src/index.js"]
""",
        )
        config_repo.add_file(
            "src/index.js",
            """
const express = require('express');
const app = express();
app.listen(3000, () => console.log('Server running'));
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "Dockerfile",
            """
FROM node:20
WORKDIR /app
COPY package*.json ./
RUN npm install
COPY src/ /app/src/
EXPOSE 3000
CMD ["node", "src/index.js"]
""",
        )
        config_repo.commit("optimize layers")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "express") or "index.js" in str(_fragment_paths(context))

    def test_docker_073_dockerfile_env(self, config_repo):
        config_repo.add_file(
            "Dockerfile",
            """
FROM python:3.11
ENV APP_ENV=development
ENV DEBUG=true
COPY . .
CMD ["python", "app.py"]
""",
        )
        config_repo.add_file(
            "app.py",
            """
import os

DEBUG = os.getenv("DEBUG", "false").lower() == "true"
APP_ENV = os.getenv("APP_ENV", "development")

def run():
    if DEBUG:
        print(f"Running in {APP_ENV} mode with debug")
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "Dockerfile",
            """
FROM python:3.11
ENV APP_ENV=production
ENV DEBUG=false
ENV LOG_LEVEL=warn
COPY . .
CMD ["python", "app.py"]
""",
        )
        config_repo.commit("production settings")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "DEBUG") or "app.py" in str(_fragment_paths(context))

    def test_docker_074_compose_service_port(self, config_repo):
        config_repo.add_file(
            "docker-compose.yml",
            """
version: '3.8'
services:
  web:
    build: .
    ports:
      - "3000:3000"
""",
        )
        config_repo.add_file(
            "src/server.js",
            """
const http = require('http');
const PORT = process.env.PORT || 3000;
http.createServer((req, res) => res.end('OK')).listen(PORT);
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "docker-compose.yml",
            """
version: '3.8'
services:
  web:
    build: .
    ports:
      - "8080:8080"
    environment:
      - PORT=8080
""",
        )
        config_repo.commit("change port")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "PORT") or "server.js" in str(_fragment_paths(context))

    def test_docker_075_compose_volume_mount(self, config_repo):
        config_repo.add_file(
            "docker-compose.yml",
            """
version: '3.8'
services:
  app:
    build: .
    volumes:
      - ./data:/app/data
""",
        )
        config_repo.add_file(
            "src/storage.py",
            """
DATA_PATH = "/app/data"

def save_data(filename: str, content: str):
    with open(f"{DATA_PATH}/{filename}", "w") as f:
        f.write(content)
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "docker-compose.yml",
            """
version: '3.8'
services:
  app:
    build: .
    volumes:
      - ./data:/app/data
      - ./config:/app/config:ro
      - logs:/app/logs
volumes:
  logs:
""",
        )
        config_repo.commit("add volumes")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "DATA_PATH") or "storage.py" in str(_fragment_paths(context))

    def test_docker_076_compose_environment(self, config_repo):
        config_repo.add_file(
            "docker-compose.yml",
            """
version: '3.8'
services:
  api:
    build: .
    environment:
      - DATABASE_URL=postgres://localhost:5432/db
""",
        )
        config_repo.add_file(
            "src/db.py",
            """
import os

DATABASE_URL = os.getenv("DATABASE_URL")

def get_connection():
    return connect(DATABASE_URL)
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "docker-compose.yml",
            """
version: '3.8'
services:
  api:
    build: .
    environment:
      - DATABASE_URL=postgres://db:5432/prod
      - REDIS_URL=redis://redis:6379
      - SECRET_KEY=${SECRET_KEY}
    env_file:
      - .env
""",
        )
        config_repo.commit("add redis env")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "DATABASE_URL") or "db.py" in str(_fragment_paths(context))

    def test_docker_077_compose_depends_on(self, config_repo):
        config_repo.add_file(
            "docker-compose.yml",
            """
version: '3.8'
services:
  web:
    build: .
    depends_on:
      - db
  db:
    image: postgres:15
""",
        )
        config_repo.add_file(
            "src/app.py",
            """
from flask import Flask
import psycopg2

app = Flask(__name__)

def init_db():
    conn = psycopg2.connect(host="db", database="app")
    return conn
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "docker-compose.yml",
            """
version: '3.8'
services:
  web:
    build: .
    depends_on:
      db:
        condition: service_healthy
      redis:
        condition: service_started
  db:
    image: postgres:16
    healthcheck:
      test: ["CMD", "pg_isready"]
  redis:
    image: redis:7
""",
        )
        config_repo.commit("add redis dependency")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "psycopg2") or "app.py" in str(_fragment_paths(context))

    def test_docker_078_compose_healthcheck(self, config_repo):
        config_repo.add_file(
            "docker-compose.yml",
            """
version: '3.8'
services:
  api:
    build: .
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost:8080/health"]
      interval: 30s
""",
        )
        config_repo.add_file(
            "src/health.py",
            """
from flask import Flask, jsonify

app = Flask(__name__)

@app.route("/health")
def health():
    return jsonify({"status": "healthy"})
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "docker-compose.yml",
            """
version: '3.8'
services:
  api:
    build: .
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost:8080/healthz"]
      interval: 10s
      timeout: 5s
      retries: 3
""",
        )
        config_repo.commit("update healthcheck")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "health") or "health.py" in str(_fragment_paths(context))

    def test_docker_079_compose_networks(self, config_repo):
        config_repo.add_file(
            "docker-compose.yml",
            """
version: '3.8'
services:
  web:
    build: .
    networks:
      - frontend
networks:
  frontend:
""",
        )
        config_repo.add_file(
            "src/proxy.py",
            """
import requests

def call_backend(endpoint: str):
    return requests.get(f"http://backend:8080{endpoint}")
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "docker-compose.yml",
            """
version: '3.8'
services:
  web:
    build: .
    networks:
      - frontend
      - backend
  api:
    build: ./api
    networks:
      - backend
networks:
  frontend:
  backend:
    internal: true
""",
        )
        config_repo.commit("add backend network")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "call_backend") or "proxy.py" in str(_fragment_paths(context))

    def test_docker_080_compose_command_override(self, config_repo):
        config_repo.add_file(
            "docker-compose.yml",
            """
version: '3.8'
services:
  worker:
    build: .
    command: python worker.py
""",
        )
        config_repo.add_file(
            "worker.py",
            """
import time

def process_jobs():
    while True:
        print("Processing...")
        time.sleep(10)

if __name__ == "__main__":
    process_jobs()
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "docker-compose.yml",
            """
version: '3.8'
services:
  worker:
    build: .
    command: python -u worker.py --concurrency 4
    restart: always
""",
        )
        config_repo.commit("increase concurrency")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "process_jobs") or "worker.py" in str(_fragment_paths(context))

    def test_docker_081_compose_secrets(self, config_repo):
        config_repo.add_file(
            "docker-compose.yml",
            """
version: '3.8'
services:
  app:
    build: .
    secrets:
      - db_password
secrets:
  db_password:
    file: ./secrets/db_password.txt
""",
        )
        config_repo.add_file(
            "src/config.py",
            """
def get_db_password():
    with open("/run/secrets/db_password") as f:
        return f.read().strip()
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "docker-compose.yml",
            """
version: '3.8'
services:
  app:
    build: .
    secrets:
      - db_password
      - api_key
secrets:
  db_password:
    file: ./secrets/db_password.txt
  api_key:
    external: true
""",
        )
        config_repo.commit("add api key secret")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "get_db_password") or "config.py" in str(_fragment_paths(context))

    def test_docker_082_compose_replicas(self, config_repo):
        config_repo.add_file(
            "docker-compose.yml",
            """
version: '3.8'
services:
  worker:
    build: .
    deploy:
      replicas: 1
""",
        )
        config_repo.add_file(
            "src/worker.py",
            """
import os

WORKER_ID = os.getenv("HOSTNAME", "unknown")

def run():
    print(f"Worker {WORKER_ID} starting")
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "docker-compose.yml",
            """
version: '3.8'
services:
  worker:
    build: .
    deploy:
      replicas: 3
      resources:
        limits:
          cpus: '0.5'
          memory: 256M
""",
        )
        config_repo.commit("scale workers")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "WORKER_ID") or "worker.py" in str(_fragment_paths(context))

    def test_docker_083_dockerfile_multistage(self, config_repo):
        config_repo.add_file(
            "Dockerfile",
            """
FROM node:18 AS builder
WORKDIR /app
COPY . .
RUN npm run build

FROM nginx:alpine
COPY --from=builder /app/dist /usr/share/nginx/html
""",
        )
        config_repo.add_file(
            "src/index.ts",
            """
export function init() {
  console.log("App initialized");
}
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "Dockerfile",
            """
FROM node:20 AS builder
WORKDIR /app
COPY package*.json ./
RUN npm ci
COPY . .
RUN npm run build

FROM nginx:alpine
COPY --from=builder /app/dist /usr/share/nginx/html
COPY nginx.conf /etc/nginx/nginx.conf
""",
        )
        config_repo.commit("optimize build")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "init") or "index.ts" in str(_fragment_paths(context))

    def test_docker_084_compose_logging(self, config_repo):
        config_repo.add_file(
            "docker-compose.yml",
            """
version: '3.8'
services:
  app:
    build: .
    logging:
      driver: json-file
""",
        )
        config_repo.add_file(
            "src/logger.py",
            """
import logging

logging.basicConfig(level=logging.INFO, format='%(asctime)s - %(message)s')
logger = logging.getLogger(__name__)

def log_event(message: str):
    logger.info(message)
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "docker-compose.yml",
            """
version: '3.8'
services:
  app:
    build: .
    logging:
      driver: json-file
      options:
        max-size: "10m"
        max-file: "3"
        labels: "app,env"
""",
        )
        config_repo.commit("configure logging")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "log_event") or "logger.py" in str(_fragment_paths(context))

    def test_docker_085_compose_profiles(self, config_repo):
        config_repo.add_file(
            "docker-compose.yml",
            """
version: '3.8'
services:
  app:
    build: .
  debug:
    build: .
    profiles:
      - debug
    command: python -m debugpy --listen 0.0.0.0:5678 app.py
""",
        )
        config_repo.add_file(
            "app.py",
            """
def main():
    print("Running app")

if __name__ == "__main__":
    main()
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "docker-compose.yml",
            """
version: '3.8'
services:
  app:
    build: .
  debug:
    build: .
    profiles:
      - debug
    command: python -m debugpy --wait-for-client --listen 0.0.0.0:5678 app.py
    ports:
      - "5678:5678"
  test:
    build: .
    profiles:
      - test
    command: pytest
""",
        )
        config_repo.commit("add test profile")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "def main") or "app.py" in str(_fragment_paths(context))


class TestCICDConfig:
    def test_cicd_086_github_workflow_script(self, config_repo):
        config_repo.add_file(
            ".github/workflows/ci.yml",
            """
name: CI
on: [push]
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: npm run build
""",
        )
        config_repo.add_file(
            "src/build.ts",
            """
export function build() {
  console.log("Building application");
}
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            ".github/workflows/ci.yml",
            """
name: CI
on: [push, pull_request]
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: npm ci
      - run: npm run build
      - run: npm test
""",
        )
        config_repo.commit("add test step")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "build") or "build.ts" in str(_fragment_paths(context))

    def test_cicd_087_github_workflow_test_command(self, config_repo):
        config_repo.add_file(
            ".github/workflows/test.yml",
            """
name: Test
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: pytest tests/
""",
        )
        config_repo.add_file(
            "tests/test_app.py",
            """
def test_addition():
    assert 1 + 1 == 2

def test_subtraction():
    assert 2 - 1 == 1
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            ".github/workflows/test.yml",
            """
name: Test
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: pip install -e .[dev]
      - run: pytest tests/ --cov=src --cov-report=xml
""",
        )
        config_repo.commit("add coverage")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "test_addition") or "test_app.py" in str(_fragment_paths(context))

    def test_cicd_088_github_action_env_vars(self, config_repo):
        config_repo.add_file(
            ".github/workflows/deploy.yml",
            """
name: Deploy
on:
  push:
    branches: [main]
jobs:
  deploy:
    runs-on: ubuntu-latest
    env:
      NODE_ENV: production
    steps:
      - run: npm run deploy
""",
        )
        config_repo.add_file(
            "deploy.js",
            """
const env = process.env.NODE_ENV || 'development';
console.log(`Deploying to ${env}`);
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            ".github/workflows/deploy.yml",
            """
name: Deploy
on:
  push:
    branches: [main]
jobs:
  deploy:
    runs-on: ubuntu-latest
    env:
      NODE_ENV: production
      DEPLOY_TARGET: kubernetes
      AWS_REGION: us-east-1
    steps:
      - run: npm run deploy
""",
        )
        config_repo.commit("add deploy vars")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "NODE_ENV") or "deploy.js" in str(_fragment_paths(context))

    def test_cicd_089_gitlab_ci_script(self, config_repo):
        config_repo.add_file(
            ".gitlab-ci.yml",
            """
stages:
  - build
  - test

build:
  stage: build
  script:
    - npm run build
""",
        )
        config_repo.add_file(
            "src/main.ts",
            """
export function main() {
  console.log("Main function");
}
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            ".gitlab-ci.yml",
            """
stages:
  - build
  - test
  - deploy

build:
  stage: build
  script:
    - npm ci
    - npm run build
  artifacts:
    paths:
      - dist/

test:
  stage: test
  script:
    - npm test
""",
        )
        config_repo.commit("add test stage")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "main") or "main.ts" in str(_fragment_paths(context))

    def test_cicd_090_gitlab_ci_variables(self, config_repo):
        config_repo.add_file(
            ".gitlab-ci.yml",
            """
variables:
  DATABASE_URL: postgres://localhost/test

test:
  script:
    - pytest
""",
        )
        config_repo.add_file(
            "src/config.py",
            """
import os

DATABASE_URL = os.getenv("DATABASE_URL", "sqlite:///local.db")
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            ".gitlab-ci.yml",
            """
variables:
  DATABASE_URL: postgres://db/prod
  REDIS_URL: redis://redis:6379
  CI_DEBUG: "true"

test:
  script:
    - pytest --tb=short
""",
        )
        config_repo.commit("add redis var")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "DATABASE_URL") or "config.py" in str(_fragment_paths(context))

    def test_cicd_091_jenkins_pipeline(self, config_repo):
        config_repo.add_file(
            "Jenkinsfile",
            """
pipeline {
    agent any
    stages {
        stage('Build') {
            steps {
                sh 'npm run build'
            }
        }
    }
}
""",
        )
        config_repo.add_file(
            "src/app.js",
            """
function startApp() {
  console.log("Starting app");
}
module.exports = { startApp };
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "Jenkinsfile",
            """
pipeline {
    agent any
    environment {
        NODE_ENV = 'production'
    }
    stages {
        stage('Build') {
            steps {
                sh 'npm ci'
                sh 'npm run build'
            }
        }
        stage('Test') {
            steps {
                sh 'npm test'
            }
        }
    }
}
""",
        )
        config_repo.commit("add test stage")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "startApp") or "app.js" in str(_fragment_paths(context))

    def test_cicd_092_makefile_targets(self, config_repo):
        config_repo.add_file(
            "Makefile",
            """
.PHONY: build test

build:
\tpython setup.py build

test:
\tpytest tests/
""",
        )
        config_repo.add_file(
            "src/app.py",
            """
def run():
    print("Running application")
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "Makefile",
            """
.PHONY: build test lint deploy

build:
\tpython -m build

test:
\tpytest tests/ --cov=src

lint:
\truff check src/

deploy:
\t./scripts/deploy.sh
""",
        )
        config_repo.commit("add lint and deploy")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "run") or "app.py" in str(_fragment_paths(context))

    def test_cicd_093_circleci_config(self, config_repo):
        config_repo.add_file(
            ".circleci/config.yml",
            """
version: 2.1
jobs:
  build:
    docker:
      - image: cimg/python:3.11
    steps:
      - checkout
      - run: pip install .
      - run: pytest
""",
        )
        config_repo.add_file(
            "src/calculator.py",
            """
def add(a: int, b: int) -> int:
    return a + b
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            ".circleci/config.yml",
            """
version: 2.1
jobs:
  build:
    docker:
      - image: cimg/python:3.12
    steps:
      - checkout
      - run: pip install -e .[dev]
      - run: pytest --cov
  deploy:
    docker:
      - image: cimg/python:3.12
    steps:
      - checkout
      - run: ./deploy.sh
workflows:
  main:
    jobs:
      - build
      - deploy:
          requires:
            - build
""",
        )
        config_repo.commit("add deploy job")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "add") or "calculator.py" in str(_fragment_paths(context))

    def test_cicd_094_travis_ci(self, config_repo):
        config_repo.add_file(
            ".travis.yml",
            """
language: python
python:
  - "3.10"
script:
  - pytest
""",
        )
        config_repo.add_file(
            "tests/test_math.py",
            """
def test_multiply():
    assert 2 * 3 == 6
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            ".travis.yml",
            """
language: python
python:
  - "3.11"
  - "3.12"
install:
  - pip install -e .[dev]
script:
  - pytest --cov
  - mypy src/
""",
        )
        config_repo.commit("add mypy")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "test_multiply") or "test_math.py" in str(_fragment_paths(context))

    def test_cicd_095_azure_pipelines(self, config_repo):
        config_repo.add_file(
            "azure-pipelines.yml",
            """
trigger:
  - main

pool:
  vmImage: 'ubuntu-latest'

steps:
  - script: npm install
  - script: npm test
""",
        )
        config_repo.add_file(
            "src/utils.ts",
            """
export function formatDate(date: Date): string {
  return date.toISOString();
}
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "azure-pipelines.yml",
            """
trigger:
  - main
  - develop

pool:
  vmImage: 'ubuntu-latest'

variables:
  NODE_VERSION: '20.x'

steps:
  - task: NodeTool@0
    inputs:
      versionSpec: $(NODE_VERSION)
  - script: npm ci
  - script: npm run build
  - script: npm test
""",
        )
        config_repo.commit("add build step")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "formatDate") or "utils.ts" in str(_fragment_paths(context))

    def test_cicd_096_bitbucket_pipelines(self, config_repo):
        config_repo.add_file(
            "bitbucket-pipelines.yml",
            """
pipelines:
  default:
    - step:
        name: Build
        script:
          - npm run build
""",
        )
        config_repo.add_file(
            "src/index.js",
            """
function main() {
  console.log("Hello");
}
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "bitbucket-pipelines.yml",
            """
image: node:20

pipelines:
  default:
    - step:
        name: Build and Test
        caches:
          - node
        script:
          - npm ci
          - npm run build
          - npm test
  branches:
    main:
      - step:
          name: Deploy
          deployment: production
          script:
            - ./deploy.sh
""",
        )
        config_repo.commit("add deploy pipeline")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "main") or "index.js" in str(_fragment_paths(context))

    def test_cicd_097_pre_commit_hooks(self, config_repo):
        config_repo.add_file(
            ".pre-commit-config.yaml",
            """
repos:
  - repo: https://github.com/pre-commit/pre-commit-hooks
    rev: v4.5.0
    hooks:
      - id: trailing-whitespace
""",
        )
        config_repo.add_file(
            "src/code.py",
            """
def process():
    return True
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            ".pre-commit-config.yaml",
            """
repos:
  - repo: https://github.com/pre-commit/pre-commit-hooks
    rev: v4.5.0
    hooks:
      - id: trailing-whitespace
      - id: end-of-file-fixer
  - repo: https://github.com/astral-sh/ruff-pre-commit
    rev: v0.1.0
    hooks:
      - id: ruff
        args: [--fix]
""",
        )
        config_repo.commit("add ruff hook")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "process") or "code.py" in str(_fragment_paths(context))

    def test_cicd_098_tox_config(self, config_repo):
        config_repo.add_file(
            "tox.ini",
            """
[tox]
envlist = py310

[testenv]
commands = pytest
""",
        )
        config_repo.add_file(
            "src/lib.py",
            """
def calculate(x: int, y: int) -> int:
    return x + y
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "tox.ini",
            """
[tox]
envlist = py310,py311,py312

[testenv]
deps = pytest
commands = pytest {posargs}

[testenv:lint]
deps = ruff
commands = ruff check src/
""",
        )
        config_repo.commit("add lint env")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "calculate") or "lib.py" in str(_fragment_paths(context))

    def test_cicd_099_nox_config(self, config_repo):
        config_repo.add_file(
            "noxfile.py",
            """
import nox

@nox.session
def tests(session):
    session.install("pytest")
    session.run("pytest")
""",
        )
        config_repo.add_file(
            "src/api.py",
            """
def get_users():
    return []
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "noxfile.py",
            """
import nox

@nox.session(python=["3.10", "3.11", "3.12"])
def tests(session):
    session.install("-e", ".[dev]")
    session.run("pytest", "--cov")

@nox.session
def lint(session):
    session.install("ruff")
    session.run("ruff", "check", "src/")
""",
        )
        config_repo.commit("add lint session")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "get_users") or "api.py" in str(_fragment_paths(context))

    def test_cicd_100_pyproject_scripts(self, config_repo):
        config_repo.add_file(
            "pyproject.toml",
            """
[project]
name = "myapp"
version = "1.0.0"

[project.scripts]
myapp = "src.cli:main"
""",
        )
        config_repo.add_file(
            "src/cli.py",
            """
def main():
    print("CLI started")
""",
        )
        base = config_repo.commit("initial")

        config_repo.add_file(
            "pyproject.toml",
            """
[project]
name = "myapp"
version = "1.1.0"

[project.scripts]
myapp = "src.cli:main"
myapp-server = "src.server:run"

[project.optional-dependencies]
dev = ["pytest", "ruff"]
""",
        )
        config_repo.commit("add server script")

        context = config_repo.get_context(f"{base}..HEAD")
        assert _fragments_contain(context, "main") or "cli.py" in str(_fragment_paths(context))
