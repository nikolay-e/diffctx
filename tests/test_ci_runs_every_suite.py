import re
import subprocess
import sys
from pathlib import Path

import pytest
import yaml

PROJECT_ROOT = Path(__file__).parent.parent
SUITES_SCRIPT = "scripts/cargo-test-suites.sh"


def _suites() -> set[str]:
    return {p.stem for p in (PROJECT_ROOT / "crates/diffctx-native/tests").glob("*.rs")}


def _missing(command: str) -> set[str]:
    if SUITES_SCRIPT in command:
        return set()
    return _suites() - set(re.findall(r"--test\s+(\w+)", command))


def _rust_jobs() -> dict[str, str]:
    ci = yaml.safe_load((PROJECT_ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8"))
    return {
        name: "\n".join(step.get("run", "") for step in job["steps"])
        for name, job in ci["jobs"].items()
        if job.get("name", "").startswith("Rust diffctx tests")
    }


def test_every_rust_suite_runs_in_each_rust_ci_job():
    """A suite under tests/ that no job runs guards nothing: the 1.20.0
    gates (unknowns, lane_*) ran nowhere for days behind a named list."""
    jobs = _rust_jobs()
    assert jobs, "no 'Rust diffctx tests' job in ci.yml"
    for name, run in jobs.items():
        assert not _missing(run), f"{name} never runs {sorted(_missing(run))}"


def test_every_rust_suite_runs_in_the_pre_commit_hook():
    config = yaml.safe_load((PROJECT_ROOT / ".pre-commit-config.yaml").read_text(encoding="utf-8"))
    hooks = [h for repo in config["repos"] for h in repo["hooks"] if h["id"] == "cargo-test"]
    assert len(hooks) == 1
    assert not _missing(hooks[0]["entry"]), sorted(_missing(hooks[0]["entry"]))


@pytest.mark.skipif(
    sys.platform == "win32",
    reason="the stand-in cargo is a POSIX script on a POSIX PATH; the Windows Rust job runs the script itself",
)
def test_the_suites_script_reads_every_suite_off_the_tree(tmp_path):
    """The script's own enumeration, run against a stand-in cargo."""
    fake = tmp_path / "cargo"
    fake.write_text('#!/usr/bin/env bash\necho "$@" >> "$CARGO_LOG"\n', encoding="utf-8")
    fake.chmod(0o755)
    log = tmp_path / "log"
    subprocess.run(
        ["bash", str(PROJECT_ROOT / SUITES_SCRIPT)],
        check=True,
        env={"PATH": f"{tmp_path}:/usr/bin:/bin", "CARGO_LOG": str(log)},
    )
    ran = set(re.findall(r"--test (\w+)", log.read_text(encoding="utf-8")))
    assert ran == _suites()
