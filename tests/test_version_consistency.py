# tests/test_version_consistency.py
"""Two questions with two answers (#570).

What this tree is: the version module, pyproject, the crate and the two locks
all say the same thing, which is a development version between releases.
What users pin: every manifest a user installs from names the last release,
which is the version server.json records. The pins move only when a release
does, and the tree moves on to the next `.dev0` the moment it has shipped.
"""

import importlib.util
import json
import re
import subprocess
import sys
from pathlib import Path

import pytest
import yaml
from packaging.version import Version

from diffctx import __version__

PROJECT_ROOT = Path(__file__).parent.parent


def _load_text(relative_path: str) -> str:
    return (PROJECT_ROOT / relative_path).read_text(encoding="utf-8")


def _load_tree_version():
    path = PROJECT_ROOT / "scripts" / "release" / "tree_version.py"
    spec = importlib.util.spec_from_file_location("tree_version", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


tree_version = _load_tree_version()
RELEASED = json.loads(_load_text("server.json"))["version"]


class TestWhatThisTreeIs:
    def test_pyproject_toml_version(self):
        match = re.search(r'^version = "([^"]+)"', _load_text("pyproject.toml"), re.MULTILINE)
        assert match, "no top-level version key found in pyproject.toml"
        assert match.group(1) == __version__

    def test_version_py_matches_dunder(self):
        match = re.search(r'^__version__ = "([^"]+)"', _load_text("src/diffctx/version.py"), re.MULTILINE)
        assert match, "no __version__ assignment found in src/diffctx/version.py"
        assert match.group(1) == __version__

    def test_native_crate_cargo_toml_version(self):
        match = re.search(r'^version = "([^"]+)"', _load_text("crates/diffctx-native/Cargo.toml"), re.MULTILINE)
        assert match, "no [package] version key found in crates/diffctx-native/Cargo.toml"
        assert match.group(1) == tree_version.cargo_spelling(__version__)

    def test_cargo_lock_diffctx_entry_version(self):
        match = re.search(r'name = "diffctx"\nversion = "([^"]+)"', _load_text("Cargo.lock"))
        assert match, "no diffctx package entry found in Cargo.lock"
        assert match.group(1) == tree_version.cargo_spelling(__version__)

    def test_uv_lock_editable_entry_version(self):
        """`uv sync --locked` refuses a lock whose editable entry lags pyproject."""
        match = re.search(r'name = "diffctx"\nversion = "([^"]+)"\nsource = \{ editable = "\." \}', _load_text("uv.lock"))
        assert match, "no editable diffctx entry found in uv.lock"
        assert match.group(1) == __version__

    def test_the_tree_is_the_release_or_a_development_version_after_it(self):
        """A development tree sits above the last release, so a build from source
        can never satisfy the `==X.Y.Z` pin of that release."""
        released = Version(RELEASED)
        assert not released.is_prerelease, RELEASED
        tree = Version(__version__)
        if tree.is_devrelease:
            assert Version(tree.base_version) > released, (__version__, RELEASED)
        else:
            assert tree == released


class TestWhatUsersPin:
    def test_server_json_package_version(self):
        assert json.loads(_load_text("server.json"))["packages"][0]["version"] == RELEASED

    def test_claude_plugin_json_version(self):
        assert json.loads(_load_text("plugin/.claude-plugin/plugin.json"))["version"] == RELEASED

    def test_action_yml_diffctx_version_default(self):
        data = yaml.safe_load(_load_text("action.yml"))
        assert str(data["inputs"]["diffctx-version"]["default"]) == RELEASED

    def test_bucket_scoop_manifest_version(self):
        assert json.loads(_load_text("bucket/diffctx.json"))["version"] == RELEASED

    def test_citation_cff_version(self):
        assert str(yaml.safe_load(_load_text("CITATION.cff"))["version"]) == RELEASED

    def test_github_action_doc_pins_match_the_release(self):
        pins = re.findall(r"nikolay-e/diffctx@v(\d+\.\d+\.\d+)", _load_text("docs/product/github-action.md"))
        assert len(pins) >= 2, f"expected at least 2 '@v<semver>' pins in the doc, found {pins}"
        assert all(pin == RELEASED for pin in pins), pins

    def test_action_reference_page_states_the_python_version_default(self):
        data = yaml.safe_load(_load_text("action.yml"))
        default = str(data["inputs"]["python-version"]["default"])
        row = re.search(r"^\| `python-version` \| `([^`]+)` \|", _load_text("docs/product/github-action.md"), re.MULTILINE)
        assert row is not None
        assert row.group(1) == default

    def test_changelog_has_a_dated_section_for_the_release(self):
        heading = re.search(
            rf"^## \[{re.escape(RELEASED)}\] - (\d{{4}}-\d{{2}}-\d{{2}})$", _load_text("CHANGELOG.md"), re.MULTILINE
        )
        assert heading is not None, f"CHANGELOG.md has no '## [{RELEASED}] - YYYY-MM-DD' section"


class TestTheReleaseWritesTheTreeSurfaces:
    @pytest.mark.parametrize(
        ("pep440", "cargo"),
        [("1.20.0", "1.20.0"), ("2.0.0.dev0", "2.0.0-dev.0"), ("1.21.0rc2", "1.21.0-rc.2"), ("1.20.1.post1", "1.20.1-post.1")],
    )
    def test_cargo_spelling_is_the_same_version(self, pep440, cargo):
        assert tree_version.cargo_spelling(pep440) == cargo
        assert Version(cargo) == Version(pep440)

    @pytest.mark.parametrize(("released", "dev"), [("1.20.0", "1.20.1.dev0"), ("2.0.9", "2.0.10.dev0")])
    def test_the_next_development_version_follows_a_final_release(self, released, dev):
        assert tree_version.next_dev(released) == dev

    @pytest.mark.parametrize("released", ["2.0.0rc1", "2.0.0.dev0", "2.0", "v2.0.0"])
    def test_only_a_final_release_opens_a_development_version(self, released):
        with pytest.raises(ValueError):
            tree_version.next_dev(released)

    def test_set_rewrites_every_tree_surface_and_nothing_else(self, tmp_path):
        surfaces = [
            "src/diffctx/version.py",
            "pyproject.toml",
            "crates/diffctx-native/Cargo.toml",
            "Cargo.lock",
            "uv.lock",
        ]
        for surface in surfaces:
            target = tmp_path / surface
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(_load_text(surface), encoding="utf-8")
        tree_version.set_tree_version("7.8.9.dev3", tmp_path)

        def lines(root: Path, surface: str) -> list[str]:
            return (root / surface).read_text(encoding="utf-8").splitlines()

        for surface in surfaces:
            before, after = lines(PROJECT_ROOT, surface), lines(tmp_path, surface)
            assert len(before) == len(after), surface
            changed = [new for old, new in zip(before, after, strict=True) if old != new]
            assert len(changed) <= 1, (surface, changed)
        assert 'version = "7.8.9.dev3"' in lines(tmp_path, "pyproject.toml")
        assert 'version = "7.8.9-dev.3"' in lines(tmp_path, "crates/diffctx-native/Cargo.toml")
        assert '__version__ = "7.8.9.dev3"' in lines(tmp_path, "src/diffctx/version.py")

    def test_the_command_line_opens_the_next_development_version(self, tmp_path):
        script = PROJECT_ROOT / "scripts" / "release" / "tree_version.py"
        refused = subprocess.run(
            [sys.executable, str(script), "open-dev", "2.0.0rc1"], capture_output=True, text=True, check=False
        )
        assert refused.returncode != 0
        assert "final release" in refused.stderr

    def test_cd_prepares_the_release_through_the_same_script_and_reopens_the_tree_after_it(self):
        cd = yaml.safe_load(_load_text(".github/workflows/cd.yml"))["jobs"]
        prepare = "\n".join(step.get("run", "") for step in cd["prepare-version"]["steps"])
        assert 'tree_version.py set "$VERSION"' in prepare
        reopen = cd["open-next-dev-version"]
        assert {"prepare-version", "finalize-release"} <= set(reopen["needs"])
        commands = "\n".join(step.get("run", "") for step in reopen["steps"])
        assert "tree_version.py open-dev" in commands
        add = re.search(r"git add ([^\n]+)", commands)
        assert add, "the dev-version job commits nothing"
        assert set(add.group(1).split()) == {
            "src/diffctx/version.py",
            "pyproject.toml",
            "crates/diffctx-native/Cargo.toml",
            "Cargo.lock",
            "uv.lock",
        }, "the dev version is for the tree only: a user pin must stay on the release"
