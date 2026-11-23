#!/usr/bin/env python3
"""Phase E 11-mutation generator and GitHub Actions runner for diffctx 2.0.0.

Covers the Phase E exit gate:
  - m3, m7 (2)
  - three of Phase 1 (3)
  - five of Phase A (5)
  - one of Phase B (1)

Safety invariant: never invokes cargo locally unless running on GitHub Actions
(GITHUB_ACTIONS=true) or explicitly passed --allow-cargo-in-ci.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import subprocess
import sys
from dataclasses import asdict, dataclass


@dataclass(frozen=True)
class Mutant:
    id: str
    group: str
    crate: str
    rel_path: str
    line_hint: int
    description: str
    target: str
    replacement: str
    gate_cmd: list[str]
    expected_red: str
    static_guard: str


MUTANTS: tuple[Mutant, ...] = (
    Mutant(
        id="m3",
        group="core",
        crate="diffctx-snapshot",
        rel_path="crates/diffctx-snapshot/src/snapshot/reader.rs",
        line_hint=344,
        description="SnapshotReader::read swallows a failed blob fetch as Ok(None)",
        target="            Decision::Admit | Decision::NotAnalysed(NotAnalysedReason::Symlink) => self.fetch(row),",
        replacement="            Decision::Admit | Decision::NotAnalysed(NotAnalysedReason::Symlink) => self.fetch(row).or(Ok(None)),",
        gate_cmd=[
            "cargo",
            "test",
            "--release",
            "--locked",
            "-p",
            "diffctx",
            "--test",
            "snapshot_faults",
        ],
        expected_red=(
            "a_blob_the_tree_lists_but_the_repository_lost_is_an_error_not_an_absent_file, "
            "an_injected_fault_fails_exactly_the_read_it_names_and_the_loss_is_recorded"
        ),
        static_guard=(
            "clippy::disallowed_methods in crates/diffctx-snapshot/clippy.toml also rejects "
            "Result::ok / Result::unwrap_or_default at compile time"
        ),
    ),
    Mutant(
        id="m7",
        group="core",
        crate="diffctx-answer",
        rel_path="crates/diffctx-answer/src/decide/cell/search.rs",
        line_hint=153,
        description="Cell::coverage drops base_gap (failed base read / BaseUnresolved) on removed symbol",
        target="        if let Some(gap) = self.base_gap(changed).filter(|gap| !gaps.contains(gap)) {",
        replacement="        if let Some(gap) = self.base_gap(changed).filter(|_gap| false) {",
        gate_cmd=[
            "cargo",
            "test",
            "--release",
            "--locked",
            "-p",
            "diffctx",
            "--test",
            "git_faults",
            "--",
            "every_blob_read_failed_in_turn",
        ],
        expected_red="every_blob_read_failed_in_turn (base-side read_failed sweep on removed_method)",
        static_guard="None (purely behavioral; base_gap is still called so dead_code stays silent)",
    ),
    Mutant(
        id="m-p1-loss-bound",
        group="phase-1",
        crate="diffctx-occurrence",
        rel_path="crates/diffctx-occurrence/src/tokens/occurrence/debug.rs",
        line_hint=323,
        description="Shadow::account_name treats a file with a recorded Loss as lossless when readable",
        target="            let Some((facts, text)) = self.read(&file).filter(|_| lost.is_none()) else {",
        replacement="            let Some((facts, text)) = self.read(&file) else {",
        gate_cmd=[
            "cargo",
            "test",
            "--release",
            "--locked",
            "-p",
            "diffctx",
            "--test",
            "loss_sweep",
        ],
        expected_red="loss_sweep across language families (missing Hedge::Unread / Gap::Unaccounted)",
        static_guard="None (lost is still read at line 329; exercises loss_sweep directly)",
    ),
    Mutant(
        id="m-p1-empty-group",
        group="phase-1",
        crate="diffctx-resolve",
        rel_path="crates/diffctx-resolve/src/vocabulary/family.rs",
        line_hint=201,
        description="Group::has_path excludes Family::Python from its own interaction group",
        target="        Self::of_name(name) == Some(self)",
        replacement="        self != Self::Family(Family::Python) && Self::of_name(name) == Some(self)",
        gate_cmd=[
            "cargo",
            "test",
            "--release",
            "--locked",
            "-p",
            "diffctx",
            "--test",
            "loss_sweep",
            "--",
            "python",
        ],
        expected_red="loss_sweep::python (plus diffctx-answer::decide::tests::a_file_belongs_to_the_group_of_its_extension & metamorphic)",
        static_guard="None (behavioral invariant on Group::has_path)",
    ),
    Mutant(
        id="m-p1-hook-clean",
        group="phase-1",
        crate="diffctx",
        rel_path="crates/diffctx-native/src/hook.rs",
        line_hint=1248,
        description="hook::changed_blobs swallows a failed git diff --raw subprocess as an empty change list",
        target='    let raw = snap::run_git_z(root, &args).map_err(|_| "error:git")?;',
        replacement="    let raw = snap::run_git_z(root, &args).unwrap_or_else(|_| Vec::new());",
        gate_cmd=[
            "cargo",
            "test",
            "--release",
            "--locked",
            "-p",
            "diffctx",
            "--test",
            "hook_property",
        ],
        expected_red="hook_property git-failure commit/push/inspection property families (3 red)",
        static_guard=(
            "#![deny(clippy::disallowed_methods)] in crates/diffctx-native/src/hook.rs:30 also "
            "rejects Result::ok / Result::unwrap_or_default at compile time"
        ),
    ),
    Mutant(
        id="m-a1-candidate-settled",
        group="phase-a",
        crate="diffctx-occurrence",
        rel_path="crates/diffctx-occurrence/src/tokens/occurrence/mod.rs",
        line_hint=444,
        description="Class 1 (occurrence not lost): Account::verdict drops OccurrenceOutcome::Candidate hedges",
        target="                    if entry.counts_for(self.relation) {",
        replacement="                    if self.files == usize::MAX && entry.counts_for(self.relation) {",
        gate_cmd=[
            "cargo",
            "test",
            "--release",
            "--locked",
            "-p",
            "diffctx",
            "--test",
            "occurrence_classes",
            "--",
            "an_occurrence_is_not_lost",
        ],
        expected_red="occurrence_classes::an_occurrence_is_not_lost (plus occurrence_outcome & class_completeness)",
        static_guard="Proof(()) private constructor (compile_fail doctests in occurrence/mod.rs:562-570)",
    ),
    Mutant(
        id="m-a2-no-alias",
        group="phase-a",
        crate="diffctx-occurrence",
        rel_path="crates/diffctx-occurrence/src/tokens/occurrence/alias.rs",
        line_hint=34,
        description="Class 2 (alias): close_aliases stops recording local aliases of the subject definition",
        target="            aliases.push(alias);",
        replacement="            if hops == usize::MAX { aliases.push(alias); }",
        gate_cmd=[
            "cargo",
            "test",
            "--release",
            "--locked",
            "-p",
            "diffctx",
            "--test",
            "occurrence_classes",
            "--",
            "an_alias_of_the_definition_is_not_lost",
        ],
        expected_red="occurrence_classes::an_alias_of_the_definition_is_not_lost & occurrence_alias (3 red)",
        static_guard="None (behavioral alias closure check)",
    ),
    Mutant(
        id="m-a3-leak-complete",
        group="phase-a",
        crate="diffctx-occurrence",
        rel_path="crates/diffctx-occurrence/src/tokens/occurrence/mod.rs",
        line_hint=434,
        description="Class 3 (value leak): Account::verdict ignores ValueRead on Relation::Calls",
        target="                    if self.relation == Relation::Calls && self.admits_calls {",
        replacement="                    if self.files == usize::MAX && self.relation == Relation::Calls && self.admits_calls {",
        gate_cmd=[
            "cargo",
            "test",
            "--release",
            "--locked",
            "-p",
            "diffctx",
            "--test",
            "occurrence_classes",
            "--",
            "a_value_leak_makes_the_calls_relation_partial",
        ],
        expected_red="occurrence_classes::a_value_leak_makes_the_calls_relation_partial (~36 seeds) & occurrence_outcome",
        static_guard="None (behavioral value-escape downgrade check)",
    ),
    Mutant(
        id="m-a4-ignore-implicit",
        group="phase-a",
        crate="diffctx-occurrence",
        rel_path="crates/diffctx-occurrence/src/tokens/occurrence/implicit.rs",
        line_hint=239,
        description="Class 4 (implicit call): implicit_kinds drops Implicit::Operator",
        target="        .filter(|kind| found.contains(kind) && !(*kind == Implicit::Test && run_by_runner))",
        replacement="        .filter(|kind| *kind != Implicit::Operator && found.contains(kind) && !(*kind == Implicit::Test && run_by_runner))",
        gate_cmd=[
            "cargo",
            "test",
            "--release",
            "--locked",
            "-p",
            "diffctx",
            "--test",
            "occurrence_classes",
            "--",
            "an_implicit_call_is_never_nobody",
        ],
        expected_red=(
            "occurrence_classes::an_implicit_call_is_never_nobody & "
            "occurrence_implicit::every_kind_has_a_seed_and_each_seed_reports_its_kind"
        ),
        static_guard="Proof(()) sealing doctest in occurrence/mod.rs:562-570 prevents forging Complete outside verdict()",
    ),
    Mutant(
        id="m-a5-always-partial",
        group="phase-a",
        crate="diffctx-occurrence",
        rel_path="crates/diffctx-occurrence/src/tokens/occurrence/mod.rs",
        line_hint=484,
        description="Class 5 (honest absence): Account::verdict injects a non-empty hedge so no clean absence is Complete",
        target="        if hedges.is_empty() {",
        replacement="        if { hedges.push(Hedge::TooCommon { occurrences: 1 }); hedges.is_empty() } {",
        gate_cmd=[
            "cargo",
            "test",
            "--release",
            "--locked",
            "-p",
            "diffctx",
            "--test",
            "occurrence_classes",
            "--",
            "a_clean_absence_stays_clean",
        ],
        expected_red="occurrence_classes::a_clean_absence_stays_clean (all 29 seeds) & occurrence_outcome",
        static_guard="None (precision gate against trivial always-partial over-hedging)",
    ),
    Mutant(
        id="m-b-testpath",
        group="phase-b",
        crate="diffctx-answer",
        rel_path="crates/diffctx-answer/src/impact/tests_reach.rs",
        line_hint=200,
        description="Phase B (same-file test): TestsReach::build admits inline_tests only when file path looks like a test file",
        target="                        .any(|&(a, b)| a <= f.start_line() && f.end_line() <= b)",
        replacement="                        .any(|&(a, b)| diffctx_base::testfiles::is_test_path(Path::new(f.path())) && a <= f.start_line() && f.end_line() <= b)",
        gate_cmd=[
            "cargo",
            "test",
            "--release",
            "--locked",
            "-p",
            "diffctx",
            "--test",
            "tests_same_file",
        ],
        expected_red=(
            "tests_same_file::a_test_in_the_same_file_is_in_view & "
            "the_definition_a_test_in_its_file_exercises_names_that_test"
        ),
        static_guard="None (inline_tests field stays read at lines 214 & 307 so dead_code stays silent)",
    ),
)

BY_ID: dict[str, Mutant] = {m.id: m for m in MUTANTS}


def verify_repo(repo: pathlib.Path) -> list[dict[str, object]]:
    results: list[dict[str, object]] = []
    for m in MUTANTS:
        fp = repo / m.rel_path
        if not fp.is_file():
            results.append({"id": m.id, "ok": False, "reason": f"missing {m.rel_path}"})
            continue
        lines = fp.read_text(encoding="utf-8").splitlines()
        matches = [idx + 1 for idx, line in enumerate(lines) if line == m.target]
        results.append(
            {
                "id": m.id,
                "ok": len(matches) == 1,
                "file": m.rel_path,
                "matches": matches,
            }
        )
    return results


def apply_mutant(repo: pathlib.Path, mutant: Mutant) -> int:
    fp = repo / mutant.rel_path
    text = fp.read_text(encoding="utf-8")
    lines = text.splitlines(keepends=True)
    hit_indices = [i for i, line in enumerate(lines) if line.rstrip("\r\n") == mutant.target]
    if len(hit_indices) != 1:
        raise RuntimeError(
            f"{mutant.id}: expected exact 1 anchor match in {mutant.rel_path}, got {len(hit_indices)}"
        )
    idx = hit_indices[0]
    ending = "\r\n" if lines[idx].endswith("\r\n") else ("\n" if lines[idx].endswith("\n") else "")
    lines[idx] = mutant.replacement + ending
    fp.write_text("".join(lines), encoding="utf-8")
    return idx + 1


def restore_mutant(repo: pathlib.Path, mutant: Mutant) -> None:
    subprocess.run(
        ["git", "-C", str(repo), "checkout", "--", mutant.rel_path],
        check=True,
    )


def run_ci_mutant(repo: pathlib.Path, mutant: Mutant, allow_cargo: bool) -> int:
    if os.environ.get("GITHUB_ACTIONS") != "true" and not allow_cargo:
        raise RuntimeError(
            "Refusing to run cargo outside GitHub Actions (GITHUB_ACTIONS!=true). "
            "Use --verify or --apply locally, or run inside GitHub Actions."
        )
    line_no = apply_mutant(repo, mutant)
    print(f"[mutate] applied {mutant.id} at {mutant.rel_path}:{line_no}", flush=True)
    try:
        proc = subprocess.run(
            mutant.gate_cmd,
            cwd=str(repo),
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
        )
        output = proc.stdout or ""
        print(output, flush=True)
        if proc.returncode == 0:
            print(f"::error::{mutant.id} SURVIVED its gate: {' '.join(mutant.gate_cmd)}", file=sys.stderr)
            return 1
        # Guard against bogus failures (e.g. mutant failed to compile rather than failing a test).
        if "test result: FAILED." not in output:
            print(
                f"::error::{mutant.id} exited {proc.returncode} WITHOUT running to 'test result: FAILED.' "
                "(compile error or harness crash does not count as behavioral test catch!)",
                file=sys.stderr,
            )
            return 2
        print(f"[mutate] CAUGHT {mutant.id} (rc={proc.returncode}, test result: FAILED.)", flush=True)
        return 0
    finally:
        restore_mutant(repo, mutant)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=pathlib.Path, default=pathlib.Path("."))
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument("--list", action="store_true", help="List all 11 required mutations")
    group.add_argument("--verify", action="store_true", help="Verify all 11 anchors exist uniquely in --repo")
    group.add_argument("--matrix-json", action="store_true", help="Emit GitHub Actions matrix JSON")
    group.add_argument("--apply", metavar="ID", choices=tuple(BY_ID), help="Apply one mutant in-place")
    group.add_argument("--restore", metavar="ID", choices=tuple(BY_ID), help="Restore mutated file via git checkout")
    group.add_argument("--run-ci", metavar="ID", choices=tuple(BY_ID), help="Apply, run gate on CI, assert caught, restore")
    parser.add_argument("--allow-cargo-in-ci", action="store_true", help="Explicit CI override when GITHUB_ACTIONS is unset")
    args = parser.parse_args()

    if args.list:
        for m in MUTANTS:
            print(f"{m.id:24s} [{m.group:7s}] {m.crate:20s} {m.rel_path}:{m.line_hint}")
        return 0

    if args.matrix_json:
        payload = [
            {
                "id": m.id,
                "group": m.group,
                "crate": m.crate,
                "file": m.rel_path,
                "gate": " ".join(m.gate_cmd),
            }
            for m in MUTANTS
        ]
        print(json.dumps(payload))
        return 0

    if args.verify:
        checks = verify_repo(args.repo)
        bad = [c for c in checks if not c["ok"]]
        for c in checks:
            status = "OK" if c["ok"] else "FAIL"
            print(f"[{status}] {c['id']:24s} -> {c.get('file', '')}:{c.get('matches', c.get('reason'))}")
        return 1 if bad else 0

    if args.apply:
        line_no = apply_mutant(args.repo, BY_ID[args.apply])
        print(f"Applied {args.apply} at {BY_ID[args.apply].rel_path}:{line_no}")
        return 0

    if args.restore:
        restore_mutant(args.repo, BY_ID[args.restore])
        print(f"Restored {BY_ID[args.restore].rel_path}")
        return 0

    if args.run_ci:
        return run_ci_mutant(args.repo, BY_ID[args.run_ci], args.allow_cargo_in_ci)

    return 0


if __name__ == "__main__":
    sys.exit(main())
