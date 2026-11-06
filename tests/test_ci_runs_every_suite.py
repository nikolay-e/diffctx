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


def _rust_jobs() -> list[dict]:
    ci = yaml.safe_load((PROJECT_ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8"))
    return [job for job in ci["jobs"].values() if job.get("name", "").startswith("Rust diffctx tests")]


def _cells(job: dict) -> list[dict[str, str]]:
    matrix = job.get("strategy", {}).get("matrix", {})
    if "include" in matrix:
        return [dict(cell) for cell in matrix["include"]]
    if "os" in matrix:
        return [{"os": os_name} for os_name in matrix["os"]]
    return [{"os": job["runs-on"]}]


def _expanded(text: object, cell: dict[str, str]) -> str:
    return re.sub(r"\$\{\{\s*matrix\.(\w+)\s*\}\}", lambda m: str(cell.get(m.group(1), "")), str(text))


def _part_suites(part: str) -> set[str]:
    corpus = "yaml_cases"
    rest = sorted(_suites() - {corpus})
    if part == "all":
        return _suites()
    if part == "corpus":
        return {corpus}
    if part == "rest":
        return set(rest)
    shard, shards = map(int, part.removeprefix("rest:").split("/"))
    return set(rest[shard - 1 :: shards])


def _covered(job: dict, cell: dict[str, str]) -> set[str]:
    """The suites one cell of a Rust job runs: named with `--test`, or by the
    suites script, whose argument is the part (read from the step's env when
    the command names a variable)."""
    covered: set[str] = set()
    for step in job["steps"]:
        run = step.get("run", "")
        covered |= set(re.findall(r"--test\s+(\w+)", run))
        if SUITES_SCRIPT in run:
            argument = re.search(r'cargo-test-suites\.sh(?:\s+"?([^"\s]+)"?)?', run).group(1) or "all"
            if argument.startswith("$"):
                argument = _expanded(step.get("env", {}).get(argument[1:], "all"), cell)
            covered |= _part_suites(argument)
    return covered


def test_every_rust_suite_runs_on_each_ci_os():
    """A suite under tests/ that no job runs guards nothing: the 1.20.0
    gates (unknowns, lane_*) ran nowhere for days behind a named list. A
    job may run a part of them (Windows is too slow for one job), so the
    parts of one OS together must hold every suite."""
    jobs = _rust_jobs()
    assert jobs, "no 'Rust diffctx tests' job in ci.yml"
    covered: dict[str, set[str]] = {}
    for job in jobs:
        for cell in _cells(job):
            covered.setdefault(_expanded(job["runs-on"], cell), set()).update(_covered(job, cell))
    for os_name, suites in covered.items():
        assert suites == _suites(), f"{os_name} never runs {sorted(_suites() - suites)}"


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


def _run_script(tmp_path: Path, args: tuple[str, ...] = (), **extra: str) -> tuple[subprocess.CompletedProcess, list[str]]:
    fake = tmp_path / "cargo"
    fake.write_text(FAKE_CARGO.format(python=sys.executable), encoding="utf-8")
    fake.chmod(0o755)
    log = tmp_path / "log"
    env = {
        "PATH": f"{tmp_path}:{Path(sys.executable).parent}:/usr/bin:/bin",
        "CARGO_LOG": str(log),
        **extra,
    }
    result = subprocess.run(
        ["bash", str(PROJECT_ROOT / SUITES_SCRIPT), *args], env=env, capture_output=True, text=True, check=False
    )
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
def test_the_parts_of_the_suites_script_run_each_suite_once(tmp_path):
    """Windows runs the script in parts: together they are the whole, and
    nothing runs twice."""
    parts = ["rest:1/2", "rest:2/2", "corpus"]
    calls = []
    for i, part in enumerate(parts):
        workdir = tmp_path / str(i)
        workdir.mkdir()
        result, run_calls = _run_script(workdir, args=(part,))
        assert result.returncode == 0, result.stderr
        calls += run_calls
    names = re.findall(r"--test (\w+)", "\n".join(calls))
    assert sorted(names) == sorted(_suites()), "a suite ran twice or not at all"
    assert sum("--lib" in call for call in calls) == 1


@POSIX_ONLY
def test_an_unknown_part_is_refused(tmp_path):
    result, calls = _run_script(tmp_path, args=("everything",))
    assert result.returncode == 2
    assert calls == []


@POSIX_ONLY
@pytest.mark.parametrize("failing", ["--lib", "--test unknowns"])
def test_one_failing_run_does_not_hide_the_others_but_fails_the_script(tmp_path, failing):
    result, calls = _run_script(tmp_path, FAIL_ON=failing)
    assert result.returncode != 0
    assert any(failing in call for call in calls)
    assert any("--test yaml_cases" in call for call in calls), "the corpus must still run after a failure"
