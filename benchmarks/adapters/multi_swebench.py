from __future__ import annotations

from collections.abc import Iterator

from benchmarks.adapters.base import BenchmarkAdapter, BenchmarkInstance, extract_patch_files
from benchmarks.adapters.dataset_pins import resolve_revision

# Best-effort language inference for Multi-SWE-bench rows that do not carry
# an explicit `language` field. Map repo namespace → language; the upstream
# dataset is partitioned this way (one repo class per language subset).
_LANG_FROM_EXTENSION: dict[str, str] = {
    "java": "java",
    "kt": "kotlin",
    "ts": "typescript",
    "tsx": "typescript",
    "js": "javascript",
    "jsx": "javascript",
    "go": "go",
    "rs": "rust",
    "c": "c",
    "h": "c",
    "cc": "cpp",
    "cpp": "cpp",
    "cxx": "cpp",
    "hpp": "cpp",
    "py": "python",
}


def _extract_base_commit(row: dict) -> str:
    base = row.get("base") or {}
    if isinstance(base, dict):
        return base.get("sha") or base.get("commit") or ""
    return str(base) if base else ""


def _build_repo_name(row: dict) -> str:
    org = row.get("org") or ""
    repo_short = row.get("repo") or ""
    if org and "/" not in repo_short:
        return f"{org}/{repo_short}"
    return repo_short or org


def _infer_language(row: dict, gold_files: frozenset[str]) -> str:
    explicit = row.get("language")
    if explicit:
        return str(explicit).lower()
    counts: dict[str, int] = {}
    for path in gold_files:
        ext = path.rsplit(".", 1)[-1].lower() if "." in path else ""
        lang = _LANG_FROM_EXTENSION.get(ext)
        if lang:
            counts[lang] = counts.get(lang, 0) + 1
    if not counts:
        return "unknown"
    return max(counts.items(), key=lambda kv: kv[1])[0]


class _MultiSWEBenchAdapterBase(BenchmarkAdapter):
    """Adapter for ByteDance Multi-SWE-bench.

    Verified 2026-04-29 against the live HF API: only `default` config and
    a single `train` split exist. The `mini` / `flash` variants advertised
    upstream are not published as HF datasets; the full set is the only
    accessible source.

    Languages: Java, TS, JS, Go, Rust, C, C++ (and minor others). Schema is
    SWE-bench-shaped (instance_id, repo, base_commit, patch, problem_statement)
    with an optional `language` field. When `language` is missing we infer
    from the dominant gold-file extension.
    """

    hf_path = "bytedance-research/Multi-SWE-bench"

    def __init__(self, revision: str | None = None) -> None:
        self._revision_override = revision

    @property
    def revision(self) -> str:
        return self._revision_override or resolve_revision(self.hf_path)

    def dataset_revision(self) -> str:
        return f"{self.hf_path}@{self.revision}"

    def _load_raw(self) -> Iterator[dict]:
        # Streaming bypasses Arrow column-type casting: as of 2026-04-29 the
        # bytedance-research/Multi-SWE-bench dataset has shards with
        # inconsistent null-vs-string typing that breaks the bulk loader.
        # The streaming iterator can still raise on a malformed shard part-
        # way through; we treat that as end-of-stream and warn so we keep
        # whatever rows came through cleanly.
        import sys

        from datasets import load_dataset
        from datasets.exceptions import DatasetGenerationError

        ds = load_dataset(self.hf_path, split="train", revision=self.revision, streaming=True)
        it = iter(ds)
        n = 0
        while True:
            try:
                row = next(it)
            except StopIteration:
                break
            except (DatasetGenerationError, TypeError, ValueError) as e:
                print(
                    f"[WARN] {self.name}: stream stopped early at row {n} ({type(e).__name__}: {str(e)[:200]})",
                    file=sys.stderr,
                )
                break
            n += 1
            yield dict(row)

    def _normalize(self, row: dict) -> BenchmarkInstance | None:
        patch = row.get("fix_patch") or row.get("patch") or ""
        if not patch.strip():
            return None
        gold_files = extract_patch_files(patch)
        if not gold_files:
            return None
        base_commit = _extract_base_commit(row)
        if not base_commit:
            return None
        repo = _build_repo_name(row)
        if not repo:
            return None
        return BenchmarkInstance(
            instance_id=f"{self.name}::{row['instance_id']}",
            source_benchmark=self.name,
            repo=repo,
            base_commit=base_commit,
            gold_patch=patch,
            gold_files=gold_files,
            language=_infer_language(row, gold_files),
            problem_statement=row.get("body") or row.get("title") or row.get("problem_statement"),
            gold_fragments=None,
            difficulty=row.get("difficulty"),
            edit_scope=len(gold_files),
            extra={
                "test_patch": row.get("test_patch"),
                "hints_text": row.get("hints"),
            },
        )


class MultiSWEBenchAdapter(_MultiSWEBenchAdapterBase):
    name = "multi_swebench"


# `MultiSWEBenchMiniAdapter` and `MultiSWEBenchFlashAdapter` removed: the
# upstream dataset only publishes a single `default` config. Use
# `MultiSWEBenchAdapter` and slice via the manifest layer if a smaller
# subset is needed.
