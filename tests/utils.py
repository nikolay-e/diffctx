# tests/utils.py
from __future__ import annotations

import subprocess
from collections.abc import Hashable
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import pytest
import tiktoken
import yaml

_ENCODER = tiktoken.get_encoding("cl100k_base")

GARBAGE_FILES = {
    "unrelated_dir/garbage_utils.py": """def completely_unrelated_garbage_function():
    return "garbage_marker_12345"

def another_unused_helper():
    return "unused_marker_67890"

class UnusedGarbageClass:
    def useless_method(self):
        return "class_garbage_marker"

def garbage_helper_alpha():
    return "garbage_helper_alpha_value"

def garbage_helper_beta():
    return "garbage_helper_beta_value"

def garbage_helper_gamma():
    return "garbage_helper_gamma_value"

def garbage_helper_delta():
    return "garbage_helper_delta_value"

def garbage_helper_epsilon():
    return "garbage_helper_epsilon_value"
""",
    "unrelated_dir/garbage_constants.py": """GARBAGE_CONSTANT_ALPHA = "garbage_alpha_constant"
GARBAGE_CONSTANT_BETA = "garbage_beta_constant"
UNUSED_CONFIG_VALUE = 99999
GARBAGE_CONST_GAMMA = "garbage_gamma_constant"
GARBAGE_CONST_DELTA = "garbage_delta_constant"
GARBAGE_CONST_EPSILON = "garbage_epsilon_constant"
GARBAGE_CONST_ZETA = "garbage_zeta_constant"
GARBAGE_CONST_ETA = "garbage_eta_constant"
GARBAGE_CONST_THETA = "garbage_theta_constant"
GARBAGE_CONST_IOTA = "garbage_iota_constant"
GARBAGE_CONST_KAPPA = "garbage_kappa_constant"
""",
    "unrelated_dir/garbage_module.js": """export function unusedJsGarbage() {
    return "js_garbage_marker_abc";
}

export const GARBAGE_JS_CONST = "js_const_garbage";

export function garbageJsHelperAlpha() {
    return "js_garbage_alpha_marker";
}

export function garbageJsHelperBeta() {
    return "js_garbage_beta_marker";
}

export function garbageJsHelperGamma() {
    return "js_garbage_gamma_marker";
}

export const GARBAGE_JS_CONFIG = {
    unusedKey1: "garbage_config_value_1",
    unusedKey2: "garbage_config_value_2",
    unusedKey3: "garbage_config_value_3",
};
""",
    "unrelated_dir/garbage_types.py": """class GarbageTypeAlpha:
    def garbage_method_one(self):
        return "garbage_type_alpha_one"

    def garbage_method_two(self):
        return "garbage_type_alpha_two"

    def garbage_method_three(self):
        return "garbage_type_alpha_three"

class GarbageTypeBeta:
    def garbage_method_one(self):
        return "garbage_type_beta_one"

    def garbage_method_two(self):
        return "garbage_type_beta_two"

class GarbageTypeGamma:
    def garbage_method_one(self):
        return "garbage_type_gamma_one"

    def garbage_method_two(self):
        return "garbage_type_gamma_two"

    def garbage_method_three(self):
        return "garbage_type_gamma_three"

    def garbage_method_four(self):
        return "garbage_type_gamma_four"
""",
    "unrelated_dir/garbage_services.py": """class GarbageServiceAlpha:
    def process_garbage_alpha(self, data):
        return f"garbage_service_alpha_{data}"

    def validate_garbage_alpha(self, item):
        return "garbage_validation_alpha"

    def transform_garbage_alpha(self, value):
        return "garbage_transform_alpha"

class GarbageServiceBeta:
    def process_garbage_beta(self, data):
        return f"garbage_service_beta_{data}"

    def validate_garbage_beta(self, item):
        return "garbage_validation_beta"

class GarbageServiceGamma:
    def process_garbage_gamma(self, data):
        return f"garbage_service_gamma_{data}"

    def execute_garbage_gamma(self, cmd):
        return "garbage_execution_gamma"

def standalone_garbage_function_one():
    return "standalone_garbage_one"

def standalone_garbage_function_two():
    return "standalone_garbage_two"

def standalone_garbage_function_three():
    return "standalone_garbage_three"
""",
    "unrelated_dir/garbage_handlers.py": """class GarbageHandlerAlpha:
    def handle_garbage_event_alpha(self, event):
        return "garbage_event_alpha_handled"

    def process_garbage_request_alpha(self, request):
        return "garbage_request_alpha_processed"

class GarbageHandlerBeta:
    def handle_garbage_event_beta(self, event):
        return "garbage_event_beta_handled"

    def process_garbage_request_beta(self, request):
        return "garbage_request_beta_processed"

class GarbageHandlerGamma:
    def handle_garbage_event_gamma(self, event):
        return "garbage_event_gamma_handled"

def garbage_event_dispatcher(event_type):
    return f"garbage_dispatched_{event_type}"

def garbage_request_router(path):
    return f"garbage_routed_{path}"
""",
    "unrelated_dir/garbage_models.py": """class GarbageModelAlpha:
    garbage_field_one = "garbage_model_alpha_field_one"
    garbage_field_two = "garbage_model_alpha_field_two"

    def garbage_model_method_alpha(self):
        return "garbage_model_alpha_method"

class GarbageModelBeta:
    garbage_field_one = "garbage_model_beta_field_one"
    garbage_field_two = "garbage_model_beta_field_two"
    garbage_field_three = "garbage_model_beta_field_three"

    def garbage_model_method_beta(self):
        return "garbage_model_beta_method"

class GarbageModelGamma:
    garbage_field_one = "garbage_model_gamma_field_one"

    def garbage_model_method_gamma(self):
        return "garbage_model_gamma_method"

    def garbage_model_validate_gamma(self, data):
        return "garbage_model_gamma_validated"
""",
    "unrelated_dir/garbage_api.py": """def garbage_api_endpoint_alpha(request):
    return {"garbage_response": "alpha"}

def garbage_api_endpoint_beta(request):
    return {"garbage_response": "beta"}

def garbage_api_endpoint_gamma(request):
    return {"garbage_response": "gamma"}

def garbage_api_endpoint_delta(request):
    return {"garbage_response": "delta"}

class GarbageApiController:
    def garbage_get_all(self):
        return "garbage_api_get_all"

    def garbage_get_one(self, id):
        return f"garbage_api_get_{id}"

    def garbage_create(self, data):
        return "garbage_api_created"

    def garbage_update(self, id, data):
        return f"garbage_api_updated_{id}"

    def garbage_delete(self, id):
        return f"garbage_api_deleted_{id}"
""",
    "unrelated_dir/garbage_validators.py": """def validate_garbage_input_alpha(value):
    return "garbage_input_alpha_valid"

def validate_garbage_input_beta(value):
    return "garbage_input_beta_valid"

def validate_garbage_input_gamma(value):
    return "garbage_input_gamma_valid"

class GarbageValidatorAlpha:
    def validate(self, data):
        return "garbage_validator_alpha_result"

class GarbageValidatorBeta:
    def validate(self, data):
        return "garbage_validator_beta_result"

    def validate_strict(self, data):
        return "garbage_validator_beta_strict"
""",
    "unrelated_dir/garbage_unrelated.yaml": """zxcvb_settings:
  zxcvb_option_alpha: garbage_yaml_alpha_value
  zxcvb_option_beta: garbage_yaml_beta_value
  zxcvb_option_gamma: garbage_yaml_gamma_value

zxcvb_features:
  zxcvb_feature_one: true
  zxcvb_feature_two: false
""",
}

