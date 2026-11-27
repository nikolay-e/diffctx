import json
import re
import subprocess
import sys
from pathlib import Path

import pytest
import yaml

PROJECT_ROOT = Path(__file__).parent.parent
SUITES_SCRIPT = "scripts/cargo-test-suites.sh"
CLASS_CORPUS = "class_completeness_corpus"


def _suites() -> set[str]:
    return {p.stem for p in (PROJECT_ROOT / "crates/diffctx-native/tests").glob("*.rs")}


def _missing(command: str) -> set[str]:
    if SUITES_SCRIPT in command:
        return set()
    return _suites() - set(re.findall(r"--test\s+(\w+)", command))


def _rust_jobs() -> list[dict]:
    ci = yaml.safe_load((PROJECT_ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8"))
    return [job for job in ci["jobs"].values() if job.get("name", "").startswith("Rust diffctx tests")]


PLATFORM_SUITES = ("snapshot_", "hook")
PLATFORM_NAMES = {"orphan_children", "reaping", "git_faults"}


def _platform_suites() -> set[str]:
    return {s for s in _suites() if s.startswith(PLATFORM_SUITES) or s in PLATFORM_NAMES}


def _cells(job: dict, push: bool = False) -> list[dict[str, str]]:
    """GitHub's expansion: the product of the vectors less every cell an exclude
    entry agrees with, then each include entry extends every product cell it
    agrees with, or becomes a cell of its own. A cell an include made is no
    product cell: two includes with the same vector values are two cells."""
    matrix = dict(job.get("strategy", {}).get("matrix", {}))
    if isinstance(matrix.get("include"), str):
        # `fromJSON(push && '[head cells]' || '[every cell]')`: the shape of the event picks one.
        literals = re.findall(r"'(\[.*?\])'", matrix["include"], re.S)
        matrix["include"] = json.loads(literals[0] if push else literals[-1])
    vectors = {k: v for k, v in matrix.items() if k not in ("include", "exclude")}
    cells: list[dict[str, str]] = [{}]
    for key, values in vectors.items():
        cells = [{**cell, key: value} for cell in cells for value in values]
    excluded = matrix.get("exclude", [])
    cells = [c for c in cells if not any(all(c.get(k) == v for k, v in out.items()) for out in excluded)]
    if not vectors:
        cells = []
    product = list(cells)
    for extra in matrix.get("include", []):
        matching = [c for c in product if vectors and all(c[k] == v for k, v in extra.items() if k in vectors)]
        for cell in matching:
            cell.update(extra)
        if not matching:
            cells.append(dict(extra))
    return cells or [{"os": job["runs-on"]}]


def _expanded(text: object, cell: dict[str, str]) -> str:
    return re.sub(r"\$\{\{\s*matrix\.(\w+)\s*\}\}", lambda m: str(cell.get(m.group(1), "")), str(text))


def _part_suites(part: str) -> set[str]:
    corpus = ["class_completeness_corpus", "yaml_cases"]
    if part == "all":
        return _suites()
    if part == "platform":
        return _platform_suites()
    kind, _, shard = part.partition(":")
    names = corpus if kind == "corpus" else sorted(_suites() - set(corpus))
    if not shard:
        return set(names)
    k, n = map(int, shard.split("/"))
    return set(names[k - 1 :: n])


def _covered(job: dict, cell: dict[str, str]) -> set[str]:
    """The suites one cell of a Rust job runs: named with `--test`, or by the
    suites script, whose argument is the part (read from the step's env when
    the command names a variable). A class corpus split by case is the piece
    `class_completeness_corpus K/N`."""
    covered: set[str] = set()
    for step in job["steps"]:
        run = step.get("run", "")
        covered |= set(re.findall(r"--test\s+(\w+)", run))
        if SUITES_SCRIPT in run:
            argument = re.search(r'cargo-test-suites\.sh(?:\s+"?([^"\s]+)"?)?', run).group(1) or "all"
            if argument.startswith("$"):
                argument = _expanded(step.get("env", {}).get(argument[1:], "all"), cell)
            suites = _part_suites(argument)
            shard = _expanded(step.get("env", {}).get("DIFFCTX_CLASS_SHARD", ""), cell)
            if shard and CLASS_CORPUS in suites:
                suites = suites - {CLASS_CORPUS} | {f"{CLASS_CORPUS} {shard}"}
            covered |= suites
    return covered


def _joined(suites: set[str]) -> set[str]:
    """The pieces of the class corpus are the suite when they are every K of one N."""
    pieces = {s for s in suites if s.startswith(f"{CLASS_CORPUS} ")}
    parts = {p.partition(" ")[2] for p in pieces}
    whole = {f"{k}/{n}" for n in {p.split("/")[1] for p in parts} for k in range(1, int(n) + 1)}
    return suites - pieces | ({CLASS_CORPUS} if parts and parts == whole else set())


def _covered_by_os(push: bool) -> dict[str, set[str]]:
    covered: dict[str, set[str]] = {}
    for job in _rust_jobs():
        for cell in _cells(job, push):
            covered.setdefault(_expanded(job["runs-on"], cell), set()).update(_covered(job, cell))
    return {os_name: _joined(suites) for os_name, suites in covered.items()}


def test_every_rust_suite_runs_on_linux_and_windows():
    """A suite under tests/ that no job runs guards nothing: the 1.20.0
    gates (unknowns, lane_*) ran nowhere for days behind a named list. A
    job may run a part of them (Windows is too slow for one job), so the
    parts of one OS together must hold every suite."""
    assert _rust_jobs(), "no 'Rust diffctx tests' job in ci.yml"
    covered = _covered_by_os(push=False)
    for os_name in ("ubuntu-latest", "windows-latest"):
        assert covered[os_name] == _suites(), f"{os_name} never runs {sorted(_suites() - covered[os_name])}"


def test_macos_runs_the_platform_suites_and_the_corpora_in_a_pull_request_into_main():
    """macOS has five slots: it runs what the operating system decides, and the
    two corpora when a PR into main or the nightly asks for the full set."""
    covered = _covered_by_os(push=False)["macos-latest"]
    assert covered == _platform_suites() | {CLASS_CORPUS, "yaml_cases"}, sorted(covered)


def test_a_push_runs_one_macos_rust_job_of_the_platform_suites():
    cells = [cell for job in _rust_jobs() for cell in _cells(job, push=True) if "macos" in _expanded(job["runs-on"], cell)]
    assert len(cells) == 1, cells
    assert _covered_by_os(push=True)["macos-latest"] == _platform_suites()


def test_the_intel_mac_cross_build_runs_in_exactly_one_macos_part_of_each_shape():
    cross = [job for job in _rust_jobs() if any("x86_64-apple-darwin" in step.get("run", "") for step in job["steps"])]
    assert len(cross) == 1
    step = next(step for step in cross[0]["steps"] if "x86_64-apple-darwin" in step.get("run", ""))
    for push in (False, True):
        ran = [
            cell
            for cell in _cells(cross[0], push)
            if "matrix.part" not in step["if"] or f"matrix.part == '{cell['part']}'" in step["if"]
        ]
        assert len(ran) == 1, (push, ran)


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
@pytest.mark.parametrize(
    "parts",
    [
        ["rest:1/2", "rest:2/2", "corpus"],
        ["rest:1/2", "rest:2/2", "corpus:1/2", "corpus:2/2"],
        ["rest:1/3", "rest:2/3", "rest:3/3", "corpus:1/2", "corpus:2/2"],
        ["rest:1/4", "rest:2/4", "rest:3/4", "rest:4/4", "corpus:1/2", "corpus:2/2"],
    ],
)
@pytest.mark.parametrize("runner_os", ["Linux", "macOS", "Windows"])
def test_the_parts_of_the_suites_script_run_each_suite_once(tmp_path, parts, runner_os):
    """CI runs the script in parts: together they are the whole, and nothing
    runs twice, on the weights of each OS."""
    calls = []
    for i, part in enumerate(parts):
        workdir = tmp_path / str(i)
        workdir.mkdir()
        result, run_calls = _run_script(workdir, args=(part,), RUNNER_OS=runner_os)
        assert result.returncode == 0, result.stderr
        calls += run_calls
    names = re.findall(r"--test (\w+)", "\n".join(calls))
    assert sorted(names) == sorted(_suites()), "a suite ran twice or not at all"
    assert sum("--lib" in call for call in calls) == 1


@POSIX_ONLY
def test_the_platform_part_runs_the_platform_suites_and_nothing_else(tmp_path):
    result, calls = _run_script(tmp_path, args=("platform",))
    assert result.returncode == 0, result.stderr
    assert set(re.findall(r"--test (\w+)", "\n".join(calls))) == _platform_suites()
    assert not any("--lib" in call or "--package" in call for call in calls)


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
