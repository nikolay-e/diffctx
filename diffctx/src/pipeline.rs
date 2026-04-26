use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::core::{compute_seed_weights, identify_core_fragments};
use crate::discovery::{
    BM25Discovery, DefaultDiscovery, DiscoveryContext, DiscoveryStrategy, EnsembleDiscovery,
    TestFileDiscovery,
};
use crate::fragmentation::process_files_for_fragments;
use crate::git::{self, CatFileBatch};
use crate::mode::{DiscoveryKind, PipelineConfig, ScoringKind, ScoringMode};
use crate::postpass;
use crate::render::{self, DiffContextOutput};
use crate::scoring::{BM25Scoring, EgoGraphScoring, PPRScoring, ScoringStrategy};
use crate::signatures::generate_signature_variants;
use crate::tokenizer::count_tokens;
use crate::types::{Fragment, FragmentId};
use crate::universe;

const UNLIMITED_BUDGET: u32 = 10_000_000;
const AUTO_BUDGET_MULTIPLIER: f64 = 5.0;
const AUTO_BUDGET_MIN: u32 = 8_000;
const AUTO_BUDGET_MAX: u32 = 124_000;
const OVERHEAD_PER_FRAGMENT: u32 = 18;

pub fn build_diff_context(
    root_dir: &Path,
    diff_range: Option<&str>,
    budget_tokens: Option<u32>,
    alpha: f64,
    tau: f64,
    no_content: bool,
    full: bool,
    scoring_mode: ScoringMode,
    timeout: u64,
) -> Result<DiffContextOutput> {
    git::set_git_timeout(timeout);
    let root_dir = root_dir.canonicalize().unwrap_or_else(|e| {
        tracing::debug!("canonicalize failed for '{}': {}", root_dir.display(), e);
        root_dir.to_path_buf()
    });

    if !git::is_git_repo(&root_dir) {
        anyhow::bail!("'{}' is not a git repository", root_dir.display());
    }
    if alpha <= 0.0 || alpha >= 1.0 {
        anyhow::bail!("alpha must be in (0, 1), got {}", alpha);
    }
    if tau < 0.0 {
        anyhow::bail!("tau must be >= 0, got {}", tau);
    }

    let mut hunks = git::parse_diff(&root_dir, diff_range)?;

    let is_working_tree_diff = diff_range.is_none();
    let mut untracked_files: Vec<PathBuf> = Vec::new();
    if is_working_tree_diff {
        if let Ok(files) = git::get_untracked_files(&root_dir) {
            for f in &files {
                if let Ok(content) = std::fs::read_to_string(f) {
                    let line_count = content.lines().count() as u32;
                    if line_count > 0 {
                        let path_str: Arc<str> = Arc::from(f.to_string_lossy().as_ref());
                        hunks.push(crate::types::DiffHunk {
                            path: path_str,
                            new_start: 1,
                            new_len: line_count,
                            old_start: 0,
                            old_len: 0,
                        });
                    }
                }
            }
            untracked_files = files;
        }
    }

    if hunks.is_empty() {
        return Ok(empty_output(&root_dir));
    }

    let diff_text = git::get_diff_text(&root_dir, diff_range)?;

    let mut changed_files = git::get_changed_files(&root_dir, diff_range)?;
    changed_files.extend(untracked_files);
    if changed_files.is_empty() {
        return Ok(empty_output(&root_dir));
    }

    let deleted_files = git::get_deleted_files(&root_dir, diff_range)?;
    let (renamed_old, pure_rename_new) = git::get_renamed_paths(&root_dir, diff_range, 100)?;
    let excluded: FxHashSet<PathBuf> = deleted_files
        .into_iter()
        .chain(renamed_old)
        .chain(pure_rename_new)
        .collect();
    let changed_files: Vec<PathBuf> = changed_files
        .into_iter()
        .filter(|f| {
            let resolved = f.canonicalize().unwrap_or_else(|_| f.clone());
            !excluded.contains(&resolved)
        })
        .collect();

    let (base_rev, head_rev) = diff_range
        .map(git::split_diff_range)
        .unwrap_or((None, None));
    let preferred_revs = build_preferred_revs(base_rev.as_deref(), head_rev.as_deref());

    let t0 = Instant::now();

    let mut seen_frag_ids: FxHashSet<FragmentId> = FxHashSet::default();
    let mut batch_reader = CatFileBatch::new(&root_dir)?;
    let mut all_fragments = process_files_for_fragments(
        &changed_files,
        &root_dir,
        &preferred_revs,
        &mut seen_frag_ids,
        Some(&mut batch_reader),
    );

    let t_parse_changed = Instant::now();

    let included_set: FxHashSet<PathBuf> = changed_files.iter().cloned().collect();
    let all_candidate_files = universe::collect_candidate_files(&root_dir, &included_set);

    let t_universe = Instant::now();

    let file_cache = build_file_cache(&all_candidate_files);
    let mode = scoring_mode;
    let mut config = PipelineConfig::from_mode(mode, all_candidate_files.len());
    if let Ok(s) = std::env::var("DIFFCTX_OBJECTIVE") {
        config.objective = crate::mode::ObjectiveMode::from_str(&s);
    }

    let mut expansion_concepts: FxHashSet<String> =
        crate::types::extract_identifiers(&diff_text, 3)
            .into_iter()
            .collect();

    if let Some(ref h) = head_rev {
        if std::env::var("DIFFCTX_NO_COMMIT_SIGNAL").as_deref() != Ok("1") {
            if let Ok(commit_msg) = git::get_commit_message(&root_dir, h) {
                for ident in crate::types::extract_identifiers(&commit_msg, 3) {
                    expansion_concepts.insert(ident);
                }
            }
        }
    }

    let discovery_ctx = DiscoveryContext {
        root_dir: root_dir.clone(),
        changed_files: changed_files.clone(),
        all_candidates: all_candidate_files,
        diff_text: diff_text.clone(),
        expansion_concepts,
        file_cache,
    };

    let discovered_files = create_discovery(&config).discover(&discovery_ctx);
    let discovered_files: Vec<PathBuf> = discovered_files
        .into_iter()
        .map(|p| universe::normalize_path(&p, &root_dir))
        .collect();

    drop(discovery_ctx);

    let t_discovery = Instant::now();

    all_fragments.extend(process_files_for_fragments(
        &discovered_files,
        &root_dir,
        &preferred_revs,
        &mut seen_frag_ids,
        Some(&mut batch_reader),
    ));

    let t_parse_discovered = Instant::now();

    assign_token_counts(&mut all_fragments);

    let core_ids = identify_core_fragments(&hunks, &all_fragments);

    let signature_frags = generate_signature_variants(&all_fragments);
    let mut sig_frags = signature_frags;
    assign_token_counts(&mut sig_frags);
    all_fragments.extend(sig_frags);

    let t_tokenization = Instant::now();

    let selected = if full {
        select_full_mode(&all_fragments, &changed_files)
    } else {
        let seed_weights = compute_seed_weights(&hunks, &core_ids, &all_fragments);
        let effective_budget = budget_tokens.unwrap_or_else(|| {
            let core_tokens: u32 = all_fragments
                .iter()
                .filter(|f| core_ids.contains(&f.id))
                .map(|f| f.token_count)
                .sum();
            let auto = (core_tokens as f64 * AUTO_BUDGET_MULTIPLIER) as u32;
            auto.clamp(AUTO_BUDGET_MIN, AUTO_BUDGET_MAX)
        });

        let discovered_path_set: FxHashSet<Arc<str>> = discovered_files
            .iter()
            .map(|p| Arc::from(p.to_string_lossy().as_ref()))
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
            Some(root_dir.as_path()),
            Some(&seed_weights),
            Some(&discovered_path_set),
        );

        let needs = crate::utility::needs::needs_from_diff(&all_fragments, &core_ids, &diff_text);

        let selection_result = match config.objective {
            crate::mode::ObjectiveMode::BoltzmannModular => {
                let beta = crate::utility::calibrate_beta(
                    &scoring_result.filtered_fragments,
                    &core_ids,
                    &scoring_result.rel_scores,
                    effective_budget,
                    0.05,
                );
                tracing::debug!("diffctx: boltzmann beta calibrated to {:.6e}", beta);
                crate::utility::boltzmann_select(
                    &scoring_result.filtered_fragments,
                    &core_ids,
                    &scoring_result.rel_scores,
                    effective_budget,
                    beta,
                )
            }
            crate::mode::ObjectiveMode::Submodular => {
                let file_importance =
                    crate::utility::compute_file_importance(&scoring_result.filtered_fragments);
                crate::select::lazy_greedy_select(
                    scoring_result.filtered_fragments.clone(),
                    &core_ids,
                    &scoring_result.rel_scores,
                    &needs,
                    effective_budget,
                    tau,
                    Some(&file_importance),
                )
            }
        };

        let mut selected = selection_result.selected;

        postpass::coherence_post_pass(
            &mut selected,
            &scoring_result.filtered_fragments,
            &scoring_result.graph,
            effective_budget,
        );

        postpass::rescue_nontrivial_context(
            &mut selected,
            &all_fragments,
            &scoring_result.rel_scores,
            &core_ids,
            effective_budget,
        );

        let used: u32 = selected.iter().map(|f| f.token_count).sum();
        let remaining = effective_budget.saturating_sub(used);
        postpass::ensure_changed_files_represented(
            &mut selected,
            &all_fragments,
            &changed_files,
            remaining,
            &root_dir,
            &preferred_revs,
            Some(&mut batch_reader),
        );

        selected
    };

    batch_reader.close();

    let t_done = Instant::now();

    tracing::debug!(
        "diffctx: timing — parse_changed {:.3}s, universe {:.3}s, discovery {:.3}s, parse_discovered {:.3}s, tokenization {:.3}s, scoring {:.3}s",
        t_parse_changed.duration_since(t0).as_secs_f64(),
        t_universe.duration_since(t_parse_changed).as_secs_f64(),
        t_discovery.duration_since(t_universe).as_secs_f64(),
        t_parse_discovered.duration_since(t_discovery).as_secs_f64(),
        t_tokenization
            .duration_since(t_parse_discovered)
            .as_secs_f64(),
        t_done.duration_since(t_tokenization).as_secs_f64(),
    );

    let mut output = render::build_diff_context_output(&root_dir, &selected, no_content);
    output.latency = Some(render::LatencyBreakdown {
        parse_changed_ms: t_parse_changed.duration_since(t0).as_secs_f64() * 1000.0,
        universe_walk_ms: t_universe.duration_since(t_parse_changed).as_secs_f64() * 1000.0,
        discovery_ms: t_discovery.duration_since(t_universe).as_secs_f64() * 1000.0,
        parse_discovered_ms: t_parse_discovered.duration_since(t_discovery).as_secs_f64() * 1000.0,
        tokenization_ms: t_tokenization
            .duration_since(t_parse_discovered)
            .as_secs_f64()
            * 1000.0,
        scoring_selection_ms: t_done.duration_since(t_tokenization).as_secs_f64() * 1000.0,
        total_ms: t_done.duration_since(t0).as_secs_f64() * 1000.0,
    });
    Ok(output)
}

