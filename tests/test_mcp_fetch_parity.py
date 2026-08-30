from __future__ import annotations

import asyncio
import json
from pathlib import Path

import pytest

from diffctx._diffctx import DEFAULT_TAU, withheld_paths
from diffctx._native import build_locate
from tests.framework.pygit2_backend import Pygit2Repo

pytest.importorskip("mcp")

from diffctx.mcp.fetch import fetch_fragments


def _repo(tmp_path):
    repo = Pygit2Repo(tmp_path / "repo")
    repo.add_file("app.py", "def a():\n    return 1\n")
    repo.commit("base")
    repo.add_file("app.py", "def a():\n    return 2\n")
    repo.add_file(".netrc", "machine example.com login bob password LEAK_NETRC\n")  # pragma: allowlist secret
    repo.add_file("build/gen.py", "GEN_OK = 1\n")
    repo.add_file(".diffctx/ignore", "hidden.py\n")
    repo.add_file("hidden.py", "LEAK_HIDDEN = 1\n")
    repo.commit("change")
    return repo


def _section(text: str, path: str) -> str:
    marker = f"## {path}"
    start = text.index(marker)
    end = text.find("\n## ", start + 1)
    return text[start : end if end != -1 else len(text)]


def test_fetch_refuses_exactly_what_the_engine_withholds(tmp_path):
    repo = _repo(tmp_path)
    loc = json.loads(build_locate(root_dir=repo.path, diff_range="HEAD~1..HEAD", budget_tokens=8000, tau=DEFAULT_TAU, timeout=60))
    ranked = [f"{i['path']}:{i['lines']}" for i in loc["items"]]
    assert ranked, "the fixture must rank something"
    probes = [".netrc", "hidden.py", "build/gen.py", "app.py"]

    bodies = fetch_fragments(Path(repo.path), "HEAD~1..HEAD", ranked + probes, 1_000_000)

    # Direction 1 (#228): a path the engine withheld is never served.
    assert "LEAK_NETRC" not in bodies
    assert "LEAK_HIDDEN" not in bodies
    # Direction 2: a path the engine ranked is never refused.
    for item in loc["items"]:
        assert "Not available" not in _section(bodies, item["path"]), item["path"]
    # `build/` is tree-mode noise, not a repository rule: selection ranks it,
    # so the fetch serves it (#228 direction 2). The glob reader, which is a
    # tree-mode reader, filters it — see the second test.
    assert "GEN_OK" in bodies

    # The invariant itself: refusal == the engine's own answer.
    all_paths = sorted({i["path"] for i in loc["items"]} | set(probes))
    withheld = set(withheld_paths(str(repo.path), all_paths))
    refused = {p for p in all_paths if "Not available" in _section(bodies, p)}
    assert refused == withheld


def test_server_and_legacy_tool_agree_with_the_engine(tmp_path):
    repo = _repo(tmp_path)
    from diffctx.mcp.server import mcp, register_legacy_tools

    result = asyncio.run(
        mcp.call_tool(
            "diffctx_context",
            {"repo_path": str(repo.path), "diff_ref": "HEAD~1..HEAD", "fragment_ids": [".netrc", "hidden.py", "app.py"]},
        )
    )
    text = result[0][0].text
    assert "LEAK_NETRC" not in text
    assert "LEAK_HIDDEN" not in text
    assert "return 2" in text

    register_legacy_tools(mcp)
    result = asyncio.run(mcp.call_tool("get_file_context", {"repo_path": str(repo.path), "patterns": ["**/*", ".netrc"]}))
    text = result[0][0].text
    assert "LEAK_NETRC" not in text
    assert "LEAK_HIDDEN" not in text
    # Two policies, not one. The glob reader is a tree-mode reader and applies
    # the same noise spec `get_tree_map` does, so `build/` stays out of it —
    # while `fetch_fragments` above serves that very file, because selection
    # ranked it and refusing a ranked fragment is the other half of #228.
    assert "GEN_OK" not in text
    assert "return 2" in text


def test_a_directory_the_repo_ignores_is_not_readable_through_a_glob(tmp_path):
    """The hole this closes: git reports `.venv/x.py` ignored **via its parent
    rule**, and the engine's attribution lookup drops ancestor-inherited
    matches on purpose (a tracked file under such a directory is not really
    ignored — #153). Reusing that lookup for a reader that walks the working
    tree answered "not ignored" for the entire contents of every ignored
    directory, so `.venv/`, `dist/` and `target/` became readable through the
    MCP glob. `.git/` is the same class and is not gitignored at all.
    """
    from diffctx._diffctx import withheld_paths

    repo = Pygit2Repo(tmp_path / "repo")
    repo.add_file(".gitignore", "secrets/\ndist/\n")
    repo.add_file("app.py", "OK = 1\n")
    repo.commit("base")
    for rel, body in [
        ("secrets/prod.env", "LEAK_DIRRULE = 1\n"),
        ("dist/bundle.js", "LEAK_DIST = 1\n"),
    ]:
        target = tmp_path / "repo" / rel
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(body)

    assert set(withheld_paths(str(repo.path), ["secrets/prod.env", "dist/bundle.js", ".git/config", "app.py"])) == {
        "secrets/prod.env",
        "dist/bundle.js",
        ".git/config",
    }

    from diffctx.mcp.server import mcp, register_legacy_tools

    register_legacy_tools(mcp)
    result = asyncio.run(mcp.call_tool("get_file_context", {"repo_path": str(repo.path), "patterns": ["**/*", ".git/config"]}))
    text = result[0][0].text
    assert "LEAK_DIRRULE" not in text
    assert "LEAK_DIST" not in text
    assert "[remote" not in text, "the glob served .git/config, which carries the remote URL"
    assert "OK = 1" in text
