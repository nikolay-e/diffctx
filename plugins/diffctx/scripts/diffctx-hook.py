#!/usr/bin/env python3
# One bridge for every host that is not Claude Code: `diffctx-hook.py <host>`
# turns the host's hook event into the Claude Code shaped payload that
# `diffctx hook <event>` of the pinned release reads, and its answer back
# into the host's contract. The release binary is what users run, so the
# hosts' differences live here, as rows of HOSTS, and not in the engine.
# Every path exits 0 with valid JSON: no hook failure blocks a tool call.

from __future__ import annotations

import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path

SPEC = "diffctx[mcp]==1.20.0"

SESSION_MESSAGE = (
    "diffctx is installed. Before committing or pushing a multi-file change, "
    'call `diffctx_context` with `mode="impact"` (omit `diff_ref` for '
    'uncommitted work, `HEAD~1..HEAD` for the last commit; `mode="pack"` '
    "when surrounding code is needed) or run: "
    "UV_ISOLATED=1 uvx diffctx==1.20.0 . --diff --mode impact -f md. "
    "To find who calls a name and which tests reach it without a pending "
    'change, call `diffctx_context(repo_path=..., symbol="NAME")` or run: '
    "UV_ISOLATED=1 uvx diffctx==1.20.0 . --symbol NAME -f md. "
    "Lifecycle hooks also inject this once a git commit/merge/push ran, after "
    "git diff, and after symbol grep; a commit with no diffctx block is not a "
    "clean result (the hook may not have run), so run the tool yourself. "
    "Sessions that share one checkout cannot tell one another's commits and "
    "edits apart by git alone: give each session its own git worktree."
)


@dataclass(frozen=True)
class Host:
    event_key: str
    session_key: str
    pre: str
    post: str
    session_events: tuple[str, ...]
    shell: str
    grep: str
    command_keys: tuple[str, ...]
    pattern_keys: tuple[str, ...]
    search_dir_keys: tuple[str, ...]
    # The host reads context only at this event, so the tool events queue it.
    drain_event: str


HOSTS = {
    # Antigravity: PostToolUse must answer `{}`; context is injected after the invocation.
    "agy": Host(
        "hookEventName",
        "conversationId",
        "PreToolUse",
        "PostToolUse",
        ("SessionStart",),
        "run_command",
        "grep_search",
        ("CommandLine", "command"),
        ("Query", "pattern"),
        ("SearchPath", "SearchDirectory", "Directory", "path"),
        "PostInvocation",
    ),
    # Codex speaks Claude Code's hook contract; it has no PostToolUseFailure.
    "codex": Host(
        "hook_event_name",
        "session_id",
        "PreToolUse",
        "PostToolUse",
        ("SessionStart", "SubagentStart"),
        "Bash",
        "",
        ("command",),
        (),
        (),
        "",
    ),
    # Gemini CLI: BeforeTool takes no additionalContext, AfterTool appends it to the tool result.
    "gemini": Host(
        "hook_event_name",
        "session_id",
        "BeforeTool",
        "AfterTool",
        ("SessionStart",),
        "run_shell_command",
        "grep_search",
        ("command",),
        ("pattern",),
        ("dir_path",),
        "AfterTool",
    ),
}

_ABS_PREFIX = r"(?:[A-Za-z]:[/\\]|/|~/)"
_CD_RE = re.compile(rf'(?:^|[\s;&|"(])cd\s+(?:"({_ABS_PREFIX}[^"]+)"|\'({_ABS_PREFIX}[^\']+)\'|({_ABS_PREFIX}[^\s;&|"\')]+))')
_GIT_C_RE = re.compile(rf'\bgit\s+-C\s+(?:"({_ABS_PREFIX}[^"]+)"|\'({_ABS_PREFIX}[^\']+)\'|({_ABS_PREFIX}[^\s;&|"\')]+))')


def emit(obj: dict) -> None:
    sys.stdout.buffer.write((json.dumps(obj, ensure_ascii=False) + "\n").encode("utf-8"))
    sys.stdout.buffer.flush()


def enabled() -> bool:
    return os.environ.get("DIFFCTX_IMPACT_HOOK", "true").lower() == "true"


def resolve_diffctx_cmd() -> list[str] | None:
    env_bin = os.environ.get("DIFFCTX_HOOK_BIN", "").strip()
    if env_bin:
        return [env_bin]
    if shutil.which("uvx"):
        return ["uvx", "-q", "--offline", "diffctx[mcp]==1.20.0"]
    return None


