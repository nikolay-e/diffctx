"""Agent Retrieval Bench `edit2ripple` (arXiv 2607.24882), read diff-seeded.

An edit2ripple sample is one anchored file change plus the files that change
rippled into, which is the question diffctx answers: a change exists, what else
must be read. The sample ships the anchor file's hunks (`query.anchor_diff`) and
a free-text intent; gold is the other source and test files of the same fix,
anchor excluded.

## What this adapter is NOT

**Not a leaderboard entry.** The benchmark query is the intent plus the anchor
diff; diffctx takes the change and nothing else, so the intent is not used.
`extra["seeding"] = "diff-only"` keeps that distinction in every row, because a
number from this adapter measures a different input than the published ones.

## Licence

Evaluator code and benchmark metadata are MIT. The release corpus is a set of
snapshots of upstream projects and stays under their licences; nothing from it
is stored in this repository, only the pinned revision and the archive hash in
`datasets/external-pins/arb_edit2ripple.json`.
"""

from __future__ import annotations

import json
import re
from collections.abc import Iterator
from pathlib import Path

from eval.harness.adapters.base import BenchmarkAdapter, BenchmarkInstance

PINS_PATH = Path(__file__).resolve().parents[3] / "datasets" / "external-pins" / "arb_edit2ripple.json"

_LANGUAGE_BY_REPO = {
    "caddyserver/caddy": "go",
    "clap-rs/clap": "rust",
    "etcd-io/etcd": "go",
    "gin-gonic/gin": "go",
    "huggingface/diffusers": "python",
    "huggingface/transformers": "python",
    "pytest-dev/pytest": "python",
    "spring-projects/spring-boot": "java",
    "tokio-rs/tokio": "rust",
    "vitejs/vite": "typescript",
}


def load_pins() -> dict:
    return dict(json.loads(PINS_PATH.read_text()))


TRUNCATION_MARKER = "...[truncated]"
_HUNK_HEADER = re.compile(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@(.*)$")


def repair_anchor_diff(anchor_diff: str) -> tuple[str, dict]:
    """The shipped hunks with their line counts made true to their bodies.

    The release stores the anchor diff as text, and 33 of its 58 samples do not
    apply as shipped: a trailing blank context line was trimmed (the header
    counts one line more than the body holds), or the text was cut at a size
    limit and ends in `...[truncated]`. Neither changes a line the diff says;
    the header counts are rewritten to match the body, the marker is dropped, and a
    hunk cut off before its first change (which holds none) is left out.
    What was repaired is returned, so a row can be told apart from an untouched
    one and a truncated seed is never reported as the whole change.
    """
    lines = anchor_diff.split("\n")
    truncated = False
    while lines and lines[-1] in ("", TRUNCATION_MARKER):
        if lines.pop() == TRUNCATION_MARKER:
            truncated = True
    hunks: list[tuple[re.Match[str], list[str]]] = []
    for line in lines:
        header = _HUNK_HEADER.match(line)
        if header:
            hunks.append((header, []))
        elif hunks:
            hunks[-1][1].append(line)
    out: list[str] = []
    recounted = 0
    dropped = 0
    for header, body in hunks:
        if not any(b[:1] in ("+", "-") for b in body):
            dropped += 1  # a hunk cut off before its first change carries none, and git rejects it
            continue
        # an empty line inside a hunk is a blank context line whose leading space was lost
        old = sum(1 for b in body if b[:1] in ("", " ", "-"))
        new = sum(1 for b in body if b[:1] in ("", " ", "+"))
        if (old, new) != (int(header.group(2) or 1), int(header.group(4) or 1)):
            recounted += 1
        out.append(f"@@ -{header.group(1)},{old} +{header.group(3)},{new} @@{header.group(5)}")
        out.extend(body)
    return ("\n".join(out) + "\n" if out else ""), {
        "truncated": truncated,
        "recounted_hunks": recounted,
        "dropped_changeless_hunks": dropped,
    }


def seed_patch(anchor_file: str, anchor_diff: str) -> str:
    """The anchor's hunks as a patch `git apply` takes (header added, counts repaired)."""
    body, _ = repair_anchor_diff(anchor_diff)
    return f"diff --git a/{anchor_file} b/{anchor_file}\n--- a/{anchor_file}\n+++ b/{anchor_file}\n{body}"


class ArbEdit2RippleAdapter(BenchmarkAdapter):
    """Rows of the release's `samples.jsonl`, read from a local file.

    The network step (fetching and verifying the archive) lives in
    `eval.external.arb_edit2ripple`; normalization stays pure.
    """

    name = "arb_edit2ripple"

    def __init__(self, samples_path: Path, repo_url_template: str = "https://github.com/{repo}.git") -> None:
        self.samples_path = samples_path
        self.repo_url_template = repo_url_template

    def dataset_revision(self) -> str:
        pins = load_pins()["dataset"]
        return f"{pins['hf_repo']}@{pins['revision']}#{pins['release']}"

    def _load_raw(self) -> Iterator[dict]:
        with self.samples_path.open(encoding="utf-8") as handle:
            for line in handle:
                if line.strip():
                    yield json.loads(line)

    def unseedable(self) -> list[str]:
        """Ids of samples that cannot be expressed as a diff plus gold.

        They are reported with their own status by the runner; a sample must
        never disappear from the denominator unnoticed.
        """
        return [str(row.get("id")) for row in self._load_raw() if self._normalize(row) is None]

    def _normalize(self, row: dict) -> BenchmarkInstance | None:
        query, gold = row.get("query") or {}, row.get("gold") or {}
        anchor, diff = query.get("anchor_file"), query.get("anchor_diff")
        if not isinstance(anchor, str) or not anchor or not isinstance(diff, str) or not repair_anchor_diff(diff)[0]:
            return None
        gold_files = frozenset(f for f in gold.get("files") or [] if isinstance(f, str) and f != anchor)
        if not gold_files or not row.get("id") or not row.get("base_commit"):
            return None
        repo = str(row["repo"])
        return BenchmarkInstance(
            instance_id=f"{self.name}::{row['id']}",
            source_benchmark=self.name,
            repo=repo,
            base_commit=str(row["base_commit"]),
            gold_patch=seed_patch(anchor, diff),
            gold_files=gold_files,
            language=_LANGUAGE_BY_REPO.get(repo, "unknown"),
            problem_statement=None,
            edit_scope=1,
            extra={
                "repo_url": self.repo_url_template.format(repo=repo),
                "sample_id": str(row["id"]),
                "anchor_file": anchor,
                "seed_repair": repair_anchor_diff(diff)[1],
                "seeding": "diff-only",
                "benchmark_seeding": "intent+anchor_diff",
                "gold_provenance": "same_fix_ripple",
            },
        )
