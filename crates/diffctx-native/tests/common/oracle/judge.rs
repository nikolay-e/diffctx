//! Compares an answer with a case's expectation. Every failure message shows
//! the relation diff, not only that one exists.

use std::collections::{BTreeMap, BTreeSet};

use super::case::{Case, Compiled, Coverage};
use super::engine::{Answer, absence_claims};
use super::model::{Kind, Node, Relation, UnknownClass, precise};
use super::report::{CaseReport, TargetReport};

#[derive(Default)]
struct Counts {
    tp: usize,
    fp: usize,
    fn_: usize,
}

pub fn judge(
    name: &str,
    case: &Case,
    compiled: &Compiled,
    language: &str,
    answer: &Answer,
) -> (Vec<String>, CaseReport) {
    let mut report = CaseReport::new(name, case, language);
    let mut failures = Vec::new();

    compare_relations(case, compiled, language, answer, &mut report, &mut failures);
    check_decoys(case, answer, &mut failures);
    check_definitions(case, answer, &mut failures);
    check_unknowns(case, answer, &mut failures);
    check_coverage(case, answer, &mut failures);
    check_lists(case, answer, &mut failures);

    report.unknowns = answer
        .unknowns
        .iter()
        .map(|(node, class, sentence)| {
            format!(
                "{node}: {}",
                class.map_or_else(|| sentence.clone(), |c| c.name().to_string())
            )
        })
        .collect();
    report.empty = answer.empty;
    report.disclosed = answer.run_disclosure || !answer.gapped.is_empty();
    report.absence_claims = answer
        .markdown
        .as_deref()
        .map(absence_claims)
        .unwrap_or_default();
    report.failures.clone_from(&failures);
    (failures, report)
}

fn pairs(expected: &Relation, actual: &Relation, strict: bool) -> bool {
    let tests = expected.kind == Kind::TestsReach;
    // The engine names the test file that reaches a caller, never the test
    // function in it; a `named_by` row names a file, never a function. Their
    // `from` is judged by the file.
    let from_agrees = if tests {
        expected.from.path.is_empty() || expected.from.path == actual.from.path
    } else {
        expected.from.covers(&actual.from)
            || (actual.kind == Kind::ReferencesPath
                && actual.from.symbol.is_none()
                && expected.from.path == actual.from.path)
    };
    // A test link carries no line: an expected site cannot be checked.
    let sites_agree = (tests && actual.sites.is_empty())
        || expected.sites.iter().all(|s| actual.sites.contains(s));
    from_agrees
        && expected.resolution == actual.resolution
        && sites_agree
        && (!strict || expected.kind == actual.kind)
}

fn compare_relations(
    case: &Case,
    compiled: &Compiled,
    language: &str,
    answer: &Answer,
    report: &mut CaseReport,
    failures: &mut Vec<String>,
) {
    for (target, class) in &compiled.listed {
        let expected: Vec<&Relation> = compiled
            .relations
            .iter()
            .filter(|r| r.to == *target && r.kind.class() == *class)
            .collect();
        let actual: Vec<&Relation> = answer
            .relations
            .iter()
            .filter(|a| target.covers(&a.to) && a.kind.class() == *class)
            .collect();
        let family = format!("{class:?}").to_lowercase();
        let mut taken = vec![false; actual.len()];
        let mut counts: BTreeMap<&'static str, Counts> = BTreeMap::new();
        let mut lines: Vec<String> = Vec::new();
        let mut red = false;

        for want in &expected {
            let strict = precise(language, want.kind) && !want.loose_kind;
            let mut hits = 0;
            for (i, got) in actual.iter().enumerate() {
                // A caller the engine reports once with several sites can satisfy
                // several expectations that each name one of its sites.
                if (taken[i] && want.sites.is_empty()) || !pairs(want, got, strict) {
                    continue;
                }
                taken[i] = true;
                hits += 1;
                counts.entry(want.kind.name()).or_default().tp += 1;
                if got.kind != want.kind && !want.loose_kind {
                    report.kind_mismatches.push(format!(
                        "{}: expected {}, found {}",
                        want.show(),
                        want.kind.name(),
                        got.kind.name()
                    ));
                }
                // A relation naming a definition stands for one caller; one
                // naming a file stands for every caller in that file.
                if want.from.symbol.is_some() {
                    break;
                }
            }
            if hits == 0 {
                red = true;
                counts.entry(want.kind.name()).or_default().fn_ += 1;
                lines.push(format!("  - missing {}", want.show()));
                if let Some(near) = actual.iter().find(|got| want.from.covers(&got.from)) {
                    lines.push(format!("      the answer has {}", near.show()));
                }
            }
        }
        for (i, got) in actual.iter().enumerate() {
            if taken[i] {
                continue;
            }
            counts.entry(got.kind.name()).or_default().fp += 1;
            if case.expect.exact {
                red = true;
                lines.push(format!("  + extra   {}", got.show()));
            }
        }
        if counts.is_empty() {
            report.targets.push(TargetReport {
                target: target.to_string(),
                relation: family.clone(),
                tp: 0,
                fp: 0,
                fn_: 0,
            });
        }
        for (relation, c) in counts {
            report.targets.push(TargetReport {
                target: target.to_string(),
                relation: relation.to_string(),
                tp: c.tp,
                fp: c.fp,
                fn_: c.fn_,
            });
        }
        if red {
            failures.push(format!(
                "relations to `{target}` ({family}):\n{}",
                lines.join("\n")
            ));
        }
    }
}

