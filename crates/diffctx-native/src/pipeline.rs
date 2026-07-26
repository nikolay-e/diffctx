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
    /// Narrow stand-ins for the core fragments that have no signature variant,
    /// keyed by the core they replace. Deliberately outside `all_fragments`:
    /// they are budget fallbacks, not graph nodes or context candidates.
    pub core_excerpts: FxHashMap<FragmentId, Fragment>,
    pub scoring_result: ScoringResult,
    pub needs: Vec<InformationNeed>,
    pub changed_files: Vec<PathBuf>,
    pub deleted_files: Vec<String>,
    pub renamed_files: Vec<(String, String)>,
    /// Lock files touched by the range, paths only: the dependency bump is
    /// signal, the checksum churn is not (#112).
    pub lockfile_changes: Vec<String>,
    pub preferred_revs: Vec<String>,
    pub commit_message: Option<String>,
    pub heavy_latency_ms: HeavyLatencyMs,
}

#[derive(Default, Clone, Copy)]
pub struct HeavyLatencyMs {
    pub parse_changed: f64,
    pub universe_walk: f64,
    pub discovery: f64,
    pub parse_discovered: f64,
    pub tokenization: f64,
    pub graph_build: f64,
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
        return Ok(empty_output_from_state(&state));
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
    let root_dir = resolve_repo_root(root_dir)?;
    if alpha <= 0.0 || alpha >= 1.0 {
        anyhow::bail!("alpha must be in (0, 1), got {}", alpha);
    }

    let mut hunks = git::parse_diff(&root_dir, diff_range)?;

    // Untracked files only matter when the diff includes the live working
    // tree. `None` and the literal "HEAD" both mean that (the CLI resolves
    // bare `--diff` to the string "HEAD" before reaching here) - a historical
    // range like `HEAD~5..HEAD~3` does not include working-tree state.
    let is_working_tree_diff = matches!(diff_range, None | Some("HEAD"));
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

    hunks.retain(|h| !is_secret_path(Path::new(&*h.path)));

    if hunks.is_empty() {
        return Ok(empty_scored_state_with_changes(root_dir, diff_range));
    }

    let ignored_rel_paths = resolve_ignored_paths(&root_dir, &hunks);
    hunks.retain(|h| !is_ignored_path(&root_dir, Path::new(&*h.path), &ignored_rel_paths));

    let mut lockfile_display: Vec<String> = hunks
        .iter()
        .filter(|h| is_lockfile_path(Path::new(&*h.path)))
        .filter_map(|h| rel_path_string(&root_dir, Path::new(&*h.path)))
        .collect();
    lockfile_display.sort();
    lockfile_display.dedup();
    hunks.retain(|h| !is_lockfile_path(Path::new(&*h.path)));

    if hunks.is_empty() {
        let mut state = empty_scored_state_with_changes(root_dir, diff_range);
        state.lockfile_changes = lockfile_display;
        return Ok(state);
    }

    let diff_text = git::get_diff_text(&root_dir, diff_range)?;

    let mut changed_files = git::get_changed_files(&root_dir, diff_range)?;
    changed_files.extend(untracked_files);
    if changed_files.is_empty() {
        return Ok(empty_scored_state_with_changes(root_dir, diff_range));
    }

