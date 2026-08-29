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
use crate::mode::{PipelineConfig, ScoringMode};
use crate::postpass;
use crate::render::{self, DiffContextOutput};
use crate::scoring::{ScoringResult, create_scoring_strategy};
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
    /// Changed files withheld by ignore rules (.diffctx/ignore, gitignore).
    /// Listed so the omission is visible: a reader of the output cannot
    /// otherwise tell a file the diff never touched from one the tool
    /// filtered, and #188 documents a reviewer concluding "no tests" from
    /// exactly that silence.
    pub ignored_changes: Vec<String>,
    /// Changed files withheld by `.diffctx/ignore` or the secret-path
    /// heuristics. Count only: both policies' point is that these paths stay
    /// out of the artifact (#85, #214), but a bare number still tells the
    /// reader the output is deliberately incomplete.
    pub policy_excluded_count: usize,
    /// Which discovery strategy first surfaced each discovered path.
    ///
    /// Read-only telemetry for the universe ceiling (#130): without it, a gold
    /// file that never reaches the output is indistinguishable between "no
    /// strategy found it" and "found but outranked", and those need different
    /// fixes. Empty for the changed files themselves, which are not discovered.
    pub discovery_source: FxHashMap<Arc<str>, &'static str>,
    pub preferred_revs: Vec<String>,
    pub commit_message: Option<String>,
    pub heavy_latency_ms: HeavyLatencyMs,
}

