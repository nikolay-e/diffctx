"""The Claude plugin's git hooks run `diffctx hook <event>` from the pinned
PyPI package under uvx: the plugin directory only runs code in the reviewed
repository or a package pinned to an exact version, so the release binary it
used to download is gone. These drive the Python entry point and the plugin
script the way Claude Code does — a JSON event on stdin, a real repository."""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

from tests.framework.pygit2_backend import Pygit2Repo

PROJECT_ROOT = Path(__file__).resolve().parent.parent
SRC_DIR = PROJECT_ROOT / "src"
IMPACT_SCRIPT = PROJECT_ROOT / "plugin" / "hooks" / "diffctx-impact.sh"
SESSION_SCRIPT = PROJECT_ROOT / "plugin" / "hooks" / "diffctx-session.sh"


@pytest.fixture
def pending_change(tmp_path):
    repo = Pygit2Repo(tmp_path / "repo")
    repo.add_file("shop/pricing.py", "def total(items):\n    return sum(i.price for i in items)\n")
    repo.add_file(
        "shop/checkout.py",
        "from shop.pricing import total\n\n\ndef charge(cart):\n    return cart.pay(total(cart.items))\n",
    )
    repo.commit("initial")
    (repo.path / "shop" / "pricing.py").write_text(
        "def total(items):\n    return round(sum(i.price for i in items) * 1.19, 2)\n", encoding="utf-8"
    )
    return repo.path


def _event(repo: Path, hook_event: str, command: str) -> str:
    return json.dumps({"hook_event_name": hook_event, "cwd": str(repo), "tool_name": "Bash", "tool_input": {"command": command}})


def _env(tmp_path: Path) -> dict[str, str]:
    # Windows' default stdout encoding, on every OS: the answer carries dashes
    # cp1252 writes as non-UTF-8 bytes, and Claude Code reads UTF-8.
    return {
        **os.environ,
        "PYTHONPATH": str(SRC_DIR),
        "DIFFCTX_CACHE_DIR": str(tmp_path / "cache"),
        "PYTHONIOENCODING": "cp1252",
    }


def _hook(tmp_path: Path, args: list[str], stdin: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, "-m", "diffctx", "hook", *args],
        input=stdin,
        capture_output=True,
        text=True,
        encoding="utf-8",
        env=_env(tmp_path),
        timeout=120,
    )


def _context(stdout: str) -> str:
    return json.loads(stdout)["hookSpecificOutput"]["additionalContext"]


def test_a_commit_hears_its_callers_once(tmp_path, pending_change):
    first = _hook(tmp_path, ["pretooluse"], _event(pending_change, "PreToolUse", "git commit -am vat"))
    assert first.returncode == 0, first.stderr
    assert "shop/checkout.py::charge" in _context(first.stdout)

    again = _hook(tmp_path, ["pretooluse"], _event(pending_change, "PreToolUse", "git commit -am vat"))
    assert again.returncode == 0
    assert again.stdout == ""


def test_the_gate_denies_once_with_the_impact_in_the_reason(tmp_path, pending_change):
    denied = _hook(tmp_path, ["pretooluse", "--gate"], _event(pending_change, "PreToolUse", "git commit -am vat"))
    assert denied.returncode == 0, denied.stderr
    answer = json.loads(denied.stdout)["hookSpecificOutput"]
    assert answer["permissionDecision"] == "deny"
    assert "shop/checkout.py::charge" in answer["permissionDecisionReason"]


def test_anything_but_a_git_event_is_silence(tmp_path, pending_change):
    for stdin in (_event(pending_change, "PreToolUse", "ls -la"), "not json", ""):
        result = _hook(tmp_path, ["pretooluse"], stdin)
        assert result.returncode == 0, result.stderr
        assert result.stdout == ""


@pytest.mark.parametrize("args", [[], ["commit"], ["posttooluse", "--gate"], ["pretooluse", "--loud"]])
def test_a_wrong_invocation_is_a_usage_error(tmp_path, args):
    result = _hook(tmp_path, args, "")
    assert result.returncode == 2
    assert "usage: diffctx hook" in result.stderr


def _bashes() -> list[str]:
    # macOS /bin/bash is 3.2, where `set -u` treats an empty array as unbound;
    # 1.18.0's script died on it with no output on every stock Mac.
    found = [b for b in ("/bin/bash", shutil.which("bash")) if b and Path(b).is_file()]
    return sorted(set(found))


