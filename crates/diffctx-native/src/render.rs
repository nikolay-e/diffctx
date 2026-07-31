use std::path::Path;
use std::sync::Arc;

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};
use serde::Serialize;

use crate::config::render::RENDER;
use crate::types::{Fragment, FragmentId, FragmentKind};

/// Orientation header for the diff-context output: tells the reader *what*
/// changed before they read the fragments. Empty for working-tree / no-commit
/// diffs.
#[derive(Default)]
pub struct ChangeSummary {
    pub commit_message: Option<String>,
    pub changed_files: Vec<String>,
    pub deleted_files: Vec<String>,
    pub renamed_files: Vec<(String, String)>,
    pub lockfile_changes: Vec<String>,
}

fn serialize_renames<S>(renames: &[(String, String)], serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    use serde::ser::SerializeSeq;
    let mut seq = serializer.serialize_seq(Some(renames.len()))?;
    for (from, to) in renames {
        let mut m = std::collections::BTreeMap::new();
        m.insert("from", from);
        m.insert("to", to);
        seq.serialize_element(&m)?;
    }
    seq.end()
}

#[derive(Serialize)]
pub struct DiffContextOutput {
    pub name: String,
    #[serde(rename = "type")]
    pub output_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit_message: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub changed_files: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub deleted_files: Vec<String>,
    #[serde(
        skip_serializing_if = "Vec::is_empty",
        serialize_with = "serialize_renames"
    )]
    pub renamed_files: Vec<(String, String)>,
    /// Lock files touched by the range. Paths only — the raw hunks are
    /// thousands of tokens of checksum churn for one line of signal (#112).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub lockfile_changes: Vec<String>,
    pub fragment_count: usize,
    pub fragments: Vec<FragmentEntry>,
    #[serde(skip)]
    pub latency: Option<LatencyBreakdown>,
}

