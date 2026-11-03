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
    Ok(judge("negative", &case, &compiled, "python", &extract(answer)).0)
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
        "changed": [{
            "path": "shop/pricing.py", "symbol": "total", "kind": "function",
            "lines": "1-2", "callers": callers,
        }],
        "changed_files": ["shop/pricing.py"],
        "empty": false,
        "tests_known": true,
    })
}

fn charge(evidence: &str, relation: &str, tested_by: Option<&str>) -> Value {
    let mut caller = json!({
        "path": "shop/checkout.py", "symbol": "charge", "lines": "4-5",
        "evidence": evidence, "relation": relation, "sites": [5],
    });
    if let Some(test) = tested_by {
        caller["tested_by"] = json!(test);
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
        "changed": [], "changed_files": ["api/schema.py"], "empty": false,
        "contracts": [{"path": path, "symbol": "Row", "kind": kind}],
    })
}

fn collection_read(path: &str, collection: &str) -> Value {
    json!({
        "changed": [], "changed_files": ["content/blog/post.md"], "empty": false,
        "collection_reads": [{"path": path, "line": 3, "collection": collection}],
    })
}

/// A1: a test expectation names a test function, the engine only the test file.
fn a1_test_file_stands_for_its_function() -> Result<(), Failed> {
    run(vec![
        Probe {
            what: "the test file the answer names is the expected one",
            case: CHARGE_IS_CALLED_AND_TESTED,
            answer: answer(&json!([charge(
                "resolved",
                "call",
                Some("tests/test_checkout.py")
            )])),
            verdict: Verdict::Passes,
        },
        Probe {
            what: "another test file reaches the caller",
            case: CHARGE_IS_CALLED_AND_TESTED,
            answer: answer(&json!([charge(
                "resolved",
                "call",
                Some("tests/test_other.py")
            )])),
            verdict: Verdict::Fails("missing tests/test_checkout.py"),
        },
        Probe {
            what: "no test reaches the caller",
            case: CHARGE_IS_CALLED_AND_TESTED,
            answer: answer(&json!([charge("resolved", "call", None)])),
            verdict: Verdict::Fails("missing tests/test_checkout.py"),
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

/// A3: a caller found only by its name is textual and yields no relation.
fn a3_a_name_match_is_no_caller() -> Result<(), Failed> {
    run(vec![Probe {
        what: "the only row for the expected caller is a word match",
        case: CHARGE_CALLS_TOTAL,
        answer: answer(&json!([charge("name", "call", None)])),
        verdict: Verdict::Fails("missing shop/checkout.py::charge -calls->"),
    }])
}

/// A4: code kinds are compared on (from, to, resolution) while no cell is precise.
fn a4_kind_is_loose_but_the_rest_is_not() -> Result<(), Failed> {
    run(vec![
        Probe {
            what: "a read stands for the expected call while no cell is precise",
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
                "evidence": "resolved", "relation": "call", "sites": [5],
            }])),
            verdict: Verdict::Fails("missing shop/checkout.py::charge -calls->"),
        },
        Probe {
            what: "the caller reads it on another line",
            case: CHARGE_CALLS_TOTAL,
            answer: answer(&json!([{
                "path": "shop/checkout.py", "symbol": "charge", "lines": "4-9",
                "evidence": "resolved", "relation": "call", "sites": [8],
            }])),
            verdict: Verdict::Fails("missing shop/checkout.py::charge -calls->"),
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

pub fn trials() -> Vec<Trial> {
    let rules: [(&str, fn() -> Result<(), Failed>); 6] = [
        (
            "a1_test_file_stands_for_its_function",
            a1_test_file_stands_for_its_function,
        ),
        (
            "a2_only_a_removed_api_contract_is_a_removal",
            a2_only_a_removed_api_contract_is_a_removal,
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
        (
            "a_zero_beside_not_determined_is_a_claim",
            a_zero_beside_not_determined_is_a_claim,
        ),
    ];
    rules
        .into_iter()
        .map(|(name, test)| {
            Trial::test(format!("harness/negative/{name}"), test).with_kind("oracle")
        })
        .collect()
}
