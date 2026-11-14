"""What a release writes on the tree: one version on every surface.

Each test copies the real manifests and locks into a temporary directory and
runs `set_tree_version` there, so a surface the script forgets or a lock entry
it cannot find shows up as a difference in text.
"""

from __future__ import annotations

import importlib.util
import re
import shutil
from pathlib import Path

import pytest

PROJECT_ROOT = Path(__file__).parent.parent
SCRIPT = PROJECT_ROOT / "scripts" / "release" / "tree_version.py"
SURFACES = [
    "src/diffctx/version.py",
    "pyproject.toml",
    "crates/diffctx-native/Cargo.toml",
    "Cargo.lock",
    "uv.lock",
]


def _load():
    spec = importlib.util.spec_from_file_location("tree_version", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


tree_version = _load()


def _read(root: Path, relative: str) -> str:
    return (root / relative).read_text(encoding="utf-8")


@pytest.fixture
def tree(tmp_path: Path) -> Path:
    for surface in SURFACES:
        target = tmp_path / "tree" / surface
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(PROJECT_ROOT / surface, target)
    return tmp_path / "tree"


class TestOneVersionEverywhere:
    @pytest.mark.parametrize(
        ("version", "cargo"),
        [("2.0.0.dev0", "2.0.0-dev.0"), ("2.0.0", "2.0.0"), ("2.1.0rc2", "2.1.0-rc.2"), ("2.0.1.post1", "2.0.1-post.1")],
    )
    def test_the_manifest_and_the_lock_entry_carry_the_cargo_spelling(self, tree, version, cargo):
        tree_version.set_tree_version(version, tree)
        assert re.search(rf'(?m)^version = "{re.escape(cargo)}"$', _read(tree, "crates/diffctx-native/Cargo.toml"))
        assert f'name = "diffctx"\nversion = "{cargo}"' in _read(tree, "Cargo.lock")

    def test_the_python_surfaces_carry_the_pep_440_string(self, tree):
        tree_version.set_tree_version("2.1.0rc2", tree)
        assert '__version__ = "2.1.0rc2"' in _read(tree, "src/diffctx/version.py")
        assert re.search(r'(?m)^version = "2\.1\.0rc2"$', _read(tree, "pyproject.toml"))
        assert re.search(
            r'(?m)^name = "diffctx"\nversion = "2\.1\.0rc2"\nsource = \{ editable = "\." \}$', _read(tree, "uv.lock")
        )

    def test_nothing_but_the_version_lines_changes(self, tree):
        before = {s: _read(tree, s).splitlines() for s in ("pyproject.toml", "Cargo.lock", "uv.lock")}
        tree_version.set_tree_version("7.8.9.dev3", tree)
        for surface, old in before.items():
            new = _read(tree, surface).splitlines()
            assert len(new) == len(old), surface
            changed = [line for was, line in zip(old, new, strict=True) if was != line]
            assert all("7.8.9" in line for line in changed), (surface, changed)

    def test_setting_the_same_version_twice_changes_nothing(self, tree):
        tree_version.set_tree_version("2.0.0", tree)
        once = {p: _read(tree, p) for p in SURFACES}
        tree_version.set_tree_version("2.0.0", tree)
        assert {p: _read(tree, p) for p in once} == once


class TestAVersionThatIsNotAReleaseWritesNothing:
    @pytest.mark.parametrize("version", ["2.0", "v2.0.0", "2.0.0-dev.0", "latest"])
    def test_it_is_refused_before_any_surface_is_touched(self, tree, version):
        before = {p: _read(tree, p) for p in SURFACES}
        with pytest.raises(ValueError, match="not a version this repository releases"):
            tree_version.set_tree_version(version, tree)
        assert {p: _read(tree, p) for p in before} == before