pub struct LatencyBreakdown {
    /// Pre-heavy-phase work: hunk parse, untracked scan, ignore resolution and
    /// the `git diff` calls. Outside every timer until #183, which is why the
    /// reported phases could not be reconciled with the wall clock.
    pub pre_phase_ms: f64,
    pub parse_changed_ms: f64,
    pub universe_walk_ms: f64,
    pub discovery_ms: f64,
    pub parse_discovered_ms: f64,
    pub tokenization_ms: f64,
    /// Typed dependency graph construction: edge builders + dedup + hub
    /// suppression + per-source cap. Carved out of `scoring_ms` (which
    /// used to absorb it) so the cost distribution is truthful. Zero for
    /// BM25 mode (no graph built).
    pub graph_build_ms: f64,
    /// Combined graph build + scoring + selection time. Kept for
    /// backward compatibility with the existing checkpoint schema; the
    /// split values below are the new diagnostic signal.
    pub scoring_selection_ms: f64,
    pub total_ms: f64,
    /// Heavy-phase rank computation only (PPR/EGO/BM25 + relevance
    /// filtering). Graph construction is reported in `graph_build_ms`;
    /// the selection stage is excluded.
    pub scoring_ms: f64,
    /// Selection stage only (lazy greedy / Boltzmann + post-passes).
    pub selection_ms: f64,
    /// Size of the candidate fragment universe handed to the scoring
    /// strategy (after fragment generation + signature variants but
    /// before per-strategy filtering). Surfaces blowup on large repos —
    /// pathological scoring time is correlated with this number, not
    /// with `fragment_count` (which is the *output* size after
    /// selection).
    pub candidate_count: usize,
    /// Edge count of the typed dependency graph used by PPR/EGO. Zero
    /// for BM25 mode (no graph built).
    pub edge_count: usize,
    /// Number of greedy iterations actually executed (selected non-core
    /// fragments). Bounded by `selected.len() - core.len()`. Pairs with
    /// `selection_ms` to spot lazy-heap blowup vs. genuine large output.
    pub greedy_iters: usize,
    /// Edge count after merge + hub suppression, before per-source cap.
    pub edges_before_cap: usize,
    /// Edges discarded by the per-source top-K cap.
    pub edges_dropped_by_cap: usize,
    /// Source nodes whose outgoing edge list was truncated by the cap.
    pub nodes_capped: usize,
    /// The K value applied for the per-source cap.
    pub max_out_edges_per_node: usize,
    /// PPR push iteration was truncated by `max_pushes_cap` before
    /// convergence. When true, `rel_scores` are biased toward seeds
    /// and absolute file_recall on this instance should be flagged
    /// in post-analysis. Always false for non-PPR scoring modes.
    pub ppr_truncated: bool,
    pub ppr_forward_pushes: usize,
    pub ppr_backward_pushes: usize,
    /// Additive stopping certificate: upper bound
    /// (`tau * peak_density * remaining_budget`) on utility foregone by
    /// adaptive stopping. 0 when the greedy loop ended for another
    /// reason (budget exhausted, no candidates, singleton override).
    pub stopping_certificate: f64,
    /// Lifetime peak physical memory of the process, sampled in-process
    /// at the end of the run. 0 when the platform query fails.
    pub peak_rss_bytes: u64,
    /// Per-category (raw, first-seen deduped) edge emission counts from
    /// pass 1 of the two-pass edge build, sorted by category name. Names
    /// the builder category behind near-dense emission blowups (#116).
    /// Empty for BM25 mode (no graph built).
    pub edge_emissions_by_category: Vec<(&'static str, u64, u64)>,
}

#[derive(Serialize, Clone)]
pub struct FragmentEntry {
    pub path: String,
    pub lines: String,
    /// `Some("changed")` for fragments overlapping the diff hunks; omitted
    /// (treated as supporting context) otherwise. This is the single signal a
    /// reader needs to tell the change apart from its context.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Arc<str>>,
}

struct SymbolPatterns {
    function: Vec<Regex>,
    class: Vec<Regex>,
    r#struct: Vec<Regex>,
    interface: Vec<Regex>,
    r#enum: Vec<Regex>,
    r#impl: Vec<Regex>,
    r#type: Vec<Regex>,
    module: Vec<Regex>,
    section: Vec<Regex>,
}

static SYMBOL_PATTERNS: Lazy<SymbolPatterns> = Lazy::new(|| {
    SymbolPatterns {
    function: vec![
        Regex::new(r"(?m)^\s*(?:async\s+)?def\s+(\w+)\s*\(").unwrap(),
        Regex::new(r"(?m)^\s*(?:export\s+)?(?:async\s+)?function\s+(\w+)\s*[\(<]").unwrap(),
        Regex::new(r"(?m)^\s*(?:export\s+)?(?:const|let|var)\s+(\w+)\s*=\s*(?:async\s+)?(?:\([^)]*\)|\w)\s*=>").unwrap(),
        Regex::new(r"(?m)^func\s+(?:\([^)]+\)\s+)?(\w+)\s*[\(\[]").unwrap(),
        Regex::new(r"(?m)^\s*(?:pub\s+)?(?:async\s+)?fn\s+(\w+)\s*[\(<]").unwrap(),
        Regex::new(r"(?m)^\s*(?:(?:public|private|protected|static)\s+)*\w[\w<>\[\],]*\s+(\w+)\s*\(").unwrap(),
    ],
    class: vec![
        Regex::new(r"(?m)^\s*class\s+(\w+)\s*[:\({\s]").unwrap(),
        Regex::new(r"(?m)^\s*(?:export\s+)?(?:abstract\s+)?class\s+(\w+)").unwrap(),
    ],
    r#struct: vec![
        Regex::new(r"(?m)^\s*(?:pub\s+)?struct\s+(\w+)").unwrap(),
        Regex::new(r"(?m)^\s*type\s+(\w+)\s+struct\s*\{").unwrap(),
    ],
    interface: vec![
        Regex::new(r"(?m)^\s*(?:export\s+)?interface\s+(\w+)").unwrap(),
        Regex::new(r"(?m)^\s*type\s+(\w+)\s+interface\s*\{").unwrap(),
        Regex::new(r"(?m)^\s*(?:pub\s+)?trait\s+(\w+)").unwrap(),
    ],
    r#enum: vec![
        Regex::new(r"(?m)^\s*(?:pub\s+)?enum\s+(\w+)").unwrap(),
        Regex::new(r"(?m)^\s*class\s+(\w+)\s*\(.*Enum\)").unwrap(),
    ],
    r#impl: vec![
        Regex::new(r"(?m)^\s*impl(?:<[^>]+>)?\s+(\w+)").unwrap(),
    ],
    r#type: vec![
        Regex::new(r"(?m)^\s*(?:export\s+)?type\s+(\w+)").unwrap(),
        Regex::new(r"(?m)^\s*type\s+(\w+)\s").unwrap(),
    ],
    module: vec![
        Regex::new(r"(?m)^\s*(?:pub\s+)?mod\s+(\w+)").unwrap(),
        Regex::new(r"(?m)^\s*package\s+(\w+)").unwrap(),
    ],
    section: vec![
        Regex::new(r"(?m)^#{1,6}\s+(\S[^\n]*)$").unwrap(),
    ],
}
});

