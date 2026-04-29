from __future__ import annotations

from collections.abc import Iterable
from dataclasses import dataclass

from benchmarks.adapters.base import BenchmarkAdapter, BenchmarkInstance


@dataclass(frozen=True)
class ContaminationKey:
    """Equivalence class for instances that share underlying state.

    Two instances with the same key are treated as referring to the same
    upstream codebase snapshot — using one for calibration and the other for
    evaluation is leakage. ContextBench is built from SWE-bench Verified
    plus PolyBench plus Multi-SWE-bench, so this dedup is necessary.
    """

    repo: str
    base_commit: str

    @classmethod
    def from_instance(cls, instance: BenchmarkInstance) -> ContaminationKey:
        return cls(repo=instance.repo, base_commit=instance.base_commit)


class ContaminationDetector:
    """Cross-benchmark dedup index.

    Build once over the union of adapters; query per-instance to find sister
    instances in other benchmarks that share the same (repo, base_commit).
    """

    def __init__(self, adapters: Iterable[BenchmarkAdapter] | None = None) -> None:
        self._key_to_ids: dict[ContaminationKey, set[str]] = {}
        if adapters is not None:
            for adapter in adapters:
                self.ingest(adapter)

    def ingest(self, adapter: BenchmarkAdapter) -> None:
        for instance in adapter.load():
            self.ingest_instance(instance)

    def ingest_instance(self, instance: BenchmarkInstance) -> None:
        key = ContaminationKey.from_instance(instance)
        self._key_to_ids.setdefault(key, set()).add(instance.instance_id)

    def find_duplicates(self, instance: BenchmarkInstance) -> set[str]:
        """All instance_ids sharing the same (repo, base_commit), excluding self."""
        key = ContaminationKey.from_instance(instance)
        return self._key_to_ids.get(key, set()) - {instance.instance_id}

    def is_contaminated(self, instance: BenchmarkInstance, blocked_ids: set[str]) -> bool:
        """True if any sister of this instance appears in the blocked set."""
        return bool(self.find_duplicates(instance) & blocked_ids)

    def filter_calibration_pool(
        self,
        candidates: Iterable[BenchmarkInstance],
        held_out_ids: set[str],
    ) -> list[BenchmarkInstance]:
        """Drop calibration candidates that share state with held-out test instances."""
        return [c for c in candidates if not self.is_contaminated(c, held_out_ids)]

    def stats(self) -> dict[str, int]:
        n_keys = len(self._key_to_ids)
        n_instances = sum(len(ids) for ids in self._key_to_ids.values())
        n_collisions = sum(1 for ids in self._key_to_ids.values() if len(ids) > 1)
        return {"keys": n_keys, "instances": n_instances, "collisions": n_collisions}
