use std::path::Path;

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
    /// Blast-radius counts over the ranked items (#135): distinct files,
    /// changed vs context fragments, and how many ranked items (changed or
    /// context) are tests.
    pub summary: Summary,
    pub item_count: usize,
    pub items: Vec<LocateItem>,
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

/// #182 unified the `TestEdge` builder and the needs matcher onto
/// `crate::testfiles`, but missed this one — locate carried a third, weaker
/// answer, and `summary.tests` in the shipped blast-radius view undercounted
/// because of it. It lowercased the path first, so every CamelCase convention
/// was invisible: `FooTest.java` outside a test directory, `AuthSpec.scala`,
/// `widget-test.js` (hyphen form) and `src/tests.rs` all read as ordinary code.
///
/// One rule moves the other way: a `testing/` directory no longer counts. The
/// shared implementation excludes it deliberately — `testing` is Go's stdlib
/// package name and such directories hold helpers, not tests — and its unit
/// tests pin `src/testing.rs` as non-test.
fn is_test_path(path: &str) -> bool {
    crate::testfiles::is_test_path(Path::new(path))
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
    if is_test_path(path) {
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
