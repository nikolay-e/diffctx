"""SWE-Explore adapter — trajectory-derived gold, which is what makes it worth having.

SWE-Explore's ground truth is not "what the patch touched". It is the set of
regions that independent agent trajectories *actually read* on their way to a
working fix, distilled from successful runs (arXiv:2606.07297, 848 issues / 203
repositories / 10 languages).

That distinction is the whole reason to integrate it. Every other corpus here
derives gold from the diff, which measures retention — can the tool re-surface
the lines that changed. Trajectory gold measures retrieval — did the tool
surface what someone needed to consult, whether or not it changed. A tool can
score well on the first and badly on the second, and until now nothing in this
harness could tell those apart with third-party labels.

## What this adapter is NOT

**Not a leaderboard entry.** SWE-Explore is issue-seeded: the explorer receives
the issue text and returns ranked regions. diffctx is diff-seeded — it needs a
change to explain. Running it here means feeding it the patch and asking whether
the context it selects matches what trajectories read. That is a coherent and
interesting question, and it is *a different input than the benchmark defines*,
so the numbers are not comparable to published SWE-Explore results and must
never be reported as if they were. `extra["seeding"]` records which input was
used so an analysis cannot lose that distinction by accident.

Issue-seeded operation would need diffctx to accept a textual seed instead of a
diff — a product capability, not an adapter concern, and not something to fake
here by pretending the issue text is a diff.

## Licence

The dataset is **CC-BY-NC-ND-4.0**, unlike the MIT-licensed evaluation code in
the upstream GitHub repository. Two consequences this adapter is built around:

- *NoDerivatives*: nothing from the dataset is copied into this repository. The
  adapter streams from the user's own HuggingFace download and stores only a
  pinned revision, following the same identifiers-not-content rule as
  `datasets/eval-splits`.
- *NonCommercial*: whether this project's use qualifies is a decision for the
  maintainer, not something an adapter can settle. It is flagged here so the
  question is asked before results are published.
"""

from __future__ import annotations

from collections.abc import Iterator

from eval.harness.adapters.base import BenchmarkAdapter, BenchmarkInstance, GoldenFragment, extract_patch_files
from eval.harness.adapters.dataset_pins import resolve_revision

_LANG_FROM_EXTENSION = {
    "py": "python",
    "rs": "rust",
    "go": "go",
    "js": "javascript",
    "jsx": "javascript",
    "ts": "typescript",
    "tsx": "typescript",
    "java": "java",
    "kt": "kotlin",
    "scala": "scala",
    "rb": "ruby",
    "php": "php",
    "c": "c",
    "h": "c",
    "cc": "cpp",
    "cpp": "cpp",
    "cs": "csharp",
    "swift": "swift",
}


def _infer_language(paths: frozenset[str] | set[str]) -> str:
    counts: dict[str, int] = {}
    for p in paths:
        ext = p.rsplit(".", 1)[-1].lower() if "." in p else ""
        lang = _LANG_FROM_EXTENSION.get(ext)
        if lang:
            counts[lang] = counts.get(lang, 0) + 1
    return max(counts, key=lambda k: counts[k]) if counts else "unknown"


def parse_regions(raw: object) -> tuple[GoldenFragment, ...]:
    """`[{"path": ..., "start": N, "end": N}, ...]` to golden fragments.

    A region missing either bound becomes a whole-file fragment rather than
    being dropped: the file is still genuinely gold, and silently discarding it
    would understate recall while looking like the tool missed nothing.
    """
    if not isinstance(raw, list):
        return ()
    out: list[GoldenFragment] = []
    for item in raw:
        if not isinstance(item, dict):
            continue
        path = item.get("path")
        if not isinstance(path, str) or not path:
            continue
        start, end = item.get("start"), item.get("end")
        if isinstance(start, int) and isinstance(end, int) and end >= start:
            out.append(GoldenFragment(path=path, start_line=start, end_line=end, kind="region"))
        else:
            out.append(GoldenFragment(path=path, kind="file"))
    return tuple(out)


