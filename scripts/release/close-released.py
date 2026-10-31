#!/usr/bin/env python3
"""Close every open issue labelled `fixed-unreleased` once its release has shipped (#595).

The label, never a commit message, says which issues a release fixed. Each is
told `Released in vX.Y.Z` and closed; one already told is not told twice, and a
closed issue is not listed again, so a re-run finishes what a failed one left.
The tokens come from the environment (`GH_TOKEN`, `FORGEJO_TOKEN`) or a file.
"""

from __future__ import annotations

import argparse
import re
import sys

from tracker import DEFAULT_REPO, LABEL, Tracker


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tracker", choices=["github", "forgejo"], required=True)
    parser.add_argument("--tag", required=True, help="the release tag, vX.Y.Z")
    parser.add_argument("--repo", default=DEFAULT_REPO)
    parser.add_argument("--api-url", help="default: GITHUB_API_URL, else api.github.com; Forgejo: FORGEJO_API_URL")
    parser.add_argument("--token-file", help="a file holding the token, instead of the environment")
    parser.add_argument("--dry-run", action="store_true", help="name the issues, change nothing")
    args = parser.parse_args(argv)
    if not re.fullmatch(r"v\d+\.\d+\.\d+", args.tag):
        parser.error(f"not a release tag: {args.tag!r}")

    tracker = Tracker.configured(args.tracker, args.repo, args.api_url, args.token_file)
    told = f"Released in {args.tag}"
    issues = tracker.labelled_issues(LABEL)
    for issue in issues:
        number = issue["number"]
        if args.dry_run:
            print(f"would close {args.tracker}#{number}: {issue.get('title', '')}")
            continue
        if not any(c.get("body", "").strip() == told for c in tracker.comments(number)):
            tracker.comment(number, told)
        tracker.close(number)
        print(f"closed {args.tracker}#{number}")
    print(f"{len(issues)} issue(s) carrying {LABEL} on {args.tracker}{' (dry run)' if args.dry_run else ''}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
