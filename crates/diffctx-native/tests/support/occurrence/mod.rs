//! The seeds of the occurrence classes and what an answer must say about
//! them. A seed is one repository: the scaffold of its family (one changed
//! definition, no user anywhere) plus the files the seed adds. The seeds are
//! data under `seeds/`, one table per class, run by one test per class.

// Every suite compiles this module and uses a part of it.
#![allow(dead_code)]

pub mod corpus;

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::Deserialize;
use serde_json::Value;

use crate::support::families::{
    Family, Files, Planted, Query, Repo, discloses, names_link, says_empty,
};

pub const SCAFFOLDS: &str = include_str!("seeds/scaffolds.yaml");

#[derive(Deserialize)]
pub struct Scaffold {
    pub definition: String,
    pub symbol: String,
    pub base: Files,
    pub change: Files,
}

pub fn scaffolds() -> &'static BTreeMap<String, Scaffold> {
    static ALL: OnceLock<BTreeMap<String, Scaffold>> = OnceLock::new();
    ALL.get_or_init(|| serde_yaml::from_str(SCAFFOLDS).expect("scaffolds.yaml"))
}

// The families that have no scaffold, with the reason: a full match, so a
// family added to the engine's list is a compile error here until it has a
// scaffold or a reason.
pub fn no_scaffold(family: Family) -> Option<&'static str> {
    match family {
        Family::Python
        | Family::TypeScript
        | Family::Rust
        | Family::Kotlin
        | Family::Java
        | Family::Go
        | Family::Php
        | Family::C
        | Family::Shell
        | Family::Swift => None,
        Family::Config | Family::Terraform | Family::Dockerfile | Family::Sql => {
            Some("no clean absence to state: its answers are read through the gap sections")
        }
    }
}

// A row of a seed table. `family` seeds add `files` to that family's scaffold;
// the others are whole repositories (`base`, then `change`).
#[derive(Deserialize)]
pub struct Seed {
    pub name: String,
    #[serde(default)]
    pub family: String,
    #[serde(default)]
    pub kind: String,
    // The file that uses the definition.
    #[serde(default)]
    pub user: String,
    #[serde(default)]
    pub symbol: String,
    #[serde(default)]
    pub files: Files,
    #[serde(default)]
    pub base: Files,
    #[serde(default)]
    pub change: Files,
}

pub fn load(table: &str) -> Vec<Seed> {
    serde_yaml::from_str(table).expect("a seed table")
}

impl Seed {
    pub fn repo(&self) -> Repo {
        if self.family.is_empty() {
            // The family names the group of a scaffold seed; an impact answer
            // reads none.
            return Repo {
                family: Family::Python,
                base: self.base.clone(),
                change: self.change.clone(),
                caller: None,
                definition: String::new(),
                symbol: self.symbol.clone(),
            };
        }
        let scaffold = &scaffolds()[&self.family];
        let mut base = scaffold.base.clone();
        base.extend(self.files.clone());
        Repo {
            family: Family::ALL
                .into_iter()
                .find(|f| f.label() == self.family)
                .expect("a family of the engine"),
            base,
            change: scaffold.change.clone(),
            caller: None,
            definition: scaffold.definition.clone(),
            symbol: scaffold.symbol.clone(),
        }
    }
}

// Every scaffold family has a seed in the table, but the ones named in `except`
// (each with its reason).
pub fn covers(class: &str, seeds: &[Seed], except: &[(&str, &str)]) {
    let missing: Vec<&String> = scaffolds()
        .keys()
        .filter(|f| !except.iter().any(|(e, _)| e == f))
        .filter(|f| !seeds.iter().any(|s| &s.family == *f))
        .collect();
    assert!(missing.is_empty(), "{class}: no seed for {missing:?}");
}

// ---- reading an answer ----

fn rows(value: &Value) -> &[Value] {
    value.as_array().map_or(&[], Vec::as_slice)
}

fn is_symbol(found: Option<&str>, symbol: &str) -> bool {
    found.is_some_and(|f| f == symbol || f.ends_with(&format!(".{symbol}")))
}

fn definition<'a>(doc: &'a Value, symbol: &str) -> Option<&'a Value> {
    rows(&doc["definitions"])
        .iter()
        .find(|row| is_symbol(row["symbol"].as_str(), symbol))
}

// A relation of the definition says it was not read to the end.
fn relation_incomplete(row: &Value) -> bool {
    row["coverage"]
        .as_object()
        .is_some_and(|all| all.values().any(|c| c["complete"] == false))
}

// The answer names the file, or says what it could not read: never silence.
pub fn accounts_for(doc: &Value, file: &str) -> bool {
    names_link(doc, file)
        || (!says_empty(doc)
            && (discloses(doc) || rows(&doc["definitions"]).iter().any(relation_incomplete)))
}

pub fn calls_complete(doc: &Value, symbol: &str) -> Option<bool> {
    definition(doc, symbol)?["coverage"]["calls"]["complete"].as_bool()
}

// The answer says nothing reaches the definition: no caller, gap or partial
// relation on its row, or, with no row, no list of entries that names it.
pub fn says_nobody(doc: &Value, symbol: &str) -> bool {
    match definition(doc, symbol) {
        Some(row) => {
            !["rows", "gaps", "missed_in"]
                .iter()
                .any(|k| !rows(&row[k]).is_empty())
                && !relation_incomplete(row)
        }
        None => !["entered_by", "called_in_diff", "layout_only"]
            .iter()
            .any(|list| {
                rows(&doc["unlisted"][list]["items"]).iter().any(|item| {
                    is_symbol(
                        item["symbol"]
                            .as_str()
                            .or_else(|| item["def"]["symbol"].as_str()),
                        symbol,
                    )
                })
            }),
    }
}

// ---- running seeds ----

// The failures of `check` over the items, a few at a time: one process each.
fn sweep<T: Sync>(items: &[T], check: impl Fn(&T) -> Result<(), String> + Sync) -> Vec<String> {
    let width = std::thread::available_parallelism().map_or(4, |n| n.get().clamp(2, 8));
    let check = &check;
    items
        .chunks(width)
        .flat_map(|batch| {
            std::thread::scope(|scope| {
                let runs: Vec<_> = batch
                    .iter()
                    .map(|item| scope.spawn(move || check(item)))
                    .collect();
                runs.into_iter()
                    .filter_map(|run| run.join().expect("seed run").err())
                    .collect::<Vec<_>>()
            })
        })
        .collect()
}

// Asks the impact question of every seed and fails with each seed `judge`
// rejects, named with the start of its answer.
pub fn run_seeds(
    class: &str,
    seeds: &[Seed],
    judge: impl Fn(&Seed, &Value) -> Result<(), String> + Sync,
) {
    let failures = sweep(seeds, |seed| {
        let doc = Planted::new(seed.repo())
            .ask(Query::Impact, &[])
            .map_err(|e| format!("{}: {e}", seed.name))?;
        judge(seed, &doc).map_err(|why| {
            let brief: String = doc.to_string().chars().take(500).collect();
            format!("{} [{}]: {why}\n    {brief}", seed.name, seed.family)
        })
    });
    assert!(
        failures.is_empty(),
        "{class}: {} of {} seeds fail\n{}",
        failures.len(),
        seeds.len(),
        failures.join("\n")
    );
}