    let deleted_files = git::get_deleted_files(&root_dir, diff_range)?;
    // Pure-rename old paths are gone from disk and cannot be fragmented; pure-rename new
    // paths exist on HEAD and must remain candidates so seeds and discovery can find them.
    let (renamed_old, _pure_rename_new) = git::get_renamed_paths(
        &root_dir,
        diff_range,
        GRAPH_FILTERING.git_rename_similarity_threshold,
    )?;
    // Display lists for the output header: deletions and renames produce no
    // fragments, but silently omitting them misrepresents the diff (a
    // deletion-only commit used to render as a bare two-line skeleton).
    let mut deleted_display: Vec<String> = deleted_files
        .iter()
        .map(|p| {
            p.strip_prefix(&root_dir)
                .unwrap_or(p)
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    deleted_display.sort();
    let renamed_display = git::get_rename_pairs(&root_dir, diff_range).unwrap_or_default();
    let excluded: FxHashSet<PathBuf> = deleted_files.into_iter().chain(renamed_old).collect();
    let changed_files: Vec<PathBuf> = changed_files
        .into_iter()
        .filter(|f| {
            let resolved = f.canonicalize().unwrap_or_else(|_| f.clone());
            !excluded.contains(&resolved)
                && !is_secret_path(f)
                && !is_lockfile_path(f)
                && !is_ignored_path(&root_dir, f, &ignored_rel_paths)
        })
        .collect();

    let (base_rev, head_rev) = diff_range
        .map(git::split_diff_range)
        .unwrap_or((None, None));
    let preferred_revs = build_preferred_revs(base_rev.as_deref(), head_rev.as_deref());
    let commit_message = head_rev
        .as_deref()
        .and_then(|h| git::get_commit_message(&root_dir, h).ok())
        .and_then(|m| {
            m.lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .map(str::to_string)
        });

    let t0 = Instant::now();

    let mut seen_frag_ids: FxHashSet<FragmentId> = FxHashSet::default();
    let mut batch_reader = CatFileBatch::new(&root_dir)?;
    let mut all_fragments = process_files_for_fragments(
        &changed_files,
        &root_dir,
        &preferred_revs,
        &mut seen_frag_ids,
        Some(&mut batch_reader),
        true,
    );

    let t_parse_changed = Instant::now();

    let included_set: FxHashSet<PathBuf> = changed_files.iter().cloned().collect();
    let all_candidate_files = candidate_files::collect_candidate_files(&root_dir, &included_set);

    let t_universe = Instant::now();

    let file_cache = build_file_cache(&all_candidate_files);
    let mode = scoring_mode;
    let mut config = PipelineConfig::from_mode(mode);
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
        token_corpus: std::sync::OnceLock::new(),
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
        false,
    ));

    let t_parse_discovered = Instant::now();

    assign_token_counts(&mut all_fragments);

    let core_ids = identify_core_fragments(&hunks, &all_fragments);

    let mut core_excerpts =
        crate::excerpt::generate_core_excerpts(&all_fragments, &core_ids, &hunks);
    assign_excerpt_token_counts(&mut core_excerpts);

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

    let graph_build_ms = scoring_result.graph_build_ms;
    let heavy_latency_ms = HeavyLatencyMs {
        parse_changed: t_parse_changed.duration_since(t0).as_secs_f64() * 1000.0,
        universe_walk: t_universe.duration_since(t_parse_changed).as_secs_f64() * 1000.0,
        discovery: t_discovery.duration_since(t_universe).as_secs_f64() * 1000.0,
        parse_discovered: t_parse_discovered.duration_since(t_discovery).as_secs_f64() * 1000.0,
        tokenization: t_tokenization
            .duration_since(t_parse_discovered)
            .as_secs_f64()
            * 1000.0,
        graph_build: graph_build_ms,
        scoring: (t_done.duration_since(t_tokenization).as_secs_f64() * 1000.0 - graph_build_ms)
            .max(0.0),
    };

    tracing::debug!(
        "diffctx heavy: parse_changed {:.3}s, universe {:.3}s, discovery {:.3}s, parse_discovered {:.3}s, tokenization {:.3}s, graph_build {:.3}s, scoring {:.3}s",
        heavy_latency_ms.parse_changed / 1000.0,
        heavy_latency_ms.universe_walk / 1000.0,
        heavy_latency_ms.discovery / 1000.0,
        heavy_latency_ms.parse_discovered / 1000.0,
        heavy_latency_ms.tokenization / 1000.0,
        heavy_latency_ms.graph_build / 1000.0,
        heavy_latency_ms.scoring / 1000.0,
    );

