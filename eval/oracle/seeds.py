"""The rule that picks the oracle's blind sets, as code: no one chooses a repository after seeing its rows.

`corpus.toml` is a regression set (its rows were read and the engine and the judge were tuned on them), so acceptance
needs sets nobody picked by hand. The rule, written before any set was drawn:

1. Pool, per oracle language, read on one stated date: GitHub repositories whose primary language is the language's,
   under a permissive licence (`corpus.PERMISSIVE_LICENSES`, a dual MIT/Apache pair read from the LICENSE-* files when the
   API gives no SPDX id), not a fork, not archived, pushed in the 12 months before that date, at most `SIZE_MAX_KB`, not
   owned by an owner of a repository in corpus.toml; ranked by stars (then name), the first `POOL_SIZE`. A language
   whose feasible repositories do not cover every set (`SET_COUNT` times its repositories in corpus.toml) reads the
   next `POOL_SIZE` by stars, and again, up to `POOL_LIMIT`: the same rule for every language, applied before any draw.
2. Feasibility, from metadata only (no clone): at least `history` commits (the window a commit is sampled from), and
   `feasibility.reject` finds no reason the oracle's indexers cannot index it.
3. Draw: a set seed is one draw of the operating system's CSPRNG, recorded with its date in `seeds/draw.toml`. Per
   language the feasible pool is ordered by `sha256("<seed>:<full_name>")` and the first k, as many repositories as
   corpus.toml has for that language, are the set; a later set draws from what the earlier ones left.
   A drawn repository the indexers cannot index is replaced by the next feasible one of its language in the same order
   that no set holds (the first draw of every set counts as held, so no other set moves): its name and the reason go to
   `replaced` of its set in `seeds/draw.toml`, and no set draws it again. The reason is the indexer's own (a timeout, an
   exit on most snapshots), never a 429 (that is retried later) and never anything read from rows.
4. Pin each repository at `git ls-remote <url> HEAD` and write `seeds/seed-<n>.toml`.

  python seeds.py pool [--as-of YYYY-MM-DD]    read the pools and their feasibility; writes seeds/pool.json
  python seeds.py draw                         draw every set of seeds/draw.toml from the pool; writes seeds/seed-<n>.toml
"""

from __future__ import annotations

import argparse
import base64
import datetime
import hashlib
import json
import re
import subprocess
import sys
import time
import tomllib
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path
from typing import Any
from urllib.parse import quote

import feasibility
import gitrepo
from corpus import LANGUAGES, PERMISSIVE_LICENSES, ROOT, load_corpus

SEEDS = ROOT / "seeds"
POOL_FILE = SEEDS / "pool.json"
DRAW_FILE = SEEDS / "draw.toml"
GITHUB_LANGUAGES = {
    "python": "Python",
    "typescript": "TypeScript",
    "go": "Go",
    "java": "Java",
    "kotlin": "Kotlin",
    "rust": "Rust",
}
POOL_SIZE = 60  # a tranche of the star ranking
POOL_LIMIT = 480  # the most a language reads: the hosting API's core budget (one listing, a few files per candidate)
SET_COUNT = 3  # seed-2, seed-3 and seed-4
PUSHED_WITHIN_DAYS = 365
SIZE_MAX_KB = 100_000
PAGE = 100
SEARCH_PAGES = 10  # the search API serves no result past the 1000th
UNDECLARED = {None, "NOASSERTION", "Other"}


@dataclass(frozen=True)
class Response:
    body: Any
    headers: dict[str, str]


Api = Callable[[str], Response]  # a GET path with its query -> the decoded body and the response headers


SEARCH_PAUSE = 2.2  # seconds: the search API allows 30 requests a minute
RATE_LIMITED = ("403", "429")
RATE_WAIT = 65
RATE_FLOOR = 20  # requests left in a window below which the next request waits for the reset


