from __future__ import annotations

import json
from collections.abc import Iterator

from eval.harness.adapters.base import (
    BenchmarkAdapter,
    BenchmarkInstance,
    GoldenFragment,
    extract_patch_files,
)
from eval.harness.adapters.dataset_pins import resolve_revision


def _parse_gold_context(raw: str) -> list[dict]:
    """Parse ContextBench `gold_context` JSON into normalized dicts.

    Mirrors `eval.workflows.contextbench.parse_gold_context` so adapters
    do not depend on the legacy script.
    """
    from eval.harness.common import normalize_gold_path

    items = json.loads(raw) if raw else []
    out: list[dict] = []
    for g in items:
        if not g.get("file") or g.get("start_line") is None:
            continue
        if g.get("end_line") is None:
            # A start without an end would make the fragment unhittable in
            # recall while still counting in the denominator.
            g["end_line"] = g["start_line"]
        g["file"] = normalize_gold_path(g["file"])
        out.append(g)
    return out


class ContextBenchAdapter(BenchmarkAdapter):
    """Adapter for the human-verified ContextBench dataset.

    Two HF configs are supported:
    - "default" — full set (~672 nontrivial after filtering)
    - "contextbench_verified" — curated subset for paper-grade evaluation
    """

    hf_path = "Contextbench/ContextBench"

    def __init__(self, config: str = "default", revision: str | None = None) -> None:
        self.config = config
        self._revision_override = revision
        self.name = "contextbench_verified" if config == "contextbench_verified" else "contextbench"

    @property
    def revision(self) -> str:
        return self._revision_override or resolve_revision(self.hf_path)

    def dataset_revision(self) -> str:
        return f"Contextbench/ContextBench[{self.config}]@{self.revision}"

    def _load_raw(self) -> Iterator[dict]:
        from datasets import load_dataset

        ds = load_dataset("Contextbench/ContextBench", self.config, split="train", revision=self.revision)
        for row in ds:
            yield dict(row)

    def _normalize(self, row: dict) -> BenchmarkInstance | None:
        patch = row.get("patch") or ""
        if not patch.strip():
            return None
        gold_files_from_patch = extract_patch_files(patch)
        gold = _parse_gold_context(row.get("gold_context") or "[]")
        gold_files_from_context = frozenset(g["file"] for g in gold)
        gold_files = gold_files_from_patch | gold_files_from_context
        if not gold_files:
            return None
        fragments: list[GoldenFragment] = []
        for g in gold:
            fragments.append(
                GoldenFragment(
                    path=g["file"],
                    start_line=g.get("start_line"),
                    end_line=g.get("end_line"),
                    kind=g.get("kind", "hunk"),
                )
            )
        return BenchmarkInstance(
            instance_id=f"{self.name}::{row.get('instance_id') or row.get('id') or row['repo']}__{row['base_commit'][:12]}",
            source_benchmark=self.name,
            repo=row["repo"],
            base_commit=row["base_commit"],
            gold_patch=patch,
            gold_files=gold_files,
            language=row.get("language") or "unknown",
            problem_statement=row.get("problem_statement"),
            gold_fragments=tuple(fragments) if fragments else None,
            edit_scope=len(gold_files_from_patch),
            extra={"repo_url": row.get("repo_url")},
        )