def warm_uvx_cache() -> None:
    if os.environ.get("DIFFCTX_HOOK_BIN", "").strip() or not shutil.which("uvx"):
        return
    kwargs: dict = {
        "stdin": subprocess.DEVNULL,
        "stdout": subprocess.DEVNULL,
        "stderr": subprocess.DEVNULL,
        "env": {**os.environ, "UV_ISOLATED": "1"},
    }
    if os.name == "nt":
        kwargs["creationflags"] = getattr(subprocess, "DETACHED_PROCESS", 0) | getattr(subprocess, "CREATE_NEW_PROCESS_GROUP", 0)
    else:
        kwargs["start_new_session"] = True
    try:
        subprocess.Popen(["uvx", "-q", "diffctx[mcp]==1.20.0", "--version"], **kwargs)
    except OSError:
        pass


def raw_cache_base() -> Path:
    explicit = os.environ.get("DIFFCTX_CACHE_DIR", "").strip()
    if explicit:
        return Path(explicit)
    xdg = os.environ.get("XDG_CACHE_HOME", "").strip()
    return (Path(xdg) if xdg else (Path.home() / ".cache")) / "diffctx"


def queue_dir(host: str, session: str) -> Path:
    pdata = os.environ.get("PLUGIN_DATA", "").strip()
    root = Path(pdata) if pdata else raw_cache_base()
    return root / f"{host}-queue" / session


def expand_path(raw: str) -> Path:
    if raw.startswith("~/"):
        return Path.home() / raw[2:]
    return Path(raw)


def resolve_git_worktree_and_common(raw_dir: str) -> tuple[str, str] | None:
    if not raw_dir:
        return None
    try:
        p = expand_path(raw_dir)
        if p.is_file():
            p = p.parent
        p = p.resolve()
        for candidate in (p, *p.parents):
            dot_git = candidate / ".git"
            if dot_git.is_dir():
                return str(candidate), str(candidate)
            if dot_git.is_file():
                line = dot_git.read_text(encoding="utf-8", errors="replace").splitlines()[0].strip()
                if line.startswith("gitdir:"):
                    gitdir = Path(line.removeprefix("gitdir:").strip())
                    gitdir = (candidate / gitdir).resolve() if not gitdir.is_absolute() else gitdir.resolve()
                    if gitdir.parent.name == "worktrees" and gitdir.parent.parent.name == ".git":
                        return str(candidate), str(gitdir.parent.parent.parent.resolve())
                    return str(candidate), str(gitdir)
                return str(candidate), str(candidate)
    except OSError:
        return None
    return None


def _matched_paths(regex: re.Pattern[str], cmd: str) -> list[str]:
    targets: list[str] = []
    for m in regex.finditer(cmd):
        tok = next((g for g in m.groups() if g), "").rstrip(";&|\"')")
        if tok:
            targets.append(tok)
    return targets


def waits_for_background(session: str) -> bool:
    return (raw_cache_base() / "seen" / f"pending.{session}").exists()


def log_failure(session: str, hook_event: str, call_id: str, why: str) -> None:
    try:
        cdir = raw_cache_base()
        cdir.mkdir(parents=True, exist_ok=True)
        version = SPEC.split("==", 1)[-1]
        line = f"{int(time.time())} {session or '-'} {hook_event} - - error:{why} - - call={call_id or '-'} roots=- {version}\n"
        with (cdir / "hook-runs.log").open("a", encoding="utf-8") as fh:
            fh.write(line)
    except OSError:
        pass


def enqueue_context(qdir: Path, text: str) -> None:
    cleaned = text.strip()
    if not cleaned:
        return
    try:
        qdir.mkdir(parents=True, exist_ok=True)
        stamp = f"{time.time_ns():020d}-{os.getpid()}"
        tmp = qdir / f".tmp.{stamp}"
        tmp.write_text(cleaned + "\n", encoding="utf-8")
        tmp.replace(qdir / f"{stamp}.msg")
    except OSError:
        pass


