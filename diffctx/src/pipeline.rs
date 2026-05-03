use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::candidate_files;
use crate::config::budget::BUDGET;
use crate::config::graph_filtering::GRAPH_FILTERING;
use crate::config::limits::LIMITS;
use crate::config::tokenization::TOKENIZATION;
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
use crate::scoring::{BM25Scoring, EgoGraphScoring, PPRScoring, ScoringResult, ScoringStrategy};
use crate::signatures::generate_signature_variants;
use crate::tokenizer::count_tokens;
use crate::types::{Fragment, FragmentId};
use crate::utility::InformationNeed;

/// Per-instance heavy-phase outputs cached for reuse across many
/// (`tau`, `core_budget_fraction`) selection cells. The selection /
/// post-pass / render pipeline then runs against this state cheaply.
///
/// All fields are owned, no shared external lifetimes; safe to move
/// into a `pyclass` and hand back to Python.
pub struct ScoredState {
    pub root_dir: PathBuf,
    pub config: PipelineConfig,
    pub all_fragments: Vec<Fragment>,
    pub core_ids: FxHashSet<FragmentId>,
    pub scoring_result: ScoringResult,
    pub needs: Vec<InformationNeed>,
    pub changed_files: Vec<PathBuf>,
    pub preferred_revs: Vec<String>,
    pub heavy_latency_ms: HeavyLatencyMs,
}

#[derive(Default, Clone, Copy)]
pub struct HeavyLatencyMs {
    pub parse_changed: f64,
    pub universe_walk: f64,
    pub discovery: f64,
    pub parse_discovered: f64,
    pub tokenization: f64,
    pub scoring: f64,
}

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
    if full {
        return build_diff_context_full(root_dir, diff_range, no_content, timeout);
    }
    let state = compute_scored_state(root_dir, diff_range, alpha, scoring_mode, timeout)?;
    if state.all_fragments.is_empty() {
        return Ok(empty_output(&state.root_dir));
    }
    Ok(select_with_params(&state, budget_tokens, tau, no_content))
}

