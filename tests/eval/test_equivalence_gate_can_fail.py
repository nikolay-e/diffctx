from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

PROJECT_ROOT = Path(__file__).resolve().parents[2]

# A gate is only a gate if it fails on a known-bad input. `python -m eval
# equivalence` printed "EQUIVALENCE FAILED" and exited 0 for its whole life
# because the CLI dispatcher discarded the subcommand's return value (#233) —
# so any CI job wired to the documented invocation would have been decorative.


def _run_dir(tmp_path: Path, name: str, selected: list[str], tokens: int) -> Path:
    d = tmp_path / name
    d.mkdir()
    row = {
        "instance_id": "repo__1",
        "status": "ok",
        "selected_files": selected,
        "used_tokens": tokens,
        "file_recall": 1.0,
        "file_precision": 1.0,
    }
    (d / "set.checkpoint.jsonl").write_text(json.dumps(row) + "\n")
    return d


def _gate(a: Path, b: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, "-m", "eval", "equivalence", "--a", str(a), "--b", str(b)],
        cwd=PROJECT_ROOT,
        capture_output=True,
        text=True,
    )


def test_the_gate_exits_nonzero_on_a_divergent_run(tmp_path):
    a = _run_dir(tmp_path, "old", ["src/a.py"], 100)
    b = _run_dir(tmp_path, "new", ["src/b.py"], 140)

    result = _gate(a, b)

    assert result.returncode != 0, f"the gate passed a divergent pair: {result.stdout}"
    assert "EQUIVALENCE FAILED" in result.stdout


def test_the_gate_exits_zero_on_an_identical_run(tmp_path):
    a = _run_dir(tmp_path, "old", ["src/a.py"], 100)
    b = _run_dir(tmp_path, "new", ["src/a.py"], 100)

    result = _gate(a, b)

    assert result.returncode == 0, result.stdout + result.stderr
    assert "EQUIVALENT" in result.stdout
