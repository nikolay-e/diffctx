#!/usr/bin/env python3
"""Whole-hook wall time per event, replayed from the hook log (fj#170).

Reads the last N runs of `hook-runs.log`, re-runs each one that can be re-run through
`<bin> hook posttooluse` against the repository it names, and reports p50 / p95 / max wall
time per kind of event next to the time the log recorded, the share of runs that end
`partial: deadline` (threshold 0, every exception listed with its cause), and every event
that was not replayed, counted by its reason.

The log does not hold the command or the transcript, so an event is rebuilt from what it
does hold: a commit, push or cherry-pick as the push of its logged `old..new` range; an
inspection as `git diff`, twice - with no transcript and with a transcript that wrote every
changed path and every unread untracked directory (the owned-directory path); a search as
`Grep` (the pattern is not in the log; what the run pays before the search does not depend
on it). A repository or a commit that is gone is skipped, with the reason counted.

The replay times a real binary, so it needs a quiet machine:

    python bench_hook.py --bin <built diffctx> --out <dir>

Exit status: 0 when no replayed run ended `partial: deadline`, 1 when one did, 2 on a usage error.
"""

from __future__ import annotations

import argparse
import json
import math
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from collections import Counter
from dataclasses import asdict, dataclass
from pathlib import Path

DEFAULT_LOGS = ("~/.cache/diffctx/hook-runs.log.1", "~/.cache/diffctx/hook-runs.log")
RANGE = re.compile(r"^([0-9a-f]{7,40})\.\.([0-9a-f]{7,40})$")
LINE = re.compile(
    r"^(?P<ts>\d+) (?P<session>\S+) (?P<event>\S+) (?P<verb>\S+) (?P<range>\S+) (?P<outcome>\S+) (?P<ms>\d+)ms (?P<rest>.*)$"
)
PARTIAL_DEADLINE = re.compile(r"\(partial: [^)]*\bdeadline\b")
RANGE_VERBS = ("commit", "push", "cherry-pick", "merge", "rebase")
HOOK_TIMEOUT_SECS = 90


@dataclass
class Entry:
    ts: int
    session: str
    event: str
    verb: str
    range: str
    outcome: str
    ms: int
    repo: str


@dataclass
class Run:
    kind: str
    verb: str
    repo: str
    range: str
    logged_ms: int
    wall_ms: float = 0.0
    exit_code: int | None = None
    outcome: str = ""
    partial_deadline: bool = False
    timed_out: bool = False
    cause: str = ""


def parse_log(paths: list[Path]) -> tuple[list[Entry], int]:
    entries: list[Entry] = []
    unparsed = 0
    for path in paths:
        if not path.is_file():
            continue
        for line in path.read_text(errors="replace").splitlines():
            m = LINE.match(line)
            if m is None:
                unparsed += 1
                continue
            rest = m["rest"]
            cut = rest.find(" call=")
            repo = rest[:cut] if cut >= 0 else rest
            entries.append(
                Entry(
                    int(m["ts"]),
                    m["session"],
                    m["event"],
                    m["verb"],
                    m["range"],
                    m["outcome"],
                    int(m["ms"]),
                    repo,
                )
            )
    return entries, unparsed


def window(entries: list[Entry], skip_sessions: set[str], count: int, triggered: bool) -> tuple[list[Entry], Counter[str]]:
    """The last `count` runs (or the last `count` triggered post-tool runs), minus the excluded sessions."""
    skipped: Counter[str] = Counter()
    kept = []
    for entry in entries:
        if entry.session in skip_sessions:
            skipped["excluded-session"] += 1
        else:
            kept.append(entry)
    if triggered:
        kept = [e for e in kept if e.event == "PostToolUse" and e.verb != "-"]
    return kept[-count:], skipped


def git_ok(repo: str, *args: str) -> bool:
    done = subprocess.run(["git", "-C", repo, *args], capture_output=True, check=False, timeout=60)
    return done.returncode == 0


def plan_kinds(entry: Entry, pattern: str) -> tuple[list[str], str]:
    """The replays one log line stands for, or why it stands for none."""
    if entry.event != "PostToolUse":
        return [], "not-a-posttooluse-run"
    if entry.verb == "-":
        return [], "no-trigger"
    if entry.repo in ("", "-") or not Path(entry.repo).is_dir():
        return [], "repo-gone"
    try:
        if not git_ok(entry.repo, "rev-parse", "--git-dir"):
            return [], "repo-gone"
    except subprocess.TimeoutExpired:
        return [], "repo-slow"
    if entry.verb in RANGE_VERBS:
        m = RANGE.match(entry.range)
        if m is None:
            return [], "no-range"
        if not all(git_ok(entry.repo, "cat-file", "-e", f"{sha}^{{commit}}") for sha in m.groups()):
            return [], "range-gone"
        return ["push"], ""
    if entry.verb == "inspect":
        return ["inspect", "inspect-owned"], ""
    if entry.verb == "grep":
        return ["grep"], ""
    return [], f"unhandled-verb:{entry.verb}"


