# tests/conftest.py
import logging
import os
import subprocess
import sys
from functools import wraps
from pathlib import Path

import pytest

from tests.framework.pygit2_backend import Pygit2Repo

# Maximum budget for diff context tests - forces algorithm to actually select
# Set to None to disable budget capping (original behavior)
DIFF_CONTEXT_MAX_BUDGET = int(os.environ.get("DIFF_CONTEXT_MAX_BUDGET", "0"))


VERIFY_NO_GARBAGE = os.environ.get("VERIFY_NO_GARBAGE", "1") == "1"


@pytest.fixture(autouse=True)
def cap_diff_context_budget(monkeypatch):
    """Auto-cap budget_tokens and verify no garbage in output.

    Features:
    - Caps budget_tokens at DIFF_CONTEXT_MAX_BUDGET (default 800) to force selection
    - Optionally verifies no garbage markers in output (VERIFY_NO_GARBAGE=1)

    Environment variables:
    - DIFF_CONTEXT_MAX_BUDGET=0: Disable budget capping
    - VERIFY_NO_GARBAGE=0: Disable garbage verification
    """
    if DIFF_CONTEXT_MAX_BUDGET == 0 and not VERIFY_NO_GARBAGE:
        yield
        return

    from treemapper import diffctx

    original_build = diffctx.build_diff_context

    @wraps(original_build)
    def enhanced_build(*args, **kwargs):
        if DIFF_CONTEXT_MAX_BUDGET > 0:
            if "budget_tokens" in kwargs and kwargs["budget_tokens"] > DIFF_CONTEXT_MAX_BUDGET:
                kwargs["budget_tokens"] = DIFF_CONTEXT_MAX_BUDGET
        result = original_build(*args, **kwargs)
        if VERIFY_NO_GARBAGE:
            _verify_no_garbage_in_context(result)
        return result

    monkeypatch.setattr(diffctx, "build_diff_context", enhanced_build)
    yield


@pytest.fixture(autouse=True)
def _use_pygit2_git(monkeypatch):
    from tests.framework import pygit2_backend as pg
    from treemapper import diffctx as diffctx_mod
    from treemapper.diffctx import git as git_mod

    for target in (git_mod, diffctx_mod):
        for name in (
            "parse_diff",
            "get_diff_text",
            "get_changed_files",
            "show_file_at_revision",
            "get_deleted_files",
            "get_renamed_paths",
            "get_untracked_files",
            "is_git_repo",
        ):
            if hasattr(target, name):
                monkeypatch.setattr(target, name, getattr(pg, name))
    monkeypatch.setattr(git_mod, "run_git", pg.run_git)
    yield
    pg.clear_repo_cache()


def _verify_no_garbage_in_context(context: dict) -> None:
    all_content = []
    for frag in context.get("fragments", []):
        if "content" in frag:
            all_content.append(frag["content"])
        if "path" in frag:
            all_content.append(frag["path"])
    full_content = "\n".join(all_content)

    for marker in GARBAGE_MARKERS:
        if marker in full_content:
            pytest.fail(
                f"Garbage marker '{marker}' found in context! Algorithm included unrelated code that should have been excluded."
            )


# Add project root/src to PYTHONPATH for subprocess tests
PROJECT_ROOT = Path(__file__).parent.parent
SRC_DIR = PROJECT_ROOT / "src"

GITIGNORE = ".gitignore"

# WSL detection with proper file handle cleanup (shared across test files)
_PROC_VERSION = Path("/proc/version")
IS_WSL = _PROC_VERSION.exists() and "microsoft" in _PROC_VERSION.read_text(errors="ignore").lower()


# --- Фикстура для создания временного проекта ---
@pytest.fixture
def temp_project(tmp_path):
    """Create a temporary project structure for testing."""
    temp_dir = tmp_path / "treemapper_test_project"
    temp_dir.mkdir()
    (temp_dir / "src").mkdir()
    (temp_dir / "src" / "main.py").write_text("def main():\n    print('hello')\n", encoding="utf-8")
    (temp_dir / "src" / "test.py").write_text("def test():\n    pass\n", encoding="utf-8")
    (temp_dir / "docs").mkdir()
    (temp_dir / "docs" / "readme.md").write_text("# Documentation\n", encoding="utf-8")
    (temp_dir / "output").mkdir()
    (temp_dir / ".git").mkdir()
    (temp_dir / ".git" / "config").write_text("git config file", encoding="utf-8")
    (temp_dir / GITIGNORE).write_text("*.pyc\n__pycache__/\n", encoding="utf-8")
    config_dir = temp_dir / ".treemapper"
    config_dir.mkdir()
    (config_dir / "ignore").write_text("output/\n.git/\n", encoding="utf-8")
    yield temp_dir


