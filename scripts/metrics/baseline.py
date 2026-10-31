#!/usr/bin/env python3
"""A snapshot of the numbers the architecture program is judged against (stage 0).

Three families, each with the definition it was counted by:

* fix-of-fix share of fix commits, by week. A hand-labelled corpus (`--labelled`,
  the TSV of an issue sweep: `sha date kind ...`) is used where it has the row;
  every other `fix` commit is classed by a heuristic, marked as such: a fix
  that touches a source file an earlier fix touched in the previous 7 days.
* returns of closed issues, per tracker: reopen events, by week. A bug that
  comes back as a new issue is not seen here, so the count is a lower bound.
* the hook log: the share of identifier-shaped searches the hook answered, and
  the share of writing git lines it reviewed. A line the hook never recognised
  leaves no row, so the reviewed share is an upper bound.

Stdlib only. Trackers are read through GITHUB_API_URL / FORGEJO_API_URL, and
GH_TOKEN / FORGEJO_TOKEN (or --tracker-token-file); nothing here names a host.
"""

from __future__ import annotations

import argparse
import collections
import csv
import datetime as dt
import json
import os
import re
import subprocess
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

SINCE = dt.date(2026, 8, 1)
HEURISTIC_WINDOW_DAYS = 7
NOT_SOURCE = re.compile(r"^(tests?/|docs?/|CHANGELOG|.*\.md$|.*\.lock$|.*\.txt$|\.github/)")
ANSWERED = {"shown", "seen"}
NOT_A_SEARCH_OF_A_NAME = {"not-a-lookup"}
REVIEWED = {"shown", "empty"}
WRITING = {"commit", "push", "merge", "cherry-pick"}


def week_of(day: dt.date) -> str:
    return (day - dt.timedelta(days=day.weekday())).isoformat()


