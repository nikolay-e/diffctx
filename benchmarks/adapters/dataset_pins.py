"""Resolve pinned HuggingFace dataset revisions.

Resolution order (first match wins):
1. Environment variable `BENCH_REVISION_<HF_PATH_UPPER_SAFE>` — emergency
   override for one-off runs without touching the pin file.
2. `benchmarks/dataset_revisions.json` — committed-to-git pinned SHAs from
   `python -m benchmarks.pin_revisions`.
3. Caller-provided `default` (typically `"main"` — fine for development,
   not bit-for-bit reproducible).

The JSON schema is::

    {
      "princeton-nlp/SWE-bench_Lite": {
        "revision": "<full-sha-40>",
        "fetched_at": "2026-04-29T10:00:00Z",
        "size": 300
      },
      ...
    }
"""

from __future__ import annotations

import json
import os
import re
from pathlib import Path

PIN_FILE = Path(__file__).resolve().parent.parent / "dataset_revisions.json"


def _env_var_name(hf_path: str) -> str:
    safe = re.sub(r"[^A-Za-z0-9]", "_", hf_path).upper()
    return f"BENCH_REVISION_{safe}"


def resolve_revision(hf_path: str, default: str = "main") -> str:
    env_var = _env_var_name(hf_path)
    if env_var in os.environ:
        return os.environ[env_var]
    if PIN_FILE.exists():
        try:
            pins = json.loads(PIN_FILE.read_text())
            entry = pins.get(hf_path)
            if entry and "revision" in entry:
                return str(entry["revision"])
        except (OSError, ValueError):
            pass
    return default


def load_pins() -> dict[str, dict]:
    if not PIN_FILE.exists():
        return {}
    return dict(json.loads(PIN_FILE.read_text()))


def save_pins(pins: dict[str, dict]) -> None:
    PIN_FILE.write_text(json.dumps(pins, indent=2, sort_keys=True) + "\n")
