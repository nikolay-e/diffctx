from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

from tests.framework.pygit2_backend import Pygit2Repo

PROJECT_ROOT = Path(__file__).parent.parent
SRC_DIR = PROJECT_ROOT / "src"


@pytest.fixture
def locate_repo(tmp_path):
    repo = Pygit2Repo(tmp_path / "locate_repo")
    repo.add_file("src/calc.py", "def add(a, b):\n    return a + b\n")
    repo.add_file(
        "src/main.py",
        "from calc import add\n\ndef run():\n    return add(1, 2)\n",
    )
    repo.add_file(
        "checks/test_calc.py",
        "from calc import add\n\ndef test_add():\n    assert add(1, 2) == 3\n",
    )
    base = repo.commit("initial")
    repo.add_file(
        "src/calc.py",
        "def add(a, b):\n    return a + b\n\ndef sub(a, b):\n    return a - b\n",
    )
    head = repo.commit("add sub")
    return repo, f"{base}..{head}"


def _run(cwd: Path, args: list[str]) -> subprocess.CompletedProcess[str]:
    env = {**os.environ, "PYTHONPATH": str(SRC_DIR)}
    return subprocess.run(
        [sys.executable, "-m", "diffctx", *args],
        cwd=cwd,
        env=env,
        capture_output=True,
        text=True,
        timeout=120,
    )


class TestLocateMode:
    def test_emits_versioned_schema_without_source(self, locate_repo):
        repo, diff_range = locate_repo
        result = _run(repo.path, [".", "--diff", diff_range, "--mode", "locate", "-q"])
        assert result.returncode == 0, result.stderr
        doc = json.loads(result.stdout)
        assert doc["schema"] == "diffctx.locate.v1"
        assert doc["item_count"] == len(doc["items"]) > 0
        for item in doc["items"]:
            assert {"path", "lines", "kind", "score", "tokens", "reasons"} <= item.keys()
            assert item["reasons"], "every ranked item carries >=1 provenance reason"
        assert "def add" not in result.stdout

        summary = doc["summary"]
        assert summary["changed"] == sum(1 for i in doc["items"] if i.get("role") == "changed")
        assert summary["context"] == doc["item_count"] - summary["changed"]
        assert summary["files"] == len({i["path"] for i in doc["items"]})
        test_items = [i for i in doc["items"] if i.get("group") == "test"]
        assert summary["tests"] == len(test_items)
        assert any(i["path"].endswith("test_calc.py") for i in test_items), "covering test file must be flagged group=test"

    def test_changed_items_and_pack_output_unaffected(self, locate_repo):
        repo, diff_range = locate_repo
        locate = _run(repo.path, [".", "--diff", diff_range, "--mode", "locate", "-q"])
        doc = json.loads(locate.stdout)
        changed = [i for i in doc["items"] if i.get("role") == "changed"]
        assert changed and all(r["type"] == "changed" for i in changed for r in i["reasons"])

        pack_default = _run(repo.path, [".", "--diff", diff_range, "-q", "-f", "yaml"])
        pack_again = _run(repo.path, [".", "--diff", diff_range, "-q", "-f", "yaml"])
        assert pack_default.stdout == pack_again.stdout
        assert "fragments:" in pack_default.stdout

    def test_locate_rejects_full_and_warns_on_format(self, locate_repo):
        repo, diff_range = locate_repo
        conflict = _run(repo.path, [".", "--diff", diff_range, "--mode", "locate", "--full"])
        assert conflict.returncode == 2
        assert "--mode locate" in conflict.stderr

        warned = _run(repo.path, [".", "--diff", diff_range, "--mode", "locate", "-f", "yaml", "-q"])
        assert warned.returncode == 0
        assert "ignored with --mode locate" in warned.stderr
        json.loads(warned.stdout)

    def test_native_build_locate_treats_an_empty_range_as_the_working_tree(self, locate_repo):
        """`build_diff_context` maps an empty diff_range to the working tree;
        `build_locate` forwarded `Some("")` into range validation instead, so the
        two entry points into the same pipeline disagreed about what an
        unspecified range means."""
        from diffctx._native.pipeline import build_locate

        repo, _ = locate_repo
        (repo.path / "src" / "calc.py").write_text("def add(a, b):\n    return a + b + 0\n")

        payload = json.loads(build_locate(repo.path, "", budget_tokens=8000))
        assert payload["schema"] == "diffctx.locate.v1"
        assert "src/calc.py" in payload["changed_files"]
