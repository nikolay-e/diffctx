# tests/test_version_consistency.py
import json
import re
from pathlib import Path

import yaml

from diffctx import __version__

PROJECT_ROOT = Path(__file__).parent.parent


def _load_text(relative_path: str) -> str:
    return (PROJECT_ROOT / relative_path).read_text(encoding="utf-8")


def test_pyproject_toml_version():
    match = re.search(r'^version = "([^"]+)"', _load_text("pyproject.toml"), re.MULTILINE)
    assert match, "no top-level version key found in pyproject.toml"
    assert match.group(1) == __version__


def test_version_py_matches_dunder():
    match = re.search(r'^__version__ = "([^"]+)"', _load_text("src/diffctx/version.py"), re.MULTILINE)
    assert match, "no __version__ assignment found in src/diffctx/version.py"
    assert match.group(1) == __version__


def test_native_crate_cargo_toml_version():
    match = re.search(r'^version = "([^"]+)"', _load_text("crates/diffctx-native/Cargo.toml"), re.MULTILINE)
    assert match, "no [package] version key found in crates/diffctx-native/Cargo.toml"
    assert match.group(1) == __version__


def test_cargo_lock_diffctx_entry_version():
    match = re.search(r'name = "diffctx"\nversion = "([^"]+)"', _load_text("Cargo.lock"))
    assert match, "no diffctx package entry found in Cargo.lock"
    assert match.group(1) == __version__


def test_server_json_top_level_version():
    data = json.loads(_load_text("server.json"))
    assert data["version"] == __version__


def test_server_json_package_version():
    data = json.loads(_load_text("server.json"))
    assert data["packages"][0]["version"] == __version__


def test_claude_plugin_json_version():
    data = json.loads(_load_text(".claude-plugin/plugin.json"))
    assert data["version"] == __version__


def test_action_yml_diffctx_version_default():
    data = yaml.safe_load(_load_text("action.yml"))
    assert str(data["inputs"]["diffctx-version"]["default"]) == __version__


def test_bucket_scoop_manifest_version():
    data = json.loads(_load_text("bucket/diffctx.json"))
    assert data["version"] == __version__


def test_github_action_doc_pins_match_version():
    pins = re.findall(r"nikolay-e/diffctx@v(\d+\.\d+\.\d+)", _load_text("docs/product/github-action.md"))
    assert len(pins) >= 2, f"expected at least 2 '@v<semver>' pins in the doc, found {pins}"
    assert all(pin == __version__ for pin in pins), pins