GARBAGE_MARKERS = [
    "garbage_marker_12345",
    "unused_marker_67890",
    "class_garbage_marker",
    "garbage_helper_alpha_value",
    "garbage_type_alpha_one",
    "garbage_service_alpha",
    "garbage_event_alpha_handled",
    "garbage_model_alpha_field",
    "garbage_api_endpoint_alpha",
    "garbage_validator_alpha_result",
    "garbage_yaml_alpha_value",
    "zxcvb_settings",
]


def count_tokens(text: str) -> int:
    return len(_ENCODER.encode(text))


def calculate_test_budget(content_tokens: int, overhead_ratio: float = 0.3, min_budget: int = 300) -> int:
    overhead = int(content_tokens * overhead_ratio)
    return max(min_budget, content_tokens + overhead)


def tight_budget(content_tokens: int, min_budget: int = 200) -> int:
    return max(min_budget, int(content_tokens * 1.1))


@dataclass
class DiffTestCase:
    name: str
    initial_files: dict[str, str]
    changed_files: dict[str, str]
    must_include: list[str] = field(default_factory=list)
    must_not_include: list[str] = field(default_factory=list)
    commit_message: str = "Update files"
    overhead_ratio: float = 0.3
    min_budget: int | None = None  # None = auto-calculate based on data size
    add_garbage_files: bool = True  # Default True: always add garbage for negative testing
    skip_garbage_check: bool = False  # Set True only for tests that intentionally include garbage markers

    def calculate_budget(self) -> int:
        fragment_overhead = 20

        all_files = {**self.initial_files, **self.changed_files}
        content_tokens = sum(count_tokens(content) for content in all_files.values())
        estimated_fragments = max(len(all_files), 2)

        budget = content_tokens + (estimated_fragments * fragment_overhead)
        budget = int(budget * 1.3)

        return max(200, budget)

    def all_files(self) -> dict[str, str]:
        result = dict(self.initial_files)
        result.update(self.changed_files)
        return result