def drain_context(qdir: Path) -> str:
    if not qdir.is_dir():
        return ""
    parts: list[str] = []
    try:
        entries = sorted(qdir.glob("*.msg"))
    except OSError:
        return ""
    for entry in entries:
        claimed = entry.with_name(f"{entry.name}.drain.{os.getpid()}")
        try:
            entry.replace(claimed)
        except OSError:
            continue
        try:
            chunk = claimed.read_text(encoding="utf-8").strip()
            if chunk and chunk not in parts:
                parts.append(chunk)
        except OSError:
            pass
        finally:
            try:
                claimed.unlink(missing_ok=True)
            except OSError:
                pass
    try:
        qdir.rmdir()
    except OSError:
        pass
    return "\n\n".join(parts)


def effective_repo_cwd(cwd: str, explicit_targets: list[str]) -> str:
    if resolve_git_worktree_and_common(cwd) is not None:
        return cwd
    resolved = [pair for d in explicit_targets if (pair := resolve_git_worktree_and_common(d)) is not None]
    commons = list(dict.fromkeys(common for _, common in resolved))
    if len(commons) == 1:
        return resolved[0][0]
    return cwd


def _first(args: dict, keys: tuple[str, ...]) -> str:
    return next((str(args[k]) for k in keys if args.get(k)), "")


@dataclass
class ToolEvent:
    session: str
    call_id: str
    cwd: str
    tool: str
    args: dict
    failed: bool


def read_tool_event(host: Host, data: dict) -> ToolEvent:
    session = re.sub(r"[^A-Za-z0-9_-]", "-", str(data.get(host.session_key) or "default"))[:80] or "default"
    workspaces = data.get("workspacePaths") or []
    cwd = str(data.get("cwd") or (workspaces[0] if workspaces else "."))
    if "toolCall" in data:
        call = data.get("toolCall") or {}
        tool, args, call_id = str(call.get("name") or ""), call.get("args") or {}, str(call.get("id") or "")
        failed = bool(data.get("error"))
    else:
        tool, args = str(data.get("tool_name") or ""), data.get("tool_input") or {}
        response = data.get("tool_response")
        failed = isinstance(response, dict) and bool(response.get("error"))
        # Gemini CLI names no call; a line's before and after events still pair on its text.
        call_id = str(data.get("tool_use_id") or "") or hashlib.sha1(json.dumps(args, sort_keys=True).encode()).hexdigest()[:16]
    if not isinstance(args, dict):
        args = {}
    # Gemini CLI runs a shell line in `dir_path` when the model names one.
    run_dir = str(args.get("dir_path") or "") if tool == host.shell else ""
    if run_dir:
        cwd = str(Path(cwd) / expand_path(run_dir))
    return ToolEvent(session, re.sub(r"[^A-Za-z0-9_-]", "_", call_id)[:64], cwd, tool, args, failed)


def build_claude_payload(host: Host, event: ToolEvent, claude_event: str, is_post: bool) -> dict | None:
    base = {"session_id": event.session, "tool_use_id": event.call_id, "hook_event_name": claude_event, "tool_response": {}}
    if event.tool == host.shell:
        cmd = _first(event.args, host.command_keys)
        if not cmd.strip():
            return None
        explicit_targets = [*_matched_paths(_CD_RE, cmd), *_matched_paths(_GIT_C_RE, cmd)]
        in_repo = any(resolve_git_worktree_and_common(d) is not None for d in (event.cwd, *explicit_targets))
        if not (("git " in cmd) or ("gh " in cmd) or waits_for_background(event.session) or in_repo):
            return None
        return {
            **base,
            "cwd": effective_repo_cwd(event.cwd, explicit_targets),
            "tool_name": "Bash",
            "tool_input": {"command": cmd},
        }
    if host.grep and event.tool == host.grep and is_post:
        pattern = _first(event.args, host.pattern_keys)
        search_path = _first(event.args, host.search_dir_keys) or event.cwd
        if not Path(expand_path(search_path)).is_absolute():
            search_path = str(Path(event.cwd) / search_path)
        resolved_search = resolve_git_worktree_and_common(search_path)
        if not pattern or (resolved_search is None and resolve_git_worktree_and_common(event.cwd) is None):
            return None
        eff_cwd = event.cwd if resolve_git_worktree_and_common(event.cwd) is not None else resolved_search[0]
        return {
            **base,
            "cwd": eff_cwd,
            "tool_name": "Grep",
            "tool_input": {"pattern": pattern, "path": str(expand_path(search_path))},
        }
    return None


