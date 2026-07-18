from __future__ import annotations

from collections.abc import Iterator

from benchmarks.adapters.base import (
    BenchmarkAdapter,
    BenchmarkInstance,
    GoldenFragment,
    extract_patch_files,
)
from benchmarks.adapters.dataset_pins import resolve_revision


def _parse_node_identity(node: object) -> tuple[str, str, str] | None:
    if not isinstance(node, str) or "->" not in node:
        return None
    segments = node.split("->")
    path = segments[0].strip()
    if not path:
        return None
    terminal = segments[-1].strip()
    kind = terminal.split(":", 1)[0] if ":" in terminal else (terminal or "node")
    return path, terminal, kind


class _PolyBenchAdapterBase(BenchmarkAdapter):
    """Adapter for amazon-science SWE-PolyBench family (Java / JS / TS / Python).

    PolyBench ships CST node-level annotations alongside the gold patch in the
    `modified_nodes` column: a JSON string holding a list of node-identity
    strings of the form
    `"path/to/File.java->program->class_declaration:Name->method_declaration:name"`.
    The annotations carry no line numbers; resolving them to line ranges needs
    a tree-sitter pass over the base-commit worktree (open work). Until then
    the nodes are surfaced as whole-file golden fragments so fragment-level
    metrics are defined at file granularity. Instances without annotations
    still expose file-level recall via the patch.

    The upstream layout (verified 2026-04-29 against the live HF API):
    - `AmazonScience/SWE-PolyBench`     — full ~2110, `PolyBenchAdapter`
    - `AmazonScience/SWE-PolyBench_500` — curated 500, `PolyBench500Adapter`
    Both have a single `default` config and a `test` split.
    """

    hf_path: str

    def __init__(self, revision: str | None = None) -> None:
        self._revision_override = revision

    @property
    def revision(self) -> str:
        return self._revision_override or resolve_revision(self.hf_path)

    def dataset_revision(self) -> str:
        return f"{self.hf_path}@{self.revision}"

    def _load_raw(self) -> Iterator[dict]:
        from datasets import load_dataset

        ds = load_dataset(self.hf_path, split="test", revision=self.revision)
        for row in ds:
            yield dict(row)

    def _normalize(self, row: dict) -> BenchmarkInstance | None:
        patch = row.get("patch") or row.get("gold_patch") or ""
        if not patch.strip():
            return None
        gold_files_from_patch = extract_patch_files(patch)
        if not gold_files_from_patch:
            return None

        fragments = self._extract_cst_fragments(row)
        gold_files = gold_files_from_patch | frozenset(f.path for f in fragments)

        language = (row.get("language") or "unknown").lower()
        return BenchmarkInstance(
            instance_id=f"{self.name}::{row['instance_id']}",
            source_benchmark=self.name,
            repo=row["repo"],
            base_commit=row["base_commit"],
            gold_patch=patch,
            gold_files=gold_files,
            language=language,
            problem_statement=row.get("problem_statement"),
            gold_fragments=tuple(fragments) if fragments else None,
            difficulty=row.get("difficulty"),
            edit_scope=len(gold_files_from_patch),
            extra={
                "test_patch": row.get("test_patch"),
                "hints_text": row.get("hints_text"),
            },
        )

    @staticmethod
    def _extract_cst_fragments(row: dict) -> list[GoldenFragment]:
        """Parse the `modified_nodes` column into golden fragments.

        The column is a JSON string of node-identity strings
        (`"path->program->kind:name[->kind:name...]"`), without line numbers.
        Each node becomes a whole-file fragment carrying the terminal node
        kind, deduplicated per (path, kind:name).
        """
        import json

        raw = row.get("modified_nodes")
        if not raw:
            return []
        if isinstance(raw, str):
            try:
                raw = json.loads(raw)
            except (ValueError, TypeError):
                return []
        if not isinstance(raw, list):
            return []
        out: list[GoldenFragment] = []
        seen: set[tuple[str, str]] = set()
        for n in raw:
            parsed = _parse_node_identity(n)
            if parsed is None or parsed[:2] in seen:
                continue
            path, terminal, kind = parsed
            seen.add((path, terminal))
            out.append(GoldenFragment(path=path, start_line=None, end_line=None, kind=kind))
        return out


class PolyBenchAdapter(_PolyBenchAdapterBase):
    name = "polybench"
    hf_path = "AmazonScience/SWE-PolyBench"


class PolyBench500Adapter(_PolyBenchAdapterBase):
    name = "polybench500"
    hf_path = "AmazonScience/SWE-PolyBench_500"


# `PolyBenchVerifiedAdapter` removed: there is no publicly available
# "verified" sub-dataset under AmazonScience as of 2026-04-29. Use
# `PolyBench500Adapter` for the curated subset instead.
