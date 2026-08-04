"""Property fuzz over the MCP path jail (#147).

The hand-written jail tests each pin one attack that was once possible. This
asks the complementary question: over a generated space of hostile path shapes,
is the jail's *contract* ever violated — regardless of which shape does it?

The contract has exactly two acceptable outcomes for any input:

1. a refusal, or
2. content, all of which lies inside the repository.

Anything else is a finding. Notably a crash is NOT acceptable: an unhandled
exception carries a traceback and absolute paths into a payload a model reads,
which is the disclosure half of the same issue.

Both admitted surfaces are covered — `fragment_ids` (the reader #127 added) and
the legacy glob tool — because a jail is only as strong as its weakest door.
"""

from __future__ import annotations

import asyncio
import string

import pytest

pytest.importorskip("mcp")
pytest.importorskip("hypothesis")

from hypothesis import HealthCheck, given, settings
from hypothesis import strategies as st
from mcp.server.fastmcp.exceptions import ToolError

from tests.framework.pygit2_backend import Pygit2Repo

CANARY = "JAIL-CANARY-must-never-surface"

# Shapes that have historically broken path handling somewhere: traversal in
# every position, absolute escapes, redundant separators, the Windows form,
# percent- and dot-encodings, and a NUL that some layers treat as a terminator.
_HOSTILE_SEGMENTS = st.sampled_from(
    [
        "..",
        "../..",
        "....//",
        ".",
        "",
        "/",
        "//",
        "\\",
        "..\\",
        "%2e%2e",
        "..%2f",
        "\x00",
        "~",
        "secret.txt",
        "sub",
        " ",
        "\n",
    ]
)

_path_like = st.lists(_HOSTILE_SEGMENTS, min_size=1, max_size=6).map("/".join)
_line_span = st.one_of(
    st.just(""),
    st.just(":1-2"),
    st.just(":0-0"),
    st.just(":-1--1"),
    st.just(":999999-999999"),
    st.text(alphabet=string.printable, max_size=6).map(lambda s: f":{s}"),
)


@pytest.fixture(scope="module")
def jailed(tmp_path_factory):
    """A repo with a secret placed just outside it, plus a symlink pointing there.

    Module-scoped: the fuzz runs hundreds of examples and re-cloning per example
    would dominate the runtime without testing anything more.
    """
    root = tmp_path_factory.mktemp("jail")
    (root / "outside").mkdir()
    (root / "outside" / "secret.txt").write_text(f"{CANARY}\n")

    repo = Pygit2Repo(root / "inside")
    repo.add_file("src/app.py", "def run():\n    return 1\n")
    repo.add_file("sub/nested.py", "def nested():\n    return 2\n")
    repo.commit("initial")
    (repo.path / "escape").symlink_to(root / "outside", target_is_directory=True)
    return repo


def _call(server, args: dict) -> str:
    """The tool's response text, or "" when it refused.

    A refusal is a pass. An exception that is not a ToolError is not — it means
    something escaped the tool's own error handling.
    """
    try:
        result = asyncio.run(server.call_tool("diffctx_context", args))
    except ToolError:
        return ""
    except Exception as e:
        pytest.fail(f"unhandled {type(e).__name__} escaped the tool: {e}")
    blocks = result[0]
    return blocks[0].text if blocks else ""


@pytest.fixture
def server():
    from diffctx.mcp.server import mcp

    return mcp


@settings(max_examples=250, deadline=None, suppress_health_check=[HealthCheck.function_scoped_fixture])
@given(path=_path_like, span=_line_span)
def test_no_fragment_id_reaches_outside_the_repository(server, jailed, path, span):
    text = _call(
        server,
        {"repo_path": str(jailed.path), "diff_ref": "HEAD", "fragment_ids": [f"{path}{span}"]},
    )
    assert CANARY not in text


@settings(max_examples=120, deadline=None, suppress_health_check=[HealthCheck.function_scoped_fixture])
@given(path=_path_like)
def test_no_fragment_id_reaches_outside_through_the_symlink(server, jailed, path):
    """`escape/` is a real directory inside the repo that points out of it.

    Containment cannot be decided lexically here — the path looks internal — so
    this is the case a naive prefix check passes and a resolving check catches.
    """
    text = _call(
        server,
        {"repo_path": str(jailed.path), "diff_ref": "HEAD", "fragment_ids": [f"escape/{path}"]},
    )
    assert CANARY not in text


@settings(max_examples=120, deadline=None, suppress_health_check=[HealthCheck.function_scoped_fixture])
@given(path=_path_like)
def test_a_refusal_never_names_a_resolved_path(server, jailed, path):
    """Refusals go to a model that may relay them. They may echo the caller's own
    argument; they may not disclose what the filesystem resolved it to."""
    try:
        asyncio.run(
            server.call_tool(
                "diffctx_context",
                {"repo_path": f"{jailed.path}/{path}", "diff_ref": "HEAD"},
            )
        )
    except ToolError as e:
        message = str(e)
        assert "Traceback" not in message
        assert str(jailed.path.parent / "outside") not in message
    except Exception as e:
        pytest.fail(f"unhandled {type(e).__name__} escaped the tool: {e}")


@settings(max_examples=120, deadline=None, suppress_health_check=[HealthCheck.function_scoped_fixture])
@given(pattern=_path_like)
def test_no_glob_pattern_reaches_outside_the_repository(server, jailed, pattern):
    from diffctx.mcp.server import register_legacy_tools

    register_legacy_tools(server)
    try:
        result = asyncio.run(server.call_tool("get_file_context", {"repo_path": str(jailed.path), "patterns": [pattern]}))
    except ToolError:
        return
    except Exception as e:
        pytest.fail(f"unhandled {type(e).__name__} escaped the tool: {e}")
    blocks = result[0]
    assert CANARY not in (blocks[0].text if blocks else "")
