use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};
use similar::{ChangeTag, TextDiff};

use crate::config::budget::BUDGET;
use crate::config::tokenization::TOKENIZATION;
use crate::core::{compute_seed_weights, identify_core_fragments};
use crate::edges;
use crate::mode::{PipelineConfig, ScoringMode};
use crate::parsers::fragment_file;
use crate::render::{DiffContextOutput, build_diff_context_output};
use crate::scoring::create_scoring_strategy;
use crate::signatures::generate_signature_variants;
use crate::types::{DiffHunk, Fragment, FragmentId};

pub struct MemoryRepo {
    pub name: String,
    pub initial_files: FxHashMap<String, String>,
    pub changed_files: FxHashMap<String, String>,
}

pub fn build_diff_context_in_memory(
    repo: &MemoryRepo,
    budget_tokens: Option<u32>,
    alpha: f64,
    tau: f64,
    no_content: bool,
    scoring_mode: ScoringMode,
) -> DiffContextOutput {
    let hunks = compute_memory_hunks(&repo.initial_files, &repo.changed_files);
    if hunks.is_empty() {
        return DiffContextOutput::empty(&repo.name);
    }

    let diff_text = compute_memory_diff_text(&repo.initial_files, &repo.changed_files);
    let all_files = merge_file_contents(&repo.initial_files, &repo.changed_files);

    let changed_paths: FxHashSet<String> =
        hunks.iter().map(|h| h.path.as_ref().to_string()).collect();

    let changed_file_paths: Vec<PathBuf> = changed_paths.iter().map(PathBuf::from).collect();
    let all_file_paths: Vec<PathBuf> = all_files.keys().map(PathBuf::from).collect();
    let file_cache: FxHashMap<PathBuf, String> = all_files
        .iter()
        .map(|(k, v)| (PathBuf::from(k), v.clone()))
        .collect();

    let discovered = edges::discover_all_related_files(
        &changed_file_paths,
        &all_file_paths,
        None,
        Some(&file_cache),
    );
    let discovered_paths: FxHashSet<String> = discovered
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();

    let allowed_paths: FxHashSet<&str> = changed_paths
        .iter()
        .chain(discovered_paths.iter())
        .map(|s| s.as_str())
        .collect();

    let mut all_fragments: Vec<Fragment> = Vec::new();
    let mut seen: FxHashSet<FragmentId> = FxHashSet::default();
    for (path, content) in &all_files {
        if !allowed_paths.contains(path.as_str()) {
            continue;
        }
        let path_arc: Arc<str> = Arc::from(path.as_str());
        let frags = fragment_file(path_arc, content);
        for f in frags {
            if seen.insert(f.id.clone()) {
                all_fragments.push(f);
            }
        }
    }

    crate::pipeline::assign_token_counts(&mut all_fragments);

    let core_ids = identify_core_fragments(&hunks, &all_fragments);

    // The same two inputs the shipped pipeline gives the selector. Passing
    // `None` for both made this harness score a different system: no
    // excerpt-downshift (#149), so an oversized core was skipped rather than
    // narrowed, and no I(f) prior, so per-file importance did not shape
    // admission. Iterating on either of those against this harness measured
    // something nobody runs.
    let mut core_excerpts =
        crate::excerpt::generate_core_excerpts(&all_fragments, &core_ids, &hunks);
    crate::pipeline::assign_excerpt_token_counts(&mut core_excerpts);

    let mut sig_frags = generate_signature_variants(&all_fragments);
    crate::pipeline::assign_token_counts(&mut sig_frags);
    all_fragments.extend(sig_frags);

    let effective_budget = budget_tokens.unwrap_or(BUDGET.unlimited);
    let mut config = PipelineConfig::from_mode(scoring_mode);
    config.ppr_alpha = alpha;
    let seed_weights = compute_seed_weights(&hunks, &core_ids, &all_fragments);

    let discovered_arc: FxHashSet<Arc<str>> = discovered_paths
        .iter()
        .map(|s| Arc::from(s.as_str()))
        .collect();

    let strategy = create_scoring_strategy(&config);

    let scoring_result = strategy.score_and_filter(
        &all_fragments,
        &core_ids,
        &hunks,
        None,
        Some(&seed_weights),
        Some(&discovered_arc),
        // The corpus harness has no timeout contract; before #210 it
        // inherited whatever ceiling the last in-process run left behind.
        crate::deadline::Deadline::none(),
    );

    let needs = crate::utility::needs::needs_from_diff(&all_fragments, &core_ids, &diff_text);

    // The same envelope charge the product pipeline applies (#241): a harness
    // that spends the budget differently scores a system nobody runs (#149).
    let mut listed: Vec<String> = changed_paths.iter().cloned().collect();
    listed.sort();
    let selection_budget =
        effective_budget.saturating_sub(crate::pipeline::envelope_token_cost(None, &listed));

    let (mut selected, _, _, stand_in_ids) = crate::pipeline::select_and_postpass(
        &scoring_result,
        &all_fragments,
        &core_ids,
        &needs,
        &core_excerpts,
        config.objective,
        selection_budget,
        tau,
    );

    let used: u32 = selected.iter().map(|f| f.token_count).sum();
    let remaining = selection_budget.saturating_sub(used);
    let changed_files: Vec<PathBuf> = changed_paths.iter().map(PathBuf::from).collect();
    crate::postpass::ensure_changed_files_represented(
        &mut selected,
        &all_fragments,
        &changed_files,
        remaining,
        Path::new("."),
        &[],
        None,
        &core_ids,
        &FxHashMap::default(),
        &stand_in_ids,
    );

    let dummy_root = Path::new(".");
    let mut changed_list: Vec<String> = changed_paths.iter().cloned().collect();
    changed_list.sort();
    let change = crate::render::ChangeSummary {
        lockfile_changes: Vec::new(),
        ignored_changes: Vec::new(),
        policy_excluded_count: 0,
        commit_message: None,
        changed_files: changed_list,
        deleted_files: Vec::new(),
        renamed_files: Vec::new(),
    };
    build_diff_context_output(
        dummy_root,
        &selected,
        no_content,
        &core_ids,
        &stand_in_ids,
        &scoring_result.rel_scores,
        change,
    )
}

