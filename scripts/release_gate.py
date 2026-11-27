"""Judge the release gates of one commit against `eval/oracle/release.toml` and write `EVIDENCE_<sha>.md`.

Every gate reads evidence that records the full sha it was measured on and is refused when that is not `--sha`; a gate
with no evidence, a missing field or an unreadable verdict is "not measured", and only a measured value inside its
threshold is green. The exit status is 0 only when every row is green.

Run it through `scripts/release-gate.sh`.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import tempfile
import tomllib
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
RELEASE_TABLE = ROOT / "eval" / "oracle" / "release.toml"
sys.path.insert(0, str(ROOT / "eval" / "oracle"))
from gates import COMPARE, SUFFIX_OPERATORS, load_gates  # noqa: E402

GREEN, RED, UNMEASURED = "GREEN", "RED", "NOT MEASURED"
RESERVED = {"evidence", "rows_of", "budgets", "required", "repo", "branch"}
OPERATORS = {**{suffix: COMPARE[name] for suffix, name in SUFFIX_OPERATORS.items()}, "_is": lambda value, limit: value == limit}
OPERATOR_TEXT = {"_max": "<=", "_min": ">=", "_below": "<", "_is": "=="}
PROTECTION_SEAM = "RELEASE_GATE_PROTECTION_FILE"
ORACLE_PASS = "pass"


@dataclass(frozen=True)
class Row:
    gate: str
    check: str
    value: object
    limit: str
    verdict: str


class ConfigError(Exception):
    pass


def git_env() -> dict[str, str]:
    env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
    return env | {"GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_SYSTEM": os.devnull}


def git(repo: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(["git", "-C", str(repo), *args], capture_output=True, text=True, check=False, env=git_env())


def comparable(value: object, limit: object) -> bool:
    if isinstance(value, bool) or isinstance(limit, bool):
        return isinstance(value, bool) and isinstance(limit, bool)
    if isinstance(limit, int | float):
        return isinstance(value, int | float)
    return isinstance(value, str) and isinstance(limit, str)


def judge_field(gate: str, key: str, limit: object, data: dict, gates: dict) -> Row:
    if limit == "read":
        field, suffix = key, ""
    else:
        suffix = next((s for s in OPERATORS if key.endswith(s)), None)
        if suffix is None:
            raise ConfigError(f"[{gate}] {key} is no threshold (_max, _min, _below, _is, or = 'read')")
        field = key.removesuffix(suffix)
    if isinstance(limit, str) and limit.startswith("@"):
        if limit[1:] not in gates:
            raise ConfigError(f"[{gate}] {key} names {limit}, which gates.toml does not hold")
        limit = gates[limit[1:]].limit
    value = data.get(field)
    shown = "read" if not suffix else f"{OPERATOR_TEXT[suffix]} {limit}"
    if value is None or (suffix and not comparable(value, limit)):
        return Row(gate, field, "n/a" if value is None else value, shown, UNMEASURED)
    ok = not suffix or OPERATORS[suffix](value, limit)
    return Row(gate, field, value, shown, GREEN if ok else RED)


def judge_table(gate: str, table: dict, data: dict, gates: dict) -> list[Row]:
    return [judge_field(gate, key, limit, data, gates) for key, limit in table.items() if key not in RESERVED]


def read_evidence(gate: str, table: dict, evidence: Path, sha: str) -> tuple[dict | None, Row | None]:
    """The gate's evidence, or the one row that refuses it: missing, unreadable, unstamped or from another commit."""
    path = evidence / table["evidence"]
    if not path.is_file():
        return None, Row(gate, "evidence", "n/a", f"{path.name} recorded on {sha[:12]}", UNMEASURED)
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except ValueError as error:
        return None, Row(gate, "evidence", f"{path.name} unreadable: {error}", "valid JSON", RED)
    return stamped(gate, path.name, data, data.get("sha") if isinstance(data, dict) else None, sha)


def stamped(gate: str, name: str, data: dict | None, recorded: object, sha: str) -> tuple[dict | None, Row | None]:
    if recorded is None:
        return None, Row(gate, "sha", "n/a", f"{name} records {sha[:12]}", UNMEASURED)
    if recorded != sha:
        return None, Row(gate, "sha", str(recorded)[:12], f"== {sha[:12]} ({name} is another commit's)", RED)
    return data, None


