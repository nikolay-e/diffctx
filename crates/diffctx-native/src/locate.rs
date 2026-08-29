use std::path::Path;

use rustc_hash::{FxHashMap, FxHashSet};
use serde::Serialize;

use crate::pipeline::{ScoredState, SelectionOutcome};
use crate::provenance::{incoming_attribution, seed_hops};
use crate::types::{Fragment, FragmentId};

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
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub ignored_changes: Vec<String>,
    #[serde(skip_serializing_if = "crate::render::is_zero", default)]
    pub policy_excluded_count: usize,
    pub budget_tokens: u32,
    /// Blast-radius counts over the ranked items (#135): distinct files,
    /// changed vs context fragments, and how many ranked items (changed or
    /// context) are tests.
    pub summary: Summary,
    pub item_count: usize,
    pub items: Vec<LocateItem>,
    /// What the run could NOT see or could not fit (#136).
    ///
    /// An agent told honestly where the selection is thin can grep the gap
    /// itself; one told nothing has to distrust the whole answer. Emitted only
    /// when there is something to report, so a clean run costs no tokens.
    #[serde(skip_serializing_if = "Coverage::is_clean")]
    pub coverage: Coverage,
    /// Ranked candidates that did not fit `budget_tokens`, without bodies.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub overflow: Vec<OverflowItem>,
    /// True total behind `overflow`, which is capped at `MAX_OVERFLOW_ITEMS`.
    #[serde(skip_serializing_if = "crate::render::is_zero")]
    pub overflow_count: usize,
}

// The reference is serde's contract for `skip_serializing_if`, not a choice.
#[allow(clippy::trivially_copy_pass_by_ref)]
/// The overflow list is a pointer to what was skipped, not a second selection.
/// Past this many entries it stops being a hint and starts being the cost the
/// budget existed to avoid.
pub const MAX_OVERFLOW_ITEMS: usize = 50;

#[derive(Serialize, Default)]
pub struct Coverage {
    /// Changed files with no symbol-level structure: every fragment is a
    /// chunk/section fallback, so the parser could not see inside them and
    /// nothing was pulled in by symbol.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unparsed_files: Vec<String>,
    /// Changed files whose fragments have no graph edge in either direction —
    /// no caller, import, type or co-change link was found, so relevance had no
    /// path to travel and context for them could only arrive by proximity.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub zero_edge_files: Vec<String>,
    /// PPR push-iteration hit its cap before converging: the ranking is a
    /// partial diffusion, so low-scoring items are less trustworthy than usual.
    /// Never set outside `--scoring ppr`.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub ppr_truncated: bool,
    /// How many of the top-ranked overflow items a 25% larger budget would
    /// admit. Zero when the budget was not what stopped selection.
    ///
    /// This answers "would paying more change the answer", which neither the raw
    /// overflow total nor a score comparison does. The total is thousands of
    /// candidates the budget correctly ignored; "scores at least as high as the
    /// weakest selected item" was worse than useless, because raising the budget
    /// lowers that bar and so *increased* the reported gap.
    #[serde(skip_serializing_if = "crate::render::is_zero")]
    pub next_up: usize,
    /// Documented heuristic in [0, 1], NOT a probability and not a promise:
    /// `parsed_share * linked_share * fit_share`, less 0.1 when PPR truncated.
    /// It says how much of the changed surface the run could see and fit — it
    /// cannot say whether what it selected is the right thing.
    pub confidence: f64,
}

impl Coverage {
    /// A run with nothing to disclose. `confidence` alone is not a finding:
    /// emitting the block for it would put a number in every response and
    /// invite it to be read as a quality score.
    pub fn is_clean(&self) -> bool {
        self.unparsed_files.is_empty()
            && self.zero_edge_files.is_empty()
            && !self.ppr_truncated
            && self.next_up == 0
    }
}

#[derive(Serialize)]
pub struct OverflowItem {
    pub path: String,
    pub lines: String,
    pub score: f64,
    pub tokens: u32,
    /// Single strongest reason, compact: the overflow list exists to be cheap,
    /// and the full `reasons` array on a selected item costs several times this.
    pub why: String,
}

#[derive(Serialize)]
pub struct Summary {
    pub files: usize,
    pub changed: usize,
    pub context: usize,
    pub tests: usize,
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
    /// Coarse impact group: `test`, `type`, or `config`; absent = general
    /// code (callers and friends). Path- and kind-derived, presentation only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<&'static str>,
    pub score: f64,
    pub tokens: u32,
    pub reasons: Vec<Reason>,
}

