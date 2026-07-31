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
def prov_repo(tmp_path):
    repo = Pygit2Repo(tmp_path / "prov_repo")
    repo.add_file("src/calc.py", "def add(a, b):\n    return a + b\n")
    repo.add_file(
        "src/main.py",
        "from calc import add\n\ndef run():\n    return add(1, 2)\n",
    )
    repo.add_file(
        "src/report.py",
        "from calc import add\n\ndef report():\n    return f'total={add(3, 4)}'\n",
    )
    base = repo.commit("initial")
    repo.add_file(
        "src/calc.py",
        "def add(a, b):\n    return a + b\n\ndef sub(a, b):\n    return a - b\n",
    )
    head = repo.commit("add sub")
    return repo, f"{base}..{head}"


def _run(cwd: Path, args: list[str], extra_env: dict[str, str] | None = None) -> str:
    env = {**os.environ, "PYTHONPATH": str(SRC_DIR)}
    if extra_env:
        env.update(extra_env)
    result = subprocess.run(
        [sys.executable, "-m", "diffctx", *args],
        cwd=cwd,
        env=env,
        capture_output=True,
        text=True,
        timeout=120,
    )
    assert result.returncode == 0, result.stderr
    return result.stdout


class TestProvenanceDump:
    """DIFFCTX_PROVENANCE_DUMP writes one JSONL record per scored candidate
    (#93). It must be strictly additive: the rendered output may not change
    by a single byte when the env var is set."""

    def test_dump_is_additive_and_attributes_inclusion(self, prov_repo, tmp_path):
        repo, diff_range = prov_repo
        args = [".", "--diff", diff_range, "-f", "yaml"]
        dump = tmp_path / "prov.jsonl"

        baseline = _run(repo.path, args)
        with_dump = _run(repo.path, args, {"DIFFCTX_PROVENANCE_DUMP": str(dump)})

        assert with_dump == baseline
        records = [json.loads(line) for line in dump.read_text().splitlines()]
        assert records

        for r in records:
            assert {"path", "start", "end", "relevance", "is_core", "selected", "seed_hops", "incoming_mass"} <= r.keys()

        cores = [r for r in records if r["is_core"]]
        context = [r for r in records if not r["is_core"]]
        assert cores
        assert context
        assert all(r["seed_hops"] == 0 for r in cores)
        assert any(r["seed_hops"] >= 1 for r in context)
        assert any(r["incoming_mass"] for r in context)

    def test_dump_is_deterministic(self, prov_repo, tmp_path):
        repo, diff_range = prov_repo
        args = [".", "--diff", diff_range, "-f", "yaml"]
        first = tmp_path / "a.jsonl"
        second = tmp_path / "b.jsonl"
        _run(repo.path, args, {"DIFFCTX_PROVENANCE_DUMP": str(first)})
        _run(repo.path, args, {"DIFFCTX_PROVENANCE_DUMP": str(second)})
        assert first.read_bytes() == second.read_bytes()