# --- Фикстура для запуска маппера ---
@pytest.fixture
def run_mapper(monkeypatch, temp_project):
    """Helper to run treemapper with given args."""

    def _run(args):
        """Runs the main function with patched CWD and sys.argv."""
        with monkeypatch.context() as m:
            m.chdir(temp_project)
            m.setattr(sys, "argv", ["treemapper", *args])
            try:
                from treemapper.treemapper import main

                main()
                return True
            except SystemExit as e:
                return e.code is None or e.code == 0

    return _run


# --- Helper for running treemapper as subprocess ---
def run_treemapper_subprocess(args, cwd=None, **kwargs):
    """Run treemapper as a subprocess with proper environment setup.

    Args:
        args: Command line arguments to pass to treemapper
        cwd: Working directory for the subprocess
        **kwargs: Additional arguments to pass to subprocess.run

    Returns:
        CompletedProcess object
    """
    command = [sys.executable, "-m", "treemapper", *args]

    # Ensure subprocess can find the treemapper module
    env = os.environ.copy()
    pythonpath = str(SRC_DIR)
    if "PYTHONPATH" in env:
        pythonpath = f"{pythonpath}{os.pathsep}{env['PYTHONPATH']}"
    env["PYTHONPATH"] = pythonpath

    # Merge with any env provided in kwargs
    if "env" in kwargs:
        env.update(kwargs["env"])
    kwargs["env"] = env

    # Set default values for common parameters
    if "capture_output" not in kwargs:
        kwargs["capture_output"] = True
    if "text" not in kwargs:
        kwargs["text"] = True
    if "encoding" not in kwargs:
        kwargs["encoding"] = "utf-8"
    if "errors" not in kwargs:
        kwargs["errors"] = "replace"

    return subprocess.run(command, cwd=cwd, **kwargs)


def _check_wsl_windows_path(path: Path) -> bool:
    if not IS_WSL:
        return False
    return "/mnt/" in str(path)


def _restore_permissions(paths_changed: list[Path], original_perms: dict[Path, int]) -> None:
    logging.debug(f"Cleaning up permissions for: {paths_changed}")
    for path in paths_changed:
        if not path.exists() or path not in original_perms:
            continue
        orig = original_perms[path]
        if orig is None:
            logging.warning(f"Original permissions for {path} were None, not restoring.")
            continue
        try:
            os.chmod(path, orig)
            logging.debug(f"Restored permissions for {path}")
        except OSError as e:
            logging.warning(f"Could not restore permissions for {path}: {e}")


@pytest.fixture
def set_perms():
    """Fixture to temporarily set file/directory permissions (non-Windows)."""
    original_perms: dict[Path, int] = {}
    paths_changed: list[Path] = []

    def _set_perms(path: Path, perms: int):
        if sys.platform == "win32":
            pytest.skip("Permission tests skipped on Windows.")
        if _check_wsl_windows_path(path):
            pytest.skip(f"Permission tests skipped on Windows-mounted paths in WSL: {path}")
        if not path.exists():
            pytest.skip(f"Path does not exist, cannot set permissions: {path}")
        try:
            original_perms[path] = path.stat().st_mode
            paths_changed.append(path)
            os.chmod(path, perms)
            logging.debug(f"Set permissions {oct(perms)} for {path}")
        except OSError as e:
            pytest.skip(f"Could not set permissions on {path}: {e}. Skipping test.")

    yield _set_perms

    _restore_permissions(paths_changed, original_perms)


# --- New fixtures for test modernization ---


