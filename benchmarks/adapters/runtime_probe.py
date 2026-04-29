"""Pre-flight checks for sweep environments.

Closes two silent footguns the user hits otherwise:
- macOS Docker Desktop VM defaults to 8 GB. A `--memory=16g` container limit
  silently OOM-kills inside that VM. Probe `/proc/meminfo` and exit early
  with a helpful message if the visible RAM is below `min_memory_gb`.
- Docker Desktop default virtual disk is 64 GB. SWE-bench `transformers`
  alone is ~4 GB; calibration touches 10-20 distinct repos. Without a disk
  probe the sweep dies mid-run on the first ENOSPC. Warn (not exit) when
  the repos volume has less than `min_disk_gb` free, since the user may
  have a pre-warmed cache.
"""

from __future__ import annotations

import shutil
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Literal

Severity = Literal["info", "warn", "error"]


@dataclass(frozen=True)
class ProbeMessage:
    severity: Severity
    message: str


def _probe_memory(min_memory_gb: float) -> list[ProbeMessage]:
    meminfo = Path("/proc/meminfo")
    if not meminfo.exists():
        return [ProbeMessage("info", "skipping memory probe: /proc/meminfo not found (non-Linux host)")]
    try:
        for line in meminfo.read_text().splitlines():
            if line.startswith("MemTotal:"):
                kb = int(line.split()[1])
                gb = kb / (1024 * 1024)
                if gb < min_memory_gb:
                    return [
                        ProbeMessage(
                            "error",
                            f"only {gb:.1f} GiB visible memory; raise the limit to "
                            f">= {min_memory_gb:.0f} GiB. On macOS, Docker Desktop -> "
                            f"Settings -> Resources -> Memory.",
                        )
                    ]
                return [ProbeMessage("info", f"memory probe OK: {gb:.1f} GiB visible")]
    except (OSError, ValueError) as e:
        return [ProbeMessage("warn", f"memory probe failed: {e}")]
    return [ProbeMessage("warn", "memory probe failed: MemTotal not found in /proc/meminfo")]


def _probe_disk(repos_dir: Path | None, min_disk_gb: float) -> list[ProbeMessage]:
    if repos_dir is None:
        return [ProbeMessage("info", "skipping disk probe: no repos_dir provided")]
    if not repos_dir.exists():
        try:
            repos_dir.mkdir(parents=True, exist_ok=True)
        except OSError as e:
            return [ProbeMessage("warn", f"repos dir {repos_dir} cannot be created: {e}")]
    try:
        free_gb = shutil.disk_usage(repos_dir).free / (1024**3)
    except OSError as e:
        return [ProbeMessage("warn", f"disk probe failed at {repos_dir}: {e}")]
    if free_gb < min_disk_gb:
        return [
            ProbeMessage(
                "warn",
                f"only {free_gb:.0f} GB free in {repos_dir}; calibration may fail "
                f"at clone stage. SWE-bench transformers alone is ~4 GB; total "
                f">= {min_disk_gb:.0f} GB recommended (or pre-warm the cache).",
            )
        ]
    return [ProbeMessage("info", f"disk probe OK: {free_gb:.0f} GB free at {repos_dir}")]


def probe_resources(
    *,
    min_memory_gb: float = 16.0,
    repos_dir: Path | None = None,
    min_disk_gb: float = 50.0,
) -> list[ProbeMessage]:
    """Run all pre-flight resource checks. Returns a flat message list."""
    return _probe_memory(min_memory_gb) + _probe_disk(repos_dir, min_disk_gb)


def report_and_maybe_exit(messages: list[ProbeMessage], *, strict: bool = True) -> None:
    """Print messages to stderr; if any are `error` and `strict`, exit 2."""
    has_error = False
    for m in messages:
        marker = {"info": "[INFO]", "warn": "[WARN]", "error": "[ERROR]"}[m.severity]
        print(f"{marker} {m.message}", file=sys.stderr)
        if m.severity == "error":
            has_error = True
    if has_error and strict:
        sys.exit(2)