fn check_decoys(case: &Case, answer: &Answer, failures: &mut Vec<String>) {
    for (key, decoys) in &case.expect.decoys {
        let target = Node::target(key);
        for decoy in decoys {
            let decoy_node = Node::parse(decoy);
            for related in answer
                .relations
                .iter()
                .filter(|a| target.covers(&a.to) && decoy_node.covers(&a.from))
            {
                failures.push(format!(
                    "decoy `{decoy}` is related to `{key}`: {}",
                    related.show()
                ));
            }
            for definition in answer
                .definitions
                .iter()
                .filter(|d| target.covers(d) && decoy_node.covers(d))
            {
                failures.push(format!(
                    "decoy `{decoy}` is answered as a definition of `{key}`: {definition}"
                ));
            }
        }
    }
}

fn compare_set(
    what: &str,
    expected: &[String],
    actual: &[Node],
    exact: bool,
    failures: &mut Vec<String>,
) {
    let wanted: Vec<Node> = expected.iter().map(|e| Node::target(e)).collect();
    // A bare file path names the file's own row, not the definitions in it.
    let names = |w: &Node, a: &Node| {
        if w.symbol.is_none() && !w.path.is_empty() {
            w.path == a.path && a.symbol.is_none()
        } else {
            w.covers(a)
        }
    };
    let missing: Vec<String> = wanted
        .iter()
        .filter(|w| !actual.iter().any(|a| names(w, a)))
        .map(ToString::to_string)
        .collect();
    let unexpected: BTreeSet<String> = if exact {
        actual
            .iter()
            .filter(|a| !wanted.iter().any(|w| names(w, a)))
            .map(ToString::to_string)
            .collect()
    } else {
        BTreeSet::new()
    };
    if !missing.is_empty() || !unexpected.is_empty() {
        let reported: BTreeSet<String> = actual.iter().map(ToString::to_string).collect();
        failures.push(format!(
            "{what}: missing [{}], unexpected [{}]; the answer names [{}]",
            missing.join(", "),
            unexpected.iter().cloned().collect::<Vec<_>>().join(", "),
            reported.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
}

fn check_definitions(case: &Case, answer: &Answer, failures: &mut Vec<String>) {
    if let Some(expected) = &case.expect.changed {
        compare_set(
            "changed definitions",
            expected,
            &answer.touched,
            case.expect.exact,
            failures,
        );
    }
    if let Some(expected) = &case.expect.definitions {
        compare_set(
            "definitions",
            expected,
            &answer.definitions,
            case.expect.exact,
            failures,
        );
    }
}

fn check_unknowns(case: &Case, answer: &Answer, failures: &mut Vec<String>) {
    let Some(unknowns) = &case.expect.unknowns else {
        return;
    };
    if unknowns.is_empty() && !answer.unknowns.is_empty() {
        let said: Vec<String> = answer
            .unknowns
            .iter()
            .map(|(node, _, sentence)| format!("{node}: \"{sentence}\""))
            .collect();
        failures.push(format!(
            "expected no unknown; the answer names {}",
            said.join("; ")
        ));
    }
    for (key, class) in unknowns {
        let target = Node::target(key);
        let named: Vec<&(Node, Option<UnknownClass>, String)> = answer
            .unknowns
            .iter()
            .filter(|(node, _, _)| target.covers(node))
            .collect();
        if !named.iter().any(|(_, c, _)| *c == Some(*class)) {
            let said = if named.is_empty() {
                "none".to_string()
            } else {
                named
                    .iter()
                    .map(|(_, _, sentence)| format!("\"{sentence}\""))
                    .collect::<Vec<_>>()
                    .join("; ")
            };
            failures.push(format!(
                "expected the unknown `{}` for `{key}`; the answer names {said}",
                class.name()
            ));
        }
    }
}

fn check_coverage(case: &Case, answer: &Answer, failures: &mut Vec<String>) {
    for (key, kinds) in &case.expect.coverage {
        let target = Node::target(key);
        let definitions: Vec<&Node> = answer
            .definitions
            .iter()
            .filter(|d| target.covers(d))
            .collect();
        if definitions.is_empty() {
            failures.push(format!(
                "coverage of `{key}`: the answer reports no such definition"
            ));
            continue;
        }
        let gapped = definitions.iter().any(|d| answer.gapped.contains(*d));
        for (kind, expected) in kinds {
            let partial =
                gapped || answer.run_disclosure || (kind == "tests_reach" && !answer.tests_known);
            let found = if partial {
                Coverage::Partial
            } else {
                Coverage::Complete
            };
            if found != *expected {
                failures.push(format!(
                    "coverage of `{key}` for {kind}: expected {expected:?}, the answer is {found:?}"
                ));
            }
        }
    }
}

fn check_lists(case: &Case, answer: &Answer, failures: &mut Vec<String>) {
    let expect = &case.expect;
    for name in &expect.untested {
        let node = Node::target(name);
        if !answer.untested.iter().any(|a| node.covers(a)) {
            failures.push(format!(
                "`{name}` is not reported as a definition without a static test link"
            ));
        }
    }
    for name in &expect.not_untested {
        let node = Node::target(name);
        if answer.untested.iter().any(|a| node.covers(a)) {
            failures.push(format!(
                "`{name}` is reported as a definition without a static test link"
            ));
        }
    }
    for path in &expect.stale {
        if !answer.stale.contains(path) {
            failures.push(format!("`{path}` is not listed as a stale reference"));
        }
    }
    for path in &expect.not_stale {
        if answer.stale.contains(path) {
            failures.push(format!("`{path}` is listed as a stale reference"));
        }
    }
    for path in &expect.named_by {
        if !answer.named_by.contains(path) {
            failures.push(format!("`{path}` is not listed as naming a changed file"));
        }
    }
    for path in &expect.not_named_by {
        if answer.named_by.contains(path) {
            failures.push(format!("`{path}` is listed as naming a changed file"));
        }
    }
    for path in &expect.contracts {
        if !answer.contracts.iter().any(|c| c.path == *path) {
            failures.push(format!("`{path}` is not listed as a changed contract"));
        }
    }
    for path in &expect.not_contracts {
        if answer.contracts.iter().any(|c| c.path == *path) {
            failures.push(format!("`{path}` is listed as a changed contract"));
        }
    }
    if let Some(removed) = &expect.removed {
        for name in removed {
            let node = Node::target(name);
            if !answer.removed.iter().any(|r| node.covers(r)) {
                failures.push(format!("`{name}` is not reported as a removed definition"));
            }
        }
        if removed.is_empty() && !answer.removed.is_empty() {
            let named: Vec<String> = answer.removed.iter().map(ToString::to_string).collect();
            failures.push(format!(
                "expected no removed definition; the answer names {}",
                named.join(", ")
            ));
        }
    }
    if expect.never_empty && answer.empty {
        failures.push("the answer is empty".to_string());
    }
    if expect.discloses && !(answer.run_disclosure || !answer.gapped.is_empty()) {
        failures.push("the answer names no limit, unanalysed file or unknown".to_string());
    }
    if expect.no_negative_claims {
        let claims = answer
            .markdown
            .as_deref()
            .map(absence_claims)
            .unwrap_or_default();
        if !claims.is_empty() {
            failures.push(format!(
                "the text answer claims an absence: {}",
                claims.join("; ")
            ));
        }
    }
}
