// The property every impact answer holds: no two rows state one fact. The rows
// of a definition are keyed as `answer::fact_key` keys them; this reads the JSON
// the binary printed, not the engine's types, so a regression in the key
// function cannot hide itself. A site has one standing as well: the line a row
// lists is not also named by a gap as a use no binding settles, and a file with a
// row is not also listed as unread.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}

fn items<'a>(value: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    value[key].as_array().into_iter().flatten()
}

// The line a word match cites: its first site, else where its function starts.
fn cited(row: &Value) -> String {
    row["sites"][0].as_u64().map_or_else(
        || {
            text(row, "lines")
                .split('-')
                .next()
                .unwrap_or("")
                .to_string()
        },
        |line| line.to_string(),
    )
}

// Every fact the answer states more than once, one line each.
pub(crate) fn repeated_facts(doc: &Value) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut repeated = Vec::new();
    for def in items(doc, "definitions") {
        let symbol = def["symbol"].as_str();
        let word = symbol.unwrap_or_else(|| text(def, "path"));
        let word = word.split('[').next().unwrap_or(word);
        let definition = format!(
            "{}::{} {}",
            text(def, "path"),
            symbol.unwrap_or(""),
            text(def, "lines")
        );
        for row in items(def, "rows") {
            // A word match states that a line spells the word, whichever of the
            // definitions of that name it was found for; any other row states a
            // use of the one definition it is listed under.
            let key = if text(row, "standing") == "name" {
                format!("{}:{} names {word}", text(row, "path"), cited(row))
            } else {
                format!(
                    "{definition} <- {}::{} {} {:?} {} {} {}",
                    text(row, "path"),
                    text(row, "symbol"),
                    text(row, "lines"),
                    row["sites"],
                    text(row, "class"),
                    text(row, "kind"),
                    text(row, "standing"),
                )
            };
            if !seen.insert(key.clone()) {
                repeated.push(key);
            }
        }
        for file in items(def, "missed_in") {
            let key = format!(
                "{}:{} names {word} (unread)",
                text(file, "path"),
                file["lines"][0].as_u64().unwrap_or(0)
            );
            if !seen.insert(key.clone()) {
                repeated.push(key);
            }
        }
        repeated.extend(
            double_standings(def)
                .into_iter()
                .map(|site| format!("{definition}: {site}")),
        );
    }
    repeated
}

// The sites of one definition that stand twice: a line two rows list under
// different standings, a line a resolved row lists that a gap names as a use
// nothing settles, a file a row stands in that is listed as unread. A candidate
// row and the gap that discloses it name one standing, not two.
fn double_standings(def: &Value) -> Vec<String> {
    let mut standing: BTreeMap<(String, u64), BTreeSet<String>> = BTreeMap::new();
    let mut rowed = BTreeSet::new();
    for row in items(def, "rows") {
        let path = text(row, "path").to_string();
        for line in items(row, "sites").filter_map(Value::as_u64) {
            standing
                .entry((path.clone(), line))
                .or_default()
                .insert(text(row, "standing").to_string());
        }
        rowed.insert(path);
    }
    let mut twice = Vec::new();
    for ((path, line), standings) in &standing {
        if standings.len() > 1 {
            twice.push(format!("{path}:{line} stands as {standings:?}"));
        }
    }
    for gap in items(def, "gaps") {
        for site in items(gap, "sites") {
            let (path, line) = (text(site, "path"), site["line"].as_u64().unwrap_or(0));
            let settled = standing
                .get(&(path.to_string(), line))
                .is_some_and(|standings| standings.contains("resolved"));
            if settled {
                twice.push(format!("{path}:{line} is a row and a use nothing settles"));
            }
        }
    }
    for file in items(def, "missed_in") {
        if rowed.contains(text(file, "path")) {
            twice.push(format!(
                "{} has a row and is listed unread",
                text(file, "path")
            ));
        }
    }
    twice
}