    Ok(ScoredState {
        root_dir,
        config,
        all_fragments,
        core_ids,
        core_excerpts,
        scoring_result,
        needs,
        changed_files,
        deleted_files: deleted_display,
        renamed_files: renamed_display,
        lockfile_changes: lockfile_display,
        preferred_revs,
        commit_message,
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
            .map(|f| f.token_count.min(BUDGET.core_token_cap_per_fragment))
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
                Some(&state.core_excerpts),
            )
        }
    };

    let selection_iters = selection_result.greedy_iters;
    let stopping_certificate = selection_result.stopping_certificate;
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
        &state.core_ids,
        &state.core_excerpts,
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
        + state.heavy_latency_ms.graph_build
        + state.heavy_latency_ms.scoring
        + select_ms;

    let cap_stats = state.scoring_result.graph.cap_stats.clone();
    let change = render::ChangeSummary {
        commit_message: state.commit_message.clone(),
        changed_files: state
            .changed_files
            .iter()
            .map(|p| {
                p.strip_prefix(&state.root_dir)
                    .unwrap_or(p)
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect(),
        deleted_files: state.deleted_files.clone(),
        renamed_files: state.renamed_files.clone(),
        lockfile_changes: state.lockfile_changes.clone(),
    };
    // An excerpt stands in for a core fragment, so it carries the change and
    // has to render as `role: "changed"` — otherwise the substitution keeps the
    // content but still loses the signal it exists to preserve.
    let mut render_core_ids = state.core_ids.clone();
    render_core_ids.extend(
        selected
            .iter()
            .filter(|f| f.kind == crate::types::FragmentKind::Excerpt)
            .map(|f| f.id.clone()),
    );

    let mut output = render::build_diff_context_output(
        &state.root_dir,
        &selected,
        no_content,
        &render_core_ids,
        &state.scoring_result.rel_scores,
        change,
    );
    output.latency = Some(render::LatencyBreakdown {
        parse_changed_ms: state.heavy_latency_ms.parse_changed,
        universe_walk_ms: state.heavy_latency_ms.universe_walk,
        discovery_ms: state.heavy_latency_ms.discovery,
        parse_discovered_ms: state.heavy_latency_ms.parse_discovered,
        tokenization_ms: state.heavy_latency_ms.tokenization,
        graph_build_ms: state.heavy_latency_ms.graph_build,
        scoring_selection_ms: state.heavy_latency_ms.graph_build
            + state.heavy_latency_ms.scoring
            + select_ms,
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
        stopping_certificate,
        ppr_forward_pushes: state.scoring_result.ppr_forward_pushes,
        ppr_backward_pushes: state.scoring_result.ppr_backward_pushes,
        peak_rss_bytes: crate::peak_rss::peak_rss_bytes(),
        edge_emissions_by_category: cap_stats
            .emissions_by_category
            .iter()
            .map(|&(category, raw, deduped)| (category.as_str(), raw, deduped))
            .collect(),
    });
    output
}

/// Special path for `--full` mode: bypass scoring entirely, return all
/// changed-file fragments. Doesn't share the `ScoredState` plumbing.
/// Private-key and keystore files must never reach LLM-bound diff context, even
/// when they appear in the diff hunks — such material is never legitimate change
/// context. Mirrors the Python tree-mode default ignores (`ignore.py`
/// DEFAULT_IGNORE_PATTERNS). Matches by file name only, so public keys (`*.pub`)
/// stay visible. `.env` files are intentionally NOT excluded here: a changed
/// `.env` is legitimate change context (see the `*_env_file_change` cases).
fn is_secret_path(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    if matches!(name, "id_rsa" | "id_dsa" | "id_ecdsa" | "id_ed25519") {
        return true;
    }
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("pem" | "key" | "pfx" | "p12" | "keystore" | "jks")
    )
}

/// Lock files, mirroring the tree-mode `DEFAULT_IGNORE_PATTERNS` list in
/// `src/diffctx/ignore.py`. Tree mode drops them outright; diff mode cannot,
/// because a bumped dependency IS part of the change — but rendering the raw
/// hunks costs thousands of tokens of checksums for a fact that fits on one
/// line, so the paths are reported and the content is left out (#112).
fn is_lockfile_path(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    matches!(
        name,
        "Cargo.lock"
            | "package-lock.json"
            | "npm-shrinkwrap.json"
            | "yarn.lock"
            | "pnpm-lock.yaml"
            | "bun.lock"
            | "bun.lockb"
            | "deno.lock"
            | "Pipfile.lock"
            | "poetry.lock"
            | "uv.lock"
            | "pdm.lock"
            | "composer.lock"
            | "Gemfile.lock"
            | "flake.lock"
            | "go.sum"
            | "mix.lock"
            | "packages.lock.json"
            | "gradle.lockfile"
            | "Package.resolved"
            | "cabal.project.freeze"
    )
}

fn rel_path_string(root_dir: &Path, path: &Path) -> Option<String> {
    path.strip_prefix(root_dir)
        .ok()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
}

