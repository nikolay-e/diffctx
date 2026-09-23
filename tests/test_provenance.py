"""Every run says what produced it: the effective configuration, its hash, the
input revisions, the selection parameters and the tokenizer travel with the
artifact on every structured surface, and an evaluation run refuses undeclared
knobs. Without this record two runs that differ cannot say what differed."""

from __future__ import annotations

import json
import os
import subprocess
import sys

import diffctx
from tests.framework.pygit2_backend import Pygit2Repo


def _repo(tmp_path):
    repo = Pygit2Repo(tmp_path / "repo")
    repo.add_file("lib.py", "def helper(x):\n    return x + 1\n")
    repo.add_file("main.py", "from lib import helper\n\nprint(helper(1))\n")
    repo.commit("initial")
    repo.add_file("lib.py", "def helper(x):\n    return x + 2\n")
    repo.commit("bump helper")
    return repo


def test_the_artifact_carries_its_provenance_on_every_structured_surface(tmp_path):
    repo = _repo(tmp_path)
    result = diffctx.build_diff_context(root_dir=repo.path, diff_range="HEAD~1", budget_tokens=4000)
    prov = result["provenance"]
    assert prov["schema"] == "diffctx.provenance.v1"
    assert prov["engine"]["name"] == "diffctx"
    assert prov["engine"]["version"] == diffctx.__version__
    assert len(prov["effective_config_hash"]) == 16
    assert "effective_config" not in prov, "the ~500-token record is opt-in; the hash travels always"
    assert prov["selection"]["budget_tokens"] == 4000
    assert prov["selection"]["budget_requested"] == 4000
    assert prov["selection"]["gate"] == "admission"
    # `git diff HEAD~1` compares a commit with the working tree: one id, no head.
    assert prov["input"]["working_tree"] is True
    assert len(prov["input"]["base"]) == 40
    assert "head" not in prov["input"]
    committed = diffctx.build_diff_context(root_dir=repo.path, diff_range="HEAD~1..HEAD")["provenance"]["input"]
    assert committed["working_tree"] is False
    assert committed["base"] == prov["input"]["base"]
    assert len(committed["head"]) == 40
    assert committed["head"] != committed["base"]
    full = json.loads(
        _child(
            {"DIFFCTX_PROVENANCE": "full"},
            f"""
import json, diffctx
r = diffctx.build_diff_context(root_dir={str(repo.path)!r}, diff_range="HEAD~1", budget_tokens=4000)
print(json.dumps(r["provenance"]))
""",
        ).stdout
    )
    cfg = full["effective_config"]
    assert cfg["schema"] == "diffctx.effective_config.v1"
    assert cfg["scoring"] == "ego"
    assert cfg["tokenizer"] == {"id": "o200k_base", "safety_factor": 1.0}
    assert cfg["profiles"]["scoring"]
    assert cfg["profiles"]["edge_weights"]
    assert cfg["profiles"]["parser"]
    assert full["resource_limits"]["max_wall_secs"] == 300
    assert full["effective_config_hash"] == prov["effective_config_hash"]

    assert json.loads(diffctx.to_json(result))["provenance"]["effective_config_hash"] == prov["effective_config_hash"]
    rendered = diffctx.to_yaml(result)
    assert "provenance:" in rendered
    assert prov["effective_config_hash"] in rendered
    assert "provenance" not in diffctx.to_markdown(result)


def test_the_same_input_and_configuration_hash_the_same(tmp_path):
    repo = _repo(tmp_path)
    first = diffctx.build_diff_context(root_dir=repo.path, diff_range="HEAD~1")
    second = diffctx.build_diff_context(root_dir=repo.path, diff_range="HEAD~1")
    assert first["provenance"]["effective_config_hash"] == second["provenance"]["effective_config_hash"]
    assert first["provenance"]["input"] == second["provenance"]["input"]


def _child(env: dict[str, str], code: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run([sys.executable, "-c", code], capture_output=True, text=True, env={**os.environ, **env})


def test_an_override_changes_the_hash_and_is_recorded(tmp_path):
    repo = _repo(tmp_path)
    code = f"""
import json, diffctx
r = diffctx.build_diff_context(root_dir={str(repo.path)!r}, diff_range="HEAD~1")
p = r["provenance"]
print(json.dumps({{"hash": p["effective_config_hash"], "depth": p["effective_config"]["graph_depth"], "overrides": p["effective_config"]["overrides"]}}))
"""
    base = json.loads(_child({"DIFFCTX_PROVENANCE": "full"}, code).stdout)
    overridden = json.loads(_child({"DIFFCTX_PROVENANCE": "full", "DIFFCTX_OP_GRAPH_DEPTH": "3"}, code).stdout)
    assert base["depth"] == 2
    assert overridden["depth"] == 3
    assert base["hash"] != overridden["hash"]
    assert overridden["overrides"] == {"DIFFCTX_OP_GRAPH_DEPTH": "3"}


def test_strict_mode_refuses_an_undeclared_override(tmp_path):
    repo = _repo(tmp_path)
    code = f"""
import diffctx
diffctx.build_diff_context(root_dir={str(repo.path)!r}, diff_range="HEAD~1")
"""
    proc = _child({"DIFFCTX_EVAL_STRICT": "1", "DIFFCTX_SECRET_KNOB": "1"}, code)
    assert proc.returncode != 0
    assert "DIFFCTX_SECRET_KNOB" in proc.stderr
    assert _child({"DIFFCTX_EVAL_STRICT": "1", "DIFFCTX_OP_GRAPH_DEPTH": "2"}, code).returncode == 0


def test_the_safety_factor_scales_every_count_and_is_recorded(tmp_path):
    repo = _repo(tmp_path)
    code = f"""
import json, diffctx
from diffctx._diffctx import count_tokens
r = diffctx.build_diff_context(root_dir={str(repo.path)!r}, diff_range="HEAD~1")
print(json.dumps({{"count": count_tokens("def helper(x): return x + 1"), "factor": r["provenance"]["effective_config"]["tokenizer"]["safety_factor"]}}))
"""
    plain = json.loads(_child({"DIFFCTX_PROVENANCE": "full"}, code).stdout)
    scaled = json.loads(_child({"DIFFCTX_PROVENANCE": "full", "DIFFCTX_TOKEN_SAFETY_FACTOR": "2"}, code).stdout)
    assert scaled["factor"] == 2.0
    assert plain["factor"] == 1.0
    assert scaled["count"] == 2 * plain["count"]