fn compute_memory_hunks(
    initial: &FxHashMap<String, String>,
    changed: &FxHashMap<String, String>,
) -> Vec<DiffHunk> {
    let mut hunks = Vec::new();

    for (path, new_content) in changed {
        let old_content = initial.get(path).map(|s| s.as_str()).unwrap_or("");
        if old_content == new_content {
            continue;
        }
        let path_arc: Arc<str> = Arc::from(path.as_str());
        let file_hunks = diff_to_hunks(&path_arc, old_content, new_content);
        hunks.extend(file_hunks);
    }

    for (path, _old_content) in initial {
        if !changed.contains_key(path) {
            let path_arc: Arc<str> = Arc::from(path.as_str());
            let old_line_count = initial[path].lines().count() as u32;
            if old_line_count > 0 {
                hunks.push(DiffHunk {
                    path: path_arc,
                    new_start: 1,
                    new_len: 0,
                    old_start: 1,
                    old_len: old_line_count,
                });
            }
        }
    }

    hunks
}

fn diff_to_hunks(path: &Arc<str>, old: &str, new: &str) -> Vec<DiffHunk> {
    let diff = TextDiff::from_lines(old, new);
    let mut hunks = Vec::new();

    let mut new_line: u32 = 0;
    let mut old_line: u32 = 0;

    let mut hunk_new_start: Option<u32> = None;
    let mut hunk_new_len: u32 = 0;
    let mut hunk_old_start: u32 = 0;
    let mut hunk_old_len: u32 = 0;

    // git's `--unified=0` reports a pure deletion's new-side start as the
    // line BEFORE the gap (`@@ -5 +4,0 @@`), and `core_selection_range`
    // anchors on that. Starting at `new_line + 1` here put the harness one
    // line below production for every deletion-only hunk, so the corpus
    // scored a selection nobody ships.
    let finish = |start: u32, new_len: u32| {
        if new_len == 0 {
            start.saturating_sub(1)
        } else {
            start
        }
    };
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Equal => {
                if let Some(start) = hunk_new_start.take() {
                    hunks.push(DiffHunk {
                        path: Arc::clone(path),
                        new_start: finish(start, hunk_new_len),
                        new_len: hunk_new_len,
                        old_start: hunk_old_start,
                        old_len: hunk_old_len,
                    });
                    hunk_new_len = 0;
                    hunk_old_len = 0;
                }
                new_line += 1;
                old_line += 1;
            }
            ChangeTag::Delete => {
                if hunk_new_start.is_none() {
                    hunk_new_start = Some(new_line + 1);
                    hunk_old_start = old_line + 1;
                }
                hunk_old_len += 1;
                old_line += 1;
            }
            ChangeTag::Insert => {
                if hunk_new_start.is_none() {
                    hunk_new_start = Some(new_line + 1);
                    hunk_old_start = old_line + 1;
                }
                hunk_new_len += 1;
                new_line += 1;
            }
        }
    }

    if let Some(start) = hunk_new_start {
        hunks.push(DiffHunk {
            path: Arc::clone(path),
            new_start: finish(start, hunk_new_len),
            new_len: hunk_new_len,
            old_start: hunk_old_start,
            old_len: hunk_old_len,
        });
    }

    hunks
}