const CONFIG_EXTENSIONS: &[&str] = &[
    "yaml",
    "yml",
    "json",
    "toml",
    "ini",
    "cfg",
    "conf",
    "env",
    "properties",
];

fn group_of(path: &str, kind: crate::types::FragmentKind) -> Option<&'static str> {
    use crate::types::FragmentKind as K;
    if crate::testfiles::is_test_path(Path::new(path)) {
        return Some("test");
    }
    if matches!(
        kind,
        K::Struct
            | K::Enum
            | K::Interface
            | K::Type
            | K::Record
            | K::StructSignature
            | K::ClassSignature
    ) {
        return Some("type");
    }
    let ext = path.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    if CONFIG_EXTENSIONS.contains(&ext.to_lowercase().as_str()) {
        return Some("config");
    }
    None
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

/// Falls back to the path as given when it lies outside the root: locate is a
/// navigation list, and an unattributable entry is still more useful named than
/// dropped. The separator handling comes from `crate::paths` so it matches the
/// pack renderer instead of unconditionally rewriting backslashes, which on
/// POSIX renames a legal file.
fn rel_path(state: &ScoredState, path: &str) -> String {
    crate::paths::display_rel(&state.root_dir, Path::new(path))
        .unwrap_or_else(|| crate::paths::to_posix_display(std::borrow::Cow::Borrowed(path)))
}