class DiffTestRunner:
    def __init__(self, tmp_path: Path):
        self.repo = tmp_path / "test_repo"
        self.repo.mkdir()
        subprocess.run(["git", "init"], cwd=self.repo, capture_output=True, check=True)
        subprocess.run(["git", "config", "user.email", "test@test.com"], cwd=self.repo, capture_output=True, check=True)
        subprocess.run(["git", "config", "user.name", "Test"], cwd=self.repo, capture_output=True, check=True)

    def add_file(self, path: str, content: str) -> Path:
        file_path = self.repo / path
        file_path.parent.mkdir(parents=True, exist_ok=True)
        file_path.write_text(content, encoding="utf-8")
        return file_path

    def commit(self, message: str) -> str:
        subprocess.run(["git", "add", "-A"], cwd=self.repo, capture_output=True, check=True)
        result = subprocess.run(
            ["git", "commit", "-m", message, "--allow-empty"],
            cwd=self.repo,
            capture_output=True,
            text=True,
        )
        if result.returncode != 0:
            subprocess.run(["git", "commit", "-m", message], cwd=self.repo, capture_output=True, check=True)
        rev = subprocess.run(["git", "rev-parse", "HEAD"], cwd=self.repo, capture_output=True, text=True, check=True)
        return rev.stdout.strip()

    def run_test_case(self, case: DiffTestCase) -> dict:
        from treemapper.diffctx import build_diff_context

        if case.add_garbage_files:
            for path, content in GARBAGE_FILES.items():
                self.add_file(path, content)
            self.commit("Add unrelated garbage files")

        for path, content in case.initial_files.items():
            self.add_file(path, content)
        base_sha = self.commit("Initial commit")

        for path, content in case.changed_files.items():
            self.add_file(path, content)
        self.commit(case.commit_message)

        budget = case.calculate_budget()

        context = build_diff_context(
            root_dir=self.repo,
            diff_range=f"{base_sha}..HEAD",
            budget_tokens=budget,
        )

        return context

    def verify_assertions(self, context: dict, case: DiffTestCase) -> None:
        all_content = self._extract_all_content(context)

        for pattern in case.must_include:
            assert pattern in all_content, f"Expected '{pattern}' to be in context, but it was not found"

        for pattern in case.must_not_include:
            assert pattern not in all_content, f"Expected '{pattern}' to NOT be in context, but it was found"

        if case.add_garbage_files and not case.skip_garbage_check:
            for marker in GARBAGE_MARKERS:
                assert marker not in all_content, (
                    f"Garbage marker '{marker}' found in context! "
                    f"Algorithm included unrelated code that should have been excluded."
                )

    def _extract_all_content(self, context: dict) -> str:
        parts = []
        for frag in context.get("fragments", []):
            if "content" in frag:
                parts.append(frag["content"])
            if "path" in frag:
                parts.append(frag["path"])
        return "\n".join(parts)


def load_yaml(path: Path) -> dict[str, Any]:
    """Load YAML file and return its contents."""
    try:
        with path.open("r", encoding="utf-8") as f:
            result = yaml.load(f, Loader=yaml.SafeLoader)
            return result if result is not None else {}
    except FileNotFoundError:
        pytest.fail(f"Output YAML file not found: {path}")
        return {}  # This will never execute but satisfies mypy
    except Exception as e:
        pytest.fail(f"Failed to load or parse YAML file {path}: {e}")
        return {}  # This will never execute but satisfies mypy


def get_all_files_in_tree(node: dict[str, Any]) -> set[str]:
    """Recursively get all file and directory names from the loaded tree structure."""

    names: set[str] = set()
    if not isinstance(node, dict) or "name" not in node:
        return names
    names.add(node["name"])
    if "children" in node and isinstance(node["children"], list):
        for child in node["children"]:
            if isinstance(child, dict):
                names.update(get_all_files_in_tree(child))
    return names


def find_node_by_path(tree: dict[str, Any], path_segments: list[str]) -> dict[str, Any] | None:
    """Find a node in the tree by list of path segments relative to root node."""
    current_node = tree
    for segment in path_segments:
        if current_node is None or "children" not in current_node or not isinstance(current_node["children"], list):
            return None
        found_child = None
        for child in current_node["children"]:
            if isinstance(child, dict) and child.get("name") == segment:
                found_child = child
                break
        if found_child is None:
            return None
        current_node = found_child
    return current_node


def make_hashable(obj: Any) -> Hashable:
    """Recursively convert dicts and lists to hashable tuples."""
    if isinstance(obj, dict):
        return tuple(sorted((k, make_hashable(v)) for k, v in obj.items()))
    if isinstance(obj, list):
        return tuple(make_hashable(item) for item in obj)
    if isinstance(obj, (str, int, float, bool, type(None))):
        return obj
    try:
        hash(obj)
        return obj
    except TypeError:
        return repr(obj)