@pytest.fixture
def project_builder(tmp_path):
    """Builder pattern for creating test project structures."""

    class ProjectBuilder:
        def __init__(self, base_path: Path):
            self.root = base_path / "treemapper_test_project"
            self.root.mkdir()

        def add_file(self, path: str, content: str = "") -> Path:
            file_path = self.root / path
            file_path.parent.mkdir(parents=True, exist_ok=True)
            file_path.write_text(content, encoding="utf-8")
            return file_path

        def add_binary(self, path: str, content: bytes = b"\x00\x01\x02") -> Path:
            file_path = self.root / path
            file_path.parent.mkdir(parents=True, exist_ok=True)
            file_path.write_bytes(content)
            return file_path

        def add_dir(self, path: str) -> Path:
            dir_path = self.root / path
            dir_path.mkdir(parents=True, exist_ok=True)
            return dir_path

        def add_gitignore(self, patterns: list[str], subdir: str = "") -> Path:
            path = self.root / subdir / GITIGNORE if subdir else self.root / GITIGNORE
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("\n".join(patterns) + "\n", encoding="utf-8")
            return path

        def add_treemapper_ignore(self, patterns: list[str]) -> Path:
            config_dir = self.root / ".treemapper"
            config_dir.mkdir(exist_ok=True)
            path = config_dir / "ignore"
            path.write_text("\n".join(patterns) + "\n", encoding="utf-8")
            return path

        def create_nested(self, depth: int, files_per_level: int = 1) -> None:
            current = self.root
            for i in range(depth):
                current = current / f"level{i}"
                current.mkdir(exist_ok=True)
                for j in range(files_per_level):
                    (current / f"file{j}.txt").write_text(f"Content {i}-{j}", encoding="utf-8")

    return ProjectBuilder(tmp_path)


@pytest.fixture
def git_repo(tmp_path):
    repo_path = tmp_path / "git_test_repo"
    Pygit2Repo(repo_path)
    return repo_path


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
    "garbage_alpha_constant",
    "garbage_beta_constant",
    "js_garbage_marker_abc",
    "js_const_garbage",
    "completely_unrelated_garbage_function",
    "another_unused_helper",
    "UnusedGarbageClass",
    "unusedJsGarbage",
    "GARBAGE_JS_CONST",
    "garbage_helper_alpha_value",
    "garbage_helper_beta_value",
    "garbage_helper_gamma_value",
    "garbage_helper_delta_value",
    "garbage_helper_epsilon_value",
    "garbage_gamma_constant",
    "garbage_delta_constant",
    "garbage_epsilon_constant",
    "garbage_zeta_constant",
    "garbage_eta_constant",
    "garbage_theta_constant",
    "garbage_iota_constant",
    "garbage_kappa_constant",
    "js_garbage_alpha_marker",
    "js_garbage_beta_marker",
    "js_garbage_gamma_marker",
    "garbage_config_value_1",
    "garbage_config_value_2",
    "garbage_config_value_3",
    "GarbageTypeAlpha",
    "GarbageTypeBeta",
    "GarbageTypeGamma",
    "garbage_type_alpha_one",
    "garbage_type_beta_one",
    "garbage_type_gamma_one",
    "GarbageServiceAlpha",
    "GarbageServiceBeta",
    "GarbageServiceGamma",
    "garbage_service_alpha",
    "garbage_service_beta",
    "garbage_service_gamma",
    "garbage_validation_alpha",
    "garbage_validation_beta",
    "garbage_transform_alpha",
    "garbage_execution_gamma",
    "standalone_garbage_one",
    "standalone_garbage_two",
    "standalone_garbage_three",
    "GarbageHandlerAlpha",
    "GarbageHandlerBeta",
    "GarbageHandlerGamma",
    "garbage_event_alpha_handled",
    "garbage_event_beta_handled",
    "garbage_event_gamma_handled",
    "garbage_request_alpha_processed",
    "garbage_request_beta_processed",
    "garbage_dispatched",
    "garbage_routed",
    "GarbageModelAlpha",
    "GarbageModelBeta",
    "GarbageModelGamma",
    "garbage_model_alpha_field",
    "garbage_model_beta_field",
    "garbage_model_gamma_field",
    "garbage_api_endpoint_alpha",
    "garbage_api_endpoint_beta",
    "garbage_api_endpoint_gamma",
    "garbage_api_endpoint_delta",
    "GarbageApiController",
    "garbage_api_get_all",
    "garbage_api_created",
    "garbage_api_updated",
    "garbage_api_deleted",
    "validate_garbage_input_alpha",
    "validate_garbage_input_beta",
    "validate_garbage_input_gamma",
    "GarbageValidatorAlpha",
    "GarbageValidatorBeta",
    "garbage_validator_alpha_result",
    "garbage_validator_beta_result",
    "garbage_validator_beta_strict",
    "garbage_yaml_alpha_value",
    "garbage_yaml_beta_value",
    "garbage_yaml_gamma_value",
    "zxcvb_settings",
    "zxcvb_features",
]


