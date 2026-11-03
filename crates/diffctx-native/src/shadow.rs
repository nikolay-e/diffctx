//! `--shadow` (hidden): per changed file, the changed definitions the impact path
//! reports beside those the facts layer reports, from one run of one range. A
//! measuring tool for the migration, not an interface: names are
//! `path::Container.name`, compared as sets, and nothing here judges which side
//! is right.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Result;
use rustc_hash::FxHashMap;
use serde_json::{Value, json};

use crate::container::named_container_of;
use crate::facts::{DefChange, DefKey, FactsCache, SemanticDef, semantic_changes};
use crate::impact::{ImpactOutput, build_impact, enclosing_class};
use crate::mode::ScoringMode;
use crate::paths::display_rel_or_abs;
use crate::pipeline::{ScoredState, compute_scored_state_with};
use crate::snapshot::{self, AnalysisContext, Loss, RangeSpec, RunBudget};
use crate::types::{Fragment, FragmentId};

/// Enclosing definitions kept per name; a class chain this deep is a cycle.
const MAX_CONTAINER_DEPTH: usize = 16;
const PLACEHOLDER_PATH: &str = "<unplaced>";

pub fn report(
    root_dir: &Path,
    range: Option<&str>,
    alpha: f64,
    scoring_mode: ScoringMode,
    timeout: u64,
) -> Result<Value> {
    let state =
        compute_scored_state_with(root_dir, range, &[], alpha, scoring_mode, timeout, true)?;
    let deadline = Instant::now() + Duration::from_secs(timeout);
    let impact = build_impact(&state, range, None, Some(deadline));
    let ctx = context(&state, range)?;
    let cache = FactsCache::in_memory();
    let set = semantic_changes(&ctx, &cache)?;

    let mut files: BTreeMap<String, Row> = BTreeMap::new();
    for (path, names) in old_names(&state, &impact) {
        files.entry(path).or_default().old = names;
    }
    let mut run_losses: Vec<&Loss> = Vec::new();
    for def in &set.defs {
        let path = def_path(def);
        let name = qualified(&path, &def.key);
        let row = files.entry(path).or_default();
        row.facts.insert(name.clone());
        row.detail.insert(name, describe(&def.change));
        if let DefChange::Unknown { loss } = &def.change {
            row.lose(loss);
        }
    }
    for loss in &set.losses {
        match loss.path() {
            Some(path) => files
                .entry(path.as_str().to_string())
                .or_default()
                .lose(loss),
            None => run_losses.push(loss),
        }
    }

    let rows: Vec<Value> = files.iter().map(|(path, row)| row.to_json(path)).collect();
    let total = |count: fn(&Row) -> usize| files.values().map(count).sum::<usize>();
    Ok(json!({
        "range": range,
        "extractors": extractors(&cache),
        "files": rows,
        "losses": run_losses.iter().map(|loss| loss_json(loss)).collect::<Vec<_>>(),
        "totals": {
            "files": files.len(),
            "old": total(|row| row.old.len()),
            "facts": total(|row| row.facts.len()),
            "only_old": total(|row| row.only_old().len()),
            "only_facts": total(|row| row.only_facts().len()),
            "facts_losses": total(|row| row.losses.len()),
        },
    }))
}

#[derive(Default)]
struct Row {
    old: BTreeSet<String>,
    facts: BTreeSet<String>,
    detail: BTreeMap<String, String>,
    losses: BTreeMap<String, Value>,
}

impl Row {
    fn only_old(&self) -> Vec<&String> {
        self.old.difference(&self.facts).collect()
    }

    fn only_facts(&self) -> Vec<&String> {
        self.facts.difference(&self.old).collect()
    }

    fn lose(&mut self, loss: &Loss) {
        self.losses.insert(loss.to_string(), loss_json(loss));
    }

    fn to_json(&self, path: &str) -> Value {
        json!({
            "path": path,
            "old": self.old,
            "facts": self.facts,
            "only_old": self.only_old(),
            "only_facts": self.only_facts(),
            "facts_losses": self.losses.values().collect::<Vec<_>>(),
            "facts_detail": self.detail,
        })
    }
}

fn loss_json(loss: &Loss) -> Value {
    json!({ "kind": loss.kind(), "message": loss.to_string() })
}

fn extractors(cache: &FactsCache) -> Vec<&'static str> {
    cache
        .registry()
        .languages()
        .map(|language| language.name())
        .collect()
}

/// The second context of the run: the same range resolved the way the analysis
/// the impact path read resolved it, under the same deadline and cancellation.
fn context(state: &ScoredState, range: Option<&str>) -> Result<AnalysisContext> {
    let repo = snapshot::open(&state.root_dir)?;
    let spec = range.map_or("HEAD", str::trim);
    let resolved = match snapshot::resolve(&repo, spec)? {
        RangeSpec::Staged { base } => RangeSpec::Review {
            base,
            head: snapshot::index_tree(&repo)?,
        },
        other => other,
    };
    let (deadline, cancel) = state.run.io_limits();
    let budget = RunBudget {
        deadline,
        cancel: Some(cancel),
        read_fault: None,
    };
    Ok(AnalysisContext::new(repo, resolved, budget)?)
}

