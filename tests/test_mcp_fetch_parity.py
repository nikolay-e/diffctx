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
    repo.add_file("src/übung ü.py", "def uebung():\n    return UNICODE_OK\n")
    repo.add_file("docs dir/with space.py", "def spaced():\n    return SPACE_OK\n")
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

    # The refused set, against an enumerated expectation rather than against
    # withheld_paths itself (the fetch calls that function, so comparing the
    # two could not go red on a policy bug).
    all_paths = sorted({i["path"] for i in loc["items"]} | set(probes))
    refused = {p for p in all_paths if "Not available" in _section(bodies, p)}
    assert refused == {".netrc", "hidden.py"}
    assert set(withheld_paths(str(repo.path), all_paths)) == refused


def test_a_unicode_or_spaced_path_round_trips_from_locate_to_fetch(tmp_path):
    # git C-quotes such paths in its diff output; an id built from the quoted
    # form names a file that does not exist.
    repo = _repo(tmp_path)
    loc = json.loads(_call({"repo_path": str(repo.path), "diff_ref": "HEAD~1..HEAD", "mode": "locate"}))
    ids = [f"{i['path']}:{i['lines']}" for i in loc["items"]]
    assert {i["path"] for i in loc["items"]} >= {"src/übung ü.py", "docs dir/with space.py"}, ids

    bodies = _call({"repo_path": str(repo.path), "diff_ref": "HEAD~1..HEAD", "fragment_ids": ids})

    assert "UNICODE_OK" in bodies
    assert "SPACE_OK" in bodies
    for fragment_id in ids:
        assert "Not available" not in _section(bodies, fragment_id.rsplit(":", 1)[0]), fragment_id
        assert "Not found" not in _section(bodies, fragment_id.rsplit(":", 1)[0]), fragment_id


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


def test_a_bare_ref_reads_bodies_from_the_working_tree(tmp_path):
    """`diff_ref="HEAD"` is git's HEAD-versus-working-tree, so the ranking
    describes the files on disk; a fetch that read the HEAD blob returned the
    code the change replaced, under the id the ranking handed out."""
    git = Pygit2Repo(tmp_path / "bare_ref")
    git.add_file("app.py", "def f():\n    return 111\n")
    git.commit("init")
    repo = git.path
    (repo / "app.py").write_text("def f():\n    return 222\n", encoding="utf-8")

    for ref in ("HEAD", "", "HEAD~0"):
        items = json.loads(build_locate(root_dir=repo, diff_range=ref, budget_tokens=8000, timeout=60))["items"]
        ids = [f"{it['path']}:{it['lines']}" for it in items]
        body = fetch_fragments(repo, ref, ids, 1_000_000)
        assert "return 222" in body, (ref, body)
        assert "return 111" not in body, (ref, body)


def test_a_file_the_revision_lacks_is_reported_not_read_from_disk(tmp_path):
    """Under a revision label the body must come from that revision. A file
    created after it, or a revision git cannot resolve, used to fall through
    to the working tree and be presented as historical source."""
    git = Pygit2Repo(tmp_path / "hist")
    git.add_file("app.py", "def f():\n    return 111\n")
    git.commit("init")
    repo = git.path
    (repo / "new.py").write_text("NEW = 1\n", encoding="utf-8")
    (repo / "app.py").write_text("def f():\n    return 222\n", encoding="utf-8")

    later = fetch_fragments(repo, "HEAD~0..HEAD", ["new.py:1-1"], 1_000_000)
    assert "Not found at HEAD" in later, later
    assert "NEW = 1" not in later, later
    bogus = fetch_fragments(repo, "HEAD..nosuchrev", ["app.py:1-2"], 1_000_000)
    assert "Not found at nosuchrev" in bogus, bogus
    assert "return 222" not in bogus, bogus


@pytest.mark.parametrize("diff_ref", ["HEAD~1..", "HEAD~1..."])
def test_a_three_dot_range_reads_bodies_from_its_right_side(tmp_path: Path, diff_ref: str) -> None:
    import subprocess

    from diffctx._native import build_diff_context

    repo = Pygit2Repo(tmp_path / "repo")
    repo.add_file("a.py", "def a():\n    return 1\n")
    repo.commit("base")
    repo.add_file("a.py", "def a():\n    return 2\n")
    repo.commit("return two")
    head_body = subprocess.run(["git", "show", "HEAD:a.py"], cwd=repo.path, capture_output=True, text=True, check=True).stdout

    result = build_diff_context(root_dir=Path(repo.path), diff_range=diff_ref, timeout=60)
    changed = [f for f in result["fragments"] if f["path"] == "a.py" and f.get("role") == "changed"]
    assert changed, result["fragments"]
    fragment = changed[0]
    start, end = (int(n) for n in fragment["lines"].split("-"))
    assert fragment["content"].rstrip("\n") == "\n".join(head_body.splitlines()[start - 1 : end])
    assert "return 2" in fragment["content"]
    assert result["commit_messages"]

    bodies = fetch_fragments(Path(repo.path), diff_ref, [f"a.py:{fragment['lines']}"], 1_000_000)
    assert "return 2" in bodies
    assert "return 1" not in bodies


