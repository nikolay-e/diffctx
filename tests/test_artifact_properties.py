"""Properties every artifact holds whatever the repository looks like: the
same input yields the same artifact, the rendered document respects the
budget or is the bare summary, every changed file is in the inventory and
says whether it is represented, every path is a repository path, and the
four formats are renderings of one artifact."""

from __future__ import annotations

import json
import tempfile
from pathlib import Path

import pytest

pytest.importorskip("hypothesis")

from hypothesis import HealthCheck, given, settings
from hypothesis import strategies as st

import diffctx
from diffctx._diffctx import count_tokens
from diffctx.writer import fit_to_budget, tree_to_string
from tests.framework.pygit2_backend import Pygit2Repo

NAMES = ["alpha", "beta", "gamma", "delta", "omega"]


def _module(name: str, callees: list[str], body_lines: int) -> str:
    calls = "\n".join(f"    {c}({i})" for i, c in enumerate(callees))
    filler = "\n".join(f"    x_{i} = {i}" for i in range(body_lines))
    return f"def {name}(n):\n{filler}\n{calls}\n    return n\n"


repos = st.fixed_dictionaries(
    {
        "modules": st.lists(st.sampled_from(NAMES), min_size=2, max_size=4, unique=True),
        "edges": st.lists(st.tuples(st.integers(0, 3), st.integers(0, 3)), max_size=4),
        "changed": st.lists(st.integers(0, 3), min_size=1, max_size=2, unique=True),
        "body": st.integers(0, 6),
        "budget": st.sampled_from([200, 600, 2000, None]),
    }
)


def _source(m: str, callees: list[str], body: int) -> str:
    imports = "".join(f"from pkg.{c} import {c}\n" for c in callees)
    return imports + _module(m, callees, body)


def _build(spec: dict) -> tuple[Pygit2Repo, list[str]]:
    mods = spec["modules"]
    root = Path(tempfile.mkdtemp()) / "repo"
    repo = Pygit2Repo(root)
    callees: dict[str, list[str]] = {m: [] for m in mods}
    for a, b in spec["edges"]:
        if a < len(mods) and b < len(mods) and a != b and mods[b] not in callees[mods[a]]:
            callees[mods[a]].append(mods[b])
    for m in mods:
        repo.add_file(f"pkg/{m}.py", _source(m, callees[m], spec["body"]))
    repo.commit("initial")
    changed = [mods[i] for i in spec["changed"] if i < len(mods)] or [mods[0]]
    for m in changed:
        repo.add_file(f"pkg/{m}.py", _source(m, callees[m], spec["body"] + 2))
    repo.commit("change " + ", ".join(changed))
    return repo, [f"pkg/{m}.py" for m in changed]


def _strip_latency(tree: dict) -> dict:
    return {k: v for k, v in tree.items() if k != "latency"}


@settings(max_examples=12, deadline=None, suppress_health_check=[HealthCheck.too_slow])
@given(spec=repos)
def test_same_input_same_artifact_within_budget_and_fully_inventoried(spec):
    repo, changed = _build(spec)
    kwargs = {"root_dir": repo.path, "diff_range": "HEAD~1"}
    if spec["budget"] is not None:
        kwargs["budget_tokens"] = spec["budget"]

    first = _strip_latency(diffctx.build_diff_context(**kwargs))
    second = _strip_latency(diffctx.build_diff_context(**kwargs))
    assert first == second, "two runs of one input differ"

    repo_files = {str(p.relative_to(repo.path)) for p in repo.path.rglob("*.py")}
    inventoried = {c["path"] for c in first["changes"]}
    assert set(changed) <= inventoried, "a changed file is missing from the inventory"
    assert inventoried <= repo_files
    represented = {f["path"] for f in first["fragments"]}
    assert represented <= repo_files, "a fragment names a path outside the repository"
    for entry in first["changes"]:
        assert entry["represented"] == (entry["path"] in represented), entry

    for fmt in ("md", "yaml", "json", "txt"):
        tree, rendered = fit_to_budget(dict(first), fmt)
        if spec["budget"] is not None and tree["fragments"]:
            assert count_tokens(rendered) <= spec["budget"], (fmt, count_tokens(rendered))
        assert {c["path"] for c in tree["changes"]} == inventoried, "the fit dropped an inventory row"

    fitted, rendered = fit_to_budget(dict(first), "json")
    doc = json.loads(rendered)
    assert [f["path"] for f in doc["fragments"]] == [f["path"] for f in fitted["fragments"]]
    assert doc["changes"] == fitted["changes"], "the fit keeps the inventory and marks what it dropped"
    assert doc["schema"] == "diffctx.context.v1"
    assert json.loads(tree_to_string(first, "json")) == doc, "tree_to_string is the same fit"