/// Resolves `.gitignore` / `.diffctx/ignore` exclusions (#85) for every path
/// touched by `hunks`, in one batched git call. diff mode previously only
/// ever excluded a hardcoded set of secret-like filenames (`is_secret_path`)
/// — a file a user explicitly excluded via `.diffctx/ignore` still had its
/// changed content surfaced in full.
fn resolve_ignored_paths(root_dir: &Path, hunks: &[crate::types::DiffHunk]) -> FxHashSet<String> {
    let rel_paths: Vec<String> = hunks
        .iter()
        .filter_map(|h| rel_path_string(root_dir, Path::new(&*h.path)))
        .collect();
    git::find_ignored_paths(root_dir, &rel_paths)
}

fn is_ignored_path(root_dir: &Path, path: &Path, ignored_rel_paths: &FxHashSet<String>) -> bool {
    rel_path_string(root_dir, path)
        .map(|rel| ignored_rel_paths.contains(&rel))
        .unwrap_or(false)
}

/// The unified diff of `diff_range` as git prints it, minus the file sections
/// diff mode never discloses: secret-like paths, ignored paths, and lock files
/// (#112). Additive output for `--with-raw-diff` (#150) — it feeds no
/// selection state, so selection is bit-identical with and without it.
pub fn raw_diff_text(root_dir: &Path, diff_range: Option<&str>, timeout: u64) -> Result<String> {
    git::set_git_timeout(timeout);
    let root_dir = resolve_repo_root(root_dir)?;
    let diff_text = git::get_diff_text(&root_dir, diff_range)?;
    Ok(keep_disclosable_sections(&root_dir, &diff_text))
}

fn keep_disclosable_sections(root_dir: &Path, diff_text: &str) -> String {
    let lines: Vec<&str> = diff_text.lines().collect();
    let sections = split_diff_sections(root_dir, &lines);
    let rel_paths: Vec<String> = sections
        .iter()
        .filter_map(|(path, _)| path.as_deref())
        .filter_map(|path| rel_path_string(root_dir, path))
        .collect();
    let ignored_rel_paths = git::find_ignored_paths(root_dir, &rel_paths);

    let mut kept: Vec<&str> = Vec::new();
    for (path, range) in sections {
        // A section whose path cannot be resolved inside the repository is
        // dropped: the bundle must never widen what diff mode is willing to
        // show, and an unattributable section cannot be policy-checked.
        let Some(path) = path else {
            continue;
        };
        if is_secret_path(&path)
            || is_lockfile_path(&path)
            || is_ignored_path(root_dir, &path, &ignored_rel_paths)
        {
            continue;
        }
        kept.extend_from_slice(&lines[range]);
    }
    if kept.is_empty() {
        return String::new();
    }
    let mut text = kept.join("\n");
    text.push('\n');
    text
}

type DiffSection = (Option<PathBuf>, std::ops::Range<usize>);

fn split_diff_sections(root_dir: &Path, lines: &[&str]) -> Vec<DiffSection> {
    let starts: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.starts_with("diff --git "))
        .map(|(index, _)| index)
        .collect();
    starts
        .iter()
        .enumerate()
        .map(|(nth, &start)| {
            let end = starts.get(nth + 1).copied().unwrap_or(lines.len());
            (section_path(root_dir, &lines[start..end]), start..end)
        })
        .collect()
}

fn section_path(root_dir: &Path, section: &[&str]) -> Option<PathBuf> {
    let mut old_path: Option<PathBuf> = None;
    let mut new_path: Option<PathBuf> = None;
    for line in section.iter().take_while(|line| !line.starts_with("@@")) {
        match git::parse_path_line(line, root_dir) {
            ("new", path) => new_path = path,
            ("old", path) => old_path = path,
            _ => {}
        }
    }
    new_path
        .or(old_path)
        .or_else(|| pathless_section(root_dir, section))
}

/// Sections with no `---`/`+++` pair at all: pure renames, binary files,
/// mode-only changes. A rename states its target outright; the rest carry the
/// same path on both sides of the `diff --git` header, so only a symmetric
/// header is attributable — `a/x b/y` without a rename line could equally be
/// one path containing " b/", and an unattributable section is dropped.
fn pathless_section(root_dir: &Path, section: &[&str]) -> Option<PathBuf> {
    if let Some(quoted) = section
        .iter()
        .find_map(|line| line.strip_prefix("rename to "))
    {
        return contained_path(root_dir, &git::unquote_c_style(quoted.trim()));
    }
    let rest = section.first()?.strip_prefix("diff --git ")?;
    let rel_path = rest.get("a/".len()..rest.find(" b/")?)?;
    if rest != format!("a/{rel_path} b/{rel_path}") {
        return None;
    }
    contained_path(root_dir, rel_path)
}