def git_fixes(repo: Path) -> list[dict]:
    out = subprocess.run(
        ["git", "log", "--no-merges", "--reverse", "--format=%x01%h%x09%aI%x09%s", "--name-only"],
        cwd=repo,
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    rows = []
    for block in out.split("\x01")[1:]:
        head, *files = block.strip("\n").split("\n")
        sha, date, subject = head.split("\t", 2)
        rows.append({"sha": sha, "date": dt.date.fromisoformat(date[:10]), "subject": subject, "files": [f for f in files if f]})
    return rows


def fix_of_fix(repo: Path, labelled: Path | None) -> dict:
    labelled_rows = []
    if labelled:
        with labelled.open(encoding="utf-8", newline="") as handle:
            labelled_rows = [
                (dt.date.fromisoformat(r["date"]), r["kind"]) for r in csv.DictReader(handle, delimiter="\t") if r["date"]
            ]
    last_labelled = max((d for d, _ in labelled_rows), default=dt.date.min)

    weeks: dict[str, collections.Counter] = collections.defaultdict(collections.Counter)
    for day, kind in labelled_rows:
        if day >= SINCE:
            weeks[week_of(day)]["labelled_" + kind] += 1

    recent_fixes: list[tuple[dt.date, set[str]]] = []
    for row in git_fixes(repo):
        if row["date"] <= last_labelled or not re.match(r"fix(\(|:|!)", row["subject"]):
            continue
        source = {f for f in row["files"] if not NOT_SOURCE.match(f)}
        recent_fixes = [(d, s) for d, s in recent_fixes if (row["date"] - d).days <= HEURISTIC_WINDOW_DAYS]
        kind = "fix_of_fix" if source & set().union(*(s for _, s in recent_fixes)) else "fix"
        recent_fixes.append((row["date"], source))
        if row["date"] >= SINCE:
            weeks[week_of(row["date"])]["heuristic_" + kind] += 1

    table = []
    for week in sorted(weeks):
        c = weeks[week]
        fof = c["labelled_fix-of-fix"] + c["heuristic_fix_of_fix"]
        fixes = fof + c["labelled_fix"] + c["labelled_hotfix"] + c["heuristic_fix"]
        table.append(
            {
                "week": week,
                "fix_commits": fixes,
                "fix_of_fix": fof,
                "share": round(fof / fixes, 3) if fixes else None,
                "labelled": c["labelled_fix"] + c["labelled_hotfix"] + c["labelled_fix-of-fix"],
                "heuristic": c["heuristic_fix"] + c["heuristic_fix_of_fix"],
            }
        )
    return {
        "since": SINCE.isoformat(),
        "last_labelled_day": last_labelled.isoformat() if labelled_rows else None,
        "weeks": table,
    }


def _get(url: str, token: str | None) -> tuple[object, dict]:
    request = urllib.request.Request(url, headers={"Accept": "application/json"})
    if token:
        request.add_header("Authorization", f"token {token}")
    with urllib.request.urlopen(request, timeout=60) as response:
        return json.loads(response.read() or b"null"), dict(response.headers)


def _pages(url: str, token: str | None, size_param: str, size: int) -> list:
    rows, page = [], 1
    while True:
        sep = "&" if "?" in url else "?"
        batch, _ = _get(f"{url}{sep}{size_param}={size}&page={page}", token)
        if not batch:
            return rows
        rows.extend(batch)
        page += 1


def reopen_counts(repo: str, github_token: str | None, forgejo: tuple[str, str | None] | None) -> dict:
    result: dict[str, dict] = {}
    api = os.environ.get("GITHUB_API_URL", "https://api.github.com")
    events = _pages(f"{api}/repos/{repo}/issues/events", github_token, "per_page", 100)
    opened = [
        e
        for e in events
        if e["event"] == "reopened" and "pull_request" not in (e.get("issue") or {}) and e["created_at"][:10] >= SINCE.isoformat()
    ]
    result["github"] = _summarise([(e["issue"]["number"], e["created_at"][:10]) for e in opened])
    if forgejo:
        base, token = forgejo
        issues = _pages(f"{base}/repos/{repo}/issues?state=all&type=issues", token, "limit", 50)
        found = []
        for issue in issues:
            timeline = _pages(f"{base}/repos/{repo}/issues/{issue['number']}/timeline", token, "limit", 50)
            found += [(issue["number"], t["created_at"][:10]) for t in timeline if t.get("type") == "reopen"]
        result["forgejo"] = _summarise([f for f in found if f[1] >= SINCE.isoformat()])
        result["forgejo"]["issues_read"] = len(issues)
    return result


def _summarise(found: list[tuple[int, str]]) -> dict:
    by_week = collections.Counter(week_of(dt.date.fromisoformat(day)) for _, day in found)
    return {"reopens": len(found), "issues": sorted({n for n, _ in found}), "by_week": dict(sorted(by_week.items()))}


def hook_log(cache: Path) -> dict:
    def read(names: list[str]) -> list[list[str]]:
        rows = []
        for name in names:
            path = cache / name
            if path.is_file():
                rows += [line.split() for line in path.read_text(encoding="utf-8", errors="replace").splitlines()]
        return [r for r in rows if len(r) >= 6 and r[0].isdigit()]

    out = {}
    for label, names in (("hook-runs.log", ["hook-runs.log.1", "hook-runs.log"]), ("hook.log", ["hook.log.1", "hook.log"])):
        rows = read(names)
        post = [r for r in rows if r[2] == "PostToolUse"]
        searches = [r for r in post if r[3] == "grep"]
        lookups = [r for r in searches if r[5] not in NOT_A_SEARCH_OF_A_NAME]
        answered = [r for r in lookups if r[5] in ANSWERED]
        writing = [r for r in post if r[3] in WRITING]
        reviewed = [r for r in writing if r[5] in REVIEWED]
        days = sorted(dt.datetime.fromtimestamp(int(r[0]), dt.timezone.utc).date() for r in rows)
        out[label] = {
            "rows": len(rows),
            "first_day": days[0].isoformat() if days else None,
            "last_day": days[-1].isoformat() if days else None,
            "searches": len(searches),
            "identifier_shaped_searches": len(lookups),
            "answered": len(answered),
            "answered_share": round(len(answered) / len(lookups), 4) if lookups else None,
            "answered_share_of_all_searches": round(len(answered) / len(searches), 4) if searches else None,
            "search_outcomes": dict(collections.Counter(r[5] for r in searches).most_common()),
            "writing_runs": len(writing),
            "reviewed": len(reviewed),
            "reviewed_share": round(len(reviewed) / len(writing), 4) if writing else None,
            "writing_outcomes": dict(collections.Counter(f"{r[3]}:{r[5]}" for r in writing).most_common()),
        }
    return out


def markdown(data: dict) -> str:
    lines = [f"# Baseline metrics, {data['taken']}", "", data["method"], ""]
    fof = data["fix_of_fix"]
    lines += [
        f"## Fix-of-fix share of fix commits, weekly since {fof['since']}",
        "",
        f"Hand-labelled rows run to {fof['last_labelled_day']}; later fixes are classed by the heuristic.",
        "",
        "| week of | fix commits | fix-of-fix | share | labelled | heuristic |",
        "| --- | --- | --- | --- | --- | --- |",
    ]
    for w in fof["weeks"]:
        share = "-" if w["share"] is None else f"{w['share']:.1%}"
        lines.append(f"| {w['week']} | {w['fix_commits']} | {w['fix_of_fix']} | {share} | {w['labelled']} | {w['heuristic']} |")
    lines += ["", "## Reopened issues (lower bound on returns)", ""]
    for tracker, r in data["returns"].items():
        lines.append(
            f"- {tracker}: {r['reopens']} reopen event(s) on {len(r['issues'])} issue(s) since {fof['since']}: {r['issues']}"
        )
        lines.append(f"  - by week: {r['by_week']}")
    lines += ["", "## Hook log", ""]
    for name, h in data["hook"].items():
        lines += [
            f"### {name} ({h['first_day']} to {h['last_day']}, {h['rows']} rows)",
            "",
            f"- identifier-shaped searches answered: {h['answered']} of {h['identifier_shaped_searches']}"
            f" ({_pct(h['answered_share'])}); of all {h['searches']} search events: {_pct(h['answered_share_of_all_searches'])}",
            f"- search outcomes: {h['search_outcomes']}",
            f"- writing git lines reviewed: {h['reviewed']} of {h['writing_runs']} logged ({_pct(h['reviewed_share'])}, an upper bound)",
            f"- writing outcomes: {h['writing_outcomes']}",
            "",
        ]
    return "\n".join(lines) + "\n"


def _pct(value: float | None) -> str:
    return "-" if value is None else f"{value:.1%}"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo-path", type=Path, default=Path.cwd(), help="a clone, for the commit history")
    parser.add_argument("--repo", default="nikolay-e/diffctx", help="owner/name on the trackers")
    parser.add_argument("--labelled", type=Path, help="TSV of hand-labelled fix history (sha, date, kind, ...)")
    parser.add_argument(
        "--hook-cache", type=Path, default=Path(os.environ.get("DIFFCTX_CACHE_DIR", Path.home() / ".cache")) / "diffctx"
    )
    parser.add_argument("--tracker-token-file", type=Path)
    parser.add_argument("--out-dir", type=Path, required=True)
    parser.add_argument("--stamp", default=dt.date.today().isoformat(), help="date in the file names")
    args = parser.parse_args(argv)

    forgejo = None
    if os.environ.get("FORGEJO_API_URL"):
        token = (
            args.tracker_token_file.read_text(encoding="utf-8").strip()
            if args.tracker_token_file
            else os.environ.get("FORGEJO_TOKEN")
        )
        forgejo = (os.environ["FORGEJO_API_URL"].rstrip("/"), token)
    data = {
        "taken": args.stamp,
        "method": "Counted by scripts/metrics/baseline.py; each section states its definition and its blind spot.",
        "fix_of_fix": fix_of_fix(args.repo_path, args.labelled),
        "returns": reopen_counts(args.repo, os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN"), forgejo),
        "hook": hook_log(args.hook_cache),
    }
    args.out_dir.mkdir(parents=True, exist_ok=True)
    (args.out_dir / f"BASELINE_{args.stamp}.json").write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
    (args.out_dir / f"BASELINE_{args.stamp}.md").write_text(markdown(data), encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
