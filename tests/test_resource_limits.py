"""A resource cap that binds produces a valid artifact that names the limit —
on every surface — instead of an error or a silently complete-looking result.
The caps are the known-bad probes of the resource contract: a clean repo can
never tell a working limit from one printing the words while doing nothing."""

from __future__ import annotations

import json
import os
import subprocess
import sys

import pytest
import yaml

import diffctx
from diffctx._diffctx import count_tokens
from tests.conftest import run_diffctx_subprocess
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


def _two_commit_repo(tmp_path, before: dict[str, str], after: dict[str, str]) -> Pygit2Repo:
    repo = Pygit2Repo(tmp_path / "repo")
    for path, content in before.items():
        repo.add_file(path, content)
    repo.commit("initial")
    for path, content in after.items():
        repo.add_file(path, content)
    repo.commit("change")
    return repo


def _docs_per_format(repo: Pygit2Repo, env: dict[str, str], *extra: str) -> dict[str, str]:
    out: dict[str, str] = {}
    for fmt in ("md", "yaml", "json"):
        result = run_diffctx_subprocess([".", "--diff", "HEAD~1..HEAD", "-f", fmt, "-q", *extra], cwd=repo.path, env=env)
        assert result.returncode == 0, (fmt, result.stderr)
        out[fmt] = result.stdout
    return out


def _assert_reason_on_every_format(docs: dict[str, str], reason: str) -> dict:
    assert reason in docs["md"], docs["md"][-600:]
    assert reason in yaml.safe_load(docs["yaml"])["coverage"]["limit_reasons"]
    doc = json.loads(docs["json"])
    assert reason in doc["coverage"]["limit_reasons"]
    return doc


_TARGET_BEFORE = "def target(x):\n    return x + 1\n"
_TARGET_AFTER = "def target(x):\n    return x + 2\n"


def _helper_of_size(size: int) -> str:
    body = "from core import target\n\n\ndef uses_target():\n    return target(1)\n\n\n"
    i = 0
    while len(body) < size:
        body += f"def filler_{i}(x):\n    return x * {i} + 12345\n\n\n"
        i += 1
    return body


@pytest.mark.parametrize(("size", "read"), [(99_000, True), (101_000, False)])
def test_a_context_file_over_the_size_cap_is_named_not_silently_dropped(tmp_path, size, read):
    repo = _two_commit_repo(tmp_path, {"core.py": _TARGET_BEFORE, "helper.py": _helper_of_size(size)}, {"core.py": _TARGET_AFTER})
    docs = _docs_per_format(repo, {})
    doc = json.loads(docs["json"])
    paths = {f["path"] for f in doc["fragments"]}
    if read:
        assert "helper.py" in paths
        assert "file_too_large" not in (doc.get("coverage") or {}).get("limit_reasons", [])
    else:
        assert "helper.py" not in paths
        _assert_reason_on_every_format(docs, "file_too_large")


def test_the_per_file_fragment_cap_keeps_the_edit_and_says_so(tmp_path):
    def module(edited: bool) -> str:
        return "".join(f"def f{i}(x):\n    return x + {999 if edited and i == 40 else i}\n\n\n" for i in range(80))

    repo = _two_commit_repo(tmp_path, {"m.py": module(False)}, {"m.py": module(True)})
    doc = _assert_reason_on_every_format(_docs_per_format(repo, {"DIFFCTX_MAX_FRAGMENTS": "5"}), "fragment_limit")
    changed = [f["symbol"] for f in doc["fragments"] if f.get("role") == "changed"]
    assert changed == ["f40"]


def test_the_per_node_edge_cap_is_disclosed(tmp_path):
    callers = {f"c{i}.py": f"from core import target\n\n\ndef call_{i}():\n    return target({i})\n" for i in range(6)}
    repo = _two_commit_repo(tmp_path, {"core.py": _TARGET_BEFORE, **callers}, {"core.py": _TARGET_AFTER})
    _assert_reason_on_every_format(_docs_per_format(repo, {"DIFFCTX_MAX_EDGES_PER_NODE": "1"}), "edge_limit")
    uncapped = json.loads(_docs_per_format(repo, {})["json"])
    assert "edge_limit" not in (uncapped.get("coverage") or {}).get("limit_reasons", [])


def _function(i: int, lines: int, value: int) -> str:
    body = "".join(
        f"    value_{j} = compute_something(argument_{j}, {value}, 'literal string number {j}')\n" for j in range(lines)
    )
    return f"def func_{i}(x):\n{body}    return x\n\n\n"


def _auto_budget(tmp_path, count: int, lines: int) -> dict:
    before = "".join(_function(i, lines, 0) for i in range(count))
    after = "".join(_function(i, lines, 1) for i in range(count))
    repo = _two_commit_repo(tmp_path, {"m.py": before}, {"m.py": after})
    result = run_diffctx_subprocess([".", "--diff", "HEAD~1..HEAD", "-f", "json", "-q"], cwd=repo.path)
    assert result.returncode == 0, result.stderr
    selection: dict = json.loads(result.stdout)["provenance"]["selection"]
    assert selection.get("budget_requested") is None
    return selection


def test_auto_budget_is_clamped(tmp_path):
    assert _auto_budget(tmp_path / "small", 1, 3)["budget_tokens"] == 8000
    assert _auto_budget(tmp_path / "wide", 30, 60)["budget_tokens"] == 48000
    per_core = 40
    core = sum(count_tokens(_function(i, 40, 1).rstrip("\n") + "\n") + per_core for i in range(4))
    assert 8000 < 3 * core < 48000
    assert _auto_budget(tmp_path / "middle", 4, 40)["budget_tokens"] == 3 * core


def _wide_change(tmp_path) -> Pygit2Repo:
    before = {f"src/package_{i}/module_with_a_long_name_{i}.py": f"def fn_{i}(x):\n    return x + {i}\n" for i in range(60)}
    after = {path: f"def fn_{i}(x):\n    y = x + {i}\n    return y * 2\n" for i, path in enumerate(before)}
    return _two_commit_repo(tmp_path, before, after)


@pytest.mark.parametrize(
    ("budget", "reasons"),
    [("4000", ["evidence_budget_exceeded", "selection_budget_exceeded"]), ("6000", ["selection_budget_exceeded"])],
)
def test_a_trimmed_diff_is_degraded_with_exact_representation(tmp_path, budget, reasons):
    repo = _wide_change(tmp_path)
    result = run_diffctx_subprocess([".", "--diff", "HEAD~1..HEAD", "-f", "json", "-q", "--budget", budget], cwd=repo.path)
    assert result.returncode == 0, result.stderr
    doc = json.loads(result.stdout)
    assert doc["coverage"]["status"] == "degraded"
    assert doc["coverage"]["limit_reasons"] == reasons
    represented = {f["path"] for f in doc["fragments"]}
    assert len(doc["changes"]) == 60
    for change in doc["changes"]:
        assert change["represented"] is (change["path"] in represented), change
    assert 0 < len(represented) < 60
