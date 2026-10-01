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