def gh_api(path: str) -> Response:
    """A read-only GET through the authenticated `gh` session; nothing of ours rides in the request but that token."""
    for attempt in range(3):
        if path.startswith("search/"):
            time.sleep(SEARCH_PAUSE)
        done = subprocess.run(["gh", "api", "--include", path], capture_output=True, text=True, check=False)
        head, _, body = done.stdout.replace("\r\n", "\n").partition("\n\n")
        status = re.match(r"HTTP/\S+ (\d+)", head)
        code = status.group(1) if status else "none"
        if code in RATE_LIMITED and attempt < 2:
            time.sleep(RATE_WAIT)
            continue
        if not code.startswith("2"):
            return Response(None, {"status": code})
        headers = {k.lower(): v for k, _, v in (line.partition(": ") for line in head.splitlines()[1:])}
        if int(headers.get("x-ratelimit-remaining", RATE_FLOOR)) < RATE_FLOOR:
            time.sleep(max(0, int(headers["x-ratelimit-reset"]) - time.time()) + 2)
        return Response(json.loads(body) if body.strip() else None, headers)
    raise AssertionError("unreachable")


def owners_of(urls: list[str]) -> set[str]:
    return {url.split("/")[3].lower() for url in urls}


def search_query(language: str, as_of: datetime.date) -> str:
    cutoff = as_of - datetime.timedelta(days=PUSHED_WITHIN_DAYS)
    q = f"language:{GITHUB_LANGUAGES[language]} fork:false archived:false pushed:>={cutoff} size:<={SIZE_MAX_KB}"
    return f"search/repositories?q={quote(q)}&sort=stars&order=desc&per_page={PAGE}"


def contents(api: Api, full_name: str, path: str = "") -> Response:
    return api(f"repos/{full_name}/contents/{quote(path)}")


def licence_of(api: Api, item: dict) -> str | None:
    """The SPDX id the API gives, or a dual MIT/Apache pair read from the LICENSE-* files when it gives none."""
    spdx = (item.get("license") or {}).get("spdx_id")
    if spdx not in UNDECLARED:
        return spdx
    listing = contents(api, item["full_name"]).body
    names = {entry["name"].lower() for entry in listing or [] if entry.get("type") == "file"}
    if any(n.startswith("license-mit") for n in names) and any(n.startswith("license-apache") for n in names):
        return "Apache-2.0 OR MIT"
    return None


def permissive(licence: str | None) -> bool:
    return licence is not None and set(licence.split(" OR ")) <= PERMISSIVE_LICENSES


def pool(api: Api, language: str, as_of: datetime.date, excluded_owners: set[str], size: int = POOL_SIZE) -> list[dict]:
    """The first `size` repositories of the rule's step 1, by stars."""
    cutoff = (as_of - datetime.timedelta(days=PUSHED_WITHIN_DAYS)).isoformat()
    found: list[dict] = []
    for page in range(1, SEARCH_PAGES + 1):
        items = (api(f"{search_query(language, as_of)}&page={page}").body or {}).get("items", [])
        for item in items:
            owner = item["owner"]["login"].lower()
            if (
                item["fork"]
                or item["archived"]
                or item.get("disabled")
                or item["language"] != GITHUB_LANGUAGES[language]
                or item["size"] > SIZE_MAX_KB
                or item["pushed_at"][:10] < cutoff
                or owner in excluded_owners
            ):
                continue
            licence = licence_of(api, item)
            if permissive(licence):
                found.append(candidate(item, licence))
        if len(found) >= size or len(items) < PAGE:
            break
    return sorted(found, key=lambda c: (-c["stars"], c["full_name"]))[:size]


def candidate(item: dict, licence: str) -> dict:
    return {
        "full_name": item["full_name"],
        "url": f"https://github.com/{item['full_name']}.git",
        "stars": item["stargazers_count"],
        "license": licence,
        "size_kb": item["size"],
        "pushed_at": item["pushed_at"],
    }