fn extract_symbol(frag: &Fragment) -> Option<String> {
    let patterns = match frag.kind {
        FragmentKind::Function | FragmentKind::FunctionSignature => &SYMBOL_PATTERNS.function,
        FragmentKind::Class | FragmentKind::ClassSignature => &SYMBOL_PATTERNS.class,
        FragmentKind::Struct | FragmentKind::StructSignature => &SYMBOL_PATTERNS.r#struct,
        FragmentKind::Interface | FragmentKind::InterfaceSignature => &SYMBOL_PATTERNS.interface,
        FragmentKind::Enum | FragmentKind::EnumSignature => &SYMBOL_PATTERNS.r#enum,
        FragmentKind::Impl => &SYMBOL_PATTERNS.r#impl,
        FragmentKind::Type => &SYMBOL_PATTERNS.r#type,
        FragmentKind::Module => &SYMBOL_PATTERNS.module,
        FragmentKind::Section => &SYMBOL_PATTERNS.section,
        _ => return None,
    };

    for pattern in patterns {
        if let Some(caps) = pattern.captures(&frag.content) {
            if let Some(m) = caps.get(1) {
                let result = m.as_str().trim();
                return Some(if frag.kind == FragmentKind::Section {
                    result
                        .chars()
                        .take(RENDER.section_symbol_max_chars)
                        .collect()
                } else {
                    result.to_string()
                });
            }
        }
    }
    None
}

// Windows paths use `\` as the component separator, so a rendered path must
// be normalized to `/` for cross-platform-readable output. On POSIX, `\` is
// a legal filename character (`src\utils.py` is one file, not a directory
// separator) — rewriting it there would report a path that does not exist
// and silently merge two distinct files under one heading in `by_path`.
#[cfg(windows)]
fn normalize_path_separators(s: std::borrow::Cow<'_, str>) -> String {
    s.replace('\\', "/")
}

#[cfg(not(windows))]
fn normalize_path_separators(s: std::borrow::Cow<'_, str>) -> String {
    s.into_owned()
}

pub(crate) fn get_relative_path(frag: &Fragment, repo_root: &Path) -> String {
    let frag_path = Path::new(frag.path());
    if !frag_path.is_absolute() {
        return normalize_path_separators(frag_path.to_string_lossy());
    }
    normalize_path_separators(
        frag_path
            .strip_prefix(repo_root)
            .unwrap_or(frag_path)
            .to_string_lossy(),
    )
}

fn create_fragment_entry(frag: &Fragment, path_str: &str) -> FragmentEntry {
    let symbol = frag.symbol_name.clone().or_else(|| extract_symbol(frag));
    let content = if frag.content.is_empty() {
        None
    } else {
        Some(Arc::clone(&frag.content))
    };

    FragmentEntry {
        path: path_str.to_string(),
        lines: format!("{}-{}", frag.start_line(), frag.end_line()),
        role: None,
        kind: frag.kind.as_str().to_string(),
        symbol,
        content,
    }
}

