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
from pathlib import Path

import pytest

from diffctx._diffctx import BUILD_LABEL
from tests.framework.pygit2_backend import Pygit2Repo

PROJECT_ROOT = Path(__file__).resolve().parent.parent
SRC_DIR = PROJECT_ROOT / "src"
IMPACT_SCRIPT = PROJECT_ROOT / "plugin" / "hooks" / "diffctx-impact.sh"
SESSION_SCRIPT = PROJECT_ROOT / "plugin" / "hooks" / "diffctx-session.sh"
BRIDGE_SCRIPT = PROJECT_ROOT / "plugins" / "diffctx" / "scripts" / "diffctx-hook.py"


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
    # The hook names the running build as the engine labels it: a development
    # tree is `2.0.0-dev.0` there and `2.0.0.dev0` in the Python metadata.
    running = BUILD_LABEL.removesuffix("+source")
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
    log = (tmp_path / "cache" / "diffctx" / "hook-runs.log").read_text(encoding="utf-8")
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


@pytest.mark.skipif(sys.platform == "win32", reason="the plugin scripts are exercised under POSIX bash")
@pytest.mark.parametrize("bash", _bashes())
def test_the_session_context_says_a_silent_commit_is_no_clean_result(tmp_path, bash):
    # #350: once Claude Code removes the running version's plugin directory no
    # hook runs again, and a commit with no diffctx block reads as nothing
    # outside the diff unless the session was told otherwise. uvx stands in
    # for the package download the hook starts in the background.
    stub = tmp_path / "bin" / "uvx"
    stub.parent.mkdir()
    stub.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    stub.chmod(0o755)
    env = {"HOME": str(tmp_path), "PATH": f"{stub.parent}:/usr/bin:/bin", "CLAUDE_PLUGIN_ROOT": str(PROJECT_ROOT / "plugin")}
    # A subagent gets the same line under its own event (#543).
    for event in ("SessionStart", "SubagentStart"):
        stdin = json.dumps({"hook_event_name": event, "session_id": "s1", "agent_type": "Explore"})
        result = subprocess.run([bash, str(SESSION_SCRIPT)], input=stdin, capture_output=True, text=True, env=env, timeout=60)
        assert result.returncode == 0, result.stderr
        assert json.loads(result.stdout)["hookSpecificOutput"]["hookEventName"] == event
        context = _context(result.stdout)
        assert "a commit with no diffctx block is not a clean result" in context
        assert re.search(r"run: UV_ISOLATED=1 uvx diffctx==[0-9.]+ \. --diff --mode impact -f md", context), context


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
    log = (tmp_path / "cache" / "diffctx" / "hook-runs.log").read_text(encoding="utf-8").splitlines()
    assert [line.split()[5] for line in log] == ["shown", "seen"]


def test_a_search_for_a_defined_name_is_answered_on_any_tree(tmp_path, pending_change):
    # #628: a name the change does not edit, and any name on a clean tree, is
    # answered as `--symbol` answers it; a filter over another command's
    # output is no search.
    unrelated = _hook(tmp_path, ["posttooluse"], _search(pending_change, "Bash", "grep -rn charge ."))
    assert unrelated.returncode == 0, unrelated.stderr
    assert unrelated.stdout == "", "charge has no caller to resolve, and no zero is asserted"
    filtered = _hook(tmp_path, ["posttooluse"], _search(pending_change, "Bash", "cat shop/pricing.py | grep total"))
    assert filtered.stdout == ""
    subprocess.run(["git", "checkout", "-q", "--", "."], cwd=pending_change, check=True)
    clean = _hook(tmp_path, ["posttooluse"], _search(pending_change, "Bash", "grep -rn total ."))
    assert "shop/checkout.py::charge" in _context(clean.stdout)
    outcomes = [line.split()[5] for line in (tmp_path / "cache" / "diffctx" / "hook-runs.log").read_text().splitlines()]
    assert outcomes == ["empty", "no-trigger", "shown"]


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
def test_the_plugin_script_launches_on_a_search_in_a_repository(tmp_path, pending_change, bash):
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
    # Inside a repository every Bash line launches, since a program may commit
    # whatever its text shows: this one is no search, and answers nothing.
    assert run(_search(pending_change, "Bash", "ls | grep total")) == ""
    assert calls.read_text().count("\n") == 6
    # A clean tree is searched too (#628); a directory outside git launches nothing.
    subprocess.run(["git", "checkout", "-q", "--", "."], cwd=pending_change, check=True)
    assert "shop/checkout.py::charge" in _context(run(_search(pending_change, "Bash", "grep -rn total .", session="s6")))
    assert calls.read_text().count("\n") == 7
    outside = json.loads(_search(pending_change, "Bash", "grep -rn total .", session="s7"))
    outside["cwd"] = str(tmp_path)
    assert run(json.dumps(outside)) == ""
    assert calls.read_text().count("\n") == 7, "outside a repository nothing launches"


