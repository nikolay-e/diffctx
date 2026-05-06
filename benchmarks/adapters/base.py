from __future__ import annotations

from abc import ABC, abstractmethod
from collections.abc import Iterator
from dataclasses import dataclass, field


@dataclass(frozen=True)
class GoldenFragment:
    """A single piece of ground-truth context within a benchmark instance.

    `start_line == end_line == None` means the whole file is golden.
    Both bounds are 1-indexed and inclusive when present.
    """

    path: str
    start_line: int | None = None
    end_line: int | None = None
    kind: str = "file"  # "file" | "function" | "class" | "hunk" | "node"

    def is_whole_file(self) -> bool:
        return self.start_line is None and self.end_line is None


@dataclass(frozen=True)
class BenchmarkInstance:
    """One benchmark task, normalized across heterogeneous sources.

    The `instance_id` is globally unique: it carries the source benchmark name
    so that instances from different benchmarks for the same (repo, base_commit)
    do not collide.
    """

    instance_id: str  # e.g. "swebench_lite::astropy__astropy-12907"
    source_benchmark: str  # e.g. "swebench_lite"
    repo: str  # "owner/name"
    base_commit: str
    gold_patch: str  # unified diff
    gold_files: frozenset[str]  # always available — derived from patch
    language: str
    problem_statement: str | None = None
    gold_fragments: tuple[GoldenFragment, ...] | None = None  # only when source provides
    difficulty: str | None = None
    edit_scope: int | None = None  # number of distinct files in gold_patch
    extra: dict[str, object] = field(default_factory=dict)


@dataclass
class EvalResult:
    """Outcome of evaluating one method on one BenchmarkInstance."""

    instance_id: str
    source_benchmark: str
    file_recall: float
    file_precision: float
    fragment_recall: float | None = None
    fragment_precision: float | None = None
    line_f1: float | None = None
    used_tokens: int = 0
    budget: int = 0
    elapsed_seconds: float = 0.0
    extra: dict[str, object] = field(default_factory=dict)


class BenchmarkAdapter(ABC):
    """Pure normalization layer for one benchmark dataset.

    Subclasses implement `_load_raw()` (fetches the heterogeneous source) and
    `_normalize(row)` (turns one source row into a `BenchmarkInstance`).
    `load()` is the only entry point callers use.
    """

    name: str

    @abstractmethod
    def dataset_revision(self) -> str:
        """Pinned dataset revision for reproducibility (commit SHA, version tag)."""

    @abstractmethod
    def _load_raw(self) -> Iterator[dict]:
        """Yield raw rows from the source. Network I/O lives here."""

    @abstractmethod
    def _normalize(self, row: dict) -> BenchmarkInstance | None:
        """Map one raw row to a normalized instance, or None to skip."""

    def load(self) -> Iterator[BenchmarkInstance]:
        for row in self._load_raw():
            instance = self._normalize(row)
            if instance is not None:
                yield instance


def extract_patch_files(patch: str) -> frozenset[str]:
    """File-level recall ground truth — files present on disk after applying the patch.

    Excludes pure deletions and old paths of pure renames so the recall
    ceiling matches what any retrieval algorithm can actually return.
    """
    from benchmarks.common import patch_files_at_head

    return frozenset(patch_files_at_head(patch))
