use std::path::Path;

use rustc_hash::{FxHashMap, FxHashSet};
use schemars::JsonSchema;
use serde::Serialize;

use crate::answer::envelope::{self, Inventory};
use crate::answer::v2::{Fit, Listing, Range};
use crate::pipeline::{ScoredState, SelectionOutcome};
use crate::provenance::{incoming_attribution, seed_hops};
use crate::types::{Fragment, FragmentId};

pub const LOCATE_SCHEMA: &str = "diffctx.locate.v2";

#[derive(Serialize, JsonSchema, Clone)]
pub struct LocateOutput {
    pub schema: &'static str,
    pub name: String,
    /// The range, and the newest of its commits' messages (sanitized: the one free
    /// text locate prints).
    pub range: Range,
    #[serde(default, skip_serializing_if = "Inventory::is_empty")]
    pub inventory: Inventory,
    /// The budget the list was cut to, and what that cost it.
    pub fit: Fit,
    /// Blast-radius counts over the ranked items (#135): distinct files,
    /// changed vs context fragments, and how many ranked items (changed or
    /// context) are tests.
    pub summary: Summary,
    pub items: Vec<LocateItem>,
    /// What the run could NOT see or could not fit (#136).
    ///
    /// An agent told honestly where the selection is thin can grep the gap
    /// itself; one told nothing has to distrust the whole answer. Emitted only
    /// when there is something to report, so a clean run costs no tokens.
    #[serde(default, skip_serializing_if = "Coverage::is_clean")]
    pub coverage: Coverage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<crate::run_provenance::ProvenanceV1>,
    /// Ranked candidates the budget left behind. The count is always stated; the
    /// entries, without bodies, only when asked for (`--overflow`), and then they
    /// are charged to the budget like every other byte.
    #[serde(default, skip_serializing_if = "Listing::is_empty")]
    pub overflow: Listing<OverflowItem>,
    /// Present only when the sanitizer replaced a credential-shaped string in
    /// a commit message, the one free text locate prints.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redactions: Option<crate::sanitize::Redactions>,
}

/// The overflow list is a pointer to what was skipped, not a second selection.
/// Past this many entries it stops being a hint and starts being the cost the
/// budget existed to avoid.
pub const MAX_OVERFLOW_ITEMS: usize = 50;

#[derive(Serialize, JsonSchema, Default, Clone)]
pub struct Coverage {
    /// What stopped the run short of a complete pass, in the one vocabulary
    /// every surface uses (`diffctx.resource.LimitReason`). Empty on a
    /// complete run.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limits: Vec<crate::resource::LimitReason>,
    /// Limits the run hit that cost these items nothing: graph caps, and
    /// per-file caps on files the answer does not touch (#306).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capped: Vec<crate::resource::LimitReason>,
    /// The files behind the per-file reasons in `limits`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limited_files: Vec<crate::resource::LimitedFile>,
    /// How well the selection could see the change.
    #[serde(default, skip_serializing_if = "Selection::is_clean")]
    pub selection: Selection,
}

/// The selection-quality facts of a locate run.
#[derive(Serialize, JsonSchema, Default, Clone)]
pub struct Selection {
    /// Changed files with no symbol-level structure: every fragment is a
    /// chunk/section fallback, so the parser could not see inside them and
    /// nothing was pulled in by symbol.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unparsed_files: Vec<String>,
    /// Changed files whose fragments have no graph edge in either direction —
    /// no caller, import, type or co-change link was found, so relevance had no
    /// path to travel and context for them could only arrive by proximity.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub zero_edge_files: Vec<String>,
    /// PPR push-iteration hit its cap before converging: the ranking is a
    /// partial diffusion, so low-scoring items are less trustworthy than usual.
    /// Never set outside `--scoring ppr`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ppr_truncated: bool,
    /// How many of the top-ranked overflow items a 25% larger budget would
    /// admit. Zero when the budget was not what stopped selection.
    ///
    /// This answers "would paying more change the answer", which neither the raw
    /// overflow total nor a score comparison does. The total is thousands of
    /// candidates the budget correctly ignored; "scores at least as high as the
    /// weakest selected item" was worse than useless, because raising the budget
    /// lowers that bar and so *increased* the reported gap.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub next_up: usize,
    /// Changed files with no ranked item at all: the inventory says they
    /// changed, the budget or the selection left nothing of them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unrepresented_changed_files: Vec<String>,
    /// Changed files that yielded no fragment at all (not code, binary, over
    /// the size cap) — absent for a reason no budget or selection controls.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub no_fragment_changed_files: Vec<String>,
}