/// The definitions the impact path considers changed: the named container of each
/// core fragment, as `build_impact` takes them, by path; plus the removed ones it
/// reported (the base side has no fragments here). Named `path::Container.name`:
/// the fragmenter's `symbol_name` is the bare name, so the enclosing definitions
/// are put in front.
fn old_names(state: &ScoredState, output: &ImpactOutput) -> BTreeMap<String, BTreeSet<String>> {
    let by_id: FxHashMap<&FragmentId, &Fragment> =
        state.all_fragments.iter().map(|f| (&f.id, f)).collect();
    let mut by_path: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut add = |path: String, name: String| {
        by_path
            .entry(path.clone())
            .or_default()
            .insert(format!("{path}::{name}"));
    };
    for core in state
        .core_ids
        .iter()
        .filter_map(|id| by_id.get(id).copied())
        .filter_map(|f| named_container_of(f, &state.all_fragments))
    {
        let Some(symbol) = core.symbol_name.as_deref() else {
            continue;
        };
        let path = display_rel_or_abs(&state.root_dir, Path::new(core.path()));
        add(path, qualified_symbol(core, symbol, &state.all_fragments));
    }
    for removed in output.changed.iter().filter(|c| c.kind == "removed") {
        if let Some(symbol) = &removed.symbol {
            add(removed.path.clone(), symbol.clone());
        }
    }
    by_path
}

/// `symbol` with the names of the definitions that enclose `core` in its file,
/// outermost first. A class fragment is its header alone, its methods cut on
/// their own, so a class is found the way the impact path finds it
/// (`enclosing_class`); a function around a nested one is found by span.
fn qualified_symbol(core: &Fragment, symbol: &str, all: &[Fragment]) -> String {
    let bare = |name: &str| name.split('[').next().unwrap_or(name).to_string();
    let file: Vec<&Fragment> = all.iter().filter(|f| f.path() == core.path()).collect();
    let mut outer: Vec<&Fragment> = file
        .iter()
        .copied()
        .filter(|f| {
            f.kind.is_definition_kind()
                && f.symbol_name.is_some()
                && f.id.start_line <= core.id.start_line
                && f.id.end_line >= core.id.end_line
                && (f.id.start_line, f.id.end_line) != (core.id.start_line, core.id.end_line)
        })
        .collect();
    let mut current = core;
    while let Some(class) = enclosing_class(current, &file) {
        if outer.len() > MAX_CONTAINER_DEPTH {
            break;
        }
        outer.push(class);
        current = class;
    }
    outer.sort_by_key(|f| (f.id.start_line, Reverse(f.id.end_line)));
    outer.dedup_by(|a, b| a.id == b.id);
    let mut name = String::new();
    for container in outer {
        name.push_str(&bare(container.symbol_name.as_deref().unwrap_or_default()));
        name.push('.');
    }
    let symbol = bare(symbol);
    if symbol.starts_with(&name) {
        name.clear();
    }
    name.push_str(&symbol);
    name
}

/// The file a definition's row belongs to: the base for a removal, the head for
/// the rest, and whichever side an `Unknown` was found on.
fn def_path(def: &SemanticDef) -> String {
    let (first, second) = match def.change {
        DefChange::Removed => (&def.base, &def.head),
        _ => (&def.head, &def.base),
    };
    first.as_ref().or(second.as_ref()).map_or_else(
        || PLACEHOLDER_PATH.to_string(),
        |path| path.as_str().to_string(),
    )
}

fn qualified(path: &str, key: &DefKey) -> String {
    let mut name = format!("{path}::");
    for container in &key.local.container {
        name.push_str(container.as_str());
        name.push('.');
    }
    name.push_str(key.local.name.as_str());
    name
}

fn describe(change: &DefChange) -> String {
    use crate::facts::Origin;
    match change {
        DefChange::Added => "added".to_string(),
        DefChange::Removed => "removed".to_string(),
        DefChange::Unknown { loss } => format!("unknown ({})", loss.kind()),
        DefChange::Changed { origin, delta } => {
            let how = match origin {
                Origin::Same => "changed",
                Origin::Moved { .. } => "moved",
                Origin::Renamed { .. } => "renamed",
            };
            let parts: Vec<&str> = [
                (delta.signature, "signature"),
                (delta.body, "body"),
                (delta.doc, "doc"),
                (delta.visibility, "visibility"),
                (delta.decorators, "decorators"),
            ]
            .into_iter()
            .filter_map(|(set, label)| set.then_some(label))
            .collect();
            if parts.is_empty() {
                format!("{how} (layout)")
            } else {
                format!("{how} ({})", parts.join(", "))
            }
        }
    }
}