def commit_count(api: Api, full_name: str) -> int | None:
    """The default branch's commit count, from the last page of a one-per-page listing."""
    response = api(f"repos/{full_name}/commits?per_page=1")
    if response.body is None:
        return None
    last = re.search(r'[?&]page=(\d+)>; rel="last"', response.headers.get("link", ""))
    return int(last.group(1)) if last else len(response.body)


def file_text(api: Api, full_name: str, path: str) -> str | None:
    body = contents(api, full_name, path).body
    if not isinstance(body, dict) or body.get("encoding") != "base64":
        return None
    return base64.b64decode(body["content"]).decode("utf-8", errors="replace")


def judge(api: Api, language: str, entry: dict, history: int) -> str | None:
    """Rule step 2 for one candidate: the reason it is out, or None."""
    name = entry["full_name"]
    commits = commit_count(api, name)
    if commits is None or commits < history:
        return f"{commits} commits, fewer than the {history} a sample is drawn from"
    listing = contents(api, name).body
    if not isinstance(listing, list):
        return "the root listing is unreadable"
    root = frozenset(item["name"] for item in listing)
    files = {path: file_text(api, name, path) for path in feasibility.files_to_read(language, root)}
    if language in feasibility.JVM_LANGUAGES:
        files |= {path: file_text(api, name, path) for path in feasibility.gradle_modules(files)}
    return feasibility.reject(language, root, files)


def read_pool(api: Api, language: str, as_of: datetime.date, excluded: set[str], history: int, wanted: int) -> list[dict]:
    """Tranches of `POOL_SIZE` by stars, each judged once, until `wanted` are feasible or the ranking or `POOL_LIMIT` ends."""
    judged: dict[str, dict] = {}
    size = POOL_SIZE
    while True:
        entries = pool(api, language, as_of, excluded, size)
        for entry in entries:
            if entry["full_name"] not in judged:
                reason = judge(api, language, entry, history)
                judged[entry["full_name"]] = {**entry, "feasible": reason is None, "reason": reason or ""}
        feasible = sum(judged[e["full_name"]]["feasible"] for e in entries)
        if feasible >= wanted or len(entries) < size or size >= POOL_LIMIT:
            return [judged[e["full_name"]] for e in entries]
        size += POOL_SIZE


def read_pools(api: Api, as_of: datetime.date) -> dict:
    corpus = load_corpus(ROOT / "corpus.toml")
    excluded = owners_of([repo.url for repo in corpus.repos])
    languages = {}
    for language in LANGUAGES:
        wanted = SET_COUNT * sum(repo.language == language for repo in corpus.repos)
        entries = read_pool(api, language, as_of, excluded, corpus.sampling.history, wanted)
        languages[language] = entries
        print(f"{language}: {len(entries)} in the pool, {sum(e['feasible'] for e in entries)} feasible", file=sys.stderr)
    return {"as_of": as_of.isoformat(), "pool_size": POOL_SIZE, "pool_limit": POOL_LIMIT, "languages": languages}


def order_key(seed: int, full_name: str) -> str:
    return hashlib.sha256(f"{seed}:{full_name}".encode()).hexdigest()


def draw_set(pools: dict, seed: int, counts: dict[str, int], taken: set[str]) -> dict[str, list[dict]]:
    """Per language the first `counts[language]` feasible candidates by `order_key`, none of them in `taken`."""
    chosen: dict[str, list[dict]] = {}
    for language, k in counts.items():
        open_entries = [e for e in pools["languages"][language] if e["feasible"] and e["full_name"] not in taken]
        chosen[language] = sorted(open_entries, key=lambda e: order_key(seed, e["full_name"]))[:k]
        if len(chosen[language]) < k:
            raise SystemExit(f"{language}: {len(chosen[language])} feasible repositories left for seed {seed}, {k} wanted")
    return chosen


def repo_name(full_name: str) -> str:
    return full_name.replace("/", "--")


def pin(url: str) -> str:
    return gitrepo.git("ls-remote", url, "HEAD").split()[0]


