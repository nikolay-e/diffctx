"""The release gate goes red on a known-bad input at every gate, and green only on one all-green input.

Each test runs the real `scripts/release-gate.sh` over a real git repository and an evidence directory it built, and reads the
table the script prints. The one seam is the branch-protection answer, a file named by RELEASE_GATE_PROTECTION_FILE instead of
the `gh` call; the diffctx binary under test is a stand-in executable that prints the provenance a real one would.
"""

import json
import os
import subprocess
import sys
import tomllib
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent
pytestmark = pytest.mark.skipif(os.name == "nt", reason="the gate is a POSIX bash script")
SCRIPT = ROOT / "scripts" / "release-gate.sh"
TABLE = tomllib.loads((ROOT / "eval" / "oracle" / "release.toml").read_text(encoding="utf-8"))
RESERVED = {"evidence", "rows_of", "budgets", "required", "repo", "branch"}
EVIDENCE_GATES = [name for name, spec in TABLE.items() if "evidence" in spec]
OTHER = "d" * 40
RELEASE_SEAM = "RELEASE_GATE_PROTECTION_FILE"
SUFFIXES = ("_max", "_min", "_below", "_is")

GREEN_EVIDENCE = {
    "ci": {"jobs": 38, "failed": 0, "unfinished": 0},
    "identity_b": {"rc": 0, "passed": 17, "failed": 0, "violations": 0, "remainder": 0},
    "mutations": {"rc": 0, "killed": 11, "survived": 0, "occurrence_survivors": 0, "decide_survivors": 0},
    "ext": {"judged": 12, "unresolved_false_absences": 0},
    "dyn": {"probes": 207, "false_absence": 0, "false_complete": 0, "hedged": 197},
    "hedge": {"cases": 900, "regressions": 0, "share_delta": 0.04},
    "ac5": {"found": 34, "noise_share": 0.5},
    "git_processes": {"rc": 0, "impact": 26, "pack": 15, "locate": 15, "symbol": 12},
    "cpu_vs_1_20_0": {"locate": 1.05, "pack": 1.05, "impact": 1.9, "symbol": 1.8},
    "determinism": {"cells": 30, "runs": 5, "mismatched": 0, "symbol_spread": 0.003},
    "p95": {
        "impact_warm_seconds": 4.9,
        "symbol_warm_seconds": 3.0,
        "hook_in_budget": True,
        "hook_partial_deadline_repeats": 0,
        "impact_cold_seconds": 9.0,
        "symbol_cold_seconds": 6.0,
    },
    "budget": {"rc": 0, "dormant_paths": 0},
    "black_box": {"clean_rounds": 2, "false_absences": 0, "leaks": 0, "delivery_breaks": 0},
    "verifier": {"verdict": "PASS"},
}
PROTECTION = {
    "enforce_admins": {"enabled": False},
    "required_status_checks": {"contexts": ["Lint & Type Check"], "checks": [{"context": "Rust diffctx tests"}]},
}


def verdict_lines(**failing: int) -> list[dict]:
    """The `verdict` of a report.json: the gates the table requires, each passing unless `failing` gives it a value."""
    lines = []
    for name in TABLE["scip_oracle"]["required"]:
        value = failing.get(name.split(".")[1], 0)
        status = "fail" if value else "pass"
        lines.append({"gate": name, "source": "oracle cells", "status": status, "value": value, "limit": "<= 0", "detail": ""})
    return lines


def git(repo: Path, *args: str) -> str:
    env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")} | {"GIT_CONFIG_GLOBAL": os.devnull}
    done = subprocess.run(["git", "-C", str(repo), *args], capture_output=True, text=True, check=True, env=env)
    return done.stdout.strip()