/// A fragment carries the `changed` role when it IS a core, or when it is the
/// hunk-window excerpt that was substituted for one. The excerpt's id is not in
/// `core_ids` — it is a synthetic span cut out of the core — so without this the
/// downshift would silently strip the change marker from the output, which is
/// worse than the over-dump it replaces. `locate.rs` already treats `Excerpt`
/// this way; both surfaces must agree.
fn carries_changed_role(frag: &Fragment, core_ids: &FxHashSet<FragmentId>) -> bool {
    core_ids.contains(&frag.id) || frag.kind == FragmentKind::Excerpt
}

/// Collapse a file's fragments (sorted by start line, ties by descending end
/// line) into the rendered entries. Two behaviors:
/// - a same-role fragment fully contained in the running range (`next.end <=
///   end`) is dropped: its content is already covered by the enclosing
///   fragment (e.g. a symbol-level "function" extraction and a hunk-level
///   "chunk" both covering the same edited lines), so keeping it is pure
///   duplication, not additional information.
/// - a same-role fragment that is line-contiguous with the running range
///   (`next.start == end + 1`) is merged into it.
/// Both are lossless on line coverage and remove the per-fragment scaffolding
/// tax that dominates output on one-line/near-duplicate snippets.
fn merge_file_fragments(
    rel_path: &str,
    frags: &[&Fragment],
    core_ids: &FxHashSet<FragmentId>,
) -> Vec<(bool, u32, FragmentEntry)> {
    let mut out: Vec<(bool, u32, FragmentEntry)> = Vec::new();
    let mut i = 0;
    while i < frags.len() {
        let first = frags[i];
        let role_changed = carries_changed_role(first, core_ids);
        let mut end = first.end_line();
        let mut parts: Vec<&str> = vec![first.content.trim_end_matches('\n')];
        let mut j = i + 1;
        while j < frags.len() {
            let next = frags[j];
            if carries_changed_role(next, core_ids) != role_changed {
                break;
            }
            if next.end_line() <= end {
                // Fully contained in the range covered so far - redundant.
                j += 1;
            } else if next.start_line() == end + 1 {
                parts.push(next.content.trim_end_matches('\n'));
                end = next.end_line();
                j += 1;
            } else {
                break;
            }
        }

        let mut entry = create_fragment_entry(first, rel_path);
        if j > i + 1 {
            entry.lines = format!("{}-{}", first.start_line(), end);
            let merged = parts.join("\n");
            entry.content = if merged.is_empty() {
                None
            } else {
                Some(Arc::from(merged.as_str()))
            };
        }
        entry.role = role_changed.then(|| "changed".to_string());
        out.push((role_changed, first.start_line(), entry));
        i = j;
    }
    out
}