@pytest.mark.skipif(sys.platform == "win32", reason="the plugin scripts are exercised under POSIX bash")
@pytest.mark.parametrize("bash", _bashes())
def test_a_session_waiting_on_a_background_line_launches_on_any_event(tmp_path, pending_change, bash):
    # A background line's review comes at the session's next event, which
    # need not name git (#617).
    calls = tmp_path / "calls"
    launcher = tmp_path / "diffctx"
    launcher.write_text(f'#!/bin/sh\necho >> "{calls}"\ncat >/dev/null\n', encoding="utf-8")
    launcher.chmod(0o755)
    env = {
        "HOME": str(tmp_path),
        "PATH": "/usr/bin:/bin",
        "DIFFCTX_CACHE_DIR": str(tmp_path / "cache"),
        "DIFFCTX_HOOK_BIN": str(launcher),
    }
    event = json.loads(_event(pending_change, "PreToolUse", "ls"))
    event["session_id"] = "s7"
    event["cwd"] = str(tmp_path)

    def run() -> None:
        result = subprocess.run(
            [bash, str(IMPACT_SCRIPT)], input=json.dumps(event), capture_output=True, text=True, env=env, timeout=60
        )
        assert result.returncode == 0, result.stderr

    run()
    assert not calls.exists(), "nothing waits: a line outside a repository launches nothing"
    seen = tmp_path / "cache" / "diffctx" / "seen"
    seen.mkdir(parents=True)
    (seen / "pending.s7").write_text("[]", encoding="utf-8")
    run()
    assert calls.read_text().count("\n") == 1


@pytest.mark.skipif(sys.platform == "win32", reason="the plugin scripts are exercised under POSIX bash")
@pytest.mark.parametrize("bash", _bashes())
def test_the_plugin_script_launches_on_any_line_in_a_repository(tmp_path, pending_change, bash):
    # A script that commits shows no git on its line; git's record of the
    # repository the call runs in says what it did.
    calls = tmp_path / "calls"
    launcher = tmp_path / "diffctx"
    launcher.write_text(f'#!/bin/sh\necho >> "{calls}"\ncat >/dev/null\n', encoding="utf-8")
    launcher.chmod(0o755)
    env = {
        "HOME": str(tmp_path),
        "PATH": "/usr/bin:/bin",
        "DIFFCTX_CACHE_DIR": str(tmp_path / "cache"),
        "DIFFCTX_HOOK_BIN": str(launcher),
    }
    for event_name in ("PreToolUse", "PostToolUse"):
        result = subprocess.run(
            [bash, str(IMPACT_SCRIPT)],
            input=_event(pending_change, event_name, "./scripts/release.sh"),
            capture_output=True,
            text=True,
            env=env,
            timeout=60,
        )
        assert result.returncode == 0, result.stderr
    assert calls.read_text().count("\n") == 2


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
    line = (tmp_path / "cache" / "diffctx" / "hook-runs.log").read_text().split()
    assert line[2:6] == ["PreToolUse", "-", "-", "error:launch-1"]
    launched = re.search(r'uvx -q --offline "diffctx\[mcp\]==([0-9.]+)"', IMPACT_SCRIPT.read_text()).group(1)
    assert line[-1] == launched, "the line names the version that wrote it (#524)"


def _agy_launcher(tmp_path: Path) -> Path:
    if sys.platform == "win32":
        launcher = tmp_path / "diffctx-hook.cmd"
        launcher.write_text(f'@echo off\r\n"{sys.executable}" -m diffctx %*\r\n', encoding="utf-8")
    else:
        launcher = tmp_path / "diffctx-hook"
        launcher.write_text(f'#!/bin/sh\nexec "{sys.executable}" -m diffctx "$@"\n', encoding="utf-8")
        launcher.chmod(0o755)
    return launcher