def _call(args: dict) -> str:
    from diffctx.mcp.server import mcp

    return asyncio.run(mcp.call_tool("diffctx_context", args))[0][0].text


def test_a_bare_clone_is_refused_on_every_surface(tmp_path):
    # Bare, the same history served `hidden.py` on fetch, pack and locate: `.diffctx/ignore` is read
    # from a checkout that does not exist, and `check-ignore` refuses to run. Parity with the
    # worktree is impossible, so the surface refuses rather than answering with the rules switched
    # off (T3.2).
    import subprocess

    from mcp.server.fastmcp.exceptions import ToolError

    repo = _repo(tmp_path)
    bare = tmp_path / "bare.git"
    subprocess.run(["git", "clone", "--bare", "--quiet", str(repo.path), str(bare)], check=True, capture_output=True)

    calls = [
        {"repo_path": str(bare), "diff_ref": "HEAD~1..HEAD", "fragment_ids": ["hidden.py", ".netrc", "app.py"]},
        {"repo_path": str(bare), "diff_ref": "HEAD~1..HEAD", "mode": "pack"},
        {"repo_path": str(bare), "diff_ref": "HEAD~1..HEAD", "mode": "locate"},
    ]
    for args in calls:
        with pytest.raises(ToolError, match="bare repository") as refused:
            _call(args)
        assert "LEAK_" not in str(refused.value)
        assert "hidden.py" not in str(refused.value)


@pytest.mark.timeout(120)
def test_every_id_locate_ranks_in_a_file_above_the_fetch_cap_returns_a_body(tmp_path):
    # The engine ranks fragments of changed files up to its own 5 MB limit; the fetch applied its
    # 256 KB cap to the whole file and answered Skipped for every id the ranking had just handed out
    # (T5.2). The cap belongs on the slice served, not on the file it is cut from.
    #
    # Which lines those ids point at is the engine's business (the wrong-id half of T5.2 is tracked
    # there); this asserts only that each id is fetchable.
    from diffctx.mcp.fetch import MAX_FETCH_IDS
    from diffctx.mcp.server import _DEFAULT_MAX_FILE_BYTES

    repo = Pygit2Repo(tmp_path / "big")
    body = "\n".join(f"def f{i}():\n    return {i}  # {'x' * 60}" for i in range(4000)) + "\n"
    assert len(body.encode()) > _DEFAULT_MAX_FILE_BYTES
    repo.add_file("big.py", body)
    repo.commit("base")
    repo.add_file("big.py", body + "\n\ndef tail():\n    return -1\n")
    repo.commit("append")

    loc = json.loads(build_locate(root_dir=repo.path, diff_range="HEAD~1..HEAD", budget_tokens=8000, tau=DEFAULT_TAU, timeout=60))
    ids = [f"{i['path']}:{i['lines']}" for i in loc["items"]]
    assert ids, loc
    assert all(i.startswith("big.py:") for i in ids), ids

    for start in range(0, len(ids), MAX_FETCH_IDS):
        chunk = ids[start : start + MAX_FETCH_IDS]
        bodies = _call({"repo_path": str(repo.path), "diff_ref": "HEAD~1..HEAD", "fragment_ids": chunk})
        assert "Skipped" not in bodies, bodies
        assert bodies.count("\n```py\n") == len(chunk), bodies

    whole = fetch_fragments(Path(repo.path), "HEAD~1..HEAD", ["big.py"], _DEFAULT_MAX_FILE_BYTES)
    assert "Skipped: fragment exceeds 262,144 bytes" in whole


@pytest.mark.parametrize("diff_ref", ["HEAD~1..", "HEAD~1..."])
def test_an_open_range_fetches_from_head_not_the_dirty_working_tree(tmp_path: Path, diff_ref: str) -> None:
    repo = Pygit2Repo(tmp_path / "repo")
    repo.add_file("a.py", "def a():\n    return 1\n")
    repo.commit("base")
    repo.add_file("a.py", "def a():\n    return 2\n")
    repo.commit("return two")
    (Path(repo.path) / "a.py").write_text("def a():\n    return 3\n", encoding="utf-8")

    bodies = fetch_fragments(Path(repo.path), diff_ref, ["a.py:1-2"], 1_000_000)
    assert "return 2" in bodies
    assert "return 3" not in bodies
