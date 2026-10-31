#!/usr/bin/env python3
"""Write release-manifest.json from the trackers' labels and comments (T7).

Every open issue labelled `fixed-unreleased` must carry a comment
`Fixed on relay/arch in <sha>; gate: <case path or test name>`; one that does
not fails the run and names the issue, so a fix without its gate cannot reach
a release. The root comes from the issue's `root:<name>` label. The file is
generated and never edited: it feeds the epoch message and the final audit.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

from tracker import DEFAULT_REPO, LABEL, Tracker, label_names

FIXED = re.compile(r"^Fixed on relay/arch in (?P<sha>[0-9a-f]{7,40}); gate: (?P<gate>\S.*?)\s*$", re.MULTILINE)


def entry(tracker: Tracker, issue: dict) -> dict | None:
    fixes = [m for c in tracker.comments(issue["number"]) for m in FIXED.finditer(c.get("body", ""))]
    if not fixes:
        return None
    last = fixes[-1]
    roots = sorted(name.removeprefix("root:") for name in label_names(issue) if name.startswith("root:"))
    return {
        "issue": issue["number"],
        "tracker": tracker.kind,
        "root": roots[0] if roots else None,
        "gate": last["gate"],
        "fixing_sha": last["sha"],
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tracker", choices=["github", "forgejo"], action="append", required=True)
    parser.add_argument("--repo", default=DEFAULT_REPO)
    parser.add_argument("--token-file", help="one token file for every tracker named")
    parser.add_argument("--output", default="release-manifest.json")
    args = parser.parse_args(argv)

    rows, missing = [], []
    for kind in args.tracker:
        tracker = Tracker.configured(kind, args.repo, token_file=args.token_file)
        for issue in tracker.labelled_issues(LABEL):
            row = entry(tracker, issue)
            if row is None:
                missing.append(f"{kind}#{issue['number']}")
            else:
                rows.append(row)
    if missing:
        print(f"no 'Fixed on relay/arch in <sha>; gate: <gate>' comment on: {', '.join(missing)}", file=sys.stderr)
        return 1
    rows.sort(key=lambda r: (r["tracker"], r["issue"]))
    Path(args.output).write_text(json.dumps({"issues": rows}, indent=2) + "\n", encoding="utf-8")
    print(f"{len(rows)} issue(s) written to {args.output}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
