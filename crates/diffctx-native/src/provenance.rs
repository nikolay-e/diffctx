use std::io::{BufWriter, Write};
use std::path::Path;

use rustc_hash::FxHashSet;

use crate::run_provenance::state::{ScoredState, incoming_mass, seed_hops};
use crate::types::{Fragment, FragmentId};

const PROVENANCE_ENV: &str = "DIFFCTX_PROVENANCE_DUMP";

/// Env-gated per-candidate inclusion-provenance dump (#93). One JSONL line
/// per scored candidate: relevance, seed distance in hops, per-edge-category
/// incoming mass (`weight x rel(src)` summed over incoming edges), and the
/// selection verdict. The default path costs one env probe and nothing else,
/// which is what keeps the instrumentation E-class.
pub(crate) fn maybe_dump(state: &ScoredState, selected: &[Fragment]) {
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