def invoke_hook_binary(subcmd: str, claude_payload: dict) -> dict | None:
    session, call_id, claude_event = (
        claude_payload["session_id"],
        claude_payload["tool_use_id"],
        claude_payload["hook_event_name"],
    )
    base_cmd = resolve_diffctx_cmd()
    if base_cmd is None:
        log_failure(session, claude_event, call_id, "no-uvx")
        return None
    full_cmd = [*base_cmd, "hook", subcmd]
    if subcmd == "pretooluse" and os.environ.get("DIFFCTX_IMPACT_GATE", "false").lower() == "true":
        full_cmd.append("--gate")
    try:
        proc = subprocess.run(
            full_cmd,
            input=json.dumps(claude_payload, ensure_ascii=False),
            capture_output=True,
            text=True,
            encoding="utf-8",
            timeout=50,
            env={**os.environ, "UV_ISOLATED": "1"},
            check=False,
        )
    except subprocess.TimeoutExpired:
        log_failure(session, claude_event, call_id, "timeout")
        return None
    except OSError:
        log_failure(session, claude_event, call_id, "oserror")
        return None
    if proc.returncode != 0:
        log_failure(session, claude_event, call_id, f"launch-{proc.returncode}")
        return None
    try:
        parsed = json.loads(proc.stdout.strip() or "{}")
    except json.JSONDecodeError:
        return None
    return parsed if isinstance(parsed, dict) else None


def allow(host_name: str, event: str) -> dict:
    return {"decision": "allow"} if host_name == "agy" and event == "PreToolUse" else {}


def context_reply(host_name: str, event: str, text: str) -> dict:
    if not text:
        return {}
    if host_name == "agy":
        return {"injectSteps": [{"systemMessage": {"systemMessage": text}}]}
    return {"hookSpecificOutput": {"hookEventName": event, "additionalContext": text}}


def deny_reply(host_name: str, event: str, reason: str) -> dict:
    if host_name == "codex":
        return {"hookSpecificOutput": {"hookEventName": event, "permissionDecision": "deny", "permissionDecisionReason": reason}}
    return {"decision": "deny", "reason": reason}


def handle(host_name: str, data: dict) -> dict:
    host = HOSTS[host_name]
    event = str(data.get(host.event_key) or "")
    session = re.sub(r"[^A-Za-z0-9_-]", "-", str(data.get(host.session_key) or "default"))[:80] or "default"
    qdir = queue_dir(host_name, session)
    if event in host.session_events:
        if not enabled():
            return {}
        if event == "SessionStart":
            warm_uvx_cache()
        return context_reply(host_name, event, SESSION_MESSAGE)
    if event == host.drain_event and event not in (host.pre, host.post):
        return context_reply(host_name, event, drain_context(qdir) if enabled() else "")
    if event not in (host.pre, host.post) or not enabled():
        return allow(host_name, event)

    is_post = event == host.post
    tool_event = read_tool_event(host, data)
    failed = is_post and tool_event.failed and host_name != "codex"
    subcmd = "pretooluse" if not is_post else ("posttoolusefailure" if failed else "posttooluse")
    claude_event = {"pretooluse": "PreToolUse", "posttooluse": "PostToolUse", "posttoolusefailure": "PostToolUseFailure"}[subcmd]
    payload = build_claude_payload(host, tool_event, claude_event, is_post)
    parsed = invoke_hook_binary(subcmd, payload) if payload is not None else None
    hso = (parsed or {}).get("hookSpecificOutput") or {}
    additional = str(hso.get("additionalContext") or "").strip()

    if not is_post and str(hso.get("permissionDecision") or "") == "deny":
        reason = "\n\n".join(p for p in (str(hso.get("permissionDecisionReason") or "").strip(), additional) if p)
        return deny_reply(host_name, event, reason or "Denied by diffctx strict gate.")
    if not host.drain_event:
        return context_reply(host_name, event, additional)
    enqueue_context(qdir, additional)
    if event == host.drain_event:
        return context_reply(host_name, event, drain_context(qdir))
    return allow(host_name, event)


def main() -> None:
    host_name = sys.argv[1] if len(sys.argv) > 1 and sys.argv[1] in HOSTS else "agy"
    raw = ""
    try:
        raw = sys.stdin.buffer.read().decode("utf-8", "replace") if not sys.stdin.isatty() else ""
        data = json.loads(raw) if raw.strip() else {}
        emit(handle(host_name, data if isinstance(data, dict) else {}))
    except Exception:
        emit({"decision": "allow"} if host_name == "agy" and '"PreToolUse"' in raw else {})


if __name__ == "__main__":
    main()