#[derive(Default, Clone, Copy)]
pub struct HeavyLatencyMs {
    /// Everything before the heavy phase begins: hunk parse, untracked scan,
    /// ignore resolution, and the `git diff` / `--name-only` / rename calls.
    /// Outside every timer until #183 — which is why a 182s run reported 5.3s of
    /// instrumented work with nothing to say where the rest went.
    pub pre_phase: f64,
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

/// `--mode locate` (#126): same heavy phase and the SAME selection as pack
/// mode, rendered as a ranked navigation list with provenance reasons and no
/// source bodies.
pub fn build_diff_context_locate(
    root_dir: &Path,
    diff_range: Option<&str>,
    budget_tokens: Option<u32>,
    alpha: f64,
    tau: f64,
    scoring_mode: ScoringMode,
    timeout: u64,
) -> Result<crate::locate::LocateOutput> {
    let state = compute_scored_state(root_dir, diff_range, alpha, scoring_mode, timeout)?;
    let outcome = if state.all_fragments.is_empty() {
        SelectionOutcome {
            selected: Vec::new(),
            effective_budget: budget_tokens.unwrap_or(0),
            selection_iters: 0,
            stopping_certificate: 0.0,
            select_ms: 0.0,
            stand_in_ids: FxHashSet::default(),
        }
    } else {
        run_selection(&state, budget_tokens, tau)
    };
    Ok(crate::locate::build_locate(&state, &outcome))
}

/// Line count for an untracked file, or `None` when it is not readable UTF-8
/// text — the same rejection `read_to_string` gave, so binaries stay excluded.
///
/// Untracked files are scanned before any size filter applies
/// (`max_changed_file_size` is enforced later, in fragmentation), so a dirty
/// tree holding one multi-GB log used to allocate all of it here just to reach
/// `.lines().count()`.
///
/// Counted over fixed byte chunks rather than by line. `BufReader::lines()`
/// bounds nothing on its own: it allocates each line, and a minified bundle or
/// a single-line JSON dump is one line hundreds of megabytes long — exactly the
/// shape this was supposed to stop loading. The buffer is the only allocation
/// that scales.
///
/// UTF-8 is validated as it streams, with the incomplete tail of one chunk
/// carried into the next, so a multi-byte character split across a chunk
/// boundary is not mistaken for the invalid byte that rejects a binary.
///
/// Counting rather than size-gating keeps this bit-identical: an oversized file
/// still gets the same hunk it always did, and the count matches `str::lines`
/// (both split on `\n` and neither counts a trailing newline as a line).
fn count_text_lines(path: &Path) -> Option<u32> {
    use std::io::Read;

    const CHUNK: usize = 64 * 1024;

    let mut file = std::fs::File::open(path).ok()?;
    let mut buf = vec![0u8; CHUNK];
    let mut carry: Vec<u8> = Vec::new();
    let mut newlines: u32 = 0;
    let mut last_byte: Option<u8> = None;

    loop {
        let read = file.read(&mut buf).ok()?;
        if read == 0 {
            break;
        }
        carry.extend_from_slice(&buf[..read]);
        let valid_upto = match std::str::from_utf8(&carry) {
            Ok(_) => carry.len(),
            // A truncated character at the end of a chunk is not an error yet;
            // anything else is a binary and rejects the file, as before.
            Err(e) if e.error_len().is_none() => e.valid_up_to(),
            Err(_) => return None,
        };
        newlines = newlines
            .saturating_add(carry[..valid_upto].iter().filter(|b| **b == b'\n').count() as u32);
        if let Some(&b) = carry[..valid_upto].last() {
            last_byte = Some(b);
        }
        carry.drain(..valid_upto);
    }

    // Trailing bytes that never completed a character mean the file ends
    // mid-sequence — invalid UTF-8, same verdict as `read_to_string`.
    if !carry.is_empty() {
        return None;
    }
    // `str::lines` does not count a trailing newline as starting a line, and
    // counts a final unterminated line as one.
    Some(match last_byte {
        None => 0,
        Some(b'\n') => newlines,
        Some(_) => newlines.saturating_add(1),
    })
}

/// Heavy phase: clone/parse/fragment/discover/tokenize/score. Independent
/// of `tau`/`core_budget_fraction`. Designed to be computed ONCE per
/// instance and reused across an arbitrary number of selection cells.
/// Everything `compute_scored_state` decides BEFORE any parsing: the hunk
/// set after the disclosure policy ran, the changed-file list after every
/// exclusion, and the display/count lists those policies owe the output.
/// Extracted so the four policy stances (#85, #188, #214: gitignore = list
/// paths, `.diffctx/ignore` = count only, secrets = count only, lockfiles =
/// paths sans churn) are readable as one unit.
struct ChangeSetData {
    hunks: Vec<crate::types::DiffHunk>,
    diff_text: String,
    changed_files: Vec<PathBuf>,
    deleted_display: Vec<String>,
    renamed_display: Vec<(String, String)>,
    lockfile_display: Vec<String>,
    ignored_display: Vec<String>,
    policy_excluded: usize,
    preferred_revs: Vec<String>,
    commit_message: Option<String>,
    head_rev: Option<String>,
    pre_phase_ms: f64,
}

enum ChangeSet {
    /// Nothing analyzable remains; carries exactly what the empty output
    /// still owes the reader (each early-exit discloses what IT withheld).
    Empty {
        lockfile_changes: Vec<String>,
        ignored_changes: Vec<String>,
        policy_excluded_count: usize,
    },
    Ready(Box<ChangeSetData>),
}

fn resolve_change_set(
    root_dir: &Path,
    diff_range: Option<&str>,
    is_working_tree_diff: bool,
    t_entry: Instant,
) -> Result<ChangeSet> {
    let mut hunks = git::parse_diff(root_dir, diff_range)?;

    let mut untracked_files: Vec<PathBuf> = Vec::new();
    if is_working_tree_diff {
        if let Ok(files) = git::get_untracked_files(root_dir) {
            for f in &files {
                if let Some(line_count) = count_text_lines(f) {
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

    // Secret-classified changed paths are withheld like `.diffctx/ignore`
    // ones, and with the same disclosure stance: count only, never the path
    // (#85, #188, #214). Disjoint from the policy set below by construction —
    // these hunks are gone before resolve_ignored_paths runs.
    let mut secret_excluded_paths: FxHashSet<String> = FxHashSet::default();
    hunks.retain(|h| {
        let p = Path::new(&*h.path);
        if is_secret_path(p) {
            if let Some(rel) = rel_path_string(root_dir, p) {
                secret_excluded_paths.insert(rel);
            }
            false
        } else {
            true
        }
    });

    if hunks.is_empty() {
        return Ok(ChangeSet::Empty {
            lockfile_changes: Vec::new(),
            ignored_changes: Vec::new(),
            policy_excluded_count: secret_excluded_paths.len(),
        });
    }

    let ignored_rel_paths = resolve_ignored_paths(root_dir, &hunks);
    // gitignore-excluded changed files are listed by path; `.diffctx/ignore`
    // is a declared confidentiality policy, so its exclusions surface as a
    // count only — re-publishing the very paths the user asked to withhold
    // would undo the policy (#85), while total silence misreads as "the diff
    // did not touch this" (#188).
    let mut ignored_display: Vec<String> = Vec::new();
    // Counted in files, not hunks: the writer says "N changed file(s)
    // withheld", and a multi-hunk withheld file must not inflate it.
    let mut policy_excluded_paths: FxHashSet<String> = FxHashSet::default();
    for h in &hunks {
        let p = Path::new(&*h.path);
        let Some(rel) = rel_path_string(root_dir, p) else {
            continue;
        };
        match ignored_rel_paths.get(&rel) {
            Some(git::IgnoreSource::Gitignore) => ignored_display.push(rel),
            Some(git::IgnoreSource::DiffctxPolicy) => {
                policy_excluded_paths.insert(rel);
            }
            None => {}
        }
    }
    let policy_excluded = policy_excluded_paths.len() + secret_excluded_paths.len();
    ignored_display.sort();
    ignored_display.dedup();
    hunks.retain(|h| !is_ignored_path(root_dir, Path::new(&*h.path), &ignored_rel_paths));

    let mut lockfile_display: Vec<String> = hunks
        .iter()
        .filter(|h| is_lockfile_path(Path::new(&*h.path)))
        .filter_map(|h| rel_path_string(root_dir, Path::new(&*h.path)))
        .collect();
    lockfile_display.sort();
    lockfile_display.dedup();
    hunks.retain(|h| !is_lockfile_path(Path::new(&*h.path)));

    if hunks.is_empty() {
        return Ok(ChangeSet::Empty {
            lockfile_changes: lockfile_display,
            ignored_changes: ignored_display,
            policy_excluded_count: policy_excluded,
        });
    }

    let diff_text = git::get_diff_text(root_dir, diff_range)?;

    let mut changed_files = git::get_changed_files(root_dir, diff_range)?;
    changed_files.extend(untracked_files);
    if changed_files.is_empty() {
        return Ok(ChangeSet::Empty {
            lockfile_changes: lockfile_display,
            ignored_changes: ignored_display,
            policy_excluded_count: policy_excluded,
        });
    }

    let deleted_files = git::get_deleted_files(root_dir, diff_range)?;
    // Rename source paths are gone from disk and cannot be fragmented; the
    // destinations exist on HEAD and stay candidates via the changed set below,
    // so seeds and discovery still find them.
    let renamed_old = git::get_renamed_paths(root_dir, diff_range)?;
    // Display lists for the output header: deletions and renames produce no
    // fragments, but silently omitting them misrepresents the diff (a
    // deletion-only commit used to render as a bare two-line skeleton).
    let mut deleted_display: Vec<String> = deleted_files
        .iter()
        .map(|p| crate::paths::display_rel_or_abs(root_dir, p))
        .collect();
    deleted_display.sort();
    let renamed_display = git::get_rename_pairs(root_dir, diff_range).unwrap_or_default();
    let excluded: FxHashSet<PathBuf> = deleted_files.into_iter().chain(renamed_old).collect();
    let changed_files: Vec<PathBuf> = changed_files
        .into_iter()
        .filter(|f| {
            let resolved = f.canonicalize().unwrap_or_else(|_| f.clone());
            !excluded.contains(&resolved)
                && !is_lockfile_path(f)
                && !is_withheld(root_dir, f, &ignored_rel_paths)
        })
        .collect();

    let (base_rev, head_rev) = diff_range
        .map(git::split_diff_range)
        .unwrap_or((None, None));
    let preferred_revs = build_preferred_revs(base_rev.as_deref(), head_rev.as_deref());
    let commit_message = head_rev
        .as_deref()
        .and_then(|h| git::get_commit_message(root_dir, h).ok())
        .and_then(|m| {
            m.lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .map(str::to_string)
        });

    let pre_phase_ms = t_entry.elapsed().as_secs_f64() * 1000.0;
    Ok(ChangeSet::Ready(Box::new(ChangeSetData {
        hunks,
        diff_text,
        changed_files,
        deleted_display,
        renamed_display,
        lockfile_display,
        ignored_display,
        policy_excluded,
        preferred_revs,
        commit_message,
        head_rev,
        pre_phase_ms,
    })))
}

pub fn compute_scored_state(
    root_dir: &Path,
    diff_range: Option<&str>,
    alpha: f64,
    scoring_mode: ScoringMode,
    timeout: u64,
) -> Result<ScoredState> {
    let t_entry = Instant::now();
    git::set_git_timeout(timeout);
    let deadline = crate::deadline::Deadline::from_timeout_secs(timeout);
    let root_dir = resolve_repo_root(root_dir)?;
    if alpha <= 0.0 || alpha >= 1.0 {
        anyhow::bail!("alpha must be in (0, 1), got {}", alpha);
    }

    let resolved = git::resolve_duration_range(&root_dir, diff_range)?;
    let diff_range = resolved.range.as_deref();
    // Untracked files only matter when the diff includes the live working
    // tree. `None` and the literal "HEAD" both mean that (the CLI resolves
    // bare `--diff` to the string "HEAD" before reaching here) - a historical
    // range like `HEAD~5..HEAD~3` does not include working-tree state. A
    // duration window ends at "now", so it includes it too.
    let is_working_tree_diff = resolved.from_duration || matches!(diff_range, None | Some("HEAD"));

    let data = match resolve_change_set(&root_dir, diff_range, is_working_tree_diff, t_entry)? {
        ChangeSet::Empty {
            lockfile_changes,
            ignored_changes,
            policy_excluded_count,
        } => {
            let mut state = empty_scored_state_with_changes(root_dir, diff_range);
            state.lockfile_changes = lockfile_changes;
            state.ignored_changes = ignored_changes;
            state.policy_excluded_count = policy_excluded_count;
            return Ok(state);
        }
        ChangeSet::Ready(data) => data,
    };
    let ChangeSetData {
        hunks,
        diff_text,
        changed_files,
        deleted_display,
        renamed_display,
        lockfile_display,
        ignored_display,
        policy_excluded,
        preferred_revs,
        commit_message,
        head_rev,
        pre_phase_ms,
    } = *data;

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
    config.ppr_alpha = alpha;
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

    let (discovered_files, discovery_attribution) =
        create_discovery(&config).discover_attributed(&discovery_ctx);
    let discovered_files: Vec<PathBuf> = discovered_files
        .into_iter()
        .map(|p| candidate_files::normalize_path(&p, &root_dir))
        .collect();
    let discovery_source: FxHashMap<Arc<str>, &'static str> = discovery_attribution
        .into_iter()
        .map(|(path, source)| {
            let normalized = candidate_files::normalize_path(&path, &root_dir);
            (Arc::from(normalized.to_string_lossy().as_ref()), source)
        })
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

    let strategy = create_scoring_strategy(&config);

    let scoring_result = strategy.score_and_filter(
        &all_fragments,
        &core_ids,
        &hunks,
        Some(root_dir.as_path()),
        Some(&seed_weights),
        Some(&discovered_path_set),
        deadline,
    );

    let needs = crate::utility::needs::needs_from_diff(&all_fragments, &core_ids, &diff_text);

    let t_done = Instant::now();
    batch_reader.close();

    let graph_build_ms = scoring_result.graph_build_ms;
    let heavy_latency_ms = HeavyLatencyMs {
        pre_phase: pre_phase_ms,
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
        "diffctx heavy: pre_phase {:.3}s, parse_changed {:.3}s, universe {:.3}s, discovery {:.3}s, parse_discovered {:.3}s, tokenization {:.3}s, graph_build {:.3}s, scoring {:.3}s",
        heavy_latency_ms.pre_phase / 1000.0,
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
        discovery_source,
        changed_files,
        deleted_files: deleted_display,
        renamed_files: renamed_display,
        lockfile_changes: lockfile_display,
        ignored_changes: ignored_display,
        policy_excluded_count: policy_excluded,
        preferred_revs,
        commit_message,
        heavy_latency_ms,
    })
}

pub struct SelectionOutcome {
    pub selected: Vec<Fragment>,
    pub effective_budget: u32,
    pub selection_iters: usize,
    pub stopping_certificate: f64,
    pub select_ms: f64,
    /// See `SelectionResult::stand_in_ids` — carried to the renderers so both
    /// surfaces read one recorded fact instead of re-deriving it (#209).
    pub stand_in_ids: FxHashSet<FragmentId>,
}

/// Selection + the two admission-gated post-passes — the git-free part of
/// the chain, shared by the product pipeline (`run_selection`) and the
/// in-memory corpus harness so the harness scores the shipped system by
/// construction. It used to be re-spelled by hand in `memory_pipeline.rs`,
/// which is how the harness once measured a system nobody runs (#149).
#[allow(clippy::too_many_arguments)]
pub fn select_and_postpass(
    scoring_result: &ScoringResult,
    all_fragments: &[Fragment],
    core_ids: &FxHashSet<FragmentId>,
    needs: &[InformationNeed],
    core_excerpts: &FxHashMap<FragmentId, Fragment>,
    objective: crate::mode::ObjectiveMode,
    effective_budget: u32,
    tau: f64,
) -> (Vec<Fragment>, usize, f64, FxHashSet<FragmentId>) {
    let tau = if scoring_result.admissible_files.is_none()
        && tau == crate::config::limits::DEFAULT_STOPPING_THRESHOLD
    {
        crate::config::limits::UNGATED_STOPPING_THRESHOLD
    } else {
        tau
    };
    let selection_result = match objective {
        crate::mode::ObjectiveMode::BoltzmannModular => {
            let beta = crate::utility::calibrate_beta(
                &scoring_result.filtered_fragments,
                core_ids,
                &scoring_result.rel_scores,
                effective_budget,
                crate::config::selection::boltzmann().calibration_tolerance,
            );
            tracing::debug!("diffctx: boltzmann beta calibrated to {:.6e}", beta);
            crate::utility::boltzmann_select(
                &scoring_result.filtered_fragments,
                core_ids,
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
                core_ids,
                &scoring_result.rel_scores,
                needs,
                effective_budget,
                tau,
                Some(&file_importance),
                Some(core_excerpts),
                scoring_result.admissible_files.as_ref(),
                scoring_result.declared_admissible_files.as_ref(),
            )
        }
    };

    let selection_iters = selection_result.greedy_iters;
    let stopping_certificate = selection_result.stopping_certificate;
    let stand_in_ids = selection_result.stand_in_ids;
    let mut selected = selection_result.selected;

    postpass::coherence_post_pass(
        &mut selected,
        &scoring_result.filtered_fragments,
        &scoring_result.graph,
        effective_budget,
        scoring_result.admissible_files.as_ref(),
    );

    postpass::rescue_nontrivial_context(
        &mut selected,
        all_fragments,
        &scoring_result.rel_scores,
        core_ids,
        effective_budget,
        scoring_result.admissible_files.as_ref(),
    );

    (
        selected,
        selection_iters,
        stopping_certificate,
        stand_in_ids,
    )
}

/// Selection + the 3 post-passes, shared verbatim by the pack renderer
/// (`select_with_params`) and the locate renderer — extracting it is pure
/// code motion so both modes select identically by construction.
pub fn run_selection(
    state: &ScoredState,
    budget_tokens: Option<u32>,
    tau: f64,
) -> SelectionOutcome {
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

    let (mut selected, selection_iters, stopping_certificate, stand_in_ids) = select_and_postpass(
        &state.scoring_result,
        &state.all_fragments,
        &state.core_ids,
        &state.needs,
        &state.core_excerpts,
        state.config.objective,
        effective_budget,
        tau,
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
        &stand_in_ids,
    );
    if let Some(mut r) = batch_reader {
        r.close();
    }

    crate::provenance::maybe_dump(state, &selected);

    let select_ms = t_start.elapsed().as_secs_f64() * 1000.0;
    SelectionOutcome {
        selected,
        effective_budget,
        selection_iters,
        stopping_certificate,
        select_ms,
        stand_in_ids,
    }
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
    let outcome = run_selection(state, budget_tokens, tau);
    let stand_in_ids = outcome.stand_in_ids;
    let selected = outcome.selected;
    let selection_iters = outcome.selection_iters;
    let stopping_certificate = outcome.stopping_certificate;
    let select_ms = outcome.select_ms;

    let total_ms = state.heavy_latency_ms.pre_phase
        + state.heavy_latency_ms.parse_changed
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
            .map(|p| crate::paths::display_rel_or_abs(&state.root_dir, p))
            .collect(),
        deleted_files: state.deleted_files.clone(),
        renamed_files: state.renamed_files.clone(),
        lockfile_changes: state.lockfile_changes.clone(),
        ignored_changes: state.ignored_changes.clone(),
        policy_excluded_count: state.policy_excluded_count,
    };
    let mut output = render::build_diff_context_output(
        &state.root_dir,
        &selected,
        no_content,
        &state.core_ids,
        &stand_in_ids,
        &state.scoring_result.rel_scores,
        change,
    );
    tracing::debug!(
        "diffctx selection: selection {:.3}s (incl. post-passes), total {:.3}s",
        select_ms / 1000.0,
        total_ms / 1000.0,
    );
    output.latency = Some(render::LatencyBreakdown {
        pre_phase_ms: state.heavy_latency_ms.pre_phase,
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

/// THE secret-path policy, for every surface. Private-key, keystore and
/// credential files never reach LLM-bound output — not as diff context, not
/// in a tree map, not through an MCP fetch. Tree mode and the MCP tools used
/// to keep a second, shorter list in `ignore.py` (#227/#228: `.netrc`,
/// `credentials` and `*.asc` were withheld by diff mode and printed by tree
/// mode); they now call this through `_diffctx.is_secret_path`. Matches by
/// file name only, so public keys (`*.pub`) stay visible. `.env` is
/// intentionally NOT here: a changed `.env` is legitimate change context (see
/// the `*_env_file_change` cases).
pub fn is_secret_path(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    // Whole-name matches. `_sk` is the sealed-secret half of an SSH key pair
    // written by `ssh-keygen -O`; `.netrc`/`credentials` carry passwords in
    // plain text and are the shapes CI images most often leak.
    if matches!(
        name,
        "id_rsa"
            | "id_dsa"
            | "id_ecdsa"
            | "id_ed25519"
            | "id_ed25519_sk"
            | "id_ecdsa_sk"
            | ".netrc"
            | "_netrc"
            | "credentials"
            | ".npmrc"
            | ".pypirc"
    ) {
        return true;
    }
    matches!(
        path.extension().and_then(|e| e.to_str()),
        // `.ppk` is PuTTY's private key, `.p8` Apple's signing key, `.asc` an
        // armoured PGP export — all private-key containers the original list
        // happened not to name.
        Some("pem" | "key" | "pfx" | "p12" | "keystore" | "jks" | "ppk" | "p8" | "asc")
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

pub(crate) fn rel_path_string(root_dir: &Path, path: &Path) -> Option<String> {
    crate::paths::display_rel(root_dir, path)
}

/// Resolves `.gitignore` / `.diffctx/ignore` exclusions (#85) for every path
/// touched by `hunks`, in one batched git call. diff mode previously only
/// ever excluded a hardcoded set of secret-like filenames (`is_secret_path`)
/// — a file a user explicitly excluded via `.diffctx/ignore` still had its
/// changed content surfaced in full.
fn resolve_ignored_paths(
    root_dir: &Path,
    hunks: &[crate::types::DiffHunk],
) -> rustc_hash::FxHashMap<String, git::IgnoreSource> {
    let rel_paths: Vec<String> = hunks
        .iter()
        .filter_map(|h| rel_path_string(root_dir, Path::new(&*h.path)))
        .collect();
    git::find_ignored_paths_with_source(root_dir, &rel_paths)
}

pub(crate) fn is_ignored_path(
    root_dir: &Path,
    path: &Path,
    ignored_rel_paths: &rustc_hash::FxHashMap<String, git::IgnoreSource>,
) -> bool {
    rel_path_string(root_dir, path)
        .map(|rel| ignored_rel_paths.contains_key(&rel))
        .unwrap_or(false)
}

/// The one admissibility predicate: secret by name, or excluded by
/// `.gitignore` / `.diffctx/ignore` as git itself resolves them. Every place
/// that decides whether a file may be shown composes these two the same way.
pub(crate) fn is_withheld(
    root_dir: &Path,
    path: &Path,
    ignored_rel_paths: &rustc_hash::FxHashMap<String, git::IgnoreSource>,
) -> bool {
    is_secret_path(path) || is_ignored_path(root_dir, path, ignored_rel_paths)
}

/// Which of `rel_paths` (repo-root-relative) the engine withholds — the
/// question the MCP fetch used to answer with a different engine (#228). One
/// batched `git check-ignore` for the whole list.
pub fn withheld_paths(root_dir: &Path, rel_paths: &[String]) -> Vec<String> {
    let ignored = git::find_ignored_paths_with_source(root_dir, rel_paths);
    rel_paths
        .iter()
        .filter(|rel| is_withheld(root_dir, &root_dir.join(rel), &ignored))
        .cloned()
        .collect()
}

/// The unified diff of `diff_range` as git prints it, minus the file sections
/// diff mode never discloses: secret-like paths, ignored paths, and lock files
/// (#112). Additive output for `--with-raw-diff` (#150) — it feeds no
/// selection state, so selection is bit-identical with and without it.
pub fn raw_diff_text(root_dir: &Path, diff_range: Option<&str>, timeout: u64) -> Result<String> {
    git::set_git_timeout(timeout);
    let root_dir = resolve_repo_root(root_dir)?;
    let resolved = git::resolve_duration_range(&root_dir, diff_range)?;
    let diff_text = git::get_diff_text(&root_dir, resolved.range.as_deref())?;
    Ok(keep_disclosable_sections(&root_dir, &diff_text))
}

fn keep_disclosable_sections(root_dir: &Path, diff_text: &str) -> String {
    // Two views over the same text. Analysis runs on terminator-free lines so
    // the exact header comparisons below keep working; output is emitted from
    // the raw slices, because the bundle is advertised as git's own patch and
    // dropping the CR of a CRLF repository yields something `git apply`
    // rejects.
    let raw: Vec<&str> = diff_text.split_inclusive('\n').collect();
    let lines: Vec<&str> = raw
        .iter()
        .map(|line| line.trim_end_matches('\n').trim_end_matches('\r'))
        .collect();
    let sections = split_diff_sections(root_dir, &lines);
    let rel_paths: Vec<String> = sections
        .iter()
        .filter_map(|(path, _)| path.as_deref())
        .filter_map(|path| rel_path_string(root_dir, path))
        .collect();
    let ignored_rel_paths = git::find_ignored_paths_with_source(root_dir, &rel_paths);

    let mut kept: Vec<&str> = Vec::new();
    for (path, range) in sections {
        // A section whose path cannot be resolved inside the repository is
        // dropped: the bundle must never widen what diff mode is willing to
        // show, and an unattributable section cannot be policy-checked.
        let Some(path) = path else {
            continue;
        };
        if is_lockfile_path(&path) || is_withheld(root_dir, &path, &ignored_rel_paths) {
            continue;
        }
        kept.extend_from_slice(&raw[range]);
    }
    if kept.is_empty() {
        return String::new();
    }
    let mut text = kept.concat();
    if !text.ends_with('\n') {
        text.push('\n');
    }
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
        return git::resolve_in_repo(root_dir, &git::unquote_c_style(quoted.trim()));
    }
    let rest = section.first()?.strip_prefix("diff --git ")?;
    let rel_path = rest.get("a/".len()..rest.find(" b/")?)?;
    if rest != format!("a/{rel_path} b/{rel_path}") {
        return None;
    }
    git::resolve_in_repo(root_dir, rel_path)
}

fn resolve_repo_root(root_dir: &Path) -> Result<PathBuf> {
    let root_dir = root_dir.canonicalize().unwrap_or_else(|e| {
        tracing::debug!("canonicalize failed for '{}': {}", root_dir.display(), e);
        root_dir.to_path_buf()
    });
    if !git::is_git_repo(&root_dir)? {
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
    // --full runs no scoring/edge phase, so the git subprocess timeout is the
    // only ceiling it needs.
    git::set_git_timeout(timeout);
    let root_dir = resolve_repo_root(root_dir)?;
    let resolved = git::resolve_duration_range(&root_dir, diff_range)?;
    let diff_range = resolved.range.as_deref();
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
    changed_files.retain(|f| !is_withheld(&root_dir, f, &ignored_rel_paths));
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
    let (deleted_display, renamed_display) = deletion_rename_displays(&root_dir, diff_range);
    let change = render::ChangeSummary {
        commit_message,
        changed_files: changed_files
            .iter()
            .map(|p| crate::paths::display_rel_or_abs(&root_dir, p))
            .collect(),
        deleted_files: deleted_display,
        renamed_files: renamed_display,
        // `--full` is the escape hatch that promises every fragment of the
        // changed files, so it keeps lockfile content instead of diverting it.
        lockfile_changes: Vec::new(),
        ignored_changes: Vec::new(),
        policy_excluded_count: 0,
    };
    Ok(render::build_diff_context_output(
        &root_dir,
        &selected,
        no_content,
        &core_ids,
        // `--full` emits whole files: nothing is substituted for a core.
        &FxHashSet::default(),
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
                .map(|p| crate::paths::display_rel_or_abs(root_dir, p))
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
        discovery_source: FxHashMap::default(),
        lockfile_changes: Vec::new(),
        ignored_changes: Vec::new(),
        policy_excluded_count: 0,
        scoring_result: ScoringResult {
            admissible_files: None,
            declared_admissible_files: None,
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
    DiffContextOutput::empty(&name)
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
    output.ignored_changes = state.ignored_changes.clone();
    output.policy_excluded_count = state.policy_excluded_count;
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
    Box::new(EnsembleDiscovery::new(vec![
        Box::new(DefaultDiscovery),
        Box::new(TestFileDiscovery),
        Box::new(BM25Discovery::new(config.bm25_top_k)),
    ]))
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

// The token-count formula, in one place. The in-memory harness spelled it out
// three more times; #149 is what happens when the harness and the shipped
// pipeline disagree about it — the corpus then measures a system nobody runs.
pub(crate) fn assign_token_counts(fragments: &mut [Fragment]) {
    fragments.par_iter_mut().for_each(|frag| {
        if frag.token_count == 0 {
            frag.token_count = count_tokens(&frag.content) + LIMITS.overhead_per_fragment;
        }
    });
}

pub(crate) fn assign_excerpt_token_counts(excerpts: &mut FxHashMap<FragmentId, Fragment>) {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The raw-diff bundle decides which sections it may disclose by resolving
    /// each `diff --git` header against the repo root. That guard used to be a
    /// second, independent copy of the one in `git::parse_path_line`, carrying
    /// the same lexical-prefix hole; both now share `git::resolve_in_repo`, so
    /// this pins that a section naming a path outside the root is dropped
    /// rather than bundled.
    #[test]
    fn a_raw_diff_section_escaping_the_root_resolves_to_nothing() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let base = tmp.path().canonicalize().expect("canonical tempdir");
        let root = base.join("repo");
        std::fs::create_dir_all(&root).expect("mkdir repo");
        std::fs::write(base.join("outside.py"), "secret = 1\n").expect("write outside");
        std::fs::write(root.join("inside.py"), "x = 1\n").expect("write inside");

        for rel in ["../outside.py", "../missing.py", "sub/../../outside.py"] {
            let header = format!("diff --git a/{rel} b/{rel}");
            assert!(
                section_path(&root, &[header.as_str()]).is_none(),
                "escaping section accepted: {rel}"
            );
        }

        let header = "diff --git a/inside.py b/inside.py";
        assert!(
            section_path(&root, &[header]).is_some_and(|p| p.ends_with("inside.py")),
            "an in-repo section was dropped"
        );
    }
}

#[cfg(test)]
mod secret_path_tests {
    use super::is_secret_path;
    use std::path::Path;

    fn secret(p: &str) -> bool {
        is_secret_path(Path::new(p))
    }

    /// The original list named the four classic SSH key stems and six
    /// certificate extensions, which left whole families of private-key
    /// container through: PuTTY, Apple signing keys, armoured PGP exports, the
    /// hardware-backed SSH variants, and the plain-text credential files CI
    /// images leak most often.
    #[test]
    fn private_key_and_credential_shapes_are_excluded() {
        for path in [
            "home/.ssh/id_rsa",
            "home/.ssh/id_ed25519",
            "home/.ssh/id_ed25519_sk",
            "home/.ssh/id_ecdsa_sk",
            "certs/server.pem",
            "certs/server.key",
            "certs/bundle.pfx",
            "certs/bundle.p12",
            "android/release.keystore",
            "android/release.jks",
            "windows/deploy.ppk",
            "apple/AuthKey_ABC123.p8",
            "gpg/private.asc",
            "home/.netrc",
            "home/_netrc",
            "aws/credentials",
            "home/.npmrc",
            "home/.pypirc",
        ] {
            assert!(secret(path), "not excluded: {path}");
        }
    }

    /// Public halves stay visible — they are not secrets, and a changed
    /// `authorized_keys` or `.pub` is legitimate review context.
    #[test]
    fn public_material_is_not_excluded() {
        for path in [
            "home/.ssh/id_rsa.pub",
            "home/.ssh/id_ed25519.pub",
            "home/.ssh/authorized_keys",
            "certs/server.crt",
        ] {
            assert!(!secret(path), "wrongly excluded: {path}");
        }
    }

    /// `.env` is deliberately NOT excluded: a changed `.env` is change context,
    /// and corpus cases assert on it. Pinned so widening the list never quietly
    /// takes it.
    #[test]
    fn env_files_remain_visible_by_design() {
        assert!(!secret(".env"));
        assert!(!secret("config/.env.production"));
    }

    /// Ordinary source that merely contains a matching word is untouched — the
    /// rule is whole-name or extension, never substring.
    #[test]
    fn ordinary_files_are_untouched() {
        for path in [
            "src/keyboard.rs",
            "src/credentials_form.tsx",
            "docs/pemphigus.md",
        ] {
            assert!(!secret(path), "wrongly excluded: {path}");
        }
    }
}