def _bridge_run(
    tmp_path: Path,
    payload: dict,
    *,
    host: str = "agy",
    gate: bool = False,
    enabled: bool = True,
    hook_bin: str | None = None,
) -> dict:
    launcher = _agy_launcher(tmp_path) if hook_bin is None else Path(hook_bin)
    env = {
        **os.environ,
        "HOME": str(tmp_path),
        "PYTHONPATH": str(SRC_DIR),
        "DIFFCTX_CACHE_DIR": str(tmp_path / "cache" / "diffctx"),
        "PLUGIN_DATA": str(tmp_path / "plugin-data"),
        "DIFFCTX_HOOK_BIN": str(launcher),
        "DIFFCTX_IMPACT_HOOK": "true" if enabled else "false",
        "DIFFCTX_IMPACT_GATE": "true" if gate else "false",
        "PYTHONIOENCODING": "cp1252",
    }
    proc = subprocess.run(
        [sys.executable, str(BRIDGE_SCRIPT), host],
        input=json.dumps(payload),
        capture_output=True,
        text=True,
        encoding="utf-8",
        env=env,
        timeout=120,
        check=False,
    )
    assert proc.returncode == 0, proc.stderr
    return json.loads(proc.stdout)


def _agy_cmd_payload(
    event: str,
    cwd: Path,
    command: str,
    *,
    conv: str = "conv-1",
    call_id: str = "call-1",
    error: str = "",
) -> dict:
    payload: dict = {
        "conversationId": conv,
        "workspacePaths": [str(cwd)],
        "cwd": str(cwd),
        "hookEventName": event,
        "toolCall": {
            "id": call_id,
            "name": "run_command",
            "args": {"CommandLine": command},
        },
    }
    if error:
        payload["error"] = error
    return payload


def test_agy_session_start_injects_context_and_respects_toggle(tmp_path):
    on = _bridge_run(tmp_path, {"conversationId": "c1", "hookEventName": "SessionStart"})
    msg = on["injectSteps"][0]["systemMessage"]["systemMessage"]
    assert "a commit with no diffctx block is not a clean result" in msg
    assert re.search(r"UV_ISOLATED=1 uvx diffctx==[0-9.]+ \. --diff --mode impact -f md", msg), msg

    off = _bridge_run(tmp_path, {"conversationId": "c1", "hookEventName": "SessionStart"}, enabled=False)
    assert off == {}


def test_agy_commit_lifecycle_queues_on_post_tool_use_and_drains_once_on_post_invocation(tmp_path, pending_change):
    cmd = "git commit -am vat"
    pre = _bridge_run(tmp_path, _agy_cmd_payload("PreToolUse", pending_change, cmd))
    assert pre == {"decision": "allow"}

    _run(pending_change, cmd)
    post = _bridge_run(tmp_path, _agy_cmd_payload("PostToolUse", pending_change, cmd))
    assert post == {}, "Antigravity PostToolUse stdout contract is {}"

    inv = _bridge_run(tmp_path, {"conversationId": "conv-1", "hookEventName": "PostInvocation"})
    delivered = inv["injectSteps"][0]["systemMessage"]["systemMessage"]
    assert "shop/checkout.py::charge" in delivered

    again = _bridge_run(tmp_path, {"conversationId": "conv-1", "hookEventName": "PostInvocation"})
    assert again == {}, "queued context drains atomically once"


def test_agy_failed_command_that_committed_still_delivers_impact(tmp_path, pending_change):
    cmd = "git commit -qam vat; git rev-parse --short HEAD no-such-ref"
    assert _bridge_run(tmp_path, _agy_cmd_payload("PreToolUse", pending_change, cmd, conv="c-fail")) == {"decision": "allow"}
    _run(pending_change, cmd)
    post = _bridge_run(
        tmp_path,
        _agy_cmd_payload("PostToolUse", pending_change, cmd, conv="c-fail", error="exit status 128"),
    )
    assert post == {}
    inv = _bridge_run(tmp_path, {"conversationId": "c-fail", "hookEventName": "PostInvocation"})
    assert "shop/checkout.py::charge" in inv["injectSteps"][0]["systemMessage"]["systemMessage"]


