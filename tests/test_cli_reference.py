"""The committed flag reference is the parsers' own help text, byte for byte."""

from __future__ import annotations

import importlib.util
from pathlib import Path

PROJECT_ROOT = Path(__file__).parent.parent


def _generator():
    spec = importlib.util.spec_from_file_location("update_cli_reference", PROJECT_ROOT / "scripts" / "update_cli_reference.py")
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


def test_cli_reference_matches_the_parsers():
    generator = _generator()
    committed = (PROJECT_ROOT / "docs" / "product" / "cli.md").read_text(encoding="utf-8")
    assert committed == generator.render(), "docs/product/cli.md drifted: run scripts/update_cli_reference.py"


def test_cli_reference_is_reachable_from_the_site_and_llms_txt():
    index = (PROJECT_ROOT / "docs" / "index.html").read_text(encoding="utf-8")
    llms = (PROJECT_ROOT / "docs" / "llms.txt").read_text(encoding="utf-8")
    readme = (PROJECT_ROOT / "README.md").read_text(encoding="utf-8")
    assert 'href="product/cli.html"' in index
    assert "product/cli.html" in llms
    assert "docs/product/cli.md" in readme