def commit_lists(repo: Path, **rows: str) -> str:
    """One commit holding both D5 list files: a header comment and the given rows (by list name)."""
    for name, path in TABLE["d5_lists"]["rows_of"].items():
        file = repo / path
        file.parent.mkdir(parents=True, exist_ok=True)
        file.write_text("# a comment\n\n" + rows.get(name, ""), encoding="utf-8")
        git(repo, "add", path)
    git(repo, "-c", "user.name=t", "-c", "user.email=t@localhost", "commit", "-q", "-m", "lists")
    return git(repo, "rev-parse", "HEAD")


class Setup:
    def __init__(self, root: Path) -> None:
        self.repo = root / "repo"
        self.repo.mkdir()
        git(self.repo, "init", "-q", "-b", "main")
        self.sha = commit_lists(self.repo)
        self.evidence = root / "evidence"
        self.evidence.mkdir()
        self.protection = root / "protection.json"
        self.protection.write_text(json.dumps(PROTECTION), encoding="utf-8")
        self.binary = self.stand_in_binary(root, self.sha)
        for name, fields in GREEN_EVIDENCE.items():
            self.write(name, fields)
        for budget in TABLE["scip_oracle"]["budgets"]:
            self.write_oracle(budget, verdict_lines())
        self.environment = {"RELEASE_GATE_PYTHON": sys.executable, RELEASE_SEAM: str(self.protection)}

    def write(self, gate: str, fields: dict, sha: str | None = None) -> None:
        path = self.evidence / TABLE[gate]["evidence"]
        path.write_text(json.dumps({"sha": sha or self.sha, **fields}), encoding="utf-8")

    def write_oracle(self, budget: str, verdict: list[dict], sha: str | None = None) -> None:
        directory = self.evidence / f"oracle-{budget}"
        directory.mkdir(exist_ok=True)
        (directory / "sha").write_text((sha or self.sha) + "\n", encoding="utf-8")
        (directory / "report.json").write_text(json.dumps({"verdict": verdict}), encoding="utf-8")

    def stand_in_binary(self, root: Path, build: str | None, name: str = "diffctx") -> Path:
        engine = {"name": "diffctx", "version": "2.0.0", **({"build": build} if build else {})}
        path = root / name
        path.write_text(f"#!/bin/sh\ncat <<'JSON'\n{json.dumps({'provenance': {'engine': engine}})}\nJSON\n", encoding="utf-8")
        path.chmod(0o755)
        return path

    def run(
        self, sha: str | None = None, binary: Path | None = None, with_binary: bool = True
    ) -> tuple[int, str, list[tuple[str, ...]]]:
        command = [str(SCRIPT), "--sha", sha or self.sha, "--evidence", str(self.evidence), "--repo", str(self.repo)]
        if with_binary:
            command += ["--binary", str(binary or self.binary)]
        done = subprocess.run(command, capture_output=True, text=True, check=False, env={**os.environ, **self.environment})
        lines = [line for line in done.stdout.splitlines() if line.startswith("| ")]
        return done.returncode, done.stdout, [tuple(c.strip() for c in line.strip("| ").split(" | ")) for line in lines[2:]]


@pytest.fixture
def gate(tmp_path: Path) -> Setup:
    return Setup(tmp_path)


def verdicts(rows: list[tuple[str, ...]], gate: str) -> set[str]:
    return {row[4] for row in rows if row[0] == gate or row[0].startswith(f"{gate}[")}


def assert_red(result: tuple[int, str, list[tuple[str, ...]]], gate: str, check: str | None = None) -> None:
    code, out, rows = result
    assert code == 1, out
    assert any(
        (row[0] == gate or row[0].startswith(f"{gate}[")) and row[4] == "RED" and (check is None or row[1] == check)
        for row in rows
    ), out


def test_all_green_input_passes_and_writes_the_evidence_file(gate: Setup) -> None:
    code, out, rows = gate.run()
    assert code == 0, out
    assert {row[4] for row in rows} == {"GREEN"}
    assert "Lint & Type Check" in out and "Rust diffctx tests" in out
    assert (gate.evidence / f"EVIDENCE_{gate.sha}.md").read_text(encoding="utf-8").startswith(f"# Release gates for {gate.sha}")