@pytest.mark.skipif(sys.platform == "win32", reason="the plugin scripts are exercised under POSIX bash")
@pytest.mark.parametrize("bash", _bashes())
@pytest.mark.parametrize("gate", [False, True])
def test_the_plugin_script_reaches_the_hook_on_an_add_and_commit_line(tmp_path, pending_change, bash, gate):
    launcher = tmp_path / "diffctx"
    launcher.write_text(f'#!/bin/sh\nexec "{sys.executable}" -m diffctx "$@"\n', encoding="utf-8")
    launcher.chmod(0o755)
    env = {
        "HOME": str(tmp_path),
        "PATH": "/usr/bin:/bin",
        "PYTHONPATH": str(SRC_DIR),
        "DIFFCTX_CACHE_DIR": str(tmp_path / "cache"),
        "DIFFCTX_HOOK_BIN": str(launcher),
        "CLAUDE_PLUGIN_ROOT": str(PROJECT_ROOT / "plugin"),
        "CLAUDE_PLUGIN_OPTION_IMPACT_GATE": "true" if gate else "false",
    }
    result = subprocess.run(
        [bash, str(IMPACT_SCRIPT)],
        input=_event(pending_change, "PreToolUse", "git add -A && git commit -m vat"),
        capture_output=True,
        text=True,
        encoding="utf-8",
        env=env,
        timeout=120,
    )
    assert result.returncode == 0, result.stderr
    answer = json.loads(result.stdout)["hookSpecificOutput"]
    text = answer["permissionDecisionReason"] if gate else answer["additionalContext"]
    assert "shop/checkout.py::charge" in text


@pytest.mark.skipif(sys.platform == "win32", reason="the plugin scripts are exercised under POSIX bash")
@pytest.mark.parametrize("bash", _bashes())
@pytest.mark.parametrize("impact_hook", ["true", "false"])
def test_session_start_removes_the_binaries_earlier_releases_downloaded(tmp_path, bash, impact_hook):
    data = tmp_path / "plugin-data"
    (data / "bin" / ".tmp.Xa81").mkdir(parents=True)
    for leftover in ("bin/diffctx-1.18.0", "bin/diffctx-1.18.1", "install-failed-1.18.1"):
        (data / leftover).write_bytes(b"\0" * 64)
    (data / "settings.json").write_text("{}", encoding="utf-8")
    env = {
        "HOME": str(tmp_path),
        "PATH": "/usr/bin:/bin",
        "CLAUDE_PLUGIN_ROOT": str(PROJECT_ROOT / "plugin"),
        "CLAUDE_PLUGIN_DATA": str(data),
        "CLAUDE_PLUGIN_OPTION_IMPACT_HOOK": impact_hook,
    }
    result = subprocess.run([bash, str(SESSION_SCRIPT)], capture_output=True, text=True, env=env, timeout=60)
    assert result.returncode == 0, result.stderr
    assert sorted(p.name for p in data.iterdir()) == ["settings.json"]


def _search(repo: Path, tool: str, query: str, session: str = "s1") -> str:
    tool_input = {"pattern": query, "path": str(repo)} if tool == "Grep" else {"command": query}
    return json.dumps(
        {"session_id": session, "hook_event_name": "PostToolUse", "cwd": str(repo), "tool_name": tool, "tool_input": tool_input}
    )


@pytest.mark.parametrize(
    ("tool", "query"),
    [("Bash", 'grep -rn "def total" .'), ("Bash", r"cd shop && rg -n 'total\(' ."), ("Grep", r"\btotal\b")],
)
def test_a_search_for_a_changed_name_hears_its_callers_once(tmp_path, pending_change, tool, query):
    first = _hook(tmp_path, ["posttooluse"], _search(pending_change, tool, query))
    assert first.returncode == 0, first.stderr
    assert "shop/checkout.py::charge" in _context(first.stdout)
    again = _hook(tmp_path, ["posttooluse"], _search(pending_change, tool, query))
    assert again.stdout == ""
    log = (tmp_path / "cache" / "diffctx" / "hook.log").read_text(encoding="utf-8").splitlines()
    assert [line.split()[5] for line in log] == ["shown", "seen"]