/// Heavy phase: clone/parse/fragment/discover/tokenize/score. Independent
/// of `tau`/`core_budget_fraction`. Designed to be computed ONCE per
/// instance and reused across an arbitrary number of selection cells.
pub fn compute_scored_state(
    root_dir: &Path,
    diff_range: Option<&str>,
    alpha: f64,
    scoring_mode: ScoringMode,
    timeout: u64,
) -> Result<ScoredState> {
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
        return Ok(empty_scored_state(root_dir));
    }

    let diff_text = git::get_diff_text(&root_dir, diff_range)?;

    let mut changed_files = git::get_changed_files(&root_dir, diff_range)?;
    changed_files.extend(untracked_files);
    if changed_files.is_empty() {
        return Ok(empty_scored_state(root_dir));
    }

    let deleted_files = git::get_deleted_files(&root_dir, diff_range)?;
    let (renamed_old, pure_rename_new) = git::get_renamed_paths(
        &root_dir,
        diff_range,
        GRAPH_FILTERING.git_rename_similarity_threshold,
    )?;
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
    let all_candidate_files = candidate_files::collect_candidate_files(&root_dir, &included_set);

    let t_universe = Instant::now();

    let file_cache = build_file_cache(&all_candidate_files);
    let mode = scoring_mode;
    let mut config = PipelineConfig::from_mode(mode, all_candidate_files.len());
    if let Ok(s) = std::env::var("DIFFCTX_OBJECTIVE") {
        config.objective = crate::mode::ObjectiveMode::from_str(&s);
    }

    let mut expansion_concepts: FxHashSet<String> =
        crate::types::extract_identifiers(&diff_text, TOKENIZATION.query_min_identifier_length)
            .into_iter()
            .collect();

    if let Some(ref h) = head_rev {
        if std::env::var("DIFFCTX_NO_COMMIT_SIGNAL").as_deref() != Ok("1") {
            if let Ok(commit_msg) = git::get_commit_message(&root_dir, h) {
                for ident in crate::types::extract_identifiers(
                    &commit_msg,
                    TOKENIZATION.query_min_identifier_length,
                ) {
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
        .map(|p| candidate_files::normalize_path(&p, &root_dir))
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

    let seed_weights = compute_seed_weights(&hunks, &core_ids, &all_fragments);

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

    let t_done = Instant::now();
    batch_reader.close();

    let heavy_latency_ms = HeavyLatencyMs {
        parse_changed: t_parse_changed.duration_since(t0).as_secs_f64() * 1000.0,
        universe_walk: t_universe.duration_since(t_parse_changed).as_secs_f64() * 1000.0,
        discovery: t_discovery.duration_since(t_universe).as_secs_f64() * 1000.0,
        parse_discovered: t_parse_discovered.duration_since(t_discovery).as_secs_f64() * 1000.0,
        tokenization: t_tokenization
            .duration_since(t_parse_discovered)
            .as_secs_f64()
            * 1000.0,
        scoring: t_done.duration_since(t_tokenization).as_secs_f64() * 1000.0,
    };

    tracing::debug!(
        "diffctx heavy: parse_changed {:.3}s, universe {:.3}s, discovery {:.3}s, parse_discovered {:.3}s, tokenization {:.3}s, scoring {:.3}s",
        heavy_latency_ms.parse_changed / 1000.0,
        heavy_latency_ms.universe_walk / 1000.0,
        heavy_latency_ms.discovery / 1000.0,
        heavy_latency_ms.parse_discovered / 1000.0,
        heavy_latency_ms.tokenization / 1000.0,
        heavy_latency_ms.scoring / 1000.0,
    );

    Ok(ScoredState {
        root_dir,
        config,
        all_fragments,
        core_ids,
        scoring_result,
        needs,
        changed_files,
        preferred_revs,
        heavy_latency_ms,
    })
}

/// Light phase: selection + 3 post-passes + render. Cheap. Re-runnable
/// against the same `ScoredState` with different (`tau`, `core_budget_fraction`)
/// to sweep a calibration grid without re-doing the heavy phase.
///
/// `core_budget_fraction` is read at the start via `selection().core_budget_fraction`
/// — set the env var `DIFFCTX_OP_SELECTION_CORE_BUDGET_FRACTION` before
/// calling to override per-cell.
pub fn select_with_params(
    state: &ScoredState,
    budget_tokens: Option<u32>,
    tau: f64,
    no_content: bool,
) -> DiffContextOutput {
    let t_start = Instant::now();
    let effective_budget = budget_tokens.unwrap_or_else(|| {
        let core_tokens: u32 = state
            .all_fragments
            .iter()
            .filter(|f| state.core_ids.contains(&f.id))
            .map(|f| f.token_count)
            .sum();
        let auto = (core_tokens as f64 * BUDGET.auto_multiplier) as u32;
        auto.clamp(BUDGET.auto_min, BUDGET.auto_max)
    });

    let selection_result = match state.config.objective {
        crate::mode::ObjectiveMode::BoltzmannModular => {
            let beta = crate::utility::calibrate_beta(
                &state.scoring_result.filtered_fragments,
                &state.core_ids,
                &state.scoring_result.rel_scores,
                effective_budget,
                crate::config::selection::boltzmann().calibration_tolerance,
            );
            tracing::debug!("diffctx: boltzmann beta calibrated to {:.6e}", beta);
            crate::utility::boltzmann_select(
                &state.scoring_result.filtered_fragments,
                &state.core_ids,
                &state.scoring_result.rel_scores,
                effective_budget,
                beta,
            )
        }
        crate::mode::ObjectiveMode::Submodular => {
            let file_importance =
                crate::utility::compute_file_importance(&state.scoring_result.filtered_fragments);
            crate::select::lazy_greedy_select(
                state.scoring_result.filtered_fragments.clone(),
                &state.core_ids,
                &state.scoring_result.rel_scores,
                &state.needs,
                effective_budget,
                tau,
                Some(&file_importance),
            )
        }
    };

    let selection_iters = selection_result.greedy_iters;
    let mut selected = selection_result.selected;

    postpass::coherence_post_pass(
        &mut selected,
        &state.scoring_result.filtered_fragments,
        &state.scoring_result.graph,
        effective_budget,
    );

    postpass::rescue_nontrivial_context(
        &mut selected,
        &state.all_fragments,
        &state.scoring_result.rel_scores,
        &state.core_ids,
        effective_budget,
    );

    let used: u32 = selected.iter().map(|f| f.token_count).sum();
    let remaining = effective_budget.saturating_sub(used);
    let mut batch_reader = match CatFileBatch::new(&state.root_dir) {
        Ok(r) => Some(r),
        Err(_) => None,
    };
    postpass::ensure_changed_files_represented(
        &mut selected,
        &state.all_fragments,
        &state.changed_files,
        remaining,
        &state.root_dir,
        &state.preferred_revs,
        batch_reader.as_mut(),
    );
    if let Some(mut r) = batch_reader {
        r.close();
    }

    let select_ms = t_start.elapsed().as_secs_f64() * 1000.0;
    let total_ms = state.heavy_latency_ms.parse_changed
        + state.heavy_latency_ms.universe_walk
        + state.heavy_latency_ms.discovery
        + state.heavy_latency_ms.parse_discovered
        + state.heavy_latency_ms.tokenization
        + state.heavy_latency_ms.scoring
        + select_ms;

    let cap_stats = state.scoring_result.graph.cap_stats;
    let mut output = render::build_diff_context_output(&state.root_dir, &selected, no_content);
    output.latency = Some(render::LatencyBreakdown {
        parse_changed_ms: state.heavy_latency_ms.parse_changed,
        universe_walk_ms: state.heavy_latency_ms.universe_walk,
        discovery_ms: state.heavy_latency_ms.discovery,
        parse_discovered_ms: state.heavy_latency_ms.parse_discovered,
        tokenization_ms: state.heavy_latency_ms.tokenization,
        scoring_selection_ms: state.heavy_latency_ms.scoring + select_ms,
        total_ms,
        scoring_ms: state.heavy_latency_ms.scoring,
        selection_ms: select_ms,
        candidate_count: state.scoring_result.filtered_fragments.len(),
        edge_count: state.scoring_result.graph.edge_count(),
        greedy_iters: selection_iters,
        edges_before_cap: cap_stats.edges_before_cap,
        edges_dropped_by_cap: cap_stats.edges_dropped_by_cap,
        nodes_capped: cap_stats.nodes_capped,
        max_out_edges_per_node: cap_stats.max_out_edges_per_node,
        ppr_truncated: state.scoring_result.ppr_truncated,
        ppr_forward_pushes: state.scoring_result.ppr_forward_pushes,
        ppr_backward_pushes: state.scoring_result.ppr_backward_pushes,
    });
    output
}

/// Special path for `--full` mode: bypass scoring entirely, return all
/// changed-file fragments. Doesn't share the `ScoredState` plumbing.
fn build_diff_context_full(
    root_dir: &Path,
    diff_range: Option<&str>,
    no_content: bool,
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
    let hunks = git::parse_diff(&root_dir, diff_range)?;
    if hunks.is_empty() {
        return Ok(empty_output(&root_dir));
    }
    let mut changed_files = git::get_changed_files(&root_dir, diff_range)?;
    if changed_files.is_empty() {
        return Ok(empty_output(&root_dir));
    }
    let (base_rev, head_rev) = diff_range
        .map(git::split_diff_range)
        .unwrap_or((None, None));
    let preferred_revs = build_preferred_revs(base_rev.as_deref(), head_rev.as_deref());
    let mut seen_frag_ids: FxHashSet<FragmentId> = FxHashSet::default();
    let mut batch_reader = CatFileBatch::new(&root_dir)?;
    let mut all_fragments = process_files_for_fragments(
        &changed_files,
        &root_dir,
        &preferred_revs,
        &mut seen_frag_ids,
        Some(&mut batch_reader),
    );
    assign_token_counts(&mut all_fragments);
    let mut sig_frags = generate_signature_variants(&all_fragments);
    assign_token_counts(&mut sig_frags);
    all_fragments.extend(sig_frags);
    changed_files.sort();
    let selected = select_full_mode(&all_fragments, &changed_files);
    batch_reader.close();
    Ok(render::build_diff_context_output(
        &root_dir, &selected, no_content,
    ))
}

fn empty_scored_state(root_dir: PathBuf) -> ScoredState {
    let config = PipelineConfig::from_mode(ScoringMode::Hybrid, 0);
    ScoredState {
        root_dir,
        config,
        all_fragments: Vec::new(),
        core_ids: FxHashSet::default(),
        scoring_result: ScoringResult {
            rel_scores: FxHashMap::default(),
            filtered_fragments: Vec::new(),
            graph: crate::graph::Graph::new(),
            ppr_truncated: false,
            ppr_forward_pushes: 0,
            ppr_backward_pushes: 0,
        },
        needs: Vec::new(),
        changed_files: Vec::new(),
        preferred_revs: Vec::new(),
        heavy_latency_ms: HeavyLatencyMs::default(),
    }
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

fn build_file_cache(candidate_files: &[PathBuf]) -> FxHashMap<PathBuf, String> {
    let mut entries: Vec<(PathBuf, String)> = candidate_files
        .par_iter()
        .filter_map(|f| {
            let meta = f.metadata().ok()?;
            if meta.len() as usize > LIMITS.max_file_size {
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
        if cache_bytes > GRAPH_FILTERING.max_cache_bytes {
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
            frag.token_count = count_tokens(&frag.content) + LIMITS.overhead_per_fragment;
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
