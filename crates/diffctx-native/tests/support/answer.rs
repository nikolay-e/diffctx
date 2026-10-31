// The parts of an answer the suites compare: the relation set of an impact
// answer, what it discloses, and the fragment and item sets of pack and locate.

use std::collections::BTreeSet;

use serde_json::Value;

fn rows(value: &Value) -> impl Iterator<Item = &Value> {
    value.as_array().into_iter().flatten()
}

fn text(value: &Value) -> &str {
    value.as_str().unwrap_or("")
}

// Every caller of every changed definition, the test that reaches it, the
// callers an answer says it could not resolve, and the stale references:
// the claims an impact answer makes. Line numbers and weights are left out,
// so a transformation that moves code does not read as a changed answer.
pub fn relations(doc: &Value) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for changed in rows(&doc["changed"]) {
        let target = format!("{}::{}", text(&changed["path"]), text(&changed["symbol"]));
        for caller in rows(&changed["callers"]) {
            let who = format!("{}::{}", text(&caller["path"]), text(&caller["symbol"]));
            let relation = caller["relation"].as_str().unwrap_or("call");
            let evidence = text(&caller["evidence"]);
            out.insert(format!("{target} <- {who} [{relation}, {evidence}]"));
            if let Some(test) = caller["tested_by"].as_str() {
                out.insert(format!("{who} is tested by {test}"));
            }
        }
        let unresolved = &changed["unresolved"];
        if !unresolved.is_null() {
            out.insert(format!("{target} has unresolved callers: {unresolved}"));
        }
    }
    for stale in rows(&doc["stale_references"]) {
        out.insert(format!("stale {}", text(&stale["path"])));
    }
    out
}

// The definitions the answer lists, with their kind.
pub fn changed_rows(doc: &Value) -> BTreeSet<String> {
    rows(&doc["changed"])
        .map(|c| {
            format!(
                "{}::{} ({})",
                text(&c["path"]),
                text(&c["symbol"]),
                text(&c["kind"])
            )
        })
        .collect()
}

// Definitions the answer says the change removed.
pub fn removed_rows(doc: &Value) -> Vec<String> {
    rows(&doc["changed"])
        .filter(|c| c["kind"] == "removed")
        .map(|c| format!("{}::{}", text(&c["path"]), text(&c["symbol"])))
        .collect()
}

// Whether an impact answer said it did not see everything.
pub fn impact_disclosed(doc: &Value) -> bool {
    let listed = |key: &str| doc[key].as_array().is_some_and(|a| !a.is_empty());
    listed("limits") || listed("unanalysed_files") || doc["withheld_files"].as_u64() > Some(0)
}

// What an impact answer says about its own completeness, in one comparable
// line. Counts, not names: a renamed file keeps its disclosure.
pub fn disclosure(doc: &Value) -> String {
    format!(
        "empty={} limits={} unanalysed={} withheld={}",
        doc["empty"],
        doc["limits"],
        rows(&doc["unanalysed_files"]).count(),
        doc["withheld_files"].as_u64().unwrap_or(0)
    )
}

// Whether a pack or locate answer carries a coverage block that owns up to a
// loss. A clean run omits the block.
pub fn coverage_disclosed(doc: &Value) -> bool {
    let coverage = &doc["coverage"];
    let listed = |key: &str| coverage[key].as_array().is_some_and(|a| !a.is_empty());
    coverage.is_object()
        && (coverage["status"].as_str().is_some_and(|s| s != "complete")
            || listed("limit_reasons")
            || listed("unparsed_files"))
}

// The fragments a pack answer selected.
pub fn fragments(doc: &Value) -> BTreeSet<String> {
    rows(&doc["fragments"])
        .map(|f| format!("{}:{}", text(&f["path"]), text(&f["lines"])))
        .collect()
}

// The items a locate answer ranked.
pub fn items(doc: &Value) -> BTreeSet<String> {
    rows(&doc["items"])
        .map(|i| format!("{}:{}", text(&i["path"]), text(&i["lines"])))
        .collect()
}

const SECTIONS: [&str; 14] = [
    "changed_files",
    "untracked_files",
    "deleted_files",
    "stale_references",
    "named_by",
    "key_searches",
    "collection_reads",
    "contracts",
    "untested_unlisted",
    "untested_called_in_diff",
    "entry_unlisted",
    "layout_only",
    "lockfile_changes",
    "unanalysed_files",
];

// How many entries each section of an impact answer holds, callers included.
pub fn section_sizes(doc: &Value) -> Vec<(String, usize)> {
    let mut sizes: Vec<(String, usize)> = SECTIONS
        .iter()
        .map(|name| ((*name).to_string(), rows(&doc[*name]).count()))
        .collect();
    sizes.push(("changed".to_string(), rows(&doc["changed"]).count()));
    sizes.push((
        "callers".to_string(),
        rows(&doc["changed"])
            .map(|c| rows(&c["callers"]).count())
            .sum(),
    ));
    sizes
}

// Lines of `want` and `got` that differ, for a failure message.
pub fn difference(want: &BTreeSet<String>, got: &BTreeSet<String>) -> String {
    let missing: Vec<&String> = want.difference(got).collect();
    let extra: Vec<&String> = got.difference(want).collect();
    format!("missing: {missing:#?}\nextra: {extra:#?}")
}
