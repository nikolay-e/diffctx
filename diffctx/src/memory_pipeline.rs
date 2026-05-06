use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use similar::{ChangeTag, TextDiff};

use crate::config::budget::BUDGET;
use crate::config::limits::LIMITS;
use crate::config::tokenization::TOKENIZATION;
use crate::core::{compute_seed_weights, identify_core_fragments};
use crate::edges;
use crate::mode::{PipelineConfig, ScoringKind, ScoringMode};
use crate::parsers::fragment_file;
use crate::render::{DiffContextOutput, build_diff_context_output};
use crate::scoring::{BM25Scoring, EgoGraphScoring, PPRScoring, ScoringStrategy};
use crate::signatures::generate_signature_variants;
use crate::tokenizer::count_tokens;
use crate::types::{DiffHunk, Fragment, FragmentId};

pub struct MemoryRepo {
    pub name: String,
    pub initial_files: FxHashMap<String, String>,
    pub changed_files: FxHashMap<String, String>,
}

pub fn build_diff_context_in_memory(
    repo: &MemoryRepo,
    budget_tokens: Option<u32>,
    _alpha: f64,
    tau: f64,
    no_content: bool,
    scoring_mode: ScoringMode,
) -> DiffContextOutput {
    let hunks = compute_memory_hunks(&repo.initial_files, &repo.changed_files);
    if hunks.is_empty() {
        return empty_output(&repo.name);
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

    all_fragments.par_iter_mut().for_each(|f| {
        f.token_count = count_tokens(&f.content) + LIMITS.overhead_per_fragment;
    });

    let core_ids = identify_core_fragments(&hunks, &all_fragments);

    let mut sig_frags = generate_signature_variants(&all_fragments);
    sig_frags.par_iter_mut().for_each(|f| {
        f.token_count = count_tokens(&f.content) + LIMITS.overhead_per_fragment;
    });
    all_fragments.extend(sig_frags);

    let effective_budget = budget_tokens.unwrap_or(BUDGET.unlimited);
    let config = PipelineConfig::from_mode(scoring_mode);
    let seed_weights = compute_seed_weights(&hunks, &core_ids, &all_fragments);

    let discovered_arc: FxHashSet<Arc<str>> = discovered_paths
        .iter()
        .map(|s| Arc::from(s.as_str()))
        .collect();

    let strategy: Box<dyn ScoringStrategy> = match config.scoring {
        ScoringKind::Ego => Box::new(EgoGraphScoring::new(config.ego_depth)),
        ScoringKind::Ppr => Box::new(PPRScoring::new(
            config.ppr_alpha,
            config.low_relevance_filter,
        )),
        ScoringKind::Bm25 => Box::new(BM25Scoring),
    };

    let scoring_result = strategy.score_and_filter(
        &all_fragments,
        &core_ids,
        &hunks,
        None,
        Some(&seed_weights),
        Some(&discovered_arc),
    );

    let needs = crate::utility::needs::needs_from_diff(&all_fragments, &core_ids, &diff_text);

    let selection = crate::select::lazy_greedy_select(
        scoring_result.filtered_fragments.clone(),
        &core_ids,
        &scoring_result.rel_scores,
        &needs,
        effective_budget,
        tau,
        None,
    );

    let mut selected = selection.selected;

    crate::postpass::coherence_post_pass(
        &mut selected,
        &scoring_result.filtered_fragments,
        &scoring_result.graph,
        effective_budget,
    );

    crate::postpass::rescue_nontrivial_context(
        &mut selected,
        &all_fragments,
        &scoring_result.rel_scores,
        &core_ids,
        effective_budget,
    );

    let used: u32 = selected.iter().map(|f| f.token_count).sum();
    let remaining = effective_budget.saturating_sub(used);
    let changed_files: Vec<PathBuf> = changed_paths.iter().map(PathBuf::from).collect();
    crate::postpass::ensure_changed_files_represented(
        &mut selected,
        &all_fragments,
        &changed_files,
        remaining,
        Path::new("."),
        &[],
        None,
    );

    let dummy_root = Path::new(".");
    build_diff_context_output(dummy_root, &selected, no_content)
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

    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Equal => {
                if let Some(start) = hunk_new_start.take() {
                    hunks.push(DiffHunk {
                        path: Arc::clone(path),
                        new_start: start,
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
            new_start: start,
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

fn empty_output(name: &str) -> DiffContextOutput {
    DiffContextOutput {
        name: name.to_string(),
        output_type: "diff_context".to_string(),
        fragment_count: 0,
        fragments: Vec::new(),
        latency: None,
    }
}