fn contained_path(root_dir: &Path, rel_path: &str) -> Option<PathBuf> {
    let joined = root_dir.join(rel_path);
    let resolved = joined.canonicalize().unwrap_or_else(|_| joined.clone());
    let resolved_root = root_dir
        .canonicalize()
        .unwrap_or_else(|_| root_dir.to_path_buf());
    resolved.starts_with(&resolved_root).then_some(joined)
}

fn resolve_repo_root(root_dir: &Path) -> Result<PathBuf> {
    let root_dir = root_dir.canonicalize().unwrap_or_else(|e| {
        tracing::debug!("canonicalize failed for '{}': {}", root_dir.display(), e);
        root_dir.to_path_buf()
    });
    if !git::is_git_repo(&root_dir) {
        anyhow::bail!("'{}' is not a git repository", root_dir.display());
    }
    Ok(git::find_toplevel(&root_dir).unwrap_or(root_dir))
}

fn build_diff_context_full(
    root_dir: &Path,
    diff_range: Option<&str>,
    no_content: bool,
    timeout: u64,
) -> Result<DiffContextOutput> {
    git::set_git_timeout(timeout);
    let root_dir = resolve_repo_root(root_dir)?;
    let mut hunks = git::parse_diff(&root_dir, diff_range)?;
    hunks.retain(|h| !is_secret_path(Path::new(&*h.path)));
    if hunks.is_empty() {
        let (deleted, renamed) = deletion_rename_displays(&root_dir, diff_range);
        let mut output = empty_output(&root_dir);
        output.deleted_files = deleted;
        output.renamed_files = renamed;
        return Ok(output);
    }
    let ignored_rel_paths = resolve_ignored_paths(&root_dir, &hunks);
    hunks.retain(|h| !is_ignored_path(&root_dir, Path::new(&*h.path), &ignored_rel_paths));
    if hunks.is_empty() {
        let (deleted, renamed) = deletion_rename_displays(&root_dir, diff_range);
        let mut output = empty_output(&root_dir);
        output.deleted_files = deleted;
        output.renamed_files = renamed;
        return Ok(output);
    }
    let mut changed_files = git::get_changed_files(&root_dir, diff_range)?;
    changed_files
        .retain(|f| !is_secret_path(f) && !is_ignored_path(&root_dir, f, &ignored_rel_paths));
    if changed_files.is_empty() {
        let (deleted, renamed) = deletion_rename_displays(&root_dir, diff_range);
        let mut output = empty_output(&root_dir);
        output.deleted_files = deleted;
        output.renamed_files = renamed;
        return Ok(output);
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
        true,
    );
    assign_token_counts(&mut all_fragments);
    let mut sig_frags = generate_signature_variants(&all_fragments);
    assign_token_counts(&mut sig_frags);
    all_fragments.extend(sig_frags);
    changed_files.sort();
    let core_ids = identify_core_fragments(&hunks, &all_fragments);
    let selected = select_full_mode(&all_fragments, &changed_files);
    batch_reader.close();
    let commit_message = head_rev
        .as_deref()
        .and_then(|h| git::get_commit_message(&root_dir, h).ok())
        .and_then(|m| {
            m.lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .map(str::to_string)
        });
    let mut deleted_display: Vec<String> = git::get_deleted_files(&root_dir, diff_range)
        .map(|set| {
            set.iter()
                .map(|p| {
                    p.strip_prefix(&root_dir)
                        .unwrap_or(p)
                        .to_string_lossy()
                        .replace('\\', "/")
                })
                .collect()
        })
        .unwrap_or_default();
    deleted_display.sort();
    let change = render::ChangeSummary {
        commit_message,
        changed_files: changed_files
            .iter()
            .map(|p| {
                p.strip_prefix(&root_dir)
                    .unwrap_or(p)
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect(),
        deleted_files: deleted_display,
        renamed_files: git::get_rename_pairs(&root_dir, diff_range).unwrap_or_default(),
        // `--full` is the escape hatch that promises every fragment of the
        // changed files, so it keeps lockfile content instead of diverting it.
        lockfile_changes: Vec::new(),
    };
    Ok(render::build_diff_context_output(
        &root_dir,
        &selected,
        no_content,
        &core_ids,
        &FxHashMap::default(),
        change,
    ))
}

fn deletion_rename_displays(
    root_dir: &Path,
    diff_range: Option<&str>,
) -> (Vec<String>, Vec<(String, String)>) {
    let mut deleted: Vec<String> = git::get_deleted_files(root_dir, diff_range)
        .map(|set| {
            set.iter()
                .map(|p| {
                    p.strip_prefix(root_dir)
                        .unwrap_or(p)
                        .to_string_lossy()
                        .replace('\\', "/")
                })
                .collect()
        })
        .unwrap_or_default();
    deleted.sort();
    let renamed = git::get_rename_pairs(root_dir, diff_range).unwrap_or_default();
    (deleted, renamed)
}

fn empty_scored_state_with_changes(root_dir: PathBuf, diff_range: Option<&str>) -> ScoredState {
    let (deleted, renamed) = deletion_rename_displays(&root_dir, diff_range);
    let mut state = empty_scored_state(root_dir);
    state.deleted_files = deleted;
    state.renamed_files = renamed;
    state
}

fn empty_scored_state(root_dir: PathBuf) -> ScoredState {
    let config = PipelineConfig::from_mode(ScoringMode::Ego);
    ScoredState {
        root_dir,
        config,
        all_fragments: Vec::new(),
        core_ids: FxHashSet::default(),
        core_excerpts: FxHashMap::default(),
        lockfile_changes: Vec::new(),
        scoring_result: ScoringResult {
            rel_scores: FxHashMap::default(),
            filtered_fragments: Vec::new(),
            graph: crate::graph::Graph::new(),
            graph_build_ms: 0.0,
            ppr_truncated: false,
            ppr_forward_pushes: 0,
            ppr_backward_pushes: 0,
        },
        needs: Vec::new(),
        changed_files: Vec::new(),
        deleted_files: Vec::new(),
        renamed_files: Vec::new(),
        preferred_revs: Vec::new(),
        commit_message: None,
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
        commit_message: None,
        changed_files: Vec::new(),
        deleted_files: Vec::new(),
        renamed_files: Vec::new(),
        lockfile_changes: Vec::new(),
        fragment_count: 0,
        fragments: Vec::new(),
        latency: None,
    }
}

/// A deletion/rename-only diff has no fragmentable content, but the file
/// lists themselves ARE the change - emit them instead of a bare skeleton.
/// pub(crate): the pybridge select_with_params empty branch must emit the
/// same lists, or MCP/benchmark consumers lose them while the CLI keeps them.
pub(crate) fn empty_output_from_state(state: &ScoredState) -> DiffContextOutput {
    let mut output = empty_output(&state.root_dir);
    output.commit_message = state.commit_message.clone();
    output.deleted_files = state.deleted_files.clone();
    output.renamed_files = state.renamed_files.clone();
    output.lockfile_changes = state.lockfile_changes.clone();
    output
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
    // Stream files one at a time to avoid materialising all content before the cap.
    // Previous par_iter().collect() allocated the full eligible corpus into an
    // intermediate Vec before truncating — on repos with thousands of files this
    // caused peak memory far above max_cache_bytes.
    let mut sorted = candidate_files.to_vec();
    sorted.sort();
    let mut cache: FxHashMap<PathBuf, String> = FxHashMap::default();
    let mut cache_bytes = 0usize;
    for path in sorted {
        if cache_bytes > GRAPH_FILTERING.max_cache_bytes {
            break;
        }
        let Ok(meta) = path.metadata() else { continue };
        if meta.len() as usize > LIMITS.max_file_size {
            continue;
        }
        if let Ok(content) = std::fs::read_to_string(&path) {
            cache_bytes += content.len();
            cache.insert(path, content);
        }
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

fn assign_excerpt_token_counts(excerpts: &mut FxHashMap<FragmentId, Fragment>) {
    for frag in excerpts.values_mut() {
        if frag.token_count == 0 {
            frag.token_count = count_tokens(&frag.content) + LIMITS.overhead_per_fragment;
        }
    }
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