def test_agy_strict_gate_denies_with_impact_reason(tmp_path, pending_change):
    staging = _bridge_run(
        tmp_path,
        _agy_cmd_payload("PreToolUse", pending_change, "git commit -am vat", conv="c-gate"),
        gate=True,
    )
    assert staging["decision"] == "deny"
    assert "as its own command" in staging["reason"]

    _run(pending_change, "git add -A")
    denied = _bridge_run(
        tmp_path,
        _agy_cmd_payload("PreToolUse", pending_change, "git commit -m vat", conv="c-gate"),
        gate=True,
    )
    assert denied["decision"] == "deny"
    assert "shop/checkout.py::charge" in denied["reason"]


def test_agy_grep_search_delivers_callers_even_from_workspace_parent(tmp_path, pending_change):
    workspace = tmp_path / "workspace-root"
    workspace.mkdir()
    payload = {
        "conversationId": "c-grep",
        "workspacePaths": [str(workspace)],
        "cwd": str(workspace),
        "hookEventName": "PostToolUse",
        "toolCall": {
            "id": "call-grep-1",
            "name": "grep_search",
            "args": {"Query": r"\btotal\b", "SearchPath": str(pending_change)},
        },
    }
    assert _bridge_run(tmp_path, payload) == {}
    inv = _bridge_run(tmp_path, {"conversationId": "c-grep", "hookEventName": "PostInvocation"})
    assert "shop/checkout.py::charge" in inv["injectSteps"][0]["systemMessage"]["systemMessage"]


def test_agy_workspace_parent_resolves_repo_and_linked_worktree(tmp_path, pending_change):
    import importlib.util

    spec = importlib.util.spec_from_file_location("diffctx_hook_bridge", BRIDGE_SCRIPT)
    mod = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    sys.modules[spec.name] = mod
    try:
        spec.loader.exec_module(mod)
    finally:
        del sys.modules[spec.name]
    assert mod._matched_paths(mod._GIT_C_RE, r'git -C "C:\Users\runner\repo" status && git -C D:/work/wt diff') == [
        r"C:\Users\runner\repo",
        "D:/work/wt",
    ]

    workspace = tmp_path / "home-workspace"
    workspace.mkdir()
    wt = tmp_path / "wt-feature"
    subprocess.run(
        ["git", "-C", str(pending_change), "worktree", "add", "-b", "wt-branch", str(wt), "HEAD"],
        check=True,
        capture_output=True,
    )
    cmd = f'git -C "{pending_change}" status --short && git -C "{wt}" status --short'
    assert _bridge_run(tmp_path, _agy_cmd_payload("PreToolUse", workspace, cmd, conv="c-wt")) == {"decision": "allow"}
    assert _bridge_run(tmp_path, _agy_cmd_payload("PostToolUse", workspace, cmd, conv="c-wt")) == {}
    inv = _bridge_run(tmp_path, {"conversationId": "c-wt", "hookEventName": "PostInvocation"})
    delivered = inv.get("injectSteps", [{}])[0].get("systemMessage", {}).get("systemMessage", "")
    assert "did not analyse this command in the repository it names" not in delivered


@pytest.mark.skipif(sys.platform == "win32", reason="uses /usr/bin/false to simulate a failed launch")
def test_agy_outside_repo_skips_launch_and_launch_error_logs_pinned_version(tmp_path, pending_change):
    outside = tmp_path / "plain-dir"
    outside.mkdir()
    assert _bridge_run(tmp_path, _agy_cmd_payload("PreToolUse", outside, "ls -la", conv="c-out"), hook_bin="/usr/bin/false") == {
        "decision": "allow"
    }
    assert not (tmp_path / "cache" / "diffctx" / "hook-runs.log").exists()

    res = _bridge_run(
        tmp_path,
        _agy_cmd_payload("PreToolUse", pending_change, "git commit -am vat", conv="c-err"),
        hook_bin="/usr/bin/false",
    )
    assert res == {"decision": "allow"}
    line = (tmp_path / "cache" / "diffctx" / "hook-runs.log").read_text(encoding="utf-8").split()
    assert line[2:6] == ["PreToolUse", "-", "-", "error:launch-1"]
    pinned = re.search(r'SPEC = "diffctx\[mcp\]==([0-9.]+)"', BRIDGE_SCRIPT.read_text(encoding="utf-8")).group(1)
    assert line[-1] == pinned