def gate_rows(gate: str, table: dict, evidence: Path, sha: str, gates: dict) -> list[Row]:
    data, refused = read_evidence(gate, table, evidence, sha)
    return [refused] if refused else judge_table(gate, table, data or {}, gates)


def d5_rows(table: dict, repo: Path, sha: str, gates: dict) -> list[Row]:
    counts: dict[str, int] = {}
    unread: list[Row] = []
    for name, path in table["rows_of"].items():
        shown = git(repo, "show", f"{sha}:{path}")
        if shown.returncode:
            unread.append(Row("d5_lists", name, "n/a", f"{path} at {sha[:12]}", UNMEASURED))
        else:
            counts[name] = sum(1 for line in shown.stdout.splitlines() if line.strip() and line[0] != "#")
    return unread + judge_table("d5_lists", table, counts, gates)


def oracle_rows(table: dict, evidence: Path, sha: str) -> list[Row]:
    rows: list[Row] = []
    for budget in table["budgets"]:
        gate = f"scip_oracle[{budget}]"
        directory = evidence / f"oracle-{budget}"
        stamp, report = directory / "sha", directory / "report.json"
        if not stamp.is_file() or not report.is_file():
            rows.append(Row(gate, "evidence", "n/a", f"oracle-{budget}/sha and report.json at {sha[:12]}", UNMEASURED))
            continue
        _, refused = stamped(gate, f"oracle-{budget}", None, stamp.read_text("utf-8").strip(), sha)
        if refused:
            rows.append(refused)
            continue
        try:
            lines = json.loads(report.read_text("utf-8"))["verdict"]
        except (ValueError, KeyError) as error:
            rows.append(Row(gate, "report.json", f"unreadable: {error!r}", "a verdict list", RED))
            continue
        rows += [oracle_row(gate, name, [line for line in lines if line["gate"] == name]) for name in table["required"]]
        failing = sorted({line["gate"] for line in lines if line["status"] == "fail" and line["gate"] not in table["required"]})
        rows.append(Row(gate, "other gates", ", ".join(failing) or "none fail", "no line fails", RED if failing else GREEN))
    return rows


def oracle_row(gate: str, name: str, lines: list[dict]) -> Row:
    if not lines:
        return Row(gate, name, "n/a", "every line passes", UNMEASURED)
    failing = [line for line in lines if line["status"] == "fail"]
    passing = sum(line["status"] == ORACLE_PASS for line in lines)
    if failing:
        first = failing[0]
        return Row(gate, name, f"{first['source']}: {first['value']} ({len(failing)} failing)", first["limit"], RED)
    verdict = GREEN if passing == len(lines) else UNMEASURED
    return Row(gate, name, f"{passing}/{len(lines)} lines pass", lines[0]["limit"], verdict)


def binary_rows(binary: Path | None, sha: str) -> list[Row]:
    gate, limit = "binary", f"== {sha[:12]}"
    if binary is None:
        return [Row(gate, "provenance.engine.build", "n/a", limit, UNMEASURED)]
    with tempfile.TemporaryDirectory() as scratch:
        repo = Path(scratch)
        for step in (("init", "-q", "-b", "main"), ("config", "user.name", "gate"), ("config", "user.email", "gate@localhost")):
            git(repo, *step)
        for number in (1, 2):
            (repo / "a.py").write_text(f"def a():\n    return {number}\n", encoding="utf-8")
            git(repo, "add", "a.py")
            git(repo, "commit", "-q", "-m", f"c{number}")
        answer = subprocess.run(
            [str(binary), ".", "--diff", "HEAD~1..HEAD", "-f", "json", "--no-content"],
            cwd=repo,
            capture_output=True,
            text=True,
            check=False,
            timeout=300,
            env=git_env(),
        )
    if answer.returncode:
        return [Row(gate, "provenance.engine.build", f"binary exited {answer.returncode}", limit, RED)]
    try:
        built = json.loads(answer.stdout)["provenance"]["engine"].get("build")
    except (ValueError, KeyError, TypeError):
        return [Row(gate, "provenance.engine.build", "no provenance in the answer", limit, RED)]
    if built is None:
        return [Row(gate, "provenance.engine.build", "unstamped (a +source build names no commit)", limit, RED)]
    return [Row(gate, "provenance.engine.build", str(built)[:12], limit, GREEN if built == sha else RED)]


