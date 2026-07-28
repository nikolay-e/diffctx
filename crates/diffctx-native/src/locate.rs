use serde::Serialize;

use crate::pipeline::{ScoredState, SelectionOutcome};
use crate::provenance::{incoming_attribution, seed_hops};
use crate::types::Fragment;

pub const LOCATE_SCHEMA: &str = "diffctx.locate.v1";

#[derive(Serialize)]
pub struct LocateOutput {
    pub schema: &'static str,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit_message: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub changed_files: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub deleted_files: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub renamed_files: Vec<RenameEntry>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub lockfile_changes: Vec<String>,
    pub budget_tokens: u32,
    pub item_count: usize,
    pub items: Vec<LocateItem>,
}

#[derive(Serialize)]
pub struct RenameEntry {
    pub from: String,
    pub to: String,
}

/// Rank is the array position (items are emitted in selection order);
/// `role` is serialized only for `"changed"` — absence means context.
#[derive(Serialize)]
pub struct LocateItem {
    pub path: String,
    pub lines: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<&'static str>,
    pub score: f64,
    pub tokens: u32,
    pub reasons: Vec<Reason>,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Reason {
    /// The fragment overlaps the diff hunks — it IS the change.
    Changed,
    /// Relevance arrived over a typed edge; `from` is the strongest source.
    Edge {
        category: String,
        from: String,
        mass: f64,
    },
    /// Graph distance from the nearest changed fragment.
    Proximity { seed_hops: u32 },
    /// Added by a selection post-pass (changed-file representation /
    /// nontrivial-context rescue) rather than by scored relevance.
    PostPass,
}

fn rel_path(state: &ScoredState, path: &str) -> String {
    let root = state.root_dir.to_string_lossy();
    path.strip_prefix(root.as_ref())
        .map(|s| s.trim_start_matches('/').to_string())
        .unwrap_or_else(|| path.to_string())
        .replace('\\', "/")
}

fn reasons_for(
    state: &ScoredState,
    frag: &Fragment,
    hops: Option<u32>,
    attribution: Option<&Vec<(String, String, f64)>>,
) -> Vec<Reason> {
    if state.core_ids.contains(&frag.id) || frag.kind == crate::types::FragmentKind::Excerpt {
        return vec![Reason::Changed];
    }
    let mut reasons: Vec<Reason> = Vec::new();
    if let Some(per_cat) = attribution {
        // Top edge only: the strongest category+source explains the pull;
        // the full per-category breakdown lives in DIFFCTX_PROVENANCE_DUMP.
        if let Some((category, from, mass)) = per_cat.first() {
            reasons.push(Reason::Edge {
                category: category.clone(),
                from: rel_path(state, from),
                mass: (mass * 1e3).round() / 1e3,
            });
        }
    }
    if let Some(h) = hops {
        reasons.push(Reason::Proximity { seed_hops: h });
    }
    if reasons.is_empty() {
        reasons.push(Reason::PostPass);
    }
    reasons
}

/// Renders the shared selection outcome as the `diffctx.locate.v1` navigation
/// list: ranked fragments with provenance reasons and NO source bodies. Uses
/// only the edge metadata and relevance the pipeline already computed.
pub fn build_locate(state: &ScoredState, outcome: &SelectionOutcome) -> LocateOutput {
    let rel = &state.scoring_result.rel_scores;
    let hops = seed_hops(state);
    let attribution = incoming_attribution(state);

    let items: Vec<LocateItem> = outcome
        .selected
        .iter()
        .map(|frag| {
            let is_changed = state.core_ids.contains(&frag.id)
                || frag.kind == crate::types::FragmentKind::Excerpt;
            LocateItem {
                path: rel_path(state, frag.id.path.as_ref()),
                lines: format!("{}-{}", frag.id.start_line, frag.id.end_line),
                kind: format!("{:?}", frag.kind).to_lowercase(),
                symbol: frag.symbol_name.clone(),
                role: if is_changed { Some("changed") } else { None },
                score: rel
                    .get(&frag.id)
                    .map(|s| (s * 1e4).round() / 1e4)
                    .unwrap_or(0.0),
                tokens: frag.token_count,
                reasons: reasons_for(
                    state,
                    frag,
                    hops.get(&frag.id).copied(),
                    attribution.get(&frag.id),
                ),
            }
        })
        .collect();

    LocateOutput {
        schema: LOCATE_SCHEMA,
        name: state
            .root_dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| state.root_dir.to_string_lossy().to_string()),
        commit_message: state.commit_message.clone(),
        changed_files: state
            .changed_files
            .iter()
            .map(|p| rel_path(state, p.to_string_lossy().as_ref()))
            .collect(),
        deleted_files: state.deleted_files.clone(),
        renamed_files: state
            .renamed_files
            .iter()
            .map(|(from, to)| RenameEntry {
                from: from.clone(),
                to: to.clone(),
            })
            .collect(),
        lockfile_changes: state.lockfile_changes.clone(),
        budget_tokens: outcome.effective_budget,
        item_count: items.len(),
        items,
    }
}
