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


# Every call is logged; FAIL_ON makes the calls that contain it exit non-zero.
FAKE_CARGO = """#!{python}
import os, sys

with open(os.environ["CARGO_LOG"], "a") as log:
    log.write(" ".join(sys.argv[1:]) + "\\n")
sys.exit(1 if os.environ.get("FAIL_ON") and os.environ["FAIL_ON"] in " ".join(sys.argv[1:]) else 0)
"""


def _run_script(tmp_path: Path, **extra: str) -> tuple[subprocess.CompletedProcess, list[str]]:
    fake = tmp_path / "cargo"
    fake.write_text(FAKE_CARGO.format(python=sys.executable), encoding="utf-8")
    fake.chmod(0o755)
    log = tmp_path / "log"
    env = {
        "PATH": f"{tmp_path}:{Path(sys.executable).parent}:/usr/bin:/bin",
        "CARGO_LOG": str(log),
        **extra,
    }
    result = subprocess.run(["bash", str(PROJECT_ROOT / SUITES_SCRIPT)], env=env, capture_output=True, text=True, check=False)
    return result, log.read_text(encoding="utf-8").splitlines() if log.exists() else []


POSIX_ONLY = pytest.mark.skipif(
    sys.platform == "win32",
    reason="the stand-in cargo is a POSIX script on a POSIX PATH; the Windows Rust job runs the script itself",
)


@POSIX_ONLY
def test_the_suites_script_reads_every_suite_off_the_tree(tmp_path):
    """The script's own enumeration, run against a stand-in cargo."""
    result, calls = _run_script(tmp_path)
    assert result.returncode == 0, result.stderr
    assert set(re.findall(r"--test (\w+)", "\n".join(calls))) == _suites()


@POSIX_ONLY
@pytest.mark.parametrize("failing", ["--lib", "--test unknowns"])
def test_one_failing_run_does_not_hide_the_others_but_fails_the_script(tmp_path, failing):
    result, calls = _run_script(tmp_path, FAIL_ON=failing)
    assert result.returncode != 0
    assert any(failing in call for call in calls)
    assert any("--test yaml_cases" in call for call in calls), "the corpus must still run after a failure"
