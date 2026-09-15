"""A resource cap that binds produces a valid artifact that names the limit —
on every surface — instead of an error or a silently complete-looking result.
The caps are the known-bad probes of the resource contract: a clean repo can
never tell a working limit from one printing the words while doing nothing."""

from __future__ import annotations

import json
import os
import subprocess
import sys

import diffctx
from tests.framework.pygit2_backend import Pygit2Repo


def _repo(tmp_path):
    repo = Pygit2Repo(tmp_path / "repo")
    for i in range(6):
        repo.add_file(f"mod_{i}.py", f"def fn_{i}(x):\n    return x + {i}\n")
    repo.add_file("main.py", "\n".join(f"from mod_{i} import fn_{i}" for i in range(6)) + "\n")
    repo.commit("initial")
    repo.add_file("main.py", "\n".join(f"from mod_{i} import fn_{i}" for i in range(6)) + "\nEXTRA = fn_0(1)\n")
    repo.commit("use fn_0")
    return repo


def _run(repo, env: dict[str, str]) -> dict:
    code = f"""
import json, diffctx
r = diffctx.build_diff_context(root_dir={str(repo.path)!r}, diff_range="HEAD~1..HEAD")
print(json.dumps({{"coverage": r.get("coverage"), "changed": r.get("changed_files"), "md": diffctx.to_markdown(r), "yaml": diffctx.to_yaml(r), "json": diffctx.to_json(r)}}))
"""
    proc = subprocess.run([sys.executable, "-c", code], capture_output=True, text=True, env={**os.environ, **env})
    assert proc.returncode == 0, proc.stderr[-600:]
    return json.loads(proc.stdout)


def test_a_complete_run_carries_no_coverage_block(tmp_path):
    result = diffctx.build_diff_context(root_dir=_repo(tmp_path).path, diff_range="HEAD~1..HEAD")
    assert "coverage" not in result
    assert "Coverage:" not in diffctx.to_markdown(result)


def test_the_contribution_cap_is_disclosed_on_every_surface(tmp_path):
    out = _run(_repo(tmp_path), {"DIFFCTX_MAX_EDGE_CONTRIBUTIONS": "1"})
    coverage = out["coverage"]
    assert coverage["status"] == "partial"
    assert "edge_contribution_limit" in coverage["limit_reasons"]
    assert coverage["resources"]["edge_contributions"] >= 1
    assert out["changed"] == ["main.py"], "the change itself is never dropped by a graph cap"
    assert "Coverage: partial" in out["md"]
    assert "edge_contribution_limit" in out["md"]
    assert "coverage:" in out["yaml"]
    assert "edge_contribution_limit" in out["yaml"]
    assert json.loads(out["json"])["coverage"]["limit_reasons"] == coverage["limit_reasons"]


def test_the_byte_cap_bounds_discovery_but_never_the_change(tmp_path):
    out = _run(_repo(tmp_path), {"DIFFCTX_MAX_SOURCE_BYTES": "1"})
    assert "total_byte_limit" in out["coverage"]["limit_reasons"]
    assert out["changed"] == ["main.py"]
    assert "main.py" in out["md"]
    assert "**changed**" in out["md"]


def test_the_candidate_cap_is_disclosed(tmp_path):
    out = _run(_repo(tmp_path), {"DIFFCTX_MAX_CANDIDATE_FILES": "1"})
    assert "candidate_limit" in out["coverage"]["limit_reasons"]
    assert out["coverage"]["resources"]["candidate_files"] == 1