fn empty_output(root_dir: &Path) -> DiffContextOutput {
    let resolved = root_dir
        .canonicalize()
        .unwrap_or_else(|_| root_dir.to_path_buf());
    let name = resolved
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| resolved.to_string_lossy().to_string());
    DiffContextOutput {
        name,
        output_type: "diff_context".to_string(),
        fragment_count: 0,
        fragments: Vec::new(),
        latency: None,
    }
}

fn build_preferred_revs(base_rev: Option<&str>, head_rev: Option<&str>) -> Vec<String> {
    let mut revs = Vec::new();
    if let Some(h) = head_rev {
        revs.push(h.to_string());
    }
    if let Some(b) = base_rev {
        if Some(b) != head_rev {
            revs.push(b.to_string());
        }
    }
    revs
}

fn create_discovery(config: &PipelineConfig) -> Box<dyn DiscoveryStrategy> {
    match config.discovery {
        DiscoveryKind::Ensemble => Box::new(EnsembleDiscovery::new(vec![
            Box::new(DefaultDiscovery),
            Box::new(TestFileDiscovery),
            Box::new(BM25Discovery::new(config.bm25_top_k)),
        ])),
        DiscoveryKind::Default => Box::new(DefaultDiscovery),
    }
}

const MAX_CACHE_BYTES: usize = 200 * 1024 * 1024;