def test_an_abbreviated_sha_is_resolved_to_the_commit(gate: Setup) -> None:
    code, out, _ = gate.run(sha=gate.sha[:9])
    assert code == 0, out


def test_a_sha_that_is_no_commit_is_refused(gate: Setup) -> None:
    code, _, _ = gate.run(sha=OTHER)
    assert code == 2


@pytest.mark.parametrize("budget", TABLE["scip_oracle"]["budgets"])
@pytest.mark.parametrize("name", ["not_ours_by_place", "false_fold", "incomplete_complete"])
def test_one_false_absence_fold_or_complete_is_red_at_every_budget(gate: Setup, budget: str, name: str) -> None:
    gate.write_oracle(budget, verdict_lines(**{name: 1}))
    assert_red(gate.run(), f"scip_oracle[{budget}]", f"safety.{name}")


def test_an_oracle_line_without_the_pass_status_is_not_measured(gate: Setup) -> None:
    lines = verdict_lines()
    lines[0]["status"] = "not decided: no fresh mark"
    gate.write_oracle("uncapped", lines)
    code, out, rows = gate.run()
    assert code == 1 and "NOT MEASURED" in verdicts(rows, "scip_oracle"), out


def test_a_failing_line_of_an_unrequired_oracle_gate_is_red(gate: Setup) -> None:
    gate.write_oracle("default", [*verdict_lines(), {"gate": "ac5.noise", "source": "head", "status": "fail", "value": 0.95}])
    assert_red(gate.run(), "scip_oracle[default]", "other gates")


@pytest.mark.parametrize("field", ["impact", "pack", "locate", "symbol"])
def test_git_processes_one_over_the_ceiling_is_red(gate: Setup, field: str) -> None:
    ceiling = TABLE["git_processes"][f"{field}_max"]
    gate.write("git_processes", {**GREEN_EVIDENCE["git_processes"], field: ceiling + 1})
    assert_red(gate.run(), "git_processes", field)


@pytest.mark.parametrize("field", ["locate", "pack"])
def test_a_cpu_ratio_over_its_ceiling_is_red(gate: Setup, field: str) -> None:
    gate.write("cpu_vs_1_20_0", {**GREEN_EVIDENCE["cpu_vs_1_20_0"], field: TABLE["cpu_vs_1_20_0"][f"{field}_max"] + 0.01})
    assert_red(gate.run(), "cpu_vs_1_20_0", field)


@pytest.mark.parametrize("name", EVIDENCE_GATES)
def test_evidence_of_another_commit_is_refused_at_every_gate(gate: Setup, name: str) -> None:
    gate.write(name, GREEN_EVIDENCE[name], sha=OTHER)
    assert_red(gate.run(), name, "sha")


@pytest.mark.parametrize("budget", TABLE["scip_oracle"]["budgets"])
def test_an_oracle_run_of_another_commit_is_refused(gate: Setup, budget: str) -> None:
    gate.write_oracle(budget, verdict_lines(), sha=OTHER)
    assert_red(gate.run(), f"scip_oracle[{budget}]", "sha")


def test_a_binary_built_from_another_commit_is_refused(gate: Setup) -> None:
    assert_red(gate.run(binary=gate.stand_in_binary(gate.evidence.parent, OTHER, "other")), "binary")


def test_a_binary_without_a_build_stamp_is_refused(gate: Setup) -> None:
    assert_red(gate.run(binary=gate.stand_in_binary(gate.evidence.parent, None, "source")), "binary")


@pytest.mark.parametrize("field", ["occurrence_survivors", "decide_survivors", "survived"])
def test_a_surviving_mutant_is_red(gate: Setup, field: str) -> None:
    gate.write("mutations", {**GREEN_EVIDENCE["mutations"], field: 1})
    assert_red(gate.run(), "mutations", field)