MINIMUM_AVERAGE_SCORE = 82.0


def _extract_scores_from_reports(terminalreporter):
    results = []
    for report in terminalreporter.stats.get("passed", []) + terminalreporter.stats.get("failed", []):
        if not hasattr(report, "user_properties"):
            continue
        props = dict(report.user_properties)
        if "score" not in props:
            continue
        case_id = report.nodeid.split("[")[-1].rstrip("]") if "[" in report.nodeid else report.nodeid
        results.append((case_id, props))
    return results


@pytest.hookimpl(trylast=True)
def pytest_sessionfinish(session, exitstatus):
    tr = session.config.pluginmanager.get_plugin("terminalreporter")
    if tr is None:
        return
    results = _extract_scores_from_reports(tr)
    if not results:
        return
    scores = [p["score"] for _, p in results]
    avg = sum(scores) / len(scores)
    session.config._diffctx_avg = avg
    session.config._diffctx_results = results
    if avg < MINIMUM_AVERAGE_SCORE:
        session.exitstatus = 1


def _compute_score_stats(results, config):
    scores = [p["score"] for _, p in results]
    return {
        "scores": scores,
        "diff_fails": sum(1 for _, p in results if not p.get("diff_covered", True)),
        "perfect": sum(1 for s in scores if s >= 100.0),
        "above_90": sum(1 for s in scores if s >= 90.0),
        "above_70": sum(1 for s in scores if s >= 70.0),
        "below_50": sum(1 for s in scores if s < 50.0),
        "avg_score": getattr(config, "_diffctx_avg", sum(scores) / len(scores) if scores else 0),
    }


def _compute_enrichment_stats(results):
    enrichments = [p["enrichment"] for _, p in results if p.get("enrichment", 0) > 0]
    total_diff_tok = sum(p.get("diff_tokens", 0) for _, p in results)
    total_ctx_tok = sum(p.get("context_tokens", 0) for _, p in results)
    return {
        "avg_enrichment": sum(enrichments) / len(enrichments) if enrichments else 0,
        "total_diff_tok": total_diff_tok,
        "total_ctx_tok": total_ctx_tok,
        "global_enrichment": (total_ctx_tok / total_diff_tok * 100) if total_diff_tok > 0 else 0,
    }


def _format_entry(case_id, props):
    flags = []
    if not props.get("diff_covered", True):
        flags.append("DIFF_MISS")
    noise = props.get("noise_rate", 0)
    if noise > 0:
        flags.append(f"noise={noise}%")
    garbage = props.get("garbage_rate", 0)
    if garbage > 0:
        flags.append(f"garbage={garbage}%")
    recall = props.get("recall", 100)
    if recall < 100:
        flags.append(f"recall={recall}%")
    enrich = props.get("enrichment", 0)
    if enrich > 0:
        flags.append(f"ctx={enrich}%")
    flag_str = f" [{', '.join(flags)}]" if flags else ""
    return f"  {props['score']:5.1f}%  {case_id}{flag_str}"


def _write_scores_report(results, stats, enrich):
    import json
    import time

    scores_dir = Path(".scores")
    scores_dir.mkdir(exist_ok=True)
    report = {
        "timestamp": time.time(),
        "total_cases": len(results),
        "average_score": round(stats["avg_score"], 2),
        "minimum_score": MINIMUM_AVERAGE_SCORE,
        "perfect_count": stats["perfect"],
        "diff_hard_fails": stats["diff_fails"],
        "avg_enrichment": round(enrich["avg_enrichment"]),
        "global_enrichment": round(enrich["global_enrichment"]),
        "total_diff_tokens": enrich["total_diff_tok"],
        "total_context_tokens": enrich["total_ctx_tok"],
        "cases": {
            case_id: {
                "score": p["score"],
                "recall": p.get("recall", 100),
                "noise_rate": p.get("noise_rate", 0),
                "garbage_rate": p.get("garbage_rate", 0),
                "diff_covered": p.get("diff_covered", True),
                "enrichment": p.get("enrichment", 0),
                "diff_tokens": p.get("diff_tokens", 0),
                "context_tokens": p.get("context_tokens", 0),
            }
            for case_id, p in results
        },
    }
    (scores_dir / "latest.json").write_text(json.dumps(report, indent=2))