fn build_file_cache(candidate_files: &[PathBuf]) -> FxHashMap<PathBuf, String> {
    let mut entries: Vec<(PathBuf, String)> = candidate_files
        .par_iter()
        .filter_map(|f| {
            let meta = f.metadata().ok()?;
            if meta.len() > 100_000 {
                return None;
            }
            let content = std::fs::read_to_string(f).ok()?;
            Some((f.clone(), content))
        })
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut cache: FxHashMap<PathBuf, String> = FxHashMap::default();
    let mut cache_bytes = 0usize;
    for (path, content) in entries {
        if cache_bytes > MAX_CACHE_BYTES {
            break;
        }
        cache_bytes += content.len();
        cache.insert(path, content);
    }
    cache
}

fn assign_token_counts(fragments: &mut [Fragment]) {
    fragments.par_iter_mut().for_each(|frag| {
        if frag.token_count == 0 {
            frag.token_count = count_tokens(&frag.content) + OVERHEAD_PER_FRAGMENT;
        }
    });
}

fn select_full_mode(all_fragments: &[Fragment], changed_files: &[PathBuf]) -> Vec<Fragment> {
    let changed_paths: FxHashSet<String> = changed_files
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();
    let mut selected: Vec<Fragment> = all_fragments
        .iter()
        .filter(|f| changed_paths.contains(f.path()))
        .cloned()
        .collect();
    selected.sort_by(|a, b| {
        a.path()
            .cmp(b.path())
            .then(a.start_line().cmp(&b.start_line()))
    });
    selected
}