def _claude_shaped(event: str, cwd: Path, tool: str, tool_input: dict, *, session: str, call_id: str | None) -> dict:
    payload = {"session_id": session, "cwd": str(cwd), "hook_event_name": event, "tool_name": tool, "tool_input": tool_input}
    if call_id is not None:
        payload["tool_use_id"] = call_id
    return payload


def test_codex_commit_answers_in_claude_code_shape(tmp_path, pending_change):
    session = _bridge_run(tmp_path, {"session_id": "cx", "hook_event_name": "SessionStart"}, host="codex")
    assert "a commit with no diffctx block" in session["hookSpecificOutput"]["additionalContext"]
    assert session["hookSpecificOutput"]["hookEventName"] == "SessionStart"

    cmd = {"command": "git commit -am vat"}
    pre = _bridge_run(
        tmp_path, _claude_shaped("PreToolUse", pending_change, "Bash", cmd, session="cx", call_id="c1"), host="codex"
    )
    assert "permissionDecision" not in pre.get("hookSpecificOutput", {})
    _run(pending_change, cmd["command"])
    post = _bridge_run(
        tmp_path, _claude_shaped("PostToolUse", pending_change, "Bash", cmd, session="cx", call_id="c1"), host="codex"
    )
    assert post["hookSpecificOutput"]["hookEventName"] == "PostToolUse", "Codex has no PostToolUseFailure"
    assert "shop/checkout.py::charge" in post["hookSpecificOutput"]["additionalContext"]


def test_codex_strict_gate_denies_through_permission_decision(tmp_path, pending_change):
    _run(pending_change, "git add -A")
    cmd = {"command": "git commit -m vat"}
    denied = _bridge_run(
        tmp_path,
        _claude_shaped("PreToolUse", pending_change, "Bash", cmd, session="cx-gate", call_id="g1"),
        host="codex",
        gate=True,
    )
    assert denied["hookSpecificOutput"]["permissionDecision"] == "deny"
    assert "shop/checkout.py::charge" in denied["hookSpecificOutput"]["permissionDecisionReason"]


def test_gemini_commit_without_call_id_pairs_before_and_after_and_appends_to_the_tool_result(tmp_path, pending_change):
    session = _bridge_run(tmp_path, {"session_id": "gm", "hook_event_name": "SessionStart"}, host="gemini")
    assert "a commit with no diffctx block" in session["hookSpecificOutput"]["additionalContext"]

    workspace = pending_change.parent
    cmd = {"command": "git commit -am vat", "dir_path": pending_change.name}
    before = _bridge_run(
        tmp_path, _claude_shaped("BeforeTool", workspace, "run_shell_command", cmd, session="gm", call_id=None), host="gemini"
    )
    assert before == {}
    _run(pending_change, cmd["command"])
    after = _bridge_run(
        tmp_path, _claude_shaped("AfterTool", workspace, "run_shell_command", cmd, session="gm", call_id=None), host="gemini"
    )
    assert after["hookSpecificOutput"]["hookEventName"] == "AfterTool"
    assert "shop/checkout.py::charge" in after["hookSpecificOutput"]["additionalContext"]


def test_gemini_grep_search_and_strict_gate(tmp_path, pending_change):
    grep = {"pattern": r"\btotal\b", "dir_path": "shop"}
    after = _bridge_run(
        tmp_path, _claude_shaped("AfterTool", pending_change, "grep_search", grep, session="gg", call_id=None), host="gemini"
    )
    assert "shop/checkout.py::charge" in after["hookSpecificOutput"]["additionalContext"]

    _run(pending_change, "git add -A")
    cmd = {"command": "git commit -m vat"}
    denied = _bridge_run(
        tmp_path,
        _claude_shaped("BeforeTool", pending_change, "run_shell_command", cmd, session="gg", call_id=None),
        host="gemini",
        gate=True,
    )
    assert denied["decision"] == "deny"
    assert "shop/checkout.py::charge" in denied["reason"]
