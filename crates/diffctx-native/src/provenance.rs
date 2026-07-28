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

fn seed_hops(state: &ScoredState) -> FxHashMap<FragmentId, u32> {
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

fn incoming_mass(state: &ScoredState) -> FxHashMap<FragmentId, FxHashMap<&'static str, f64>> {
    let graph = &state.scoring_result.graph;
    let rel = &state.scoring_result.rel_scores;
    let mut mass: FxHashMap<FragmentId, FxHashMap<&'static str, f64>> = FxHashMap::default();
    graph.for_each_categorized_edge(|src, dst, cat| {
        let src_rel = rel.get(src).copied().unwrap_or(0.0);
        if src_rel <= 0.0 {
            return;
        }
        let w = graph.forward_edge_weight(src, dst).unwrap_or(0.0);
        if w <= 0.0 {
            return;
        }
        *mass
            .entry(dst.clone())
            .or_default()
            .entry(cat.as_str())
            .or_insert(0.0) += w * src_rel;
    });
    mass
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
            "incoming_mass": serde_json::Value::Object(contrib),
        });
        writeln!(w, "{line}")?;
    }
    w.flush()
}