def test_a_search_answers_only_a_changed_name_on_a_pending_change(tmp_path, pending_change):
    unrelated = _hook(tmp_path, ["posttooluse"], _search(pending_change, "Bash", "grep -rn charge ."))
    assert unrelated.returncode == 0, unrelated.stderr
    assert unrelated.stdout == ""
    filtered = _hook(tmp_path, ["posttooluse"], _search(pending_change, "Bash", "cat shop/pricing.py | grep total"))
    assert filtered.stdout == ""
    subprocess.run(["git", "checkout", "-q", "--", "."], cwd=pending_change, check=True)
    clean = _hook(tmp_path, ["posttooluse"], _search(pending_change, "Bash", "grep -rn total ."))
    assert clean.stdout == ""
    outcomes = [line.split()[5] for line in (tmp_path / "cache" / "diffctx" / "hook.log").read_text().splitlines()]
    assert outcomes == ["no-symbol", "no-trigger", "clean"]


def test_a_changed_name_nothing_calls_is_said_in_one_line(tmp_path, pending_change):
    (pending_change / "shop" / "pricing.py").write_text(
        "def total(items):\n    return round(sum(i.price for i in items) * 1.19, 2)\n\n\ndef unused():\n    return 1\n",
        encoding="utf-8",
    )
    result = _hook(tmp_path, ["posttooluse"], _search(pending_change, "Grep", "def unused"))
    context = _context(result.stdout)
    assert "`unused`" in context and "nothing outside the diff calls it" in context
    assert "\n" not in context


@pytest.mark.skipif(sys.platform == "win32", reason="the plugin scripts are exercised under POSIX bash")
@pytest.mark.parametrize("bash", _bashes())
def test_the_plugin_script_launches_on_a_search_only_over_a_pending_change(tmp_path, pending_change, bash):
    calls = tmp_path / "calls"
    launcher = tmp_path / "diffctx"
    launcher.write_text(f'#!/bin/sh\necho >> "{calls}"\nexec "{sys.executable}" -m diffctx "$@"\n', encoding="utf-8")
    launcher.chmod(0o755)
    env = {
        "HOME": str(tmp_path),
        "PATH": "/usr/bin:/bin",
        "PYTHONPATH": str(SRC_DIR),
        "DIFFCTX_CACHE_DIR": str(tmp_path / "cache"),
        "DIFFCTX_HOOK_BIN": str(launcher),
        "CLAUDE_PLUGIN_ROOT": str(PROJECT_ROOT / "plugin"),
    }

    def run(stdin: str) -> str:
        result = subprocess.run(
            [bash, str(IMPACT_SCRIPT)], input=stdin, capture_output=True, text=True, encoding="utf-8", env=env, timeout=120
        )
        assert result.returncode == 0, result.stderr
        return result.stdout

    assert "shop/checkout.py::charge" in _context(run(_search(pending_change, "Bash", 'grep -rn "def total" .')))
    assert "shop/checkout.py::charge" in _context(run(_search(pending_change, "Grep", "total", session="s2")))
    assert run(_search(pending_change, "Bash", "ls | grep total")) == ""
    assert calls.read_text().count("\n") == 2
    subprocess.run(["git", "checkout", "-q", "--", "."], cwd=pending_change, check=True)
    assert run(_search(pending_change, "Bash", "grep -rn total .")) == ""
    assert calls.read_text().count("\n") == 2, "a clean tree launches nothing"


@pytest.mark.skipif(sys.platform == "win32", reason="the plugin scripts are exercised under POSIX bash")
@pytest.mark.parametrize("bash", _bashes())
def test_a_launch_that_fails_leaves_a_line_in_the_hook_log(tmp_path, pending_change, bash):
    env = {
        "HOME": str(tmp_path),
        "PATH": "/usr/bin:/bin",
        "DIFFCTX_CACHE_DIR": str(tmp_path / "cache"),
        "DIFFCTX_HOOK_BIN": "/usr/bin/false",
    }
    result = subprocess.run(
        [bash, str(IMPACT_SCRIPT)],
        input=_event(pending_change, "PreToolUse", "git commit -am vat"),
        capture_output=True,
        text=True,
        env=env,
        timeout=60,
    )
    assert result.returncode == 0, result.stderr
    assert result.stdout == ""
    line = (tmp_path / "cache" / "diffctx" / "hook.log").read_text().split()
    assert line[2:6] == ["PreToolUse", "-", "-", "error:launch-1"]
