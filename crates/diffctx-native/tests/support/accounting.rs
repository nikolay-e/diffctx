// The property every impact answer holds: each changed definition the engine
// took up is named, in a row of `definitions` or in an item of an unlisted list.
// `summary.traced` counts the definitions the engine's listing loops took up,
// layout-only ones apart (those are always listed, in `unlisted.layout_only`);
// this reads the JSON the binary printed, so an engine that drops a definition
// cannot also drop it from the count it is held to.
//
// Not judged: a symbol query (it names a definition asked for), a run that
// stopped (its limits and the rows it left unfinished say what was not read) and
// an answer the cap fitted (`omitted` and the shed names are its own account).

use std::collections::BTreeMap;

use serde_json::Value;

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}

fn items<'a>(value: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    value[key].as_array().into_iter().flatten()
}

fn named(def: &Value) -> String {
    format!(
        "{}::{} ({})",
        text(def, "path"),
        text(def, "symbol"),
        text(def, "lines")
    )
}

fn judged(doc: &Value) -> bool {
    // A released binary is judged by the corpus it shipped with.
    if std::env::var_os("DIFFCTX_BIN").is_some() {
        return false;
    }
    let cap = doc["fit"]["cap"].as_u64().unwrap_or(0);
    let needs = doc["fit"]["needs"].as_u64().unwrap_or(0);
    let stopped =
        items(doc, "definitions").any(|d| items(d, "gaps").any(|g| g["kind"] == "stopped"));
    doc["range"]["kind"] != "symbol"
        && doc["omitted"].is_null()
        && needs <= cap
        && !stopped
        && items(&doc["coverage"], "limits").next().is_none()
        && doc["coverage"]["not_searched"].as_u64().unwrap_or(0) == 0
}

// The definitions the engine took up that the answer names nowhere: how many,
// and the names it does give so a reader can see which rows are missing.
pub fn lost_definitions(doc: &Value) -> Option<String> {
    if !judged(doc) {
        return None;
    }
    let traced = usize::try_from(doc["summary"]["traced"].as_u64().unwrap_or(0)).unwrap_or(0);
    let rows: Vec<&Value> = items(doc, "definitions")
        .filter(|d| d["change"]["kind"] != "removed")
        .collect();
    let unlisted = &doc["unlisted"];
    // A definition a test does not reach and only the diff calls stands in a row
    // and in `called_in_diff`: it is one definition named twice.
    let mut rowed: BTreeMap<String, usize> = BTreeMap::new();
    for row in &rows {
        *rowed.entry(named(row)).or_default() += 1;
    }
    let mut also_listed = 0;
    for item in items(&unlisted["called_in_diff"], "items") {
        if let Some(n) = rowed.get_mut(&named(item)).filter(|n| **n > 0) {
            *n -= 1;
            also_listed += 1;
        }
    }
    let listed = |key: &str| items(&unlisted[key], "items").count();
    let entered = items(&unlisted["entered_by"], "items").count();
    let called = listed("called_in_diff") - also_listed;
    let accounted = rows.len() + listed("no_caller") + entered + called;
    (accounted < traced).then(|| {
        format!(
            "the answer traced {traced} changed definition(s) and names {accounted}: {} row(s), \
             {} in no_caller, {entered} in entered_by, {called} in called_in_diff; \
             rows: {:?}",
            rows.len(),
            listed("no_caller"),
            rows.iter().map(|r| named(r)).collect::<Vec<_>>()
        )
    })
}
