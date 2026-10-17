"""The Claude plugin's git hooks run `diffctx hook <event>` from the pinned
PyPI package under uvx: the plugin directory only runs code in the reviewed
repository or a package pinned to an exact version, so the release binary it
used to download is gone. These drive the Python entry point and the plugin
script the way Claude Code does — a JSON event on stdin, a real repository."""

from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import sys
from importlib.metadata import version
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


def _run(repo: Path, command: str) -> None:
    """The line itself, as the Bash tool runs it, under a git config of its own."""
    env = {
        **os.environ,
        "GIT_CONFIG_GLOBAL": os.devnull,
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_AUTHOR_NAME": "diffctx tests",
        "GIT_AUTHOR_EMAIL": "test@example.com",
        "GIT_COMMITTER_NAME": "diffctx tests",
        "GIT_COMMITTER_EMAIL": "test@example.com",
    }
    subprocess.run(["sh", "-c", command], cwd=repo, env=env, capture_output=True, check=False, timeout=60)


def _commit(tmp_path: Path, repo: Path) -> subprocess.CompletedProcess[str]:
    before = _hook(tmp_path, ["pretooluse"], _event(repo, "PreToolUse", "git commit -am vat"))
    assert before.returncode == 0, before.stderr
    assert before.stdout == "", "a commit is answered from what it recorded"
    _run(repo, "git commit -am vat")
    after = _hook(tmp_path, ["posttooluse"], _event(repo, "PostToolUse", "git commit -am vat"))
    assert after.returncode == 0, after.stderr
    return after


def test_a_commit_hears_its_callers_once_it_landed(tmp_path, pending_change):
    assert "shop/checkout.py::charge" in _context(_commit(tmp_path, pending_change).stdout)
    assert _commit(tmp_path, pending_change).stdout == "", "the same line again recorded nothing"


def test_the_gate_denies_once_with_the_impact_in_the_reason(tmp_path, pending_change):
    staging = _hook(tmp_path, ["pretooluse", "--gate"], _event(pending_change, "PreToolUse", "git commit -am vat"))
    assert staging.returncode == 0, staging.stderr
    answer = json.loads(staging.stdout)["hookSpecificOutput"]
    assert answer["permissionDecision"] == "deny"
    assert "as its own command" in answer["permissionDecisionReason"]
    _run(pending_change, "git add -A")
    denied = _hook(tmp_path, ["pretooluse", "--gate"], _event(pending_change, "PreToolUse", "git commit -m vat"))
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


def _script(
    tmp_path: Path, bash: str, gate: bool, event: str, plugin_root: Path | None = None
) -> subprocess.CompletedProcess[str]:
    launcher = tmp_path / "diffctx"
    launcher.write_text(f'#!/bin/sh\nexec "{sys.executable}" -m diffctx "$@"\n', encoding="utf-8")
    launcher.chmod(0o755)
    env = {
        "HOME": str(tmp_path),
        "PATH": "/usr/bin:/bin",
        "PYTHONPATH": str(SRC_DIR),
        "DIFFCTX_CACHE_DIR": str(tmp_path / "cache"),
        "DIFFCTX_HOOK_BIN": str(launcher),
        "CLAUDE_PLUGIN_ROOT": str(plugin_root or PROJECT_ROOT / "plugin"),
        "CLAUDE_PLUGIN_OPTION_IMPACT_GATE": "true" if gate else "false",
    }
    return subprocess.run(
        [bash, str(IMPACT_SCRIPT)], input=event, capture_output=True, text=True, encoding="utf-8", env=env, timeout=120
    )


@pytest.mark.skipif(sys.platform == "win32", reason="the plugin scripts are exercised under POSIX bash")
@pytest.mark.parametrize("bash", _bashes())
@pytest.mark.parametrize("gate", [False, True])
def test_the_plugin_script_reaches_the_hook_around_an_add_and_commit_line(tmp_path, pending_change, bash, gate):
    line = "git add -A && git commit -m vat"
    before = _script(tmp_path, bash, gate, _event(pending_change, "PreToolUse", line))
    assert before.returncode == 0, before.stderr
    if gate:
        answer = json.loads(before.stdout)["hookSpecificOutput"]
        assert answer["permissionDecision"] == "deny"
        assert "as its own command" in answer["permissionDecisionReason"]
        return
    assert before.stdout == ""
    _run(pending_change, line)
    after = _script(tmp_path, bash, gate, _event(pending_change, "PostToolUse", line))
    assert after.returncode == 0, after.stderr
    assert "shop/checkout.py::charge" in _context(after.stdout)


