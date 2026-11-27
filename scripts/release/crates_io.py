"""The workspace crates on crates.io: who owns each name, and publishing them leaf first.

crates.io limits new names to a burst of 5 and then one per 10 minutes, and its
trusted-publishing token lives about 30 minutes. `publish` therefore stops with
`pending=true` when the next wait would outlive its budget, and the workflow
authenticates again and calls it once more. It publishes nothing that the registry
already serves, so any number of calls, and a re-run after a failed smoke, converge.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import time
import urllib.error
import urllib.request
from collections.abc import Callable
from datetime import UTC, datetime
from email.utils import parsedate_to_datetime
from pathlib import Path
from typing import Any

REGISTRY = "https://crates.io"
USER_AGENT = "release-check"
CRAWL_PAUSE_SECONDS = 1.1
NEW_NAME_REFILL_SECONDS = 600
CLOCK_MARGIN_SECONDS = 5
# Every name the architecture reserves: the ones the workspace does not hold yet are checked too.
PLANNED = (
    "diffctx-base",
    "diffctx-config",
    "diffctx-snapshot",
    "diffctx-facts",
    "diffctx-resolve",
    "diffctx-occurrence",
    "diffctx-graph",
    "diffctx-select",
    "diffctx-answer",
    "diffctx",
)
_RETRY_AFTER = re.compile(r"try again after (\w{3}, \d{2} \w{3} \d{4} \d{2}:\d{2}:\d{2}) GMT")


def workspace(root: Path) -> dict[str, dict[str, Any]]:
    out = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1", "--locked"],
        cwd=root,
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    packages = json.loads(out)["packages"]
    return {p["name"]: p for p in packages if p["publish"] != []}


def leaf_first(packages: dict[str, dict[str, Any]]) -> list[str]:
    """Dependency order over normal and build edges, ties by name. A cycle is an error."""
    below = {
        name: {d["name"] for d in p["dependencies"] if d["kind"] != "dev" and d["name"] in packages}
        for name, p in packages.items()
    }
    order: list[str] = []
    while below:
        ready = sorted(n for n, deps in below.items() if not deps)
        if not ready:
            raise SystemExit(f"crate dependency cycle among {sorted(below)}")
        order += ready
        below = {n: deps - set(ready) for n, deps in below.items() if n not in ready}
    return order


def retry_wait(output: str, now: datetime) -> float | None:
    """Seconds a 429 asks us to wait, None when the failure is not a rate limit."""
    if "429" not in output and "Too Many Requests" not in output:
        return None
    match = _RETRY_AFTER.search(output)
    if match is None:
        return NEW_NAME_REFILL_SECONDS
    at = parsedate_to_datetime(match.group(1) + " GMT")
    return max((at - now).total_seconds(), 0) + CLOCK_MARGIN_SECONDS


def served(name: str, version: str, registry: str = REGISTRY) -> bool:
    return _get(f"{registry}/api/v1/crates/{name}/{version}") is not None


def _get(url: str) -> dict[str, Any] | None:
    request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    for attempt in range(4):
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                body: dict[str, Any] = json.load(response)
                return body
        except urllib.error.HTTPError as error:
            if error.code == 404:
                return None
            if error.code < 500 or attempt == 3:
                raise
        except urllib.error.URLError:
            if attempt == 3:
                raise
        time.sleep(5 * (attempt + 1))
    raise AssertionError("unreachable")


def foreign_names(names: list[str], repository: str, registry: str = REGISTRY) -> list[str]:
    """Names a crate of someone else already holds. A name is ours when its repository is this one."""
    taken = []
    for name in names:
        info = _get(f"{registry}/api/v1/crates/{name}")
        time.sleep(CRAWL_PAUSE_SECONDS)
        if info is not None and _same_repository(info["crate"].get("repository"), repository):
            print(f"{name}: ours, already on the registry")
        elif info is not None:
            print(f"::error::{name} is held by {info['crate'].get('repository') or 'a crate with no repository'}")
            taken.append(name)
        else:
            print(f"{name}: free")
    return taken


def _same_repository(found: str | None, ours: str) -> bool:
    def norm(url: str | None) -> str:
        return (url or "").removesuffix("/").removesuffix(".git").lower()

    return norm(found) == norm(ours)


def publish_all(
    order: list[str],
    version: str,
    *,
    is_served: Callable[[str], bool],
    publish_one: Callable[[str], tuple[int, str]],
    sleep: Callable[[float], None],
    now: Callable[[], datetime],
    budget_seconds: float,
) -> list[str]:
    """Publish what the registry lacks, in order. Returns the crates left for another call."""
    deadline = now().timestamp() + budget_seconds
    for index, name in enumerate(order):
        while not is_served(name):
            code, output = publish_one(name)
            if code == 0 or is_served(name):
                break
            wait = retry_wait(output, now())
            if wait is None:
                raise SystemExit(f"cargo publish of {name} {version} failed (exit {code}), not a rate limit")
            if now().timestamp() + wait > deadline:
                print(f"{name}: rate limited for {wait:.0f}s, past this call's budget; {len(order) - index} crates left")
                return order[index:]
            print(f"{name}: rate limited, waiting {wait:.0f}s")
            sleep(wait)
    return []


def _cargo_publish(name: str) -> tuple[int, str]:
    run = subprocess.run(["cargo", "publish", "--locked", "--package", name], capture_output=True, text=True)
    sys.stdout.write(run.stdout)
    sys.stderr.write(run.stderr)
    return run.returncode, run.stdout + run.stderr


def _crates(root: Path, version: str) -> tuple[dict[str, dict[str, Any]], list[str]]:
    packages = workspace(root)
    wrong = {n: p["version"] for n, p in packages.items() if p["version"] != version}
    if wrong:
        raise SystemExit(f"crate versions {wrong} differ from the requested {version}")
    return packages, leaf_first(packages)


def _emit(key: str, value: str) -> None:
    print(f"{key}={value}")
    if path := os.environ.get("GITHUB_OUTPUT"):
        with open(path, "a", encoding="utf-8") as handle:
            handle.write(f"{key}={value}\n")


def main(argv: list[str] | None = None) -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("names", help="fail when a name this repository will publish is held by someone else").add_argument(
        "--root", type=Path, default=Path.cwd()
    )
    publish = commands.add_parser("publish", help="publish the crates the registry lacks, leaf first")
    verify = commands.add_parser("verify", help="fail unless the registry serves every crate at the version")
    for command in (publish, verify):
        command.add_argument("--version", required=True)
        command.add_argument("--root", type=Path, default=Path.cwd())
    publish.add_argument("--budget-minutes", type=float, default=25)
    args = parser.parse_args(argv)

    if args.command == "names":
        packages = workspace(args.root)
        repository = next(p["repository"] for p in packages.values() if p["name"] == "diffctx")
        names = sorted({*packages, *PLANNED})
        if foreign_names(names, repository):
            raise SystemExit(1)
        return

    packages, order = _crates(args.root, args.version)
    print("order:", " ".join(order))
    if args.command == "verify":
        missing = [n for n in order if not served(n, args.version)]
        if missing:
            raise SystemExit(f"not on crates.io at {args.version}: {missing}")
        return

    os.chdir(args.root)
    left = publish_all(
        order,
        args.version,
        is_served=lambda n: served(n, args.version),
        publish_one=_cargo_publish,
        sleep=time.sleep,
        now=lambda: datetime.now(UTC),
        budget_seconds=args.budget_minutes * 60,
    )
    _emit("pending", "true" if left else "false")


if __name__ == "__main__":
    main()
