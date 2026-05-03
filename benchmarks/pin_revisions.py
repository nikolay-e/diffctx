"""Fetch the current HuggingFace dataset SHA for every adapter and write
`benchmarks/dataset_revisions.json`.

Run BEFORE the first calibration sweep. Commit the resulting JSON so the
splits and final eval are bit-for-bit reproducible.

Example::

    python -m benchmarks.pin_revisions
    git add benchmarks/dataset_revisions.json
    git commit -m "chore(benchmarks): pin dataset revisions for v1 calibration"
"""

from __future__ import annotations

import datetime as dt
import sys
from collections.abc import Iterable

from benchmarks.adapters import (
    ContextBenchAdapter,
    MultiSWEBenchAdapter,
    PolyBench500Adapter,
    PolyBenchAdapter,
    SWEBenchLiteAdapter,
    SWEBenchVerifiedAdapter,
)
from benchmarks.adapters.dataset_pins import PIN_FILE, load_pins, save_pins


def _hf_paths_to_pin() -> Iterable[str]:
    """One entry per HF repo we depend on. Sub-configs share the same SHA."""
    seen = set()
    for adapter in (
        SWEBenchLiteAdapter(),
        SWEBenchVerifiedAdapter(),
        PolyBenchAdapter(),
        PolyBench500Adapter(),
        MultiSWEBenchAdapter(),
        ContextBenchAdapter(config="default"),
        ContextBenchAdapter(config="contextbench_verified"),
    ):
        path = getattr(adapter, "hf_path", None)
        if path and path not in seen:
            seen.add(path)
            yield path


def main() -> int:
    try:
        from huggingface_hub import HfApi
    except ImportError:
        print("huggingface_hub not installed; pip install huggingface_hub", file=sys.stderr)
        return 2

    api = HfApi()
    pins = load_pins()
    now = dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds")
    failures: list[tuple[str, str]] = []

    for hf_path in _hf_paths_to_pin():
        try:
            info = api.repo_info(hf_path, repo_type="dataset")
        except Exception as e:
            failures.append((hf_path, str(e)))
            print(f"  FAIL {hf_path}: {e}")
            continue
        sha = info.sha
        prev = pins.get(hf_path, {}).get("revision")
        pins[hf_path] = {
            "revision": sha,
            "fetched_at": now,
            "hf_path": hf_path,
        }
        prev_marker = f"was {prev[:12]}" if prev else "was main"
        marker = "(unchanged)" if prev == sha else f"({prev_marker})"
        print(f"  OK   {hf_path:<45} {sha[:12]} {marker}")

    save_pins(pins)
    print(f"\nWrote {len(pins)} entries to {PIN_FILE}")
    if failures:
        print(f"\n{len(failures)} datasets could not be pinned:")
        for path, err in failures:
            print(f"  {path}: {err}")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
