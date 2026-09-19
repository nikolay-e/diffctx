"""A token pasted into a source file the withhold policy cannot know about
must not cross any read surface: the diff artifact, the raw diff bundle, the
MCP fetch by fragment id, the MCP glob reader, and tree mode."""

from __future__ import annotations

import json

import pytest

import diffctx
from tests.conftest import run_diffctx_subprocess
from tests.framework.pygit2_backend import Pygit2Repo

# Assembled at runtime so the repository's own secret scanners do not trip on
# the known-bad input this file exists to prove.
AWS = "AKIA" + "IOSFODNN7EXAMPLE"
GH = "ghp_" + "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghij1234"  # pragma: allowlist secret
PEM_BODY = "MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQC"  # pragma: allowlist secret
_KEY = "PRIVATE KEY"
PEM = f"-----BEGIN RSA {_KEY}-----\n{PEM_BODY}\n-----END RSA {_KEY}-----"
LEAKS = [AWS, GH, PEM_BODY]


def _repo_with_planted_secrets(tmp_path):
    repo = Pygit2Repo(tmp_path / "repo")
    repo.add_file("src/settings.py", "REGION = 'eu-central-1'\n\ndef client():\n    return REGION\n")
    repo.commit("initial")
    repo.add_file(
        "src/settings.py",
        f"REGION = 'eu-central-1'\nACCESS_KEY = '{AWS}'\nGH_TOKEN = '{GH}'\nPEM = '''{PEM}'''\n\ndef client():\n    return REGION\n",
    )
    repo.commit(f"wire the client; temporary key {AWS}")
    return repo


def _assert_clean(text: str, where: str) -> None:
    for leak in LEAKS:
        assert leak not in text, (where, leak)


def test_the_artifact_redacts_planted_secrets_and_says_so(tmp_path):
    repo = _repo_with_planted_secrets(tmp_path)
    tree = diffctx.build_diff_context(root_dir=repo.path, diff_range="HEAD~1")

    for fmt in (diffctx.to_json, diffctx.to_yaml, diffctx.to_markdown, diffctx.to_text):
        _assert_clean(fmt(tree), fmt.__name__)

    doc = json.loads(diffctx.to_json(tree))
    assert doc["redactions"]["count"] >= 4
    assert {"aws_access_key", "github_token", "private_key"} <= set(doc["redactions"]["categories"])
    assert "sanitization_redaction" in doc["coverage"]["limit_reasons"]
    joined = "\n".join(f["content"] for f in doc["fragments"] if f.get("content"))
    assert "[REDACTED:aws_access_key]" in joined
    assert "[REDACTED:private_key]" in joined
    assert "REGION = 'eu-central-1'" in joined, "the rest of the fragment is untouched"
    assert all("[REDACTED:aws_access_key]" in m for m in doc["commit_messages"])


def test_the_raw_diff_bundle_is_sanitized_too(tmp_path):
    repo = _repo_with_planted_secrets(tmp_path)
    result = run_diffctx_subprocess(
        [".", "--diff", "HEAD~1", "--with-raw-diff", "-f", "md", "-q"],
        cwd=repo.path,
    )
    assert result.returncode == 0, result.stderr
    _assert_clean(result.stdout, "raw diff")
    assert "[REDACTED:github_token]" in result.stdout


def test_tree_mode_redacts_and_warns(tmp_path):
    repo = _repo_with_planted_secrets(tmp_path)
    result = run_diffctx_subprocess([".", "-f", "yaml"], cwd=repo.path)
    assert result.returncode == 0, result.stderr
    _assert_clean(result.stdout, "tree mode")
    assert "[REDACTED:aws_access_key]" in result.stdout
    assert "redactions: 3" in result.stdout, "the node says how many strings it lost"
    assert "3 credential-shaped string(s) redacted" in result.stderr


async def test_mcp_fetch_and_glob_surfaces_redact(tmp_path):
    pytest.importorskip("mcp")
    from diffctx.mcp.server import mcp, register_legacy_tools
    from tests.test_mcp import _get_text

    repo = _repo_with_planted_secrets(tmp_path)
    fetched = await mcp.call_tool(
        "diffctx_context",
        {"repo_path": str(repo.path), "diff_ref": "HEAD~1..HEAD", "fragment_ids": ["src/settings.py:1-7"]},
    )
    text = _get_text(fetched)
    _assert_clean(text, "mcp fetch")
    assert "credential-shaped string(s) redacted" in text

    register_legacy_tools(mcp)
    globbed = await mcp.call_tool("get_file_context", {"repo_path": str(repo.path), "patterns": ["src/*.py"]})
    _assert_clean(_get_text(globbed), "mcp glob")
