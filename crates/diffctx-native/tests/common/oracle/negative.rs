//! Negative tests for the judge's lenient rules (wave 1's A1-A5): each rule
//! accepts something a stricter reading would not, and each is probed with a
//! synthetic engine answer carrying the defect the rule must still catch. A
//! control beside it shows the leniency itself still holds, so a rule cannot
//! be tightened into a flood of false reds either.

use libtest_mimic::{Failed, Trial};
use serde_json::{Value, json};

use super::case::{self, Mode};
use super::engine::{absence_claims, extract};
use super::judge::judge;
use super::model::Kind;

enum Verdict {
    /// The evaluation fails, and a failure mentions this text.
    Fails(&'static str),
    Passes,
}

struct Probe {
    what: &'static str,
    case: &'static str,
    answer: Value,
    verdict: Verdict,
}

fn failures(case_yaml: &str, answer: &Value) -> Result<Vec<String>, String> {
    let case = case::parse(case_yaml, "negative", Mode::Impact)?;
    let compiled = case.expect.compile()?;
    Ok(judge("negative", &case, &compiled, "python", &extract(answer)?).0)
}

fn run(probes: Vec<Probe>) -> Result<(), Failed> {
    let mut slipped = Vec::new();
    for probe in probes {
        let found = failures(probe.case, &probe.answer).map_err(Failed::from)?;
        match probe.verdict {
            Verdict::Fails(needle) if !found.iter().any(|f| f.contains(needle)) => {
                slipped.push(format!(
                    "{}: expected a failure mentioning `{needle}`, got {found:?}",
                    probe.what
                ));
            }
            Verdict::Passes if !found.is_empty() => {
                slipped.push(format!("{}: expected a pass, got {found:?}", probe.what));
            }
            _ => {}
        }
    }
    if slipped.is_empty() {
        Ok(())
    } else {
        Err(Failed::from(slipped.join("\n")))
    }
}

/// An impact answer for a change to `shop/pricing.py::total` with these callers.
fn answer(callers: &Value) -> Value {
    json!({
        "definitions": [{
            "path": "shop/pricing.py", "symbol": "total", "kind": "function",
            "lines": "1-2", "change": {"kind": "edited"}, "rows": callers,
        }],
        "inventory": {"changed": {"total": 1, "items": ["shop/pricing.py"]}},
        "empty": false,
        "tests": {"known": true},
    })
}

fn charge(evidence: &str, relation: &str, tested_by: Option<&str>) -> Value {
    let mut caller = json!({
        "path": "shop/checkout.py", "symbol": "charge", "lines": "4-5",
        "standing": evidence, "class": relation, "sites": [5],
        "guard": {"state": "none"},
    });
    if let Some(test) = tested_by {
        caller["guard"] = json!({"state": "linked", "test": test});
    }
    caller
}

const CHARGE_IS_CALLED_AND_TESTED: &str = r"
mode: impact
source: synthetic
repo:
  changed_files: {shop/pricing.py: x}
expect:
  relations:
    - {from: 'shop/checkout.py::charge', to: 'shop/pricing.py::total', kind: calls, site: 5}
    - {from: 'tests/test_checkout.py::test_charge', to: 'shop/checkout.py::charge', kind: tests_reach}
";

const CHARGE_CALLS_TOTAL: &str = r"
mode: impact
source: synthetic
repo:
  changed_files: {shop/pricing.py: x}
expect:
  relations:
    - {from: 'shop/checkout.py::charge', to: 'shop/pricing.py::total', kind: calls, site: 5}
";

const CHARGE_MAY_CALL_TOTAL: &str = r"
mode: impact
source: synthetic
repo:
  changed_files: {shop/pricing.py: x}
expect:
  relations:
    - {from: 'shop/checkout.py::charge', to: 'shop/pricing.py::total', kind: calls, resolution: candidate, site: 5}
";

const CHARGE_READS_TOTAL: &str = r"
mode: impact
source: synthetic
repo:
  changed_files: {shop/pricing.py: x}
expect:
  relations:
    - {from: 'shop/checkout.py::charge', to: 'shop/pricing.py::total', kind: reads, site: 5}
";

const CHARGE_ANNOTATES_TOTAL: &str = r"
mode: impact
source: synthetic
repo:
  changed_files: {shop/pricing.py: x}
expect:
  relations:
    - {from: 'shop/checkout.py::charge', to: 'shop/pricing.py::total', kind: type_use, site: 5}
";

const CHARGE_IS_A_CALLER: &str = r"
mode: impact
source: synthetic
repo:
  changed_files: {shop/pricing.py: x}
expect:
  callers: {total: ['shop/checkout.py::charge']}
";

const SCHEMA_ROW_IS_REMOVED: &str = r"
mode: impact
source: synthetic
repo:
  changed_files: {api/schema.py: x}
expect:
  removed: ['api/schema.py::Row']
";

const POST_IS_READ_BY_THE_PAGE: &str = r"
mode: impact
source: synthetic
repo:
  changed_files: {content/blog/post.md: x}
expect:
  relations:
    - {from: 'src/pages/index.astro', to: 'content/blog/post.md', kind: reads}
";

fn contract(path: &str, kind: &str) -> Value {
    json!({
        "definitions": [],
        "inventory": {"changed": {"total": 1, "items": ["api/schema.py"]}},
        "empty": false,
        "contracts": {"total": 1, "items": [{"path": path, "symbol": "Row", "kind": kind}]},
    })
}

fn collection_read(path: &str, collection: &str) -> Value {
    json!({
        "definitions": [],
        "inventory": {"changed": {"total": 1, "items": ["content/blog/post.md"]}},
        "empty": false,
        "named": {"collections": {"total": 1, "items": [{"path": path, "line": 3, "collection": collection}]}},
    })
}

/// A1: a test expectation that names a test function is met only by an answer
/// that names it; one that names the file alone is judged by the file.
fn a1_a_named_test_function_needs_the_answer_to_name_it() -> Result<(), Failed> {
    let named = |test: &str, name: &str| {
        let mut caller = charge("resolved", "call", Some(test));
        caller["guard"]["name"] = json!(name);
        answer(&json!([caller]))
    };
    run(vec![
        Probe {
            what: "the answer names the expected test function",
            case: CHARGE_IS_CALLED_AND_TESTED,
            answer: named("tests/test_checkout.py", "test_charge"),
            verdict: Verdict::Passes,
        },
        Probe {
            what: "the answer names the file but not the function",
            case: CHARGE_IS_CALLED_AND_TESTED,
            answer: answer(&json!([charge(
                "resolved",
                "call",
                Some("tests/test_checkout.py")
            )])),
            verdict: Verdict::Fails("missing tests/test_checkout.py::test_charge"),
        },
        Probe {
            what: "another function of the file reaches the caller",
            case: CHARGE_IS_CALLED_AND_TESTED,
            answer: named("tests/test_checkout.py", "test_other"),
            verdict: Verdict::Fails("missing tests/test_checkout.py::test_charge"),
        },
        Probe {
            what: "another test file reaches the caller",
            case: CHARGE_IS_CALLED_AND_TESTED,
            answer: answer(&json!([charge(
                "resolved",
                "call",
                Some("tests/test_other.py")
            )])),
            verdict: Verdict::Fails("missing tests/test_checkout.py::test_charge"),
        },
        Probe {
            what: "no test reaches the caller",
            case: CHARGE_IS_CALLED_AND_TESTED,
            answer: answer(&json!([charge("resolved", "call", None)])),
            verdict: Verdict::Fails("missing tests/test_checkout.py::test_charge"),
        },
    ])
}

/// A2: a removed public API in `contracts` counts as a removed definition.
fn a2_only_a_removed_api_contract_is_a_removal() -> Result<(), Failed> {
    run(vec![
        Probe {
            what: "the expected contract is removed",
            case: SCHEMA_ROW_IS_REMOVED,
            answer: contract("api/schema.py", "removed_api"),
            verdict: Verdict::Passes,
        },
        Probe {
            what: "the contract only changed",
            case: SCHEMA_ROW_IS_REMOVED,
            answer: contract("api/schema.py", "changed_api"),
            verdict: Verdict::Fails("`api/schema.py::Row` is not reported as a removed definition"),
        },
        Probe {
            what: "another file's contract was removed",
            case: SCHEMA_ROW_IS_REMOVED,
            answer: contract("api/other.py", "removed_api"),
            verdict: Verdict::Fails("`api/schema.py::Row` is not reported as a removed definition"),
        },
    ])
}

const MODULE_M4_IS_DELETED: &str = r"
mode: impact
source: synthetic
repo:
  deleted_files: [pkg/m4.py]
expect:
  removed: ['pkg/m4.py']
";

fn deleted_modules(paths: &[&str], total: u64) -> Value {
    let items: Vec<Value> = paths
        .iter()
        .map(|path| json!({"path": path, "definitions": 5, "importers": []}))
        .collect();
    json!({
        "definitions": [],
        "inventory": {"changed": {"total": total, "items": paths}},
        "empty": false,
        "deleted_modules": {"total": total, "items": items},
    })
}

/// A2b: a module the answer lists under `deleted_modules` is a removal; one the
/// listing shed, or none at all, is not.
fn a2b_only_a_listed_deleted_module_is_a_removal() -> Result<(), Failed> {
    run(vec![
        Probe {
            what: "the expected module is listed as deleted",
            case: MODULE_M4_IS_DELETED,
            answer: deleted_modules(&["pkg/m3.py", "pkg/m4.py"], 2),
            verdict: Verdict::Passes,
        },
        Probe {
            what: "the listing shed the expected module",
            case: MODULE_M4_IS_DELETED,
            answer: deleted_modules(&["pkg/m1.py", "pkg/m2.py"], 3),
            verdict: Verdict::Fails("`pkg/m4.py` is not reported as a removed definition"),
        },
        Probe {
            what: "no module is listed as deleted",
            case: MODULE_M4_IS_DELETED,
            answer: json!({"definitions": [], "empty": false}),
            verdict: Verdict::Fails("`pkg/m4.py` is not reported as a removed definition"),
        },
    ])
}

/// A3: a caller found only by its name is textual and yields no relation.
fn a3_a_name_match_is_no_caller() -> Result<(), Failed> {
    run(vec![Probe {
        what: "the only row for the expected caller is a word match",
        case: CHARGE_CALLS_TOTAL,
        answer: answer(&json!([charge("name", "call", None)])),
        verdict: Verdict::Fails("missing shop/checkout.py::charge -calls->"),
    }])
}

/// A4: while an answer states no kind, code kinds are compared on (from, to,
/// resolution) alone.
fn a4_kind_is_loose_but_the_rest_is_not() -> Result<(), Failed> {
    run(vec![
        Probe {
            what: "a read stands for the expected call while the answer states no kind",
            case: CHARGE_CALLS_TOTAL,
            answer: answer(&json!([charge("resolved", "reference", None)])),
            verdict: Verdict::Passes,
        },
        Probe {
            what: "the caller is only a candidate",
            case: CHARGE_CALLS_TOTAL,
            answer: answer(&json!([charge("candidate", "call", None)])),
            verdict: Verdict::Fails("missing shop/checkout.py::charge -calls->"),
        },
        Probe {
            what: "another function calls it",
            case: CHARGE_CALLS_TOTAL,
            answer: answer(&json!([{
                "path": "shop/report.py", "symbol": "build", "lines": "4-5",
                "standing": "resolved", "class": "call", "sites": [5],
            }])),
            verdict: Verdict::Fails("missing shop/checkout.py::charge -calls->"),
        },
        Probe {
            what: "the caller reads it on another line",
            case: CHARGE_CALLS_TOTAL,
            answer: answer(&json!([{
                "path": "shop/checkout.py", "symbol": "charge", "lines": "4-9",
                "standing": "resolved", "class": "call", "sites": [8],
            }])),
            verdict: Verdict::Fails("missing shop/checkout.py::charge -calls->"),
        },
    ])
}

/// A7: a row the answer folds as not affected is no caller (the reader is told
/// the change cannot have reached it) and stands only for an expectation of the
/// kind its rule folds, while a candidate folded for its
/// untyped receiver stays a disclosed row.
fn a7_a_folded_row_is_no_caller() -> Result<(), Failed> {
    let folded = |evidence: &str, rule: &str| {
        let mut row = charge(evidence, "call", None);
        row["folded_by"] = json!(rule);
        row
    };
    run(vec![
        Probe {
            what: "the row is folded as a declared type",
            case: CHARGE_CALLS_TOTAL,
            answer: answer(&json!([folded("resolved", "declared-type")])),
            verdict: Verdict::Fails("folds shop/checkout.py::charge"),
        },
        Probe {
            what: "the row is folded as a call that still works",
            case: CHARGE_CALLS_TOTAL,
            answer: answer(&json!([folded("resolved", "check-the-default")])),
            verdict: Verdict::Fails("folds shop/checkout.py::charge"),
        },
        Probe {
            what: "a row folded as a declared type still stands for an expected type use",
            case: CHARGE_ANNOTATES_TOTAL,
            answer: answer(&json!([folded("resolved", "declared-type")])),
            verdict: Verdict::Passes,
        },
        Probe {
            what: "a row folded as a declared type is no expected read",
            case: CHARGE_READS_TOTAL,
            answer: answer(&json!([folded("resolved", "declared-type")])),
            verdict: Verdict::Fails("folds shop/checkout.py::charge"),
        },
        Probe {
            what: "a candidate folded for its untyped receiver is a disclosed row",
            case: CHARGE_MAY_CALL_TOTAL,
            answer: answer(&json!([folded("candidate", "untyped-receiver")])),
            verdict: Verdict::Passes,
        },
        Probe {
            what: "the same row listed",
            case: CHARGE_CALLS_TOTAL,
            answer: answer(&json!([charge("resolved", "call", None)])),
            verdict: Verdict::Passes,
        },
    ])
}

/// A6: a row that states its kind (a precise cell's) is compared on it, and on
/// its site; one that states none is not.
fn a6_a_stated_kind_is_compared() -> Result<(), Failed> {
    let stated = |kind: &str, sites: &[u32]| {
        let mut row = charge("resolved", "call", None);
        row["kind"] = json!(kind);
        row["sites"] = json!(sites);
        row
    };
    run(vec![
        Probe {
            what: "the stated kind is the expected one",
            case: CHARGE_CALLS_TOTAL,
            answer: answer(&json!([stated("calls", &[5])])),
            verdict: Verdict::Passes,
        },
        Probe {
            what: "the answer states a read where a call is expected",
            case: CHARGE_CALLS_TOTAL,
            answer: answer(&json!([stated("reads", &[5])])),
            verdict: Verdict::Fails("missing shop/checkout.py::charge -calls->"),
        },
        Probe {
            what: "the stated kind is right and the site is not",
            case: CHARGE_CALLS_TOTAL,
            answer: answer(&json!([stated("calls", &[9])])),
            verdict: Verdict::Fails("missing shop/checkout.py::charge -calls->"),
        },
        Probe {
            what: "the answer states a type use where a read is expected",
            case: CHARGE_READS_TOTAL,
            answer: answer(&json!([stated("type_use", &[5])])),
            verdict: Verdict::Fails("missing shop/checkout.py::charge -reads->"),
        },
        Probe {
            what: "the answer states a type use and a type use is expected",
            case: CHARGE_ANNOTATES_TOTAL,
            answer: answer(&json!([stated("type_use", &[5])])),
            verdict: Verdict::Passes,
        },
        Probe {
            what: "a convenience field names no kind, so any stated code kind satisfies it",
            case: CHARGE_IS_A_CALLER,
            answer: answer(&json!([stated("reads", &[5])])),
            verdict: Verdict::Passes,
        },
    ])
}

/// A stated kind no schema names is an answer the harness cannot evaluate, not a red case.
fn a_kind_no_schema_names_cannot_be_evaluated() -> Result<(), Failed> {
    let mut row = charge("resolved", "call", None);
    row["kind"] = json!("invokes");
    match failures(CHARGE_CALLS_TOTAL, &answer(&json!([row]))) {
        Err(why) if why.contains("no relation kind") => Ok(()),
        other => Err(Failed::from(format!(
            "a row stating `invokes` gave {other:?}, not an evaluation error"
        ))),
    }
}

/// One vocabulary: the engine's relation kinds (`lang::RelationKind`, which the
/// resolver's relations map onto), the harness's and the case schema's are the
/// same names, each spelled once.
fn the_engines_kinds_the_harnesss_and_the_schemas_are_one_vocabulary() -> Result<(), Failed> {
    let schema =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/cases/impact/SCHEMA.md");
    let text = std::fs::read_to_string(&schema)
        .map_err(|e| Failed::from(format!("read {}: {e}", schema.display())))?;
    let bullet = text
        .split("- `kind`:")
        .nth(1)
        .and_then(|rest| rest.split("- `resolution`").next())
        .ok_or_else(|| Failed::from("SCHEMA.md no longer lists the `kind` values"))?;
    let mut wrong = Vec::new();
    for engine in _diffctx::RelationKind::ALL {
        let name = engine.name();
        if Kind::parse(name).is_none_or(|k| k.name() != name) {
            wrong.push(format!("the harness does not know the engine's `{name}`"));
        }
        if !bullet.contains(&format!("`{name}`")) {
            wrong.push(format!("SCHEMA.md does not list the engine's `{name}`"));
        }
    }
    for harness in Kind::ALL {
        let name = harness.name();
        if _diffctx::RelationKind::parse(name).is_none_or(|k| k.name() != name) {
            wrong.push(format!("the engine does not know the harness's `{name}`"));
        }
    }
    if wrong.is_empty() {
        Ok(())
    } else {
        Err(Failed::from(wrong.join("\n")))
    }
}

const PACKAGE_REEXPORTS_TOTAL: &str = r"
mode: impact
source: synthetic
repo:
  changed_files: {shop/pricing.py: x}
expect:
  relations:
    - {from: 'shop/__init__.py', to: 'shop/pricing.py::total', kind: reexports, resolution: resolved, site: 1}
";

const SOMEONE_CALLS_TOTAL: &str = r"
mode: impact
source: synthetic
repo:
  changed_files: {shop/pricing.py: x}
expect:
  callers: {total: ['shop/__init__.py']}
";

/// A6b: a static re-export is a `reexports` relation, `resolved`; it satisfies
/// no caller expectation, and a caller row does not satisfy it.
fn a_static_reexport_is_a_resolved_reexports_relation() -> Result<(), Failed> {
    let reexport = json!({
        "path": "shop/__init__.py", "lines": "1-1", "standing": "resolved",
        "class": "reexports", "sites": [1],
    });
    run(vec![
        Probe {
            what: "the re-export row is a resolved `reexports` relation",
            case: PACKAGE_REEXPORTS_TOTAL,
            answer: answer(&json!([reexport.clone()])),
            verdict: Verdict::Passes,
        },
        Probe {
            what: "a call row is no re-export",
            case: PACKAGE_REEXPORTS_TOTAL,
            answer: answer(&json!([{
                "path": "shop/__init__.py", "lines": "1-1", "standing": "resolved",
                "class": "call", "sites": [1],
            }])),
            verdict: Verdict::Fails("missing shop/__init__.py -reexports->"),
        },
        Probe {
            what: "a re-export row is no caller",
            case: SOMEONE_CALLS_TOTAL,
            answer: answer(&json!([reexport])),
            verdict: Verdict::Fails("missing shop/__init__.py -calls->"),
        },
    ])
}

/// A5: `collection_reads` yield `reads` from the reading file to the changed
/// files under `content/<collection>/`.
fn a5_a_collection_read_reaches_only_its_collection() -> Result<(), Failed> {
    run(vec![
        Probe {
            what: "the page reads the collection the file sits in",
            case: POST_IS_READ_BY_THE_PAGE,
            answer: collection_read("src/pages/index.astro", "blog"),
            verdict: Verdict::Passes,
        },
        Probe {
            what: "the page reads another collection",
            case: POST_IS_READ_BY_THE_PAGE,
            answer: collection_read("src/pages/index.astro", "docs"),
            verdict: Verdict::Fails("missing src/pages/index.astro -reads-> content/blog/post.md"),
        },
        Probe {
            what: "another file reads the collection",
            case: POST_IS_READ_BY_THE_PAGE,
            answer: collection_read("src/pages/about.astro", "blog"),
            verdict: Verdict::Fails("missing src/pages/index.astro -reads-> content/blog/post.md"),
        },
    ])
}

/// A symbol where a contract's file belongs matches nothing, so a `not_contracts` entry
/// like it asserts nothing: the load refuses it, and accepts the file's path.
fn a_contract_entry_that_is_no_file_fails_the_load() -> Result<(), Failed> {
    let with = |key: &str, entry: &str| {
        format!(
            "mode: impact\nsource: synthetic\nrepo:\n  changed_files: {{src/rate.ts: x}}\nexpect:\n  {key}: [{entry}]\n"
        )
    };
    for key in ["contracts", "not_contracts"] {
        let by_name = case::parse(&with(key, "mono"), "negative", Mode::Impact);
        if !by_name
            .as_ref()
            .is_err_and(|e| e.contains("no file of the case"))
        {
            return Err(Failed::from(format!(
                "`{key}: [mono]` should fail the load, got {:?}",
                by_name.map(|_| ())
            )));
        }
        if let Err(refused) = case::parse(&with(key, "src/rate.ts"), "negative", Mode::Impact) {
            return Err(Failed::from(format!(
                "`{key}: [src/rate.ts]` should load, got {refused}"
            )));
        }
    }
    Ok(())
}

/// A partial answer's zero is a claim whichever way it says callers were left out.
fn a_zero_beside_not_determined_is_a_claim() -> Result<(), Failed> {
    let said =
        absence_claims("1 changed file(s), 0 caller(s) outside the diff; callers not determined");
    let counted = absence_claims("1 changed file(s), 3 caller(s) outside the diff");
    if said.is_empty() || !counted.is_empty() {
        return Err(Failed::from(format!(
            "a zero beside `not determined` gave {said:?}; a plain count gave {counted:?}"
        )));
    }
    Ok(())
}

/// A site stands once: the same line as a row and as a use nothing settles, or a file
/// with a row that is also listed unread, is the answer contradicting itself.
fn a_site_has_one_standing() -> Result<(), Failed> {
    let defined = |rows: Value, gaps: Value, missed: Value| {
        json!({"definitions": [{
            "path": "shop/pricing.py", "symbol": "total", "lines": "1-2",
            "rows": rows, "gaps": gaps, "missed_in": missed,
        }]})
    };
    let row = |standing: &str| charge(standing, "call", None);
    let gap = |line: u64| {
        json!([{"kind": "unlanded", "uses": 1,
            "sites": [{"path": "shop/checkout.py", "line": line, "via": "call"}]}])
    };
    let said = |doc: &Value| crate::fact_keys::repeated_facts(doc);
    let twice = [
        defined(json!([row("resolved")]), gap(5), json!([])),
        defined(
            json!([row("resolved"), row("candidate")]),
            json!([]),
            json!([]),
        ),
        defined(
            json!([row("resolved")]),
            json!([]),
            json!([{"path": "shop/checkout.py", "lines": [5]}]),
        ),
    ];
    let once = [
        defined(json!([row("resolved")]), gap(9), json!([])),
        defined(json!([row("candidate")]), gap(5), json!([])),
        defined(
            json!([charge("resolved", "reference", None)]),
            gap(5),
            json!([]),
        ),
        defined(
            json!([row("resolved")]),
            json!([]),
            json!([{"path": "shop/other.py", "lines": []}]),
        ),
    ];
    let slipped: Vec<String> = twice
        .iter()
        .filter(|doc| said(doc).is_empty())
        .map(|doc| format!("a double standing went unnoticed: {doc}"))
        .chain(
            once.iter()
                .filter(|doc| !said(doc).is_empty())
                .map(|doc| format!("a single standing was refused: {doc}")),
        )
        .collect();
    if slipped.is_empty() {
        Ok(())
    } else {
        Err(Failed::from(slipped.join("\n")))
    }
}

pub(crate) fn trials() -> Vec<Trial> {
    let rules: [(&str, fn() -> Result<(), Failed>); 14] = [
        (
            "a1_a_named_test_function_needs_the_answer_to_name_it",
            a1_a_named_test_function_needs_the_answer_to_name_it,
        ),
        (
            "a2_only_a_removed_api_contract_is_a_removal",
            a2_only_a_removed_api_contract_is_a_removal,
        ),
        (
            "a2b_only_a_listed_deleted_module_is_a_removal",
            a2b_only_a_listed_deleted_module_is_a_removal,
        ),
        ("a3_a_name_match_is_no_caller", a3_a_name_match_is_no_caller),
        (
            "a4_kind_is_loose_but_the_rest_is_not",
            a4_kind_is_loose_but_the_rest_is_not,
        ),
        (
            "a5_a_collection_read_reaches_only_its_collection",
            a5_a_collection_read_reaches_only_its_collection,
        ),
        ("a6_a_stated_kind_is_compared", a6_a_stated_kind_is_compared),
        (
            "a_kind_no_schema_names_cannot_be_evaluated",
            a_kind_no_schema_names_cannot_be_evaluated,
        ),
        (
            "the_engines_kinds_the_harnesss_and_the_schemas_are_one_vocabulary",
            the_engines_kinds_the_harnesss_and_the_schemas_are_one_vocabulary,
        ),
        (
            "a_static_reexport_is_a_resolved_reexports_relation",
            a_static_reexport_is_a_resolved_reexports_relation,
        ),
        (
            "a_contract_entry_that_is_no_file_fails_the_load",
            a_contract_entry_that_is_no_file_fails_the_load,
        ),
        (
            "a_zero_beside_not_determined_is_a_claim",
            a_zero_beside_not_determined_is_a_claim,
        ),
        ("a7_a_folded_row_is_no_caller", a7_a_folded_row_is_no_caller),
        ("a_site_has_one_standing", a_site_has_one_standing),
    ];
    rules
        .into_iter()
        .map(|(name, test)| {
            Trial::test(format!("harness/negative/{name}"), test).with_kind("oracle")
        })
        .collect()
}