def _write_score_histogram(terminalreporter, results):
    scores = [p["score"] for _, p in results]
    if not scores:
        return

    bucket_labels = ["0-10", "10-20", "20-30", "30-40", "40-50", "50-60", "60-70", "70-80", "80-90", "90-100", "100"]
    counts = dict.fromkeys(bucket_labels, 0)
    for s in scores:
        if s >= 100.0:
            counts["100"] += 1
        else:
            lo = int(s // 10) * 10
            counts[f"{lo}-{lo + 10}"] += 1

    total = len(scores)
    max_count = max(counts.values()) if counts else 1

    terminalreporter.write_line("")
    terminalreporter.write_line("  Score distribution:")
    for label in bucket_labels:
        count = counts[label]
        bar_len = int(40 * count / max_count) if max_count > 0 else 0
        bar = "\u2588" * bar_len
        pct = 100 * count / total
        if label == "100":
            terminalreporter.write_line(f"     {label}% \u2502 {bar:<40} {count:>4} ({pct:>5.1f}%)")
        else:
            terminalreporter.write_line(f"  {label:>6}% \u2502 {bar:<40} {count:>4} ({pct:>5.1f}%)")
    terminalreporter.write_line("")

    for thresh in [50, 70, 90, 100]:
        above = sum(1 for s in scores if s >= thresh)
        op = "=" if thresh == 100 else "\u2265"
        terminalreporter.write_line(f"  {op}{thresh:>3}%: {above:>5} / {total} ({100 * above / total:.1f}%)")

    below_50 = sum(1 for s in scores if s < 50)
    terminalreporter.write_line(f"   <50%: {below_50:>5} / {total} ({100 * below_50 / total:.1f}%)")
    terminalreporter.write_line("")


def pytest_terminal_summary(terminalreporter, exitstatus, config):
    results = getattr(config, "_diffctx_results", None)
    if results is None:
        results = _extract_scores_from_reports(terminalreporter)
    if not results:
        return

    stats = _compute_score_stats(results, config)
    enrich = _compute_enrichment_stats(results)

    terminalreporter.write_sep("=", "DIFFCTX QUALITY SCORES")
    terminalreporter.write_line(f"  Cases scored:    {len(results)}")
    terminalreporter.write_line(f"  Average score:   {stats['avg_score']:.1f}% (min: {MINIMUM_AVERAGE_SCORE}%)")
    terminalreporter.write_line(f"  Perfect (100%):  {stats['perfect']}")
    terminalreporter.write_line(f"  Above 90%:       {stats['above_90']}")
    terminalreporter.write_line(f"  Above 70%:       {stats['above_70']}")
    terminalreporter.write_line(f"  Below 50%:       {stats['below_50']}")
    terminalreporter.write_line(f"  Diff hard fails: {stats['diff_fails']}")
    terminalreporter.write_line("")
    terminalreporter.write_line(f"  Context enrichment (avg per-case):  {enrich['avg_enrichment']:.0f}%")
    terminalreporter.write_line(f"  Context enrichment (global):        {enrich['global_enrichment']:.0f}%")
    terminalreporter.write_line(f"  Total diff tokens:    {enrich['total_diff_tok']:,}")
    terminalreporter.write_line(f"  Total context tokens: {enrich['total_ctx_tok']:,}")

    _write_score_histogram(terminalreporter, results)

    if stats["avg_score"] < MINIMUM_AVERAGE_SCORE:
        terminalreporter.write_sep("!", "SCORE REGRESSION")
        terminalreporter.write_line(f"  Average score {stats['avg_score']:.1f}% dropped below minimum {MINIMUM_AVERAGE_SCORE}%")

    sorted_results = sorted(results, key=lambda r: r[1]["score"])

    worst = [r for r in sorted_results if r[1]["score"] < 100.0][:300]
    if worst:
        terminalreporter.write_sep("-", f"LOWEST SCORES ({len(worst)})")
        for case_id, props in worst:
            terminalreporter.write_line(_format_entry(case_id, props))

    best = [r for r in reversed(sorted_results) if r[1]["score"] > 0.0][:300]
    if best:
        terminalreporter.write_sep("-", f"HIGHEST SCORES ({len(best)})")
        for case_id, props in best:
            terminalreporter.write_line(_format_entry(case_id, props))

    _write_scores_report(results, stats, enrich)