def seed_file(
    number: int, seed: int, drawn: str, holdout: bool, as_of: str, chosen: dict[str, list[dict]], pinned: str, pins: dict
) -> str:
    kind = "holdout: its rows are never read, only counters" if holdout else "working set: its rows may be read"
    lines = [
        f"# Seed set {number} ({kind}).",
        f"# Drawn by `seeds.py draw` from seeds/pool.json (read {as_of}) under the rule at the top of seeds.py, with seed {seed}",
        f"# (one draw of the operating system's CSPRNG on {drawn}). Pins read with `git ls-remote <url> HEAD` on {pinned};",
        "# licences from the hosting API. Counts, thresholds and pinned tools are corpus.toml's.",
        "",
        'extends = "../corpus.toml"',
        *(["holdout = true"] if holdout else []),
        "",
        "[sampling]",
        f"seed = {seed}",
    ]
    for language in LANGUAGES:
        for position, entry in enumerate(chosen[language]):
            lines += [
                "",
                "[[repo]]",
                f'name = "{repo_name(entry["full_name"])}"',
                f'url = "{entry["url"]}"',
                f'language = "{language}"',
                f'license = "{entry["license"]}"',
                f'sha = "{pins[entry["full_name"]]}"  # pragma: allowlist secret gitleaks:allow',
                *(["fast = true"] * (position == 0)),
            ]
    return "\n".join(lines) + "\n"


def names_of(chosen: dict[str, list[dict]]) -> set[str]:
    return {e["full_name"] for group in chosen.values() for e in group}


def replaced_names(as_of_pools: dict, plan: dict) -> set[str]:
    """Every name some set replaced, each with a reason and each in the pool: none of them is drawn by any set."""
    known = {e["full_name"] for entries in as_of_pools["languages"].values() for e in entries}
    replaced = [item for entry in plan["set"] for item in entry.get("replaced", [])]
    unfit = [item.get("name") for item in replaced if item.get("name") not in known or not str(item.get("reason", "")).strip()]
    if unfit:
        raise SystemExit(f"replaced without a pool name and a reason: {unfit}")
    return {item["name"] for item in replaced}


def draw(as_of_pools: dict, plan: dict, pins: Callable[[str], str], today: str) -> dict[Path, str]:
    corpus = load_corpus(ROOT / "corpus.toml")
    counts = {language: sum(r.language == language for r in corpus.repos) for language in LANGUAGES}
    taken = replaced_names(as_of_pools, plan)
    first_draw: list[set[str]] = []
    for entry in plan["set"]:
        first_draw.append(names_of(draw_set(as_of_pools, entry["seed"], counts, set().union(*first_draw))))
    held = set().union(*first_draw)
    written = {}
    for entry, own in zip(plan["set"], first_draw, strict=True):
        chosen = draw_set(as_of_pools, entry["seed"], counts, taken | (held - own))
        taken |= names_of(chosen)
        pinned = {e["full_name"]: pins(e["url"]) for group in chosen.values() for e in group}
        text = seed_file(
            entry["number"], entry["seed"], entry["drawn"], entry["holdout"], as_of_pools["as_of"], chosen, today, pinned
        )
        written[SEEDS / f"seed-{entry['number']}.toml"] = text
    return written


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="seeds", description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    pool_parser = commands.add_parser("pool")
    pool_parser.add_argument("--as-of", type=datetime.date.fromisoformat, default=datetime.date.today())
    commands.add_parser("draw")
    options = parser.parse_args(argv)
    if options.command == "pool":
        SEEDS.mkdir(exist_ok=True)
        POOL_FILE.write_text(json.dumps(read_pools(gh_api, options.as_of), indent=1, sort_keys=True) + "\n", encoding="utf-8")
        return 0
    pools = json.loads(POOL_FILE.read_text(encoding="utf-8"))
    plan = tomllib.loads(DRAW_FILE.read_text(encoding="utf-8"))
    for path, text in draw(pools, plan, pin, datetime.date.today().isoformat()).items():
        path.write_text(text, encoding="utf-8")
        print(f"wrote {path}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