def payload(kind: str, entry: Entry, pattern: str, transcript: Path | None) -> dict[str, object]:
    base: dict[str, object] = {
        "session_id": "bench",
        "hook_event_name": "PostToolUse",
        "cwd": entry.repo,
    }
    if kind == "push":
        old, new = RANGE.match(entry.range).groups()  # type: ignore[union-attr]
        return {
            **base,
            "tool_name": "Bash",
            "tool_input": {"command": "git push"},
            "tool_response": {
                "stdout": "",
                "stderr": f"   {old}..{new}  main -> main\n",
            },
        }
    if kind == "grep":
        return {**base, "tool_name": "Grep", "tool_input": {"pattern": pattern}}
    doc: dict[str, object] = {
        **base,
        "tool_name": "Bash",
        "tool_input": {"command": "git diff"},
    }
    if transcript is not None:
        doc["transcript_path"] = str(transcript)
    return doc


def changed_paths(repo: str) -> list[str]:
    """What a pending change is made of, as the hook's own change listing names it: tracked
    changes, untracked files and untracked directories collapsed to `dir/`."""
    names: list[str] = []
    for args in (
        ("diff", "--name-only", "-z", "--no-renames", "HEAD", "--"),
        ("ls-files", "-z", "--others", "--exclude-standard", "--directory"),
    ):
        done = subprocess.run(["git", "-C", repo, *args], capture_output=True, check=False, timeout=120)
        names += [n for n in done.stdout.decode(errors="replace").split("\0") if n]
    return names


def write_transcript(path: Path, repo: str) -> None:
    lines = [
        json.dumps(
            {
                "type": "user",
                "sessionId": "bench",
                "message": {"role": "user", "content": "go"},
            }
        )
    ]
    for name in changed_paths(repo):
        # A directory is owned through a file the session wrote in it.
        target = Path(repo) / (name + ".bench" if name.endswith("/") else name)
        lines.append(
            json.dumps(
                {
                    "type": "assistant",
                    "sessionId": "bench",
                    "message": {
                        "role": "assistant",
                        "content": [
                            {
                                "type": "tool_use",
                                "id": "t1",
                                "name": "Edit",
                                "input": {"file_path": str(target)},
                            }
                        ],
                    },
                }
            )
        )
    path.write_text("\n".join(lines) + "\n")


def replay(binary: str, kind: str, entry: Entry, pattern: str, work: Path) -> Run:
    transcript = None
    if kind == "inspect-owned":
        transcript = work / "transcript.jsonl"
        write_transcript(transcript, entry.repo)
    cache = work / "cache"
    env = {k: v for k, v in os.environ.items() if k != "CLAUDE_PLUGIN_ROOT"}
    env.update({"DIFFCTX_CACHE_DIR": str(cache)})
    run = Run(kind, entry.verb, entry.repo, entry.range, entry.ms)
    stdin = json.dumps(payload(kind, entry, pattern, transcript))
    began = time.perf_counter()
    try:
        done = subprocess.run(
            [binary, "hook", "posttooluse"],
            input=stdin,
            capture_output=True,
            text=True,
            cwd=entry.repo,
            env=env,
            timeout=HOOK_TIMEOUT_SECS,
            check=False,
        )
        stdout, run.exit_code = done.stdout, done.returncode
    except subprocess.TimeoutExpired:
        stdout, run.timed_out = "", True
    run.wall_ms = (time.perf_counter() - began) * 1000
    log = cache / "diffctx" / "hook-runs.log"
    if log.is_file() and log.read_text().strip():
        found = LINE.match(log.read_text().strip().splitlines()[-1])
        run.outcome = found["outcome"] if found else ""
    run.partial_deadline = bool(PARTIAL_DEADLINE.search(stdout)) or run.outcome == "timeout"
    if run.timed_out:
        run.cause = f"the hook did not return in {HOOK_TIMEOUT_SECS} s"
    elif run.exit_code not in (0, None):
        run.cause = f"exit {run.exit_code}"
    elif run.partial_deadline:
        run.cause = "the answer says partial: deadline" if PARTIAL_DEADLINE.search(stdout) else "log outcome timeout"
    shutil.rmtree(cache, ignore_errors=True)
    return run


def percentile(values: list[float], q: float) -> float:
    ordered = sorted(values)
    return ordered[max(0, math.ceil(q * len(ordered)) - 1)]


