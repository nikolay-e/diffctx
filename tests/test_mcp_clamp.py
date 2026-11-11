"""`max_tokens` is the ceiling of a reply and `budget_tokens` is what the caller
asked for; the engine fits the first to the second (`diffctx::delivery`) and the
server only carries them. A budget not asked for is the engine's default for the
mode, the same one the CLI uses, so "under 2k tokens" holds through every door."""

from __future__ import annotations

import json

import pytest

pytest.importorskip("mcp", reason="mcp package not installed")

from diffctx._diffctx import count_tokens
from tests.framework.pygit2_backend import Pygit2Repo

CALLERS = 90


@pytest.fixture
def server():
    from diffctx.mcp.server import mcp

    return mcp


@pytest.fixture
def busy_repo(tmp_path):
    """One edited function with ninety callers, none of them folded away: more than any default answer lists."""
    repo = Pygit2Repo(tmp_path / "busy")
    repo.add_file("core.py", "def total(items):\n    return sum(items)\n")
    for i in range(CALLERS):
        repo.add_file(f"users/user_{i}.py", f"from core import total\n\n\ndef use_{i}(cart):\n    return total(cart) + {i}\n")
    repo.commit("initial")
    repo.add_file("core.py", "def total(items):\n    return round(sum(items), 2)\n")
    repo.commit("total rounds")
    return repo


async def _call(server, repo, **arguments) -> str:
    result = await server.call_tool("diffctx_context", {"repo_path": str(repo.path), "diff_ref": "HEAD~1..HEAD", **arguments})
    return result.content[0].text


@pytest.mark.asyncio
async def test_the_budget_is_optional_and_its_default_is_none(server):
    tool = next(t for t in await server.list_tools() if t.name == "diffctx_context")
    assert tool.input_schema["properties"]["budget_tokens"]["default"] is None


@pytest.mark.asyncio
async def test_an_impact_answer_is_under_two_thousand_tokens_without_a_budget(server, busy_repo):
    default = await _call(server, busy_repo)
    assert count_tokens(default) <= 2000, count_tokens(default)
    wider = await _call(server, busy_repo, budget_tokens=8000)
    assert count_tokens(wider) > 2000, "the fixture must have more to say than the default budget holds"


@pytest.mark.asyncio
@pytest.mark.parametrize("mode", ["impact", "pack"])
async def test_a_ceiling_below_the_answer_fits_it_and_never_refuses_it(server, busy_repo, mode):
    full = await _call(server, busy_repo, mode=mode, budget_tokens=-1, max_tokens=200_000)
    ceiling = count_tokens(full) // 2
    assert ceiling > 600, "the ceiling must leave room for the least the answer can say"
    reply = await _call(server, busy_repo, mode=mode, max_tokens=ceiling)
    assert "exceeding max_tokens" not in reply, reply
    assert count_tokens(reply) <= ceiling, f"{mode}: {count_tokens(reply)} tokens over max_tokens={ceiling}"


@pytest.mark.asyncio
async def test_a_ceiling_below_the_locate_document_cuts_items_and_keeps_json(server, busy_repo):
    full = json.loads(await _call(server, busy_repo, mode="locate", budget_tokens=-1, max_tokens=200_000))
    ceiling = count_tokens(json.dumps(full, separators=(",", ":"))) // 2
    reply = await _call(server, busy_repo, mode="locate", max_tokens=ceiling)
    assert "exceeding max_tokens" not in reply, reply
    assert count_tokens(reply) <= ceiling
    assert {(i["path"], i["lines"]) for i in json.loads(reply)["items"]} <= {(i["path"], i["lines"]) for i in full["items"]}


@pytest.mark.asyncio
async def test_a_ceiling_below_the_least_the_answer_can_say_sends_a_notice(server, busy_repo):
    reply = await _call(server, busy_repo, max_tokens=1)
    assert "exceeding max_tokens=1" in reply
    assert "core.py" not in reply, "nothing of the answer is sent past the ceiling"


@pytest.mark.asyncio
async def test_a_clipboard_that_fails_costs_the_reply_its_notice_not_its_ceiling(server, busy_repo, monkeypatch):
    monkeypatch.setattr("diffctx.clipboard.detect_clipboard_command", lambda: None)
    ceiling = 1200
    reply = await _call(server, busy_repo, mode="pack", clipboard=True, max_tokens=ceiling)
    assert reply.startswith("Note: clipboard unavailable"), reply
    assert count_tokens(reply) <= ceiling, f"{count_tokens(reply)} tokens over max_tokens={ceiling}"