pub fn build_diff_context_output(
    repo_root: &Path,
    selected: &[Fragment],
    no_content: bool,
    core_ids: &FxHashSet<FragmentId>,
    rel_scores: &FxHashMap<FragmentId, f64>,
    change: ChangeSummary,
) -> DiffContextOutput {
    let mut by_path: FxHashMap<String, Vec<&Fragment>> = FxHashMap::default();
    for frag in selected {
        by_path
            .entry(get_relative_path(frag, repo_root))
            .or_default()
            .push(frag);
    }

    // Changed code first (the answer to "what changed"), then supporting
    // context ordered by descending per-file relevance so the reader's primacy
    // attention lands on the most relevant material, not on alphabetical noise.
    let mut changed: Vec<(String, u32, FragmentEntry)> = Vec::new();
    let mut context: Vec<(f64, String, u32, FragmentEntry)> = Vec::new();
    for (rel_path, frags) in &by_path {
        let mut sorted: Vec<&Fragment> = frags.clone();
        // Tie-break by descending end line so, among same-start fragments, the
        // widest range sorts first and containment-absorption below (which scans
        // forward from the first entry of a run) sees the enclosing range before
        // any of its nested sub-fragments.
        sorted.sort_by_key(|f| (f.start_line(), std::cmp::Reverse(f.end_line())));
        let file_rel = sorted
            .iter()
            .map(|f| rel_scores.get(&f.id).copied().unwrap_or(0.0))
            .fold(0.0_f64, f64::max);
        for (role_changed, start, mut entry) in merge_file_fragments(rel_path, &sorted, core_ids) {
            if no_content {
                entry.content = None;
            }
            if role_changed {
                changed.push((rel_path.clone(), start, entry));
            } else {
                context.push((file_rel, rel_path.clone(), start, entry));
            }
        }
    }

    changed.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    context.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.1.cmp(&b.1))
            .then(a.2.cmp(&b.2))
    });

    let mut fragments_out: Vec<FragmentEntry> = Vec::with_capacity(changed.len() + context.len());
    fragments_out.extend(changed.into_iter().map(|(_, _, e)| e));
    fragments_out.extend(context.into_iter().map(|(_, _, _, e)| e));

    let resolved = repo_root
        .canonicalize()
        .unwrap_or_else(|_| repo_root.to_path_buf());
    let name = resolved
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| resolved.to_string_lossy().to_string());

    DiffContextOutput {
        name,
        output_type: "diff_context".to_string(),
        commit_message: change.commit_message,
        changed_files: change.changed_files,
        deleted_files: change.deleted_files,
        renamed_files: change.renamed_files,
        lockfile_changes: change.lockfile_changes,
        fragment_count: fragments_out.len(),
        fragments: fragments_out,
        latency: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_output(renamed_files: Vec<(String, String)>) -> DiffContextOutput {
        DiffContextOutput {
            name: "repo".to_string(),
            output_type: "diff_context".to_string(),
            commit_message: None,
            changed_files: Vec::new(),
            deleted_files: Vec::new(),
            renamed_files,
            lockfile_changes: Vec::new(),
            fragment_count: 0,
            fragments: Vec::new(),
            latency: None,
        }
    }

    #[test]
    fn renamed_files_serialize_as_labelled_from_to_in_yaml() {
        let out = empty_output(vec![("old.py".to_string(), "new.py".to_string())]);
        let yaml = serde_yaml::to_string(&out).unwrap();
        assert!(
            yaml.contains("from: old.py"),
            "expected labelled `from:` entry, got:\n{yaml}"
        );
        assert!(
            yaml.contains("to: new.py"),
            "expected labelled `to:` entry, got:\n{yaml}"
        );
        // Guards against serde's default tuple-as-two-element-sequence shape
        // (`- - old.py\n  - new.py`), which drops the from/to labels.
        assert!(!yaml.contains("- - old.py"));
    }

    #[test]
    fn renamed_files_serialize_as_labelled_from_to_in_json() {
        let out = empty_output(vec![("old.py".to_string(), "new.py".to_string())]);
        let json = serde_json::to_value(&out).unwrap();
        let renamed = json["renamed_files"]
            .as_array()
            .expect("renamed_files must serialize as an array");
        assert_eq!(renamed.len(), 1);
        assert_eq!(renamed[0]["from"], "old.py");
        assert_eq!(renamed[0]["to"], "new.py");
        assert!(
            renamed[0].is_object(),
            "must not serialize as a positional [old, new] tuple: {renamed:?}"
        );
    }

    #[test]
    fn renamed_files_empty_is_omitted_from_output() {
        let out = empty_output(Vec::new());
        let json = serde_json::to_value(&out).unwrap();
        assert!(json.get("renamed_files").is_none());
    }

    fn frag_at(path: &str) -> Fragment {
        Fragment {
            id: FragmentId::new(Arc::from(path), 1, 5),
            kind: FragmentKind::Function,
            content: Arc::from(""),
            identifiers: FxHashSet::default(),
            token_count: 1,
            symbol_name: None,
        }
    }

    #[cfg(unix)]
    #[test]
    fn get_relative_path_posix_backslash_in_filename_round_trips_unchanged() {
        // On POSIX `\` is a legal filename character: `src\utils.py` is one
        // file, not `src/utils.py` in a subdirectory. Rewriting the
        // separator here would report a path that does not exist.
        let frag = frag_at("src\\utils.py");
        let root = Path::new("/repo");
        let rel = get_relative_path(&frag, root);
        assert_eq!(rel, "src\\utils.py");
    }

    #[cfg(unix)]
    #[test]
    fn get_relative_path_strips_repo_root_on_posix() {
        let frag = frag_at("/repo/src/lib.rs");
        let root = Path::new("/repo");
        let rel = get_relative_path(&frag, root);
        assert_eq!(rel, "src/lib.rs");
    }
}
