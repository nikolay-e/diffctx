"""The artifact validates against the schema checked in beside it. The schema
is generated from the Rust type (`cargo test --test context_schema`), so this
is the proof that what the Python surface hands out is that same document."""

from __future__ import annotations

import json
from pathlib import Path

import pytest

import diffctx
from tests.framework.pygit2_backend import Pygit2Repo

jsonschema = pytest.importorskip("jsonschema")

SCHEMA = json.loads((Path(__file__).resolve().parents[1] / "schemas" / "diffctx.context.v1.json").read_text())


def _repo(tmp_path):
    repo = Pygit2Repo(tmp_path / "repo")
    repo.add_file("lib.py", "def helper(x):\n    return x + 1\n")
    repo.add_file("main.py", "from lib import helper\n\nprint(helper(1))\n")
    repo.commit("initial")
    repo.add_file("lib.py", "def helper(x):\n    return x + 2\n")
    repo.add_file("gone.py", "x = 1\n")
    repo.commit("bump helper")
    repo.remove_file("gone.py")
    repo.commit("drop gone")
    return repo


def test_every_surface_emits_the_same_validating_document(tmp_path):
    repo = _repo(tmp_path)
    validator = jsonschema.Draft202012Validator(SCHEMA)
    for diff_range in ("HEAD~1..HEAD", "HEAD~2..HEAD~1", "HEAD~2..HEAD"):
        result = diffctx.build_diff_context(root_dir=repo.path, diff_range=diff_range)
        assert result["schema"] == "diffctx.context.v1"
        document = json.loads(diffctx.to_json(result))
        document.pop("latency", None)
        errors = sorted(validator.iter_errors(document), key=lambda e: list(e.path))
        assert not errors, [f"{list(e.path)}: {e.message}" for e in errors][:5]
        assert diffctx.to_yaml(result).startswith("schema: diffctx.context.v1\n")


def test_a_capped_run_still_validates(tmp_path, monkeypatch):
    import os
    import subprocess
    import sys

    repo = _repo(tmp_path)
    code = f"""
import json, diffctx
r = diffctx.build_diff_context(root_dir={str(repo.path)!r}, diff_range="HEAD~2..HEAD")
d = json.loads(diffctx.to_json(r)); d.pop("latency", None); print(json.dumps(d))
"""
    proc = subprocess.run(
        [sys.executable, "-c", code],
        capture_output=True,
        text=True,
        env={**os.environ, "DIFFCTX_MAX_EDGE_CONTRIBUTIONS": "1"},
    )
    assert proc.returncode == 0, proc.stderr[-400:]
    document = json.loads(proc.stdout)
    assert document["coverage"]["status"] == "partial"
    jsonschema.Draft202012Validator(SCHEMA).validate(document)