def _core_files(ground_truth: dict) -> frozenset[str]:
    """Files every successful trajectory read.

    Falls back to the paths named by the core regions: `read_core_files` and
    `read_core_regions` should agree, and taking the union of what is present
    rather than trusting one field keeps a schema drift in either from silently
    emptying the gold set.
    """
    files = {f for f in (ground_truth.get("read_core_files") or []) if isinstance(f, str)}
    files |= {r.path for r in parse_regions(ground_truth.get("read_core_regions"))}
    return frozenset(files)


class SweExploreAdapter(BenchmarkAdapter):
    """`SWE-Explore-Bench/SWE-Explore-Bench` on HuggingFace, test split.

    Layout per the upstream README: each row carries `instance_id`, a
    `ground_truth` object with `read_core_files` / `read_core_regions` /
    `read_optional_*_map` / `modified_core_files`, plus `read_step_info` and
    `meta`. Optional regions are model-specific diagnostic context and are
    surfaced in `extra` rather than as gold — scoring against them would
    penalise a tool for not reproducing one model's detours.
    """

    hf_path = "SWE-Explore-Bench/SWE-Explore-Bench"
    name = "swe_explore"

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

    @staticmethod
    def _seed_patch(row: dict) -> str | None:
        # The seed. diffctx needs a change to explain, so an instance without a
        # patch cannot be run diff-seeded and is skipped rather than run against
        # a fabricated one — a patch synthesised from `modified_core_files`
        # would be this adapter inventing the input and calling the result a
        # measurement.
        patch = row.get("patch") or row.get("gold_patch") or ""
        if not isinstance(patch, str) or not patch.strip():
            return None
        return patch

    def _normalize(self, row: dict) -> BenchmarkInstance | None:
        gt = row.get("ground_truth")
        if not isinstance(gt, dict):
            return None
        gold_files = _core_files(gt)
        if not gold_files:
            return None

        patch = self._seed_patch(row)
        if patch is None:
            return None
        patch_files = extract_patch_files(patch)

        fragments = parse_regions(gt.get("read_core_regions"))
        modified = frozenset(f for f in (gt.get("modified_core_files") or []) if isinstance(f, str))
        instance_id = row.get("instance_id") or row.get("id")
        if not isinstance(instance_id, str) or not instance_id:
            return None

        raw_meta = row.get("meta")
        meta: dict = raw_meta if isinstance(raw_meta, dict) else {}
        raw_optional = gt.get("read_optional_regions_map")
        optional_map: dict = raw_optional if isinstance(raw_optional, dict) else {}
        repo = meta.get("repo") or row.get("repo") or "unknown/unknown"
        base_commit = meta.get("base_commit") or row.get("base_commit") or ""

        return BenchmarkInstance(
            instance_id=f"swe_explore::{instance_id}",
            source_benchmark=self.name,
            repo=str(repo),
            base_commit=str(base_commit),
            gold_patch=patch,
            gold_files=gold_files,
            language=_infer_language(gold_files or patch_files),
            problem_statement=row.get("problem_statement") or meta.get("problem_statement"),
            gold_fragments=fragments or None,
            edit_scope=len(patch_files) if patch_files else None,
            extra={
                # Records that this is a diff-seeded reading of an issue-seeded
                # benchmark. Without it, a later analysis could compare these
                # numbers to published SWE-Explore results, which measure a
                # different input.
                "seeding": "diff",
                "benchmark_seeding": "issue",
                "gold_provenance": "agent_trajectories",
                "modified_core_files": sorted(modified),
                # Gold that the patch did NOT touch: the retrieval half, and the
                # reason this corpus is here rather than another diff-derived one.
                "nontrivial_gold": sorted(gold_files - modified),
                "nontrivial_gold_count": len(gold_files - modified),
                "optional_region_models": sorted(optional_map),
                "dataset_license": "CC-BY-NC-ND-4.0",
            },
        )
