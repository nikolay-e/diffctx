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

// The entries of a `{total, items}` listing of an impact answer.
fn listed<'a>(value: &'a Value) -> impl Iterator<Item = &'a Value> {
    rows(&value["items"])
}

// What a row's evidence was in the claims the suites compare: a re-export is
// counted apart from a resolved caller.
fn evidence(row: &Value) -> &str {
    if row["class"] == "reexports" {
        "reexport"
    } else {
        text(&row["standing"])
    }
}

// Every caller of every changed definition, the test that reaches it, the
// gaps an answer says it could not close, and the stale references: the claims
// an impact answer makes. Line numbers are left out, so a transformation that
// moves code does not read as a changed answer.
pub fn relations(doc: &Value) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for changed in rows(&doc["definitions"]) {
        let target = format!("{}::{}", text(&changed["path"]), text(&changed["symbol"]));
        for caller in rows(&changed["rows"]) {
            let who = format!("{}::{}", text(&caller["path"]), text(&caller["symbol"]));
            let class = caller["class"].as_str().unwrap_or("call");
            out.insert(format!("{target} <- {who} [{class}, {}]", evidence(caller)));
            if let Some(test) = caller["guard"]["test"]
                .as_str()
                .filter(|_| caller["guard"]["state"] == "linked")
            {
                out.insert(format!("{who} is tested by {test}"));
            }
        }
        // Files that spell the name but were not read are named by `missed_in`.
        for gap in rows(&changed["gaps"]).filter(|g| g["kind"] != "unaccounted") {
            out.insert(format!("{target} has unresolved callers: {gap}"));
        }
    }
    for stale in listed(&doc["stale"]) {
        out.insert(format!("stale {}", text(&stale["path"])));
    }
    out
}

// The definitions the answer lists, with their kind (or what became of them).
pub fn changed_rows(doc: &Value) -> BTreeSet<String> {
    rows(&doc["definitions"])
        .map(|c| {
            let kind = c["kind"]
                .as_str()
                .unwrap_or_else(|| text(&c["change"]["kind"]));
            format!("{}::{} ({kind})", text(&c["path"]), text(&c["symbol"]))
        })
        .collect()
}

// Definitions the answer says the change removed.
pub fn removed_rows(doc: &Value) -> Vec<String> {
    rows(&doc["definitions"])
        .filter(|c| c["change"]["kind"] == "removed")
        .map(|c| format!("{}::{}", text(&c["path"]), text(&c["symbol"])))
        .collect()
}

// Whether an impact answer said it did not see everything.
pub fn impact_disclosed(doc: &Value) -> bool {
    doc["coverage"]["limits"]
        .as_array()
        .is_some_and(|a| !a.is_empty())
        || doc["coverage"]["unanalysed"]["total"].as_u64() > Some(0)
        || doc["inventory"]["withheld"].as_u64() > Some(0)
}

// What an impact answer says about its own completeness, in one comparable
// line. Counts, not names: a renamed file keeps its disclosure.
pub fn disclosure(doc: &Value) -> String {
    format!(
        "empty={} limits={} unanalysed={} withheld={}",
        doc["empty"],
        doc["coverage"]["limits"],
        doc["coverage"]["unanalysed"]["total"].as_u64().unwrap_or(0),
        doc["inventory"]["withheld"].as_u64().unwrap_or(0)
    )
}

// Whether a pack or locate answer carries a coverage block that owns up to a
// loss. A clean run omits the block.
pub fn coverage_disclosed(doc: &Value) -> bool {
    let coverage = &doc["coverage"];
    let listed = |key: &str| coverage[key].as_array().is_some_and(|a| !a.is_empty());
    coverage.is_object()
        && (coverage["status"].as_str().is_some_and(|s| s != "complete")
            || listed("limits")
            || coverage["selection"]["unparsed_files"]
                .as_array()
                .is_some_and(|a| !a.is_empty()))
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

// The sections of an impact answer, each named as it was in v1, with where its
// entries are in v2.
const SECTIONS: [(&str, &[&str]); 14] = [
    ("changed_files", &["/inventory/changed/items"]),
    ("untracked_files", &["/inventory/untracked/items"]),
    ("deleted_files", &["/inventory/deleted/items"]),
    ("stale_references", &["/stale/items"]),
    ("named_by", &["/named/by_path/items", "/named/by_key/items"]),
    ("key_searches", &["/named/key_searches"]),
    ("collection_reads", &["/named/collections/items"]),
    ("contracts", &["/contracts/items"]),
    ("untested_unlisted", &["/unlisted/no_caller/items"]),
    (
        "untested_called_in_diff",
        &["/unlisted/called_in_diff/items"],
    ),
    ("entry_unlisted", &["/unlisted/entered_by/items"]),
    ("layout_only", &["/unlisted/layout_only/items"]),
    ("lockfile_changes", &["/inventory/lockfiles/items"]),
    ("unanalysed_files", &["/coverage/unanalysed/items"]),
];

// How many entries each section of an impact answer holds, callers included.
pub fn section_sizes(doc: &Value) -> Vec<(String, usize)> {
    let mut sizes: Vec<(String, usize)> = SECTIONS
        .iter()
        .map(|(name, at)| {
            let size = at
                .iter()
                .map(|p| doc.pointer(p).map_or(0, |v| rows(v).count()))
                .sum();
            ((*name).to_string(), size)
        })
        .collect();
    sizes.push(("changed".to_string(), rows(&doc["definitions"]).count()));
    sizes.push((
        "callers".to_string(),
        rows(&doc["definitions"])
            .map(|c| rows(&c["rows"]).count())
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
