use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};
use similar::{ChangeTag, TextDiff};

use crate::config::budget::BUDGET;
use crate::config::tokenization::TOKENIZATION;
use crate::mode::{PipelineConfig, ScoringMode};
use crate::parsers::fragment_file;
use crate::render::{DiffContextOutput, build_diff_context_output};
use crate::types::{DiffHunk, Fragment, FragmentId};

pub struct MemoryRepo {
    pub name: String,
    pub initial_files: FxHashMap<String, String>,
    pub changed_files: FxHashMap<String, String>,
}

pub struct FragmentRow {
    pub kind: &'static str,
    pub symbol: Option<String>,
    pub start_line: u32,
    pub end_line: u32,
}

pub fn fragment_rows(path: &str, content: &str) -> Vec<FragmentRow> {
    let mut rows: Vec<FragmentRow> = fragment_file(Arc::from(path), content)
        .into_iter()
        .map(|f| FragmentRow {
            kind: f.kind.as_str(),
            symbol: f.symbol_name,
            start_line: f.id.start_line,
            end_line: f.id.end_line,
        })
        .collect();
    rows.sort_by(|a, b| {
        (a.start_line, a.end_line, a.kind, &a.symbol).cmp(&(
            b.start_line,
            b.end_line,
            b.kind,
            &b.symbol,
        ))
    });
    rows
}

pub fn build_diff_context_in_memory(
    repo: &MemoryRepo,
    budget_tokens: Option<u32>,
    alpha: f64,
    tau: Option<f64>,
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

    let mut config = PipelineConfig::from_mode(scoring_mode);
    config.ppr_alpha = alpha;

    // The product's discovery ensemble — structural, test-file, BM25 top-k —
    // over the in-memory files. The harness used to run the structural
    // strategy alone, so the corpus measured a narrower universe than the
    // one shipped (#232): a test file or a lexical neighbour the product
    // would have offered the selector never reached the oracle here.
    let expansion_concepts: FxHashSet<String> =
        crate::types::extract_identifiers(&diff_text, TOKENIZATION.query_min_identifier_length)
            .into_iter()
            .collect();
    let discovery_ctx = crate::discovery::DiscoveryContext {
        root_dir: PathBuf::from("."),
        changed_files: changed_file_paths.clone(),
        all_candidates: all_file_paths.clone(),
        diff_text: diff_text.clone(),
        expansion_concepts,
        file_cache: file_cache.clone(),
        token_corpus: std::sync::OnceLock::new(),
    };
    let (discovered, _attribution) =
        crate::pipeline::create_discovery(&config).discover_attributed(&discovery_ctx);
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

    let effective_budget = budget_tokens.unwrap_or(BUDGET.unlimited);

    let discovered_arc: FxHashSet<Arc<str>> = discovered_paths
        .iter()
        .map(|s| Arc::from(s.as_str()))
        .collect();

    // The corpus harness has no timeout contract; before #210 it inherited
    // whatever ceiling the last in-process run left behind.
    let run = crate::resource::RunContext::unbounded();
    let crate::pipeline::ScoredFragments {
        all_fragments,
        core_ids,
        core_excerpts,
        scoring_result,
        needs,
        ..
    } = crate::pipeline::score_from_fragments(
        all_fragments,
        &hunks,
        &diff_text,
        &config,
        None,
        &discovered_arc,
        &run,
    );

    // The same envelope charge the product pipeline applies (#241): a harness
    // that spends the budget differently scores a system nobody runs (#149).
    let mut listed: Vec<String> = changed_paths.iter().cloned().collect();
    listed.sort();
    let selection_budget =
        effective_budget.saturating_sub(crate::pipeline::envelope_token_cost(None, &listed));

    let dummy_root = Path::new(".");
    let mut changed_files: Vec<PathBuf> = changed_paths.iter().map(PathBuf::from).collect();
    changed_files.sort();
    let change_classes = crate::pipeline::classify_changes(
        dummy_root,
        &changed_files,
        &hunks,
        &diff_text,
        &all_fragments,
    );

    let crate::pipeline::PostpassOutcome {
        mut selected,
        stand_in_ids,
        ..
    } = crate::pipeline::select_and_postpass(
        &scoring_result,
        &all_fragments,
        &core_ids,
        &needs,
        &core_excerpts,
        config.objective,
        selection_budget,
        tau,
        &crate::pipeline::evidence_priority_of(&changed_files, &change_classes),
    );

    let used: u32 = selected.iter().map(|f| f.token_count).sum();
    let remaining = selection_budget.saturating_sub(used);
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

    let mut changed_list: Vec<String> = changed_paths.iter().cloned().collect();
    changed_list.sort();
    let change = crate::render::ChangeSummary {
        lockfile_changes: Vec::new(),
        ignored_changes: Vec::new(),
        policy_excluded_count: 0,
        commit_message: None,
        commit_messages: Vec::new(),
        commit_count: 0,
        changes: change_classes,
        fragmentless: Default::default(),
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
            .args([
                "diff",
                "--no-index",
                "--unified=0",
                "-M",
                "old.py",
                "new.py",
            ])
            .current_dir(tmp.path())
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