def protection_text(table: dict) -> tuple[str | None, str]:
    seam = os.environ.get(PROTECTION_SEAM)
    if seam:
        return Path(seam).read_text(encoding="utf-8"), f"read from {seam}"
    endpoint = f"repos/{table['repo']}/branches/{table['branch']}/protection"
    done = subprocess.run(["gh", "api", endpoint], capture_output=True, text=True, check=False)
    return (
        (done.stdout, endpoint)
        if done.returncode == 0
        else (None, done.stderr.strip().splitlines()[0] if done.stderr else "gh failed")
    )


def protection_rows(table: dict, gates: dict) -> tuple[list[Row], list[str]]:
    gate = "branch_protection"
    text, source = protection_text(table)
    if text is None:
        return [Row(gate, "protection", source, "readable with gh", UNMEASURED)], []
    try:
        data = json.loads(text)
        enforced = data["enforce_admins"]["enabled"]
    except (ValueError, KeyError, TypeError):
        return [Row(gate, "enforce_admins", "no enforce_admins.enabled in the answer", "readable", UNMEASURED)], []
    checks = data.get("required_status_checks") or {}
    required = sorted({*checks.get("contexts", []), *(c["context"] for c in checks.get("checks", []))})
    return judge_table(gate, table, {"enforce_admins": enforced}, gates), required


def collect(table: dict, repo: Path, evidence: Path, sha: str, binary: Path | None) -> tuple[list[Row], list[str]]:
    gates = load_gates()
    rows: list[Row] = []
    for gate, spec in table.items():
        if gate == "d5_lists":
            rows += d5_rows(spec, repo, sha, gates)
        elif gate == "scip_oracle":
            rows += oracle_rows(spec, evidence, sha)
        elif gate == "branch_protection":
            continue
        else:
            rows += gate_rows(gate, spec, evidence, sha, gates)
    rows += binary_rows(binary, sha)
    protection, required = protection_rows(table["branch_protection"], gates)
    return rows + protection, required


def cell(value: object) -> str:
    return str(value).replace("|", "\\|").replace("\n", " ")


def render(sha: str, rows: list[Row], required: list[str]) -> str:
    verdicts = [row.verdict for row in rows]
    summary = (
        "GREEN: every gate measured on this commit and inside its threshold."
        if verdicts and set(verdicts) == {GREEN}
        else f"NOT GREEN: {verdicts.count(RED)} red, {verdicts.count(UNMEASURED)} not measured. A gate not measured is not a pass."
    )
    lines = [
        f"# Release gates for {sha}",
        "",
        summary,
        "",
        "| Gate | Check | Value | Limit | Verdict |",
        "| --- | --- | --- | --- | --- |",
    ]
    lines += [f"| {cell(r.gate)} | {cell(r.check)} | {cell(r.value)} | {cell(r.limit)} | {r.verdict} |" for r in rows]
    lines += ["", f"Required checks on the protected branch: {', '.join(required) if required else 'none listed'}.", ""]
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="release-gate.sh", description=__doc__)
    parser.add_argument("--sha", required=True, help="the commit every gate is measured on")
    parser.add_argument("--evidence", type=Path, help="the evidence directory (default: gate-evidence/<sha>)")
    parser.add_argument("--binary", type=Path, help="the diffctx binary under test; its provenance.engine.build must be --sha")
    parser.add_argument("--repo", type=Path, default=ROOT, help="the repository the sha belongs to")
    parser.add_argument("--out", type=Path, help="where EVIDENCE_<sha>.md goes (default: the evidence directory)")
    options = parser.parse_args(argv)
    resolved = git(options.repo, "rev-parse", "--verify", "--quiet", f"{options.sha}^{{commit}}")
    if resolved.returncode:
        print(f"release-gate: {options.sha} is no commit of {options.repo}", file=sys.stderr)
        return 2
    sha = resolved.stdout.strip()
    evidence = options.evidence or Path("gate-evidence") / sha
    try:
        table = tomllib.loads(RELEASE_TABLE.read_text(encoding="utf-8"))
        rows, required = collect(table, options.repo, evidence, sha, options.binary)
    except ConfigError as error:
        print(f"release-gate: {RELEASE_TABLE}: {error}", file=sys.stderr)
        return 2
    report = render(sha, rows, required)
    out = options.out or evidence
    out.mkdir(parents=True, exist_ok=True)
    (out / f"EVIDENCE_{sha}.md").write_text(report, encoding="utf-8")
    print(report)
    return 0 if rows and all(row.verdict == GREEN for row in rows) else 1


if __name__ == "__main__":
    raise SystemExit(main())