@pytest.mark.skipif(sys.platform == "win32", reason="the plugin scripts are exercised under POSIX bash")
@pytest.mark.parametrize("bash", _bashes())
def test_a_commit_line_that_ends_in_a_failing_command_is_still_reviewed(tmp_path, pending_change, bash):
    # Claude Code reports a line that exits non-zero as PostToolUseFailure (#532).
    line = "git commit -qam vat; git rev-parse --short HEAD no-such-ref"
    before = _script(tmp_path, bash, False, _event(pending_change, "PreToolUse", line))
    assert before.returncode == 0, before.stderr
    _run(pending_change, line)
    after = _script(tmp_path, bash, False, _event(pending_change, "PostToolUseFailure", line))
    assert after.returncode == 0, after.stderr
    answer = json.loads(after.stdout)["hookSpecificOutput"]
    assert answer["hookEventName"] == "PostToolUseFailure"
    assert "shop/checkout.py::charge" in answer["additionalContext"]


@pytest.mark.skipif(sys.platform == "win32", reason="the plugin scripts are exercised under POSIX bash")
@pytest.mark.parametrize("bash", _bashes())
def test_a_session_on_an_older_plugin_hears_once_that_a_newer_one_is_installed(tmp_path, pending_change, bash):
    running = version("diffctx")
    plugins = tmp_path / "plugins"
    for name in (running, "999.0.0"):
        (plugins / name).mkdir(parents=True)
    event = _event(pending_change, "PreToolUse", "git diff")
    first = _script(tmp_path, bash, False, event, plugins / running)
    assert first.returncode == 0, first.stderr
    assert _context(first.stdout).startswith(f"diffctx 999.0.0 is installed; this session runs {running}.")
    again = _script(tmp_path, bash, False, event, plugins / running)
    assert again.stdout == "", "once per session"


@pytest.mark.skipif(sys.platform == "win32", reason="the plugin scripts are exercised under POSIX bash")
@pytest.mark.parametrize("bash", _bashes())
def test_the_gate_option_leaves_post_tool_use_answers_alone(tmp_path, pending_change, bash):
    result = _script(tmp_path, bash, True, _event(pending_change, "PostToolUse", "git diff"))
    assert result.returncode == 0, result.stderr
    assert "shop/checkout.py::charge" in _context(result.stdout)
    log = (tmp_path / "cache" / "diffctx" / "hook.log").read_text(encoding="utf-8")
    assert "error:" not in log, log


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
    assert "`unused`" in context
    assert "no resolved static caller outside the diff was found in the analysed scope" in context
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
    # `cd ~/repo` from another directory names the repository (#377).
    from_home = json.loads(_search(pending_change, "Bash", 'cd ~/repo && grep -rn "def total" .', session="s3"))
    from_home["cwd"] = str(tmp_path)
    assert "shop/checkout.py::charge" in _context(run(json.dumps(from_home)))
    # A `cd` the grep printed is its output, not where it ran.
    printed = json.loads(_search(pending_change, "Bash", 'grep -rn "def total" .', session="s4"))
    printed["tool_response"] = {"stdout": "./deploy.sh:3:  cd /srv/app && make\n"}
    assert "shop/checkout.py::charge" in _context(run(json.dumps(printed)))
    # A Grep searches its own path, wherever the session's cwd is (#407).
    elsewhere = json.loads(_search(pending_change, "Grep", "total", session="s5"))
    elsewhere["cwd"] = str(tmp_path)
    assert "shop/checkout.py::charge" in _context(run(json.dumps(elsewhere)))
    assert run(_search(pending_change, "Bash", "ls | grep total")) == ""
    assert calls.read_text().count("\n") == 5
    subprocess.run(["git", "checkout", "-q", "--", "."], cwd=pending_change, check=True)
    assert run(_search(pending_change, "Bash", "grep -rn total .")) == ""
    assert calls.read_text().count("\n") == 5, "a clean tree launches nothing"


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
    launched = re.search(r'uvx -q --offline "diffctx\[mcp\]==([0-9.]+)"', IMPACT_SCRIPT.read_text()).group(1)
    assert line[-1] == launched, "the line names the version that wrote it (#524)"