// The reference is serde's contract for `skip_serializing_if`, not a choice.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero(n: &usize) -> bool {
    *n == 0
}

impl Selection {
    fn is_clean(&self) -> bool {
        self.unparsed_files.is_empty()
            && self.zero_edge_files.is_empty()
            && !self.ppr_truncated
            && self.next_up == 0
            && self.unrepresented_changed_files.is_empty()
            && self.no_fragment_changed_files.is_empty()
    }
}

impl Coverage {
    /// A run with nothing to disclose. A cap nothing needed is not a finding:
    /// emitting the block for it would put a line in every response.
    pub fn is_clean(&self) -> bool {
        self.limits.is_empty() && self.selection.is_clean()
    }
}

#[derive(Serialize, JsonSchema, Clone)]
pub struct OverflowItem {
    pub path: String,
    pub lines: String,
    pub score: f64,
    pub tokens: u32,
    /// Single strongest reason, compact: the overflow list exists to be cheap,
    /// and the full `reasons` array on a selected item costs several times this.
    pub why: String,
}

#[derive(Serialize, JsonSchema, Clone)]
pub struct Summary {
    pub files: usize,
    pub changed: usize,
    pub context: usize,
    pub tests: usize,
}

impl Summary {
    fn of(items: &[LocateItem]) -> Self {
        Self {
            files: items
                .iter()
                .map(|i| i.path.as_str())
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            changed: items.iter().filter(|i| i.role == Some("changed")).count(),
            context: items.iter().filter(|i| i.role.is_none()).count(),
            tests: items.iter().filter(|i| i.group == Some("test")).count(),
        }
    }
}

impl LocateOutput {
    /// A list that says nothing a caller can act on: no item and no file deleted,
    /// renamed, locked, withheld or ignored.
    pub fn carries_nothing(&self) -> bool {
        self.items.is_empty() && !self.inventory.says_something_without_fragments()
    }

    /// #300: the budget bounds what the reader receives, and that is the
    /// document as the door prints it — a row per item costs more than the item's
    /// own tokens. Past the budget: the listed overflow, then context from the
    /// tail, then a changed file's second and later items; the first item of
    /// every changed file stays. Each dropped item is counted in `overflow` and
    /// the cut is recorded as a limit. The overflow entries are in the document
    /// only when `list_overflow`, and charged then.
    pub fn fit_to(&mut self, budget: u32, list_overflow: bool) {
        if !list_overflow {
            self.overflow.items.clear();
        }
        if budget == 0 {
            return;
        }
        let cost = |o: &Self| {
            crate::tokenizer::count_tokens(&format!(
                "{}\n",
                serde_json::to_string(o).unwrap_or_default()
            ))
        };
        self.fit.needs = cost(self).div_ceil(100) * 100;
        if cost(self) <= budget {
            return;
        }
        crate::fit::shed_to_fit(self, budget, &cost);
        // The least the answer can say may outgrow the budget: said, not cut into.
        let least = cost(self);
        if least > budget {
            self.fit.least = Some(least);
        }
    }
}

impl crate::fit::Shed for LocateOutput {
    /// A listed overflow entry; then the last context item; then a changed file's
    /// second and later items, the first item of every changed file staying. An
    /// item is counted in `overflow` and the cut is recorded as a limit.
    fn shed_one(&mut self) -> bool {
        if self.overflow.items.pop().is_some() {
            return true;
        }
        let victim = self
            .items
            .iter()
            .rposition(|i| i.role.is_none())
            .or_else(|| {
                self.items.iter().enumerate().rev().find_map(|(at, i)| {
                    self.items[..at]
                        .iter()
                        .any(|earlier| earlier.path == i.path)
                        .then_some(at)
                })
            });
        let Some(at) = victim else {
            return false;
        };
        self.items.remove(at);
        self.overflow.total += 1;
        self.summary = Summary::of(&self.items);
        let reason = crate::resource::LimitReason::SelectionBudgetExceeded;
        if !self.coverage.limits.contains(&reason) {
            self.coverage.limits.push(reason);
            self.coverage.limits.sort();
        }
        true
    }
}