fn compute_memory_diff_text(
    initial: &FxHashMap<String, String>,
    changed: &FxHashMap<String, String>,
) -> String {
    let mut result = String::new();

    let mut paths: Vec<&String> = changed.keys().collect();
    paths.sort();

    for path in paths {
        let new_content = &changed[path];
        let old_content = initial.get(path).map(|s| s.as_str()).unwrap_or("");
        if old_content == new_content {
            continue;
        }

        let diff = TextDiff::from_lines(old_content, new_content);
        let mut udiff = diff.unified_diff();
        let formatted = udiff
            .context_radius(TOKENIZATION.diff_context_radius)
            .header(&format!("a/{path}"), &format!("b/{path}"));
        let _ = write!(result, "{formatted}");
    }

    let mut deleted_paths: Vec<&String> = initial
        .keys()
        .filter(|p| !changed.contains_key(*p))
        .collect();
    deleted_paths.sort();

    for path in deleted_paths {
        let old_content = &initial[path];
        let empty = String::new();
        let diff = TextDiff::from_lines(old_content, &empty);
        let mut udiff = diff.unified_diff();
        let formatted = udiff
            .context_radius(TOKENIZATION.diff_context_radius)
            .header(&format!("a/{path}"), "/dev/null");
        let _ = write!(result, "{formatted}");
    }

    result
}

fn merge_file_contents(
    initial: &FxHashMap<String, String>,
    changed: &FxHashMap<String, String>,
) -> FxHashMap<String, String> {
    let mut merged = initial.clone();
    for (path, content) in changed {
        merged.insert(path.clone(), content.clone());
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same deletion through both parsers: the harness's in-memory differ
    /// and git's own `--unified=0` output through the production parser.
    ///
    /// The sibling test below compares against a hand-written `DiffHunk`, which
    /// cannot notice `git.rs` drifting away from it — the harness would go on
    /// scoring a system nobody ships and the literal would still be green
    /// (#245). This one has no literal: git produces the bytes.
    #[test]
    fn both_pipelines_anchor_a_pure_deletion_identically() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let old_file = tmp.path().join("old.py");
        let new_file = tmp.path().join("new.py");
        let old = "l1\nl2\nl3\nl4\nl5\nl6\nl7\n";
        let new = "l1\nl2\nl3\nl4\nl6\nl7\n";
        std::fs::write(&old_file, old).expect("write old");
        std::fs::write(&new_file, new).expect("write new");

        // `--no-index` needs no repository and emits exactly the header and
        // hunk syntax `parse_diff` consumes in production.
        let output = std::process::Command::new("git")
            .args(["diff", "--no-index", "--unified=0", "-M"])
            .arg(&old_file)
            .arg(&new_file)
            .output()
            .expect("git diff --no-index");
        let diff_text = String::from_utf8_lossy(&output.stdout);
        let from_git = crate::git::parse_hunks_from_diff_output(&diff_text, tmp.path());
        assert_eq!(from_git.len(), 1, "git reported {diff_text:?}");

        let path: Arc<str> = Arc::from("f.py");
        let from_harness = diff_to_hunks(&path, old, new);
        assert_eq!(from_harness.len(), 1);

        assert_eq!(
            (
                from_harness[0].new_start,
                from_harness[0].new_len,
                from_harness[0].old_start,
                from_harness[0].old_len
            ),
            (
                from_git[0].new_start,
                from_git[0].new_len,
                from_git[0].old_start,
                from_git[0].old_len
            ),
            "the harness anchors a pure deletion where git does not"
        );
        assert_eq!(
            from_harness[0].core_selection_range(),
            from_git[0].core_selection_range(),
            "same hunk, different core window"
        );
    }

    /// The harness and production must anchor a pure deletion on the same
    /// line. git's `--unified=0` reports the deletion of old line 5 as
    /// `@@ -5 +4,0 @@` — new-side start is the line BEFORE the gap — and
    /// `core_selection_range` anchors on that value; the in-memory path used
    /// to start one line later, so the corpus scored a selection nobody ships.
    #[test]
    fn a_pure_deletion_anchors_where_git_anchors_it() {
        let path: Arc<str> = Arc::from("f.py");
        let old = "l1\nl2\nl3\nl4\nl5\nl6\nl7\n";
        let new = "l1\nl2\nl3\nl4\nl6\nl7\n";
        let hunks = diff_to_hunks(&path, old, new);
        assert_eq!(hunks.len(), 1);
        let from_git = DiffHunk {
            path: path.clone(),
            new_start: 4,
            new_len: 0,
            old_start: 5,
            old_len: 1,
        };
        assert_eq!(
            (
                hunks[0].new_start,
                hunks[0].new_len,
                hunks[0].old_start,
                hunks[0].old_len
            ),
            (
                from_git.new_start,
                from_git.new_len,
                from_git.old_start,
                from_git.old_len
            )
        );
        assert_eq!(
            hunks[0].core_selection_range(),
            from_git.core_selection_range()
        );

        // A deletion at the very top: git says `@@ -1 +0,0 @@`, and both
        // sides clamp the anchor to line 1.
        let hunks = diff_to_hunks(&path, "a\nb\n", "b\n");
        assert_eq!((hunks[0].new_start, hunks[0].new_len), (0, 0));
        assert_eq!(hunks[0].core_selection_range(), (1, 1));

        // A replacement keeps its start: no off-by-one the other way.
        let hunks = diff_to_hunks(&path, "a\nb\nc\n", "a\nB\nc\n");
        assert_eq!((hunks[0].new_start, hunks[0].new_len), (2, 1));
    }
}