def test_fewer_killed_mutants_than_the_table_demands_is_red(gate: Setup) -> None:
    gate.write("mutations", {**GREEN_EVIDENCE["mutations"], "killed": TABLE["mutations"]["killed_min"] - 1})
    assert_red(gate.run(), "mutations", "killed")


@pytest.mark.parametrize("name", [n for n in EVIDENCE_GATES if "rc_is" in TABLE[n]])
def test_a_suite_with_a_nonzero_rc_is_red_whatever_else_it_says(gate: Setup, name: str) -> None:
    gate.write(name, {**GREEN_EVIDENCE[name], "rc": 101})
    assert_red(gate.run(), name, "rc")


def test_a_gate_with_no_evidence_file_is_not_measured_and_fails_the_run(gate: Setup) -> None:
    for name in EVIDENCE_GATES:
        (gate.evidence / TABLE[name]["evidence"]).unlink()
    code, out, rows = gate.run()
    assert code == 1, out
    for name in EVIDENCE_GATES:
        assert verdicts(rows, name) == {"NOT MEASURED"}, name


def test_a_missing_oracle_run_or_binary_or_protection_is_not_measured(gate: Setup) -> None:
    for budget in TABLE["scip_oracle"]["budgets"]:
        (gate.evidence / f"oracle-{budget}" / "report.json").unlink()
    gate.protection.write_text("{}", encoding="utf-8")
    code, out, rows = gate.run(with_binary=False)
    assert code == 1, out
    for name in ("scip_oracle", "binary", "branch_protection"):
        assert verdicts(rows, name) == {"NOT MEASURED"}, name


def field_of(key: str) -> str:
    return next((key.removesuffix(s) for s in SUFFIXES if key.endswith(s)), key)


CHECKED_FIELDS = [(name, field_of(key)) for name in GREEN_EVIDENCE for key in TABLE[name] if key not in RESERVED]


@pytest.mark.parametrize(("name", "field"), [(n, f) for n, f in CHECKED_FIELDS if f in GREEN_EVIDENCE[n]])
def test_a_field_the_evidence_lacks_is_not_measured_not_a_pass(gate: Setup, name: str, field: str) -> None:
    fields = {k: v for k, v in GREEN_EVIDENCE[name].items() if k != field}
    gate.write(name, fields)
    code, out, rows = gate.run()
    assert code == 1, out
    assert any(row[0] == name and row[1] == field and row[4] == "NOT MEASURED" for row in rows), out


def test_a_vacuous_run_with_no_probes_is_not_a_pass(gate: Setup) -> None:
    gate.write("dyn", {**GREEN_EVIDENCE["dyn"], "probes": 0})
    assert_red(gate.run(), "dyn", "probes")


def test_admins_bound_by_the_protection_is_red_and_the_checks_are_still_listed(gate: Setup) -> None:
    gate.protection.write_text(json.dumps({**PROTECTION, "enforce_admins": {"enabled": True}}), encoding="utf-8")
    result = gate.run()
    assert_red(result, "branch_protection", "enforce_admins")
    assert "Lint & Type Check, Rust diffctx tests" in result[1]


@pytest.mark.parametrize("name", ["known_red", "known_false_hedges"])
def test_a_row_in_a_d5_list_at_the_sha_is_red(gate: Setup, name: str) -> None:
    sha = commit_lists(gate.repo, **{name: "some/case  # a defect\n"})
    for gate_name, fields in GREEN_EVIDENCE.items():
        gate.write(gate_name, fields, sha=sha)
    for budget in TABLE["scip_oracle"]["budgets"]:
        gate.write_oracle(budget, verdict_lines(), sha=sha)
    gate.binary = gate.stand_in_binary(gate.evidence.parent, sha)
    assert_red(gate.run(sha=sha), "d5_lists", name)