/// Rank is the array position (items are emitted in selection order);
/// `role` is serialized only for `"changed"` — absence means context.
#[derive(Serialize, JsonSchema, Clone)]
pub struct LocateItem {
    pub path: String,
    pub lines: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<&'static str>,
    /// Coarse impact group: `test`, `type`, or `config`; absent = general
    /// code (callers and friends). Path- and kind-derived, presentation only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
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

#[derive(Serialize, JsonSchema, Clone)]
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
    let fragmentless = crate::pipeline::fragmentless_changed_files(
        &state.root_dir,
        &state.changed_files,
        &state.all_fragments,
    );

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
            // reported by the inventory's deleted and lockfiles lists, and claiming it as
            // a parse failure would be a second, wrong story about it.
            continue;
        }
        // Only where definitions could be missed: code with no grammar in
        // this build, or a parse that broke. A `.md`, `.json` or `.sql` file,
        // or a script that parses cleanly and defines nothing, has none to
        // miss — listing it is true and useless (#330, #454).
        let has_definition = frags.iter().any(|f| f.kind.is_definition_kind());
        if crate::parsers::unanalysed_reason(file, has_definition, || {
            state.run.source().read_bytes(&state.root_dir.join(file))
        })
        .is_some()
        {
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

    let about = crate::pipeline::answer_paths(state, outcome.selected.iter());
    let (limits, capped, limited_files) =
        crate::resource::classify_limits(&state.run, &[], &|p| about.contains(p));

    Coverage {
        limits,
        capped,
        limited_files,
        selection: Selection {
            unparsed_files: unparsed,
            zero_edge_files: zero_edge,
            ppr_truncated: state.scoring_result.ppr_truncated,
            next_up,
            unrepresented_changed_files: {
                let represented: FxHashSet<String> = outcome
                    .selected
                    .iter()
                    .map(|f| rel_path(state, f.id.path.as_ref()))
                    .collect();
                changed
                    .iter()
                    .filter(|p| !represented.contains(*p) && !fragmentless.contains(*p))
                    .cloned()
                    .collect()
            },
            no_fragment_changed_files: changed
                .iter()
                .filter(|p| fragmentless.contains(*p))
                .cloned()
                .collect(),
        },
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
    let headroom = outcome.selection_budget.saturating_sub(spent);
    let smallest_skipped = skipped.iter().map(|f| f.token_count).min().unwrap_or(0);
    let budget_bound = !skipped.is_empty() && headroom < smallest_skipped;
    let next_up = if budget_bound {
        // Walk the overflow ranking against a 25% budget increase. Bounded by
        // that increment, so — unlike a score threshold — it does not grow just
        // because a larger budget selected more and lowered the bar.
        let extra = outcome.selection_budget / 4;
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

/// Renders the shared selection outcome as the `diffctx.locate.v2` navigation
/// list: ranked fragments with provenance reasons and NO source bodies. Uses
/// only the edge metadata and relevance the pipeline already computed.
pub fn build_locate(state: &ScoredState, outcome: &SelectionOutcome) -> LocateOutput {
    let rel = &state.scoring_result.rel_scores;
    let hops = seed_hops(state);
    let attribution = incoming_attribution(state);
    let (overflow, overflow_total, next_up) = build_overflow(state, outcome, &hops, &attribution);

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

    let mut redactions = crate::sanitize::Redactions::default();
    let mut sanitized = |message: &String| {
        let (clean, r) = crate::sanitize::sanitize(message);
        redactions.merge(r);
        clean
    };
    let commit_message = state.commit_message.as_ref().map(&mut sanitized);
    let commit_messages: Vec<String> = outcome.commit_messages.iter().map(&mut sanitized).collect();
    let mut coverage = build_coverage(state, outcome, next_up, &attribution);
    if !redactions.is_empty() {
        coverage
            .limits
            .push(crate::resource::LimitReason::SanitizationRedaction);
        coverage.limits.sort();
        coverage.limits.dedup();
    }

    LocateOutput {
        schema: LOCATE_SCHEMA,
        name: state
            .root_dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| state.root_dir.to_string_lossy().to_string()),
        range: envelope::range_of(
            state.provenance.range.as_deref(),
            state.index_tree.as_deref(),
            state.commit_count,
            envelope::messages_of(commit_message, commit_messages),
        ),
        inventory: Inventory::new(
            state
                .changed_files
                .iter()
                .map(|p| rel_path(state, p.to_string_lossy().as_ref()))
                .collect(),
            state.deleted_files.clone(),
            state.renamed_files.clone(),
            state.lockfile_changes.clone(),
            state.ignored_changes.clone(),
            state.policy_excluded_count,
        ),
        fit: envelope::fit_of(outcome.budget_requested, outcome.effective_budget),
        summary: Summary::of(&items),
        items,
        coverage,
        overflow: Listing {
            total: overflow_total,
            items: overflow,
        },
        redactions: (!redactions.is_empty()).then_some(redactions),
        provenance: Some(
            state
                .provenance
                .finish(Some(outcome.selection_provenance())),
        ),
    }
}

/// JSON Schema 2020-12 for `diffctx.locate.v2`, generated from the type and
/// pinned by `tests/context_schema.rs`.
pub fn locate_schema() -> serde_json::Value {
    let mut generator = schemars::generate::SchemaSettings::draft2020_12().into_generator();
    let schema = generator.root_schema_for::<LocateOutput>();
    serde_json::to_value(schema).expect("schema serializes")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::FragmentKind;

    /// locate carried its own test-file rule until it was delegated to
    /// `crate::testfiles`. It lowercased the path first, so every CamelCase
    /// convention was invisible and `summary.tests` in the blast-radius view
    /// (#135) undercounted. This repository's own tests are all `test_*.py`
    /// under `tests/` — a shape both rules agree on — so the divergence only
    /// shows up on cases like these.
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

    fn item(path: &str, role: Option<&'static str>) -> LocateItem {
        LocateItem {
            path: path.to_string(),
            lines: "1-5".to_string(),
            kind: "function".to_string(),
            symbol: None,
            role,
            group: None,
            score: 0.5,
            tokens: 10,
            reasons: vec![Reason::PostPass],
        }
    }

    fn answer(items: Vec<LocateItem>, overflow_total: usize) -> LocateOutput {
        LocateOutput {
            schema: LOCATE_SCHEMA,
            name: "repo".to_string(),
            range: envelope::worktree_range(),
            inventory: Inventory::default(),
            fit: envelope::fit_of(None, 0),
            summary: Summary::of(&items),
            items,
            coverage: Coverage::default(),
            provenance: None,
            overflow: Listing {
                total: overflow_total,
                items: (0..overflow_total.min(5))
                    .map(|i| OverflowItem {
                        path: format!("other_{i}.py"),
                        lines: "1-5".to_string(),
                        score: 0.1,
                        tokens: 10,
                        why: "post-pass candidate".to_string(),
                    })
                    .collect(),
            },
            redactions: None,
        }
    }

    fn cost(output: &LocateOutput) -> u32 {
        crate::tokenizer::count_tokens(&format!("{}\n", serde_json::to_string(output).unwrap()))
    }

    /// The overflow is counted whether or not it is listed; the entries come only
    /// when asked for.
    #[test]
    fn an_unrequested_overflow_is_counted_and_not_listed() {
        let mut out = answer(vec![item("a.py", Some("changed"))], 7);
        out.fit_to(100_000, false);
        assert_eq!(out.overflow.total, 7);
        assert!(out.overflow.items.is_empty());
        let json = serde_json::to_value(&out).unwrap();
        assert_eq!(json["overflow"]["total"], 7);
        assert!(json.get("overflow_count").is_none());

        let mut listed = answer(vec![item("a.py", Some("changed"))], 7);
        listed.fit_to(100_000, true);
        assert_eq!(listed.overflow.items.len(), 5);
    }

    /// A listed overflow is charged, and the first thing a tight budget takes: no
    /// ranked item goes while an entry of the pointer list is left, and what it
    /// sheds stays counted.
    #[test]
    fn a_listed_overflow_is_charged_and_shed_before_any_item() {
        let items = vec![
            item("a.py", Some("changed")),
            item("b.py", None),
            item("c.py", None),
        ];
        let mut out = answer(items, 7);
        let full = cost(&out);
        out.fit_to(full - 1, true);
        assert_eq!(out.items.len(), 3, "an item went before the overflow");
        assert!(out.overflow.items.len() < 5);
        assert_eq!(out.overflow.total, 7);
        assert!(out.fit.least.is_none());
        assert!(cost(&out) < full);
    }

    /// Below the least the answer can say it says so, and cuts nothing more: the
    /// first item of every changed file stays, and every shed item is counted.
    #[test]
    fn a_budget_below_the_least_is_declared() {
        let items = vec![
            item("a.py", Some("changed")),
            item("a.py", Some("changed")),
            item("b.py", None),
        ];
        let mut out = answer(items, 0);
        out.fit_to(1, false);
        assert_eq!(out.items.len(), 1);
        assert_eq!(out.items[0].path, "a.py");
        assert_eq!(out.overflow.total, 2);
        assert!(out.fit.least.is_some_and(|least| least > 1));
        assert!(
            out.coverage
                .limits
                .contains(&crate::resource::LimitReason::SelectionBudgetExceeded)
        );
    }
}