fn reasons_for(
    state: &ScoredState,
    frag: &Fragment,
    stand_in_ids: &rustc_hash::FxHashSet<FragmentId>,
    hops: Option<u32>,
    attribution: Option<&Vec<(String, String, f64)>>,
) -> Vec<Reason> {
    if crate::types::carries_change(frag, &state.core_ids, stand_in_ids) {
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

/// Fragment kinds that mean "the parser produced structure here".
///
/// `Chunk` is the fallback for a file no grammar could parse; `Excerpt` is a
/// budget stand-in for a core, not evidence of parsing. `Section` is neither —
/// it is the markdown parser's genuine structural output, and counting it as a
/// degradation reported every documentation file in a diff as a blind spot.
fn is_structural(kind: crate::types::FragmentKind) -> bool {
    use crate::types::FragmentKind as K;
    !matches!(kind, K::Chunk | K::Excerpt)
}

fn build_coverage(
    state: &ScoredState,
    outcome: &SelectionOutcome,
    next_up: usize,
    attribution: &FxHashMap<FragmentId, Vec<(String, String, f64)>>,
) -> Coverage {
    let graph = &state.scoring_result.graph;
    let changed: Vec<String> = state
        .changed_files
        .iter()
        .map(|p| rel_path(state, p.to_string_lossy().as_ref()))
        .collect();

    // One grouping pass, not one scan per changed file: the naive form is
    // O(changed x all_fragments) with a path allocation per pair, and both
    // factors grow with the diff on exactly the repos already at risk of the
    // timeouts in #121.
    let mut by_file: FxHashMap<String, Vec<&Fragment>> = FxHashMap::default();
    for f in &state.all_fragments {
        by_file
            .entry(rel_path(state, f.id.path.as_ref()))
            .or_default()
            .push(f);
    }

    let mut unparsed: Vec<String> = Vec::new();
    let mut zero_edge: Vec<String> = Vec::new();
    let empty: Vec<&Fragment> = Vec::new();
    for file in &changed {
        let frags = by_file.get(file).unwrap_or(&empty);
        if frags.is_empty() {
            // Deleted, ignored, or never fragmented: absence here is already
            // reported by deleted_files / lockfile_changes, and claiming it as
            // a parse failure would be a second, wrong story about it.
            continue;
        }
        // Only where structure was expected. A `.md` or `.json` file has no
        // symbols to find, so listing it as a blind spot is true and useless:
        // there is nothing for the caller to grep for.
        let parseable = crate::languages::get_language_for_file(file).is_some();
        if parseable && !frags.iter().any(|f| is_structural(f.kind)) {
            unparsed.push(file.clone());
        }
        let linked = frags.iter().any(|f| {
            if attribution.contains_key(&f.id) {
                return true;
            }
            let mut has_out = false;
            graph.for_each_forward_neighbor(&f.id, |_, _| has_out = true);
            has_out
        });
        if !linked {
            zero_edge.push(file.clone());
        }
    }
    unparsed.sort();
    unparsed.dedup();
    zero_edge.sort();
    zero_edge.dedup();

    let n_changed = changed.len().max(1) as f64;
    let parsed_share = 1.0 - unparsed.len() as f64 / n_changed;
    let linked_share = 1.0 - zero_edge.len() as f64 / n_changed;
    // Context only: a changed fragment that did not fit is a budget floor
    // problem the caller already sees in `budget_tokens`, and counting it here
    // would make a tiny budget look like a discovery failure.
    let selected_context = outcome
        .selected
        .iter()
        .filter(|f| !state.core_ids.contains(&f.id))
        .count();
    // Against what one more budget step would add, not against the whole
    // admitted universe. Selecting 21 of 3153 admitted candidates is a budget
    // working correctly, and reading that as 0.7% coverage made `confidence` 0.0
    // on every real run.
    let fit_share = if selected_context + next_up == 0 {
        1.0
    } else {
        selected_context as f64 / (selected_context + next_up) as f64
    };
    let truncated = state.scoring_result.ppr_truncated;
    let raw = parsed_share * linked_share * fit_share - if truncated { 0.1 } else { 0.0 };

    Coverage {
        unparsed_files: unparsed,
        zero_edge_files: zero_edge,
        ppr_truncated: truncated,
        next_up,
        confidence: (raw.clamp(0.0, 1.0) * 1e2).round() / 1e2,
    }
}

/// Ranked admitted candidates that the budget left behind.
///
/// Returns the capped list, the true total, and the near-miss count. The first
/// two differ whenever the cap bites — reporting only the capped length would
/// understate the gap in exactly the runs where it matters most — and the third
/// is the only one of the three that says whether a bigger budget would have
/// helped.
fn build_overflow(
    state: &ScoredState,
    outcome: &SelectionOutcome,
    hops: &FxHashMap<FragmentId, u32>,
    attribution: &FxHashMap<FragmentId, Vec<(String, String, f64)>>,
) -> (Vec<OverflowItem>, usize, usize) {
    let selected: FxHashSet<&FragmentId> = outcome.selected.iter().map(|f| &f.id).collect();
    let rel = &state.scoring_result.rel_scores;
    let mut skipped: Vec<&Fragment> = state
        .scoring_result
        .filtered_fragments
        .iter()
        .filter(|f| !selected.contains(&f.id) && !state.core_ids.contains(&f.id))
        .collect();
    skipped.sort_by(|a, b| {
        let sa = rel.get(&a.id).copied().unwrap_or(0.0);
        let sb = rel.get(&b.id).copied().unwrap_or(0.0);
        sb.total_cmp(&sa).then_with(|| a.id.cmp(&b.id))
    });
    let total = skipped.len();
    // Only when the budget is what stopped selection. With headroom left the stop
    // came from the adaptive tau threshold, and then no larger budget admits
    // anything — an earlier version ignored that and reported 2711 near misses
    // at `--budget -1`, where by construction nothing was crowded out at all.
    let spent: u32 = outcome.selected.iter().map(|f| f.token_count).sum();
    let headroom = outcome.effective_budget.saturating_sub(spent);
    let smallest_skipped = skipped.iter().map(|f| f.token_count).min().unwrap_or(0);
    let budget_bound = !skipped.is_empty() && headroom < smallest_skipped;
    let next_up = if budget_bound {
        // Walk the overflow ranking against a 25% budget increase. Bounded by
        // that increment, so — unlike a score threshold — it does not grow just
        // because a larger budget selected more and lowered the bar.
        let extra = outcome.effective_budget / 4;
        let mut spare = headroom + extra;
        let mut n = 0;
        for f in &skipped {
            if f.token_count > spare {
                break;
            }
            spare -= f.token_count;
            n += 1;
        }
        n
    } else {
        0
    };
    let items = skipped
        .into_iter()
        .take(MAX_OVERFLOW_ITEMS)
        .map(|frag| OverflowItem {
            path: rel_path(state, frag.id.path.as_ref()),
            lines: format!("{}-{}", frag.id.start_line, frag.id.end_line),
            score: rel
                .get(&frag.id)
                .map(|s| (s * 1e4).round() / 1e4)
                .unwrap_or(0.0),
            tokens: frag.token_count,
            why: overflow_why(
                state,
                hops.get(&frag.id).copied(),
                attribution.get(&frag.id),
            ),
        })
        .collect();
    (items, total, next_up)
}

fn overflow_why(
    state: &ScoredState,
    hops: Option<u32>,
    attribution: Option<&Vec<(String, String, f64)>>,
) -> String {
    if let Some((category, from, _)) = attribution.and_then(|rows| rows.first()) {
        return format!("{category} from {}", rel_path(state, from));
    }
    match hops {
        Some(h) => format!("{h} hop(s) from a change"),
        None => "post-pass candidate".to_string(),
    }
}

/// Renders the shared selection outcome as the `diffctx.locate.v1` navigation
/// list: ranked fragments with provenance reasons and NO source bodies. Uses
/// only the edge metadata and relevance the pipeline already computed.
pub fn build_locate(state: &ScoredState, outcome: &SelectionOutcome) -> LocateOutput {
    let rel = &state.scoring_result.rel_scores;
    let hops = seed_hops(state);
    let attribution = incoming_attribution(state);
    let (overflow, overflow_count, next_up) = build_overflow(state, outcome, &hops, &attribution);

    let items: Vec<LocateItem> = outcome
        .selected
        .iter()
        .map(|frag| {
            let is_changed =
                crate::types::carries_change(frag, &state.core_ids, &outcome.stand_in_ids);
            let path = rel_path(state, frag.id.path.as_ref());
            let group = group_of(&path, frag.kind);
            LocateItem {
                path,
                lines: format!("{}-{}", frag.id.start_line, frag.id.end_line),
                kind: format!("{:?}", frag.kind).to_lowercase(),
                symbol: frag.symbol_name.clone(),
                role: if is_changed { Some("changed") } else { None },
                group,
                score: rel
                    .get(&frag.id)
                    .map(|s| (s * 1e4).round() / 1e4)
                    .unwrap_or(0.0),
                tokens: frag.token_count,
                reasons: reasons_for(
                    state,
                    frag,
                    &outcome.stand_in_ids,
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
        ignored_changes: state.ignored_changes.clone(),
        policy_excluded_count: state.policy_excluded_count,
        budget_tokens: outcome.effective_budget,
        summary: Summary {
            files: items
                .iter()
                .map(|i| i.path.as_str())
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            changed: items.iter().filter(|i| i.role == Some("changed")).count(),
            context: items.iter().filter(|i| i.role.is_none()).count(),
            tests: items.iter().filter(|i| i.group == Some("test")).count(),
        },
        item_count: items.len(),
        items,
        coverage: build_coverage(state, outcome, next_up, &attribution),
        overflow,
        overflow_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::FragmentKind;

    /// locate carried its own test-file rule until it was delegated to
    /// `crate::testfiles`. It lowercased the path first, so every CamelCase
    /// convention was invisible and `summary.tests` in the blast-radius view
    /// (#135) undercounted. The bitcheck fixture is this repo, whose tests are
    /// all `test_*.py` under `tests/` — a shape both rules agree on — so the
    /// divergence only shows up on cases like these.
    #[test]
    fn the_grouping_uses_the_shared_test_classifier() {
        for path in [
            "src/main/java/com/example/FooTest.java",
            "src/main/scala/AuthSpec.scala",
            "src/XMLTest.java",
            "ui/widget-spec.js",
            "src/tests.rs",
        ] {
            assert_eq!(
                group_of(path, FragmentKind::Function),
                Some("test"),
                "not grouped as a test: {path}"
            );
        }
    }

    /// The one rule that moved the other way: `testing/` used to count as a test
    /// directory here. The shared implementation excludes it on purpose —
    /// `testing` is Go's stdlib package name and such directories hold helpers.
    #[test]
    fn a_testing_directory_is_not_itself_a_test() {
        assert_eq!(
            group_of("src/testing/helpers.go", FragmentKind::Function),
            None
        );
    }

    /// Grouping is ordered: a type declaration inside a test file is a test,
    /// not a type, because the test grouping is checked first. Pinned because
    /// swapping the order silently re-buckets every test fixture struct.
    #[test]
    fn a_type_in_a_test_file_groups_as_test() {
        assert_eq!(
            group_of("tests/fixtures.rs", FragmentKind::Struct),
            Some("test")
        );
        assert_eq!(group_of("src/model.rs", FragmentKind::Struct), Some("type"));
    }
}
