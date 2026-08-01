use std::collections::VecDeque;
use std::io::{BufWriter, Write};
use std::path::Path;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::pipeline::ScoredState;
use crate::types::{Fragment, FragmentId};

pub const PROVENANCE_ENV: &str = "DIFFCTX_PROVENANCE_DUMP";

/// Env-gated per-candidate inclusion-provenance dump (#93). One JSONL line
/// per scored candidate: relevance, seed distance in hops, per-edge-category
/// incoming mass (`weight x rel(src)` summed over incoming edges), and the
/// selection verdict. The default path costs one env probe and nothing else,
/// which is what keeps the instrumentation E-class.
pub fn maybe_dump(state: &ScoredState, selected: &[Fragment]) {
    let Ok(path) = std::env::var(PROVENANCE_ENV) else {
        return;
    };
    if path.is_empty() {
        return;
    }
    if let Err(e) = dump(state, selected, Path::new(&path)) {
        tracing::debug!("provenance dump to '{}' failed: {}", path, e);
    }
}

pub fn seed_hops(state: &ScoredState) -> FxHashMap<FragmentId, u32> {
    let graph = &state.scoring_result.graph;
    let mut hops: FxHashMap<FragmentId, u32> = FxHashMap::default();
    let mut queue: VecDeque<FragmentId> = VecDeque::new();
    for core in &state.core_ids {
        hops.insert(core.clone(), 0);
        queue.push_back(core.clone());
    }
    // Relevance flows along edges in both directions (PPR blends forward and
    // backward pushes), so distance is measured on the undirected graph.
    let mut undirected: FxHashMap<FragmentId, Vec<FragmentId>> = FxHashMap::default();
    graph.for_each_categorized_edge(|src, dst, _| {
        undirected.entry(src.clone()).or_default().push(dst.clone());
        undirected.entry(dst.clone()).or_default().push(src.clone());
    });
    while let Some(node) = queue.pop_front() {
        let d = hops[&node];
        if let Some(neighbors) = undirected.get(&node) {
            for n in neighbors {
                if !hops.contains_key(n) {
                    hops.insert(n.clone(), d + 1);
                    queue.push_back(n.clone());
                }
            }
        }
    }
    hops
}

struct CatMass {
    mass: f64,
    top_source: FragmentId,
    top_contribution: f64,
}

fn per_category_mass(
    state: &ScoredState,
) -> FxHashMap<FragmentId, FxHashMap<&'static str, CatMass>> {
    let graph = &state.scoring_result.graph;
    let rel = &state.scoring_result.rel_scores;
    let mut mass: FxHashMap<FragmentId, FxHashMap<&'static str, CatMass>> = FxHashMap::default();
    graph.for_each_categorized_edge(|src, dst, cat| {
        let src_rel = rel.get(src).copied().unwrap_or(0.0);
        if src_rel <= 0.0 {
            return;
        }
        let w = graph.forward_edge_weight(src, dst).unwrap_or(0.0);
        if w <= 0.0 {
            return;
        }
        let contribution = w * src_rel;
        let entry = mass
            .entry(dst.clone())
            .or_default()
            .entry(cat.as_str())
            .or_insert_with(|| CatMass {
                mass: 0.0,
                top_source: src.clone(),
                top_contribution: 0.0,
            });
        entry.mass += contribution;
        // Strict `>` with deterministic iteration would still tie-break by
        // visit order; prefer the lexically-smaller source on equal
        // contribution so the attribution is order-independent.
        if contribution > entry.top_contribution
            || (contribution == entry.top_contribution && src.path < entry.top_source.path)
        {
            entry.top_source = src.clone();
            entry.top_contribution = contribution;
        }
    });
    mass
}

/// Per-fragment incoming relevance mass grouped by edge category, sorted by
/// mass descending: `(category, strongest_source_path, mass)`. Shared by the
/// provenance dump and the locate renderer (#126) — one attribution pass,
/// two consumers.
pub fn incoming_attribution(
    state: &ScoredState,
) -> FxHashMap<FragmentId, Vec<(String, String, f64)>> {
    per_category_mass(state)
        .into_iter()
        .map(|(id, cats)| {
            let mut rows: Vec<(String, String, f64)> = cats
                .into_iter()
                .map(|(cat, m)| (cat.to_string(), m.top_source.path.to_string(), m.mass))
                .collect();
            rows.sort_by(|a, b| {
                b.2.partial_cmp(&a.2)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(a.0.cmp(&b.0))
            });
            (id, rows)
        })
        .collect()
}

fn incoming_mass(state: &ScoredState) -> FxHashMap<FragmentId, FxHashMap<&'static str, f64>> {
    per_category_mass(state)
        .into_iter()
        .map(|(id, cats)| (id, cats.into_iter().map(|(c, m)| (c, m.mass)).collect()))
        .collect()
}

fn dump(state: &ScoredState, selected: &[Fragment], out_path: &Path) -> std::io::Result<()> {
    let rel = &state.scoring_result.rel_scores;
    let selected_ids: FxHashSet<&FragmentId> = selected.iter().map(|f| &f.id).collect();
    let hops = seed_hops(state);
    let mass = incoming_mass(state);

    let mut fragments: Vec<&Fragment> = state.scoring_result.filtered_fragments.iter().collect();
    fragments.sort_by(|a, b| {
        a.id.path
            .cmp(&b.id.path)
            .then(a.id.start_line.cmp(&b.id.start_line))
            .then(a.id.end_line.cmp(&b.id.end_line))
    });

    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let mut w = BufWriter::new(std::fs::File::create(out_path)?);
    for frag in fragments {
        let contrib: serde_json::Map<String, serde_json::Value> = mass
            .get(&frag.id)
            .map(|per_cat| {
                let mut sorted: Vec<_> = per_cat.iter().collect();
                sorted.sort_by(|a, b| a.0.cmp(b.0));
                sorted
                    .into_iter()
                    .map(|(cat, v)| ((*cat).to_string(), serde_json::json!(v)))
                    .collect()
            })
            .unwrap_or_default();
        let line = serde_json::json!({
            "path": frag.id.path.as_ref(),
            "start": frag.id.start_line,
            "end": frag.id.end_line,
            "kind": format!("{:?}", frag.kind).to_lowercase(),
            "tokens": frag.token_count,
            "relevance": rel.get(&frag.id).copied().unwrap_or(0.0),
            "is_core": state.core_ids.contains(&frag.id),
            "selected": selected_ids.contains(&frag.id),
            "seed_hops": hops.get(&frag.id).map(|h| *h as i64).unwrap_or(-1),
            // Which discovery strategy put this file in the universe at all.
            // `null` for a changed file, which is never discovered — it is the
            // seed. Splits "never surfaced" from "surfaced but not selected",
            // which the selected set alone cannot distinguish (#130).
            "discovery_source": state
                .discovery_source
                .get(&frag.id.path)
                .map(|s| serde_json::json!(s))
                .unwrap_or(serde_json::Value::Null),
            "incoming_mass": serde_json::Value::Object(contrib),
        });
        writeln!(w, "{line}")?;
    }
    w.flush()
}