def spread(values: list[float]) -> dict[str, float]:
    if not values:
        return {}
    return {
        "p50": round(percentile(values, 0.5), 1),
        "p95": round(percentile(values, 0.95), 1),
        "max": round(max(values), 1),
    }


def summarize(runs: list[Run], skipped: Counter[str], total_lines: int) -> dict[str, object]:
    kinds: dict[str, object] = {}
    for kind in sorted({r.kind for r in runs}):
        mine = [r for r in runs if r.kind == kind]
        kinds[kind] = {
            "n": len(mine),
            "wall_ms": spread([r.wall_ms for r in mine]),
            "logged_ms": spread([float(r.logged_ms) for r in mine]),
            "partial_deadline": sum(r.partial_deadline for r in mine),
        }
    exceptions = [asdict(r) for r in runs if r.partial_deadline or r.cause]
    return {
        "log_lines_considered": total_lines,
        "replayed": len(runs),
        "skipped": dict(skipped),
        "kinds": kinds,
        "all_runs_wall_ms": spread([r.wall_ms for r in runs]),
        "partial_deadline": sum(r.partial_deadline for r in runs),
        "partial_deadline_share": (round(sum(r.partial_deadline for r in runs) / len(runs), 4) if runs else None),
        "exceptions": exceptions,
    }


def report(summary: dict[str, object]) -> str:
    out = [f"replayed {summary['replayed']} of {summary['log_lines_considered']} log lines"]
    skipped = summary["skipped"]
    assert isinstance(skipped, dict)
    out += [f"  skipped {n:>4}  {why}" for why, n in sorted(skipped.items())]
    kinds = summary["kinds"]
    assert isinstance(kinds, dict)
    for kind, row in kinds.items():
        out.append(
            f"{kind:<14} n={row['n']:<4} wall_ms {row['wall_ms']}  logged_ms {row['logged_ms']}"
            f"  partial_deadline={row['partial_deadline']}"
        )
    out.append(f"all            wall_ms {summary['all_runs_wall_ms']}")
    exceptions = summary["exceptions"]
    assert isinstance(exceptions, list)
    out.append(f"gate 0: {summary['partial_deadline']} run(s) ended partial: deadline (threshold 0)")
    for run in exceptions:
        out.append(
            f"  EXCEPTION {run['kind']} {run['verb']} {run['range']} {run['repo']}: {run['cause']} ({run['wall_ms']:.0f} ms)"
        )
    return "\n".join(out)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--bin", required=True, help="the diffctx binary to run the hook with")
    parser.add_argument(
        "--out",
        required=True,
        help="directory for runs.jsonl, skipped.json and summary.json",
    )
    parser.add_argument(
        "--log",
        action="append",
        help="a hook log; repeatable, read in order (default: the user's hook-runs.log.1 and hook-runs.log)",
    )
    parser.add_argument("--events", type=int, default=200, help="how many of the last runs to consider")
    parser.add_argument(
        "--triggered",
        action="store_true",
        help="count --events over triggered PostToolUse runs only",
    )
    parser.add_argument(
        "--skip-session",
        action="append",
        default=["s9"],
        help="a session whose runs are left out (default: s9)",
    )
    parser.add_argument(
        "--pattern",
        default="def main",
        help="the Grep pattern a logged search is replayed with",
    )
    args = parser.parse_args(argv)

    binary = str(Path(args.bin).expanduser())
    if not os.access(binary, os.X_OK):
        print(f"bench_hook: {binary} is not an executable", file=sys.stderr)
        return 2
    logs = [Path(p).expanduser() for p in (args.log or DEFAULT_LOGS)]
    entries, unparsed = parse_log(logs)
    chosen, skipped = window(entries, set(args.skip_session), args.events, args.triggered)
    if unparsed:
        skipped["unparsed-log-line"] += unparsed
    out = Path(args.out).expanduser()
    out.mkdir(parents=True, exist_ok=True)
    runs: list[Run] = []
    with tempfile.TemporaryDirectory(prefix="bench-hook-") as scratch:
        for entry in chosen:
            kinds, why = plan_kinds(entry, args.pattern)
            if not kinds:
                skipped[why] += 1
                continue
            for kind in kinds:
                work = Path(scratch)
                try:
                    runs.append(replay(binary, kind, entry, args.pattern, work))
                except (subprocess.TimeoutExpired, OSError) as e:
                    skipped[f"replay-failed:{type(e).__name__}"] += 1
    summary = summarize(runs, skipped, len(chosen))
    (out / "runs.jsonl").write_text("".join(json.dumps(asdict(r)) + "\n" for r in runs))
    (out / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    text = report(summary)
    (out / "summary.txt").write_text(text + "\n")
    print(text)
    return 1 if summary["partial_deadline"] else 0


if __name__ == "__main__":
    sys.exit(main())
