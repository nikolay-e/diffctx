use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::answer::envelope::Inventory;
use crate::answer::v2::Listing;
use crate::candidate_files;
use crate::config::budget::BUDGET;
use crate::config::graph_filtering::GRAPH_FILTERING;
use crate::config::limits::LIMITS;
use crate::config::tokenization::TOKENIZATION;
use crate::core::{compute_changed_lines, compute_seed_weights, identify_core_fragments};
use crate::discovery::{
    BM25Discovery, DefaultDiscovery, DiscoveryContext, DiscoveryStrategy, EnsembleDiscovery,
    ReverseReferenceDiscovery, TestFileDiscovery,
};
use crate::fragmentation::process_files_for_fragments;
use crate::mode::{PipelineConfig, ScoringMode};
use crate::postpass;
use crate::render::{self, DiffContextOutput};
use crate::run_provenance::state::{
    HeavyLatencyMs, ScoredState, SelectionOutcome, answer_paths, fragmentless_changed_files,
};
use crate::scoring::{ScoringResult, create_scoring_strategy};
use crate::signatures::generate_signature_variants;
use crate::snapshot;
use crate::source::{Admitted, Source, Why};
use crate::tokenizer::count_tokens;
use crate::types::{Fragment, FragmentId, unquote_c_style};
use crate::utility::InformationNeed;
use crate::{diff_parse, grep};

/// NaN is the case a `< 0.0` check alone misses: every comparison against it
/// is false, so it read as "not negative" and became a threshold nothing could
/// ever exceed. Checked at the two `Result`-returning entry points; the
/// Python bridge reaches both.
fn validate_tau(tau: Option<f64>) -> Result<()> {
    match tau {
        Some(t) if !(t.is_finite() && t >= 0.0) => {
            anyhow::bail!("tau must be a finite value >= 0, got {}", t)
        }
        _ => Ok(()),
    }
}

/// `paths` narrow the change set and the discovery universe (repo-relative,
/// or absolute under the repository). With none, a `root_dir` below the
/// repository root narrows to that subtree — `cd pkg && diffctx .` and
/// `diffctx pkg` mean the same thing.
#[allow(clippy::too_many_arguments)]
pub fn build_diff_context_in(
    root_dir: &Path,
    diff_range: Option<&str>,
    paths: &[String],
    budget_tokens: Option<u32>,
    alpha: f64,
    tau: Option<f64>,
    no_content: bool,
    full: bool,
    scoring_mode: ScoringMode,
    timeout: u64,
) -> Result<DiffContextOutput> {
    let _run = crate::profile::run(if full { "full" } else { "pack" });
    if full {
        return build_diff_context_full(root_dir, diff_range, paths, no_content, timeout);
    }
    validate_tau(tau)?;
    let state = compute_scored_state(root_dir, diff_range, paths, alpha, scoring_mode, timeout)?;
    if state.all_fragments.is_empty() {
        return Ok(empty_output_from_state(&state));
    }
    Ok(select_with_params(&state, budget_tokens, tau, no_content))
}

/// `--mode locate` (#126): same heavy phase and the SAME selection as pack
/// mode, rendered as a ranked navigation list with provenance reasons and no
/// source bodies.
#[allow(clippy::too_many_arguments)]
pub fn build_diff_context_locate(
    root_dir: &Path,
    diff_range: Option<&str>,
    paths: &[String],
    budget_tokens: Option<u32>,
    alpha: f64,
    tau: Option<f64>,
    scoring_mode: ScoringMode,
    timeout: u64,
    include_overflow: bool,
) -> Result<crate::locate::LocateOutput> {
    let _run = crate::profile::run("locate");
    validate_tau(tau)?;
    let state = compute_scored_state(root_dir, diff_range, paths, alpha, scoring_mode, timeout)?;
    let outcome = if state.all_fragments.is_empty() {
        SelectionOutcome {
            selected: Vec::new(),
            effective_budget: budget_tokens.unwrap_or(0),
            selection_budget: budget_tokens.unwrap_or(0),
            selection_iters: 0,
            stopping_certificate: 0.0,
            select_ms: 0.0,
            stand_in_ids: FxHashSet::default(),
            budget_requested: budget_tokens,
            tau: tau.unwrap_or(crate::config::limits::DEFAULT_STOPPING_THRESHOLD),
            gated: false,
            limit_reasons: Vec::new(),
            commit_messages: state.commit_messages.clone(),
        }
    } else {
        run_selection(&state, budget_tokens, tau)
    };
    let _render = crate::profile::phase("render");
    let mut output = crate::locate::build_locate(&state, &outcome);
    // The first item of every changed file stays whatever the budget: one too
    // small for the selection to hold them is cut from the answer the selection
    // would make, so the least it declares is the one every budget below it says.
    if budget_tokens.is_some_and(|b| b > 0)
        && !output
            .coverage
            .selection
            .unrepresented_changed_files
            .is_empty()
    {
        let mut whole = run_selection(&state, None, tau);
        whole.effective_budget = outcome.effective_budget;
        whole.budget_requested = outcome.budget_requested;
        whole.selection_budget = outcome.selection_budget;
        output = crate::locate::build_locate(&state, &whole);
    }
    output.fit_to(outcome.effective_budget, include_overflow);
    Ok(output)
}

/// `--mode impact` (#310): the same scored state as locate, rendered as what
/// the change reaches outside its own diff. No selection runs: the answer is
/// a graph walk from the changed symbols, capped on its serialized size.
pub fn build_diff_context_impact(
    root_dir: &Path,
    diff_range: Option<&str>,
    paths: &[String],
    alpha: f64,
    scoring_mode: ScoringMode,
    timeout: u64,
) -> Result<crate::impact::ImpactOutput> {
    impact_of(
        root_dir,
        Anchor::Diff(diff_range),
        diff_range,
        paths,
        alpha,
        scoring_mode,
        timeout,
    )
}

/// `--mode impact` over a range the caller holds as a value (a push, a
/// force-push): the same run, with no range text to read again.
pub fn build_range_impact(
    root_dir: &Path,
    range: crate::snapshot::RangeSpec,
    paths: &[String],
    alpha: f64,
    scoring_mode: ScoringMode,
    timeout: u64,
) -> Result<crate::impact::ImpactOutput> {
    impact_of(
        root_dir,
        Anchor::Range(range),
        None,
        paths,
        alpha,
        scoring_mode,
        timeout,
    )
}

fn impact_of(
    root_dir: &Path,
    anchor: Anchor<'_>,
    label: Option<&str>,
    paths: &[String],
    alpha: f64,
    scoring_mode: ScoringMode,
    timeout: u64,
) -> Result<crate::impact::ImpactOutput> {
    let _run = crate::profile::run("impact");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout);
    let state =
        compute_scored_state_anchored(root_dir, anchor, paths, alpha, scoring_mode, timeout, true)?;
    // A range handed in as a value names itself by what the run resolved.
    let label = label.or(state.analysed_range.as_deref());
    let _in_run = state.run.enter();
    Ok(crate::impact::build_impact(
        &state,
        label,
        None,
        Some(deadline),
    ))
}

/// `--symbol` (#336): impact anchored on a name's definitions instead of a
/// diff — where it is defined, who calls it, which tests reach each caller.
pub fn build_symbol_impact(
    root_dir: &Path,
    query: &str,
    paths: &[String],
    alpha: f64,
    scoring_mode: ScoringMode,
    timeout: u64,
) -> Result<crate::impact::ImpactOutput> {
    let _run = crate::profile::run("symbol");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout);
    let state = match compute_scored_state_anchored(
        root_dir,
        Anchor::Symbol(query),
        paths,
        alpha,
        scoring_mode,
        timeout,
        true,
    ) {
        Ok(state) => state,
        Err(e) => {
            return match e.downcast::<SymbolQueryError>() {
                Ok(SymbolQueryError { locals, .. }) if !locals.is_empty() => {
                    Ok(crate::impact::symbol_local_output(root_dir, query, locals))
                }
                Ok(mut q) => {
                    let module = if q.malformed {
                        None
                    } else {
                        crate::impact::symbol_module_output(root_dir, query, deadline)?
                    };
                    match (module, q.named.take()) {
                        (Some(output), _) => Ok(output),
                        (None, Some(named)) => {
                            Ok(crate::impact::symbol_named_output(root_dir, query, named))
                        }
                        (None, None) => Err(q.into()),
                    }
                }
                Err(e) => Err(e),
            };
        }
    };
    let _in_run = state.run.enter();
    Ok(crate::impact::build_impact(
        &state,
        None,
        Some(query),
        Some(deadline),
    ))
}

/// Line count for an untracked file, or `None` when it is binary — the same
/// NUL test the fragment reader applies, so a Latin-1 source file counts
/// (and is later decoded lossily, with disclosure) exactly as it does once
/// committed, while a real binary stays excluded.
///
/// Untracked files are scanned before any size filter applies
/// (`max_changed_file_size` is enforced later, in fragmentation); the reader
/// refuses a blob past its read cap, so a dirty tree holding one multi-GB log
/// is not allocated here, and the refusal is noted on the run.
///
/// The count matches `str::lines` (both split on `\n` and neither counts a
/// trailing newline as a line).
fn count_text_lines(source: &Source, path: &Path) -> Option<u32> {
    const CHUNK: usize = 64 * 1024;

    let bytes = source.read_bytes(path)?;
    if crate::fragmentation::looks_binary(&bytes[..bytes.len().min(CHUNK)]) {
        return None;
    }
    let newlines = bytes.iter().filter(|b| **b == b'\n').count() as u32;
    Some(match bytes.last() {
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
    untracked_files: Vec<PathBuf>,
    deleted_display: Vec<String>,
    renamed_display: Vec<(String, String)>,
    lockfile_display: Vec<String>,
    ignored_display: Vec<String>,
    policy_excluded: usize,
    commit_message: Option<String>,
    commit_messages: Vec<String>,
    commit_count: usize,
    head_rev: Option<String>,
    /// The newest commit the history channels may read: the range's base,
    /// so a change's own commits never vouch for its files (#340).
    history_rev: Option<String>,
    /// `--symbol`: the files naming the symbol beside its definitions' own.
    named_files: Vec<PathBuf>,
    pre_phase_ms: f64,
}

enum ChangeSet {
    /// Nothing analyzable remains; carries exactly what the empty output
    /// still owes the reader (each early-exit discloses what IT withheld).
    /// `changed_files` are the paths git reports changed without a text hunk
    /// — a mode flip, a submodule bump, a `-diff` attribute — listed so the
    /// reader sees an unrepresented change rather than "no change".
    Empty {
        changed_files: Vec<PathBuf>,
        untracked_files: Vec<PathBuf>,
        lockfile_changes: Vec<String>,
        ignored_changes: Vec<String>,
        policy_excluded_count: usize,
    },
    Ready(Box<ChangeSetData>),
}

/// The diff plus, for a working-tree range, the untracked files — which are
/// changes with no diff of their own, so git never reports them and they have
/// to be synthesised as whole-file hunks.
///
/// Shared with `--full`. It used to call `parse_diff` alone, which made
/// `diffctx . --full` blind to every new file in the tree while the default
/// mode saw them: the escape hatch that promises MORE context returned less.
pub(crate) fn collect_hunks_with_untracked(
    root_dir: &Path,
    diff_range: Option<&str>,
    pathspec: &[String],
    is_working_tree_diff: bool,
    source: &Source,
    run: Option<&crate::resource::RunContext>,
) -> Result<(Vec<crate::types::DiffHunk>, Vec<PathBuf>)> {
    let mut hunks = diff_parse::parse_diff(root_dir, diff_range, pathspec)?;
    if !is_working_tree_diff {
        return Ok((hunks, Vec::new()));
    }
    let mut untracked_files =
        unless_cut_short(run, diff_parse::get_untracked_files(root_dir, pathspec))?;
    // The untracked scan is the one path that READS a working-tree name
    // before anything else has vetted it, so containment is re-asked here in
    // its canonicalising form: `evil -> /etc/shadow` is lexically inside the
    // root and would otherwise be line-counted and then fragmented out of the
    // file it points at.
    let within = crate::paths::Containment::new(root_dir);
    untracked_files.retain(|f| within.contains(f));
    // An untracked binary — a local database, a build artefact — is no
    // source change: counted, it inflated every "N changed file(s)" with
    // files nobody meant to commit and diffctx cannot read (#423).
    let mut text_files = Vec::with_capacity(untracked_files.len());
    for f in untracked_files {
        let Some(line_count) = count_text_lines(source, &f) else {
            continue;
        };
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
        text_files.push(f);
    }
    Ok((hunks, text_files))
}

/// A listing of what the change touches. The run's own deadline or
/// cancellation cutting it short leaves it empty with the reason noted, so
/// the artifact says it is partial; anything else is git refusing to answer
/// and stays an error, as an interruption does outside a run (`--full` has
/// no partial form). Swallowed, either one read as "nothing changed" (#408).
fn unless_cut_short<T: Default>(
    run: Option<&crate::resource::RunContext>,
    answer: snapshot::Result<T>,
) -> Result<T> {
    match (answer, run) {
        (Ok(answer), _) => Ok(answer),
        (Err(e), Some(run)) if e.is_interruption() => {
            run.note(match e {
                snapshot::SnapshotError::Cancelled => crate::resource::LimitReason::Cancelled,
                _ => crate::resource::LimitReason::Deadline,
            });
            Ok(T::default())
        }
        (Err(e), _) => Err(e.into()),
    }
}

/// Secret-classified changed paths are withheld like `.diffctx/ignore` ones,
/// and with the same disclosure stance: count only, never the path (#85, #188,
/// #214). Disjoint from the policy set by construction — these hunks are gone
/// before the admission of the remaining hunks runs.
fn withhold_secret_hunks(
    root_dir: &Path,
    source: &Source,
    hunks: &mut Vec<crate::types::DiffHunk>,
) -> FxHashSet<String> {
    let mut secret_excluded_paths: FxHashSet<String> = FxHashSet::default();
    hunks.retain(|h| {
        let Some(rel) = rel_path_string(root_dir, Path::new(&*h.path)) else {
            return true;
        };
        if source.admitted(&rel) == Admitted::Withheld(Why::Secret) {
            secret_excluded_paths.insert(rel);
            false
        } else {
            true
        }
    });
    secret_excluded_paths
}

/// gitignore-excluded changed files are listed by path; `.diffctx/ignore` is a
/// declared confidentiality policy, so its exclusions surface as a count only
/// — re-publishing the very paths the user asked to withhold would undo the
/// policy (#85), while total silence misreads as "the diff did not touch this"
/// (#188). Counted in files, not hunks: the writer says "N changed file(s)
/// withheld", and a multi-hunk withheld file must not inflate it.
fn ignore_disclosure(
    root_dir: &Path,
    source: &Source,
    hunks: &[crate::types::DiffHunk],
) -> (Vec<String>, FxHashSet<String>) {
    let mut ignored_display: Vec<String> = Vec::new();
    let mut policy_excluded_paths: FxHashSet<String> = FxHashSet::default();
    for h in hunks {
        let p = Path::new(&*h.path);
        let Some(rel) = rel_path_string(root_dir, p) else {
            continue;
        };
        match source.admitted(&rel) {
            Admitted::Withheld(Why::Gitignore) => ignored_display.push(rel),
            Admitted::Withheld(Why::Policy | Why::Secret) => {
                policy_excluded_paths.insert(rel);
            }
            Admitted::Yes => {}
        }
    }
    ignored_display.sort();
    ignored_display.dedup();
    (ignored_display, policy_excluded_paths)
}

/// The changed paths git reports for a diff that yields no text hunk at all —
/// a mode flip, a submodule pointer, a `-diff` attribute — minus everything
/// the run withholds. Listed so the artifact says "changed, unrepresented"
/// where it used to say nothing.
/// What the range's head no longer holds: a deleted path and a rename's
/// old name. Their bodies exist only at the base, so none of them is the
/// change's content; `--full` once showed a deleted file's old body as
/// changed (#409).
fn gone_at_head(root_dir: &Path, names: &diff_parse::DiffNames) -> FxHashSet<PathBuf> {
    names
        .deleted
        .iter()
        .cloned()
        .chain(names.renamed_sources(root_dir))
        .collect()
}

fn is_gone(gone: &FxHashSet<PathBuf>, file: &Path) -> bool {
    gone.contains(&dunce::canonicalize(file).unwrap_or_else(|_| file.to_path_buf()))
}

fn hunkless_changed_files(
    root_dir: &Path,
    source: &Source,
    diff_range: Option<&str>,
    pathspec: &[String],
    untracked_files: Vec<PathBuf>,
    run: Option<&crate::resource::RunContext>,
) -> Result<Vec<PathBuf>> {
    let names = unless_cut_short(run, diff_parse::diff_names(root_dir, diff_range, pathspec))?;
    let excluded = gone_at_head(root_dir, &names);
    let mut changed = names.changed;
    changed.extend(untracked_files);
    if changed.is_empty() {
        return Ok(changed);
    }
    changed.retain(|f| {
        !is_gone(&excluded, f) && !is_lockfile_path(f) && !is_withheld(root_dir, source, f)
    });
    changed.sort();
    changed.dedup();
    Ok(changed)
}

fn resolve_change_set(
    root_dir: &Path,
    diff_range: Option<&str>,
    staged: Option<&snapshot::StagedSnapshot>,
    pathspec: &[String],
    is_working_tree_diff: bool,
    run: &crate::resource::RunContext,
    t_entry: Instant,
) -> Result<ChangeSet> {
    let source = run.source();
    let (mut hunks, untracked_files) = collect_hunks_with_untracked(
        root_dir,
        diff_range,
        pathspec,
        is_working_tree_diff,
        &source,
        Some(run),
    )?;
    let secret_excluded_paths = withhold_secret_hunks(root_dir, &source, &mut hunks);

    if hunks.is_empty() {
        return Ok(ChangeSet::Empty {
            changed_files: hunkless_changed_files(
                root_dir,
                &source,
                diff_range,
                pathspec,
                untracked_files.clone(),
                Some(run),
            )?,
            untracked_files,
            lockfile_changes: Vec::new(),
            ignored_changes: Vec::new(),
            policy_excluded_count: secret_excluded_paths.len(),
        });
    }

    // gitignore-excluded changed files are listed by path; `.diffctx/ignore`
    // is a declared confidentiality policy, so its exclusions surface as a
    // count only — re-publishing the very paths the user asked to withhold
    // would undo the policy (#85), while total silence misreads as "the diff
    // did not touch this" (#188).
    let (ignored_display, policy_excluded_paths) = ignore_disclosure(root_dir, &source, &hunks);
    let policy_excluded = policy_excluded_paths.len() + secret_excluded_paths.len();
    hunks.retain(|h| is_admitted(root_dir, &source, Path::new(&*h.path)));

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
            changed_files: hunkless_changed_files(
                root_dir,
                &source,
                diff_range,
                pathspec,
                untracked_files.clone(),
                Some(run),
            )?,
            untracked_files,
            lockfile_changes: lockfile_display,
            ignored_changes: ignored_display,
            policy_excluded_count: policy_excluded,
        });
    }

    let diff_text = diff_parse::get_diff_text(root_dir, diff_range, pathspec)?;

    let names = diff_parse::diff_names(root_dir, diff_range, pathspec)?;
    let mut changed_files = names.changed.clone();
    changed_files.extend(untracked_files.iter().cloned());
    if changed_files.is_empty() {
        return Ok(ChangeSet::Empty {
            changed_files: Vec::new(),
            untracked_files: Vec::new(),
            lockfile_changes: lockfile_display,
            ignored_changes: ignored_display,
            policy_excluded_count: policy_excluded,
        });
    }

    // Rename destinations exist on the head and stay candidates via the
    // changed set below, so seeds and discovery still find them.
    let excluded = gone_at_head(root_dir, &names);
    // Display lists for the output header: deletions and renames produce no
    // fragments, but silently omitting them misrepresents the diff (a
    // deletion-only commit used to render as a bare two-line skeleton).
    let (deleted_display, renamed_display) = deletion_rename_displays(root_dir, &source, &names);
    let changed_files: Vec<PathBuf> = changed_files
        .into_iter()
        .filter(|f| {
            !is_gone(&excluded, f) && !is_lockfile_path(f) && !is_withheld(root_dir, &source, f)
        })
        .collect();

    let (base_rev, head_rev) = diff_range
        .map(snapshot::split_diff_range)
        .unwrap_or((None, None));
    // A staged snapshot's head is a tree: it has content and no history. Its
    // commits are none, and the history channels anchor on the commit it
    // sits on.
    let head_rev_is_worktree = head_rev.is_none();
    let (head_rev, history_head) = match staged {
        Some(s) => (None, s.head.clone()),
        None => (head_rev.clone(), head_rev),
    };
    // A message git could not read is the run's error, or its deadline when
    // that is what cut the read: never a change with no title.
    let commit_message = match head_rev.as_deref() {
        Some(h) => unless_cut_short(Some(run), snapshot::commit_message(root_dir, h))?,
        None => String::new(),
    };
    let commit_message = commit_message
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string);

    // `A..B` names both ends; a bare `X` diffs X against the working tree,
    // whose commits are `X..HEAD`. Every commit of the range travels whole
    // (subject and body); `commit_message` stays the head's subject.
    let subject_range = match (staged, base_rev.as_deref(), head_rev.as_deref()) {
        (Some(_), _, _) => None,
        (None, Some(base), Some(head)) => Some((base.to_string(), head.to_string())),
        (None, None, None) => diff_range.map(|rev| (rev.to_string(), "HEAD".to_string())),
        _ => None,
    };
    let (commit_count, commit_messages) = match subject_range {
        Some((base, head)) => (
            unless_cut_short(Some(run), snapshot::commit_count(root_dir, &base, &head))?,
            unless_cut_short(
                Some(run),
                snapshot::commit_messages(
                    root_dir,
                    &base,
                    &head,
                    MAX_COMMIT_MESSAGES,
                    MAX_COMMIT_MESSAGE_CHARS,
                ),
            )?,
        ),
        None => (0, Vec::new()),
    };

    let pre_phase_ms = t_entry.elapsed().as_secs_f64() * 1000.0;
    Ok(ChangeSet::Ready(Box::new(ChangeSetData {
        commit_count,
        hunks,
        diff_text,
        changed_files,
        untracked_files,
        deleted_display,
        renamed_display,
        lockfile_display,
        ignored_display,
        policy_excluded,
        commit_message,
        commit_messages,
        // A bare revision (a resolved time window included) diffs against
        // the working tree: the commits after it are the range's own.
        history_rev: base_rev.clone().or_else(|| match (staged, diff_range) {
            (None, Some(rev)) if head_rev_is_worktree => Some(rev.to_string()),
            _ => history_head.clone(),
        }),
        head_rev: history_head,
        named_files: Vec::new(),
        pre_phase_ms,
    })))
}

/// A name a change could not anchor (#336): its definitions stand in for
/// the changed hunks, so the run that follows is impact's own — reverse
/// discovery from the definition, the call edges into it, the tests that
/// reach each caller.
pub enum Anchor<'a> {
    Diff(Option<&'a str>),
    /// A range already resolved to ids by its caller.
    Range(crate::snapshot::RangeSpec),
    Symbol(&'a str),
}

const MAX_SYMBOL_FILES: usize = 256;

/// A `--symbol` the caller can correct: a query that names nothing a
/// definition could be (`malformed`), or a name nothing defines.
#[derive(Debug)]
pub struct SymbolQueryError {
    pub message: String,
    pub malformed: bool,
    /// Files that spell a name nothing defines: the answer instead of the
    /// error (Forgejo #114).
    pub named: Option<crate::impact::SymbolNamed>,
    /// Definitions the name is a local of: the answer when no definition has
    /// it (a nested function that never leaves its function).
    pub(crate) locals: Vec<crate::impact::LocalOf>,
}

impl std::fmt::Display for SymbolQueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SymbolQueryError {}

/// A name every language spells without quoting.
fn is_identifier(segment: &str) -> bool {
    segment
        .chars()
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$')
        && segment
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '$' || c == '-')
}

fn malformed_symbol(query: &str) -> SymbolQueryError {
    SymbolQueryError {
        message: format!(
            "--symbol takes NAME, PATH:NAME or a qualified Owner.NAME (a name with spaces or punctuation goes in backticks), got {query:?}"
        ),
        malformed: true,
        named: None,
        locals: Vec::new(),
    }
}

/// The files naming a symbol that this build read as text: the definition may well
/// be in one of them, and without them an answer of "no definition found" reads as
/// "the name does not exist" (#454).
fn unanalysed_namers(
    root_dir: &Path,
    files: &[PathBuf],
    fragments: &[Fragment],
    run: &crate::resource::RunContext,
) -> Vec<crate::impact::UnanalysedFile> {
    let source = run.source();
    let mut unread = Vec::new();
    for file in files {
        let Some(rel) = rel_path_string(root_dir, file) else {
            continue;
        };
        let has_definition = fragments
            .iter()
            .any(|f| Path::new(f.path()) == file.as_path() && f.kind.is_definition_kind());
        let Some((reason, lines)) =
            crate::parsers::unanalysed_reason(&rel, has_definition, || source.read_bytes(file))
        else {
            continue;
        };
        unread.push(crate::impact::UnanalysedFile {
            language: crate::languages::get_language_for_file(&rel),
            path: rel,
            reason,
            lines,
        });
    }
    unread
}

/// What the files read as text are said to be, after "no definition found" on the
/// command line's error.
fn unanalysed_namers_text(unread: &[crate::impact::UnanalysedFile]) -> String {
    if unread.is_empty() {
        return String::new();
    }
    let mut groups: Vec<(String, Vec<&str>)> = Vec::new();
    for file in unread {
        let why = format!(
            "{}{}",
            file.language.map(|l| format!("{l}: ")).unwrap_or_default(),
            crate::parsers::unanalysed_text(file.reason)
        );
        match groups.iter_mut().find(|(w, _)| *w == why) {
            Some((_, paths)) => paths.push(&file.path),
            None => groups.push((why, vec![&file.path])),
        }
    }
    let groups: Vec<String> = groups
        .into_iter()
        .map(|(why, paths)| {
            let shown = paths.len().min(4);
            let more = if paths.len() > shown {
                format!(" and {} more", paths.len() - shown)
            } else {
                String::new()
            };
            format!("({why}) {}{more}", paths[..shown].join(", "))
        })
        .collect();
    format!(
        " in the analysed files; {} file(s) naming it were read as text, so a definition or caller there is not seen: {}",
        unread.len(),
        groups.join("; ")
    )
}

/// The facts an extractor states about a file; `None` for a language none reads.
fn local_facts(path: &str, text: &str) -> Option<crate::facts::BlobFacts> {
    static REGISTRY: std::sync::OnceLock<crate::facts::Registry> = std::sync::OnceLock::new();
    let name = Path::new(path).file_name()?.to_str()?;
    let language = crate::facts::Language::detect(
        &crate::snapshot::RepoPath::new(name).ok()?,
        text.as_bytes(),
    )?;
    let extractor = REGISTRY
        .get_or_init(crate::facts::lang::builtin)
        .get(language)?;
    Some(extractor.extract(text.as_bytes(), &crate::snapshot::RunBudget::default()))
}

/// A fragment spans whole lines, so a second declarator on them
/// (`const S = 1, T = 2`) has none of its own: the fragment of the first
/// stands for it, and the cell reads the definition from the facts.
fn shadowed_definitions<'f>(
    fragments: &'f [Fragment],
    found: &[&Fragment],
    name: &str,
    container: &str,
    run: &crate::resource::RunContext,
) -> Vec<&'f Fragment> {
    use crate::facts::Fact;
    let mut stand_ins: Vec<&Fragment> = Vec::new();
    let mut paths: Vec<&str> = fragments.iter().map(Fragment::path).collect();
    paths.sort_unstable();
    paths.dedup();
    for path in paths {
        let Some(text) = run
            .source()
            .read_to_string(Path::new(path))
            .filter(|text| text.contains(name))
        else {
            continue;
        };
        let Some(blob) = local_facts(path, &text) else {
            continue;
        };
        for fact in &blob.facts {
            let Fact::Definition(d) = fact else { continue };
            let joined = d
                .local
                .container
                .iter()
                .map(|n| n.as_str())
                .collect::<Vec<_>>()
                .join(".");
            if d.local.name.as_str() != name || joined != container {
                continue;
            }
            let stands = fragments.iter().find(|f| {
                f.path() == path
                    && (f.start_line(), f.end_line()) == d.span.lines
                    && f.symbol_name.as_deref().is_some_and(|s| s != name)
            });
            if let Some(f) =
                stands.filter(|f| !found.iter().chain(&stand_ins).any(|k| k.id == f.id))
            {
                stand_ins.push(f);
            }
        }
    }
    stand_ins
}

/// A fragment the fragmenter named `name` that the facts hold as a local of
/// a definition and as no definition.
fn is_local_fragment(fragment: &Fragment, name: &str, run: &crate::resource::RunContext) -> bool {
    use crate::facts::{Fact, LocalKind};
    let Some(text) = run.source().read_to_string(Path::new(fragment.path())) else {
        return false;
    };
    let Some(blob) = local_facts(fragment.path(), &text) else {
        return false;
    };
    let (from, to) = (fragment.start_line(), fragment.end_line());
    let defined = blob.facts.iter().any(|f| {
        matches!(f, Fact::Definition(d)
            if d.local.name.as_str() == name && d.span.lines.0 <= to && from <= d.span.lines.1)
    });
    !defined
        && blob.facts.iter().any(|f| {
            matches!(f, Fact::LocalBinding(l)
                if l.kind == LocalKind::Closure
                    && l.name.as_str() == name
                    && from <= l.site.lines.0
                    && l.site.lines.1 <= to)
        })
}

/// The definitions (and callbacks, outermost last) a scope sits in.
fn scope_chain(scope: &crate::facts::Scope) -> (Vec<&str>, Vec<&crate::facts::Scope>) {
    use crate::facts::Scope;
    let mut callbacks = Vec::new();
    let mut current = scope;
    loop {
        match current {
            Scope::Callback { parent, .. } => {
                callbacks.push(current);
                current = parent;
            }
            Scope::Def(key) => {
                let path = key
                    .container
                    .iter()
                    .chain([&key.name])
                    .map(|n| n.as_str())
                    .collect();
                return (path, callbacks);
            }
            Scope::Module => return (Vec::new(), callbacks),
        }
    }
}

/// Every definition that `name` is a local of (a nested function, class or
/// closure that never leaves it), with each use of the name inside that
/// definition: the answer to a `--symbol` no definition has. A use is a plain
/// reference to the name, in the scope of the local or in a definition
/// nested in it, that no parameter or local of the same name hides.
fn local_uses(
    root_dir: &Path,
    files: &[PathBuf],
    name: &str,
    run: &crate::resource::RunContext,
) -> Vec<crate::impact::LocalOf> {
    use crate::facts::{Fact, LocalKind, Receiver};
    let mut found: Vec<crate::impact::LocalOf> = Vec::new();
    for file in files {
        let Some(text) = run.source().read_to_string(file) else {
            continue;
        };
        let display = crate::paths::display_rel_or_abs(root_dir, file);
        let Some(blob) = local_facts(&display, &text) else {
            continue;
        };
        let lines: Vec<&str> = text.split('\n').collect();
        let indent = |line: u32| {
            lines
                .get(line as usize - 1)
                .map_or(0, |l| l.len() - l.trim_start().len())
        };
        let locals: Vec<&crate::facts::Local> = blob
            .facts
            .iter()
            .filter_map(|f| match f {
                Fact::LocalBinding(l) => Some(l),
                _ => None,
            })
            .collect();
        for local in locals
            .iter()
            .filter(|l| l.kind == LocalKind::Closure && l.name.as_str() == name)
        {
            let (home, home_callbacks) = scope_chain(&local.enclosing);
            let enclosing = if home.is_empty() {
                display.clone()
            } else {
                format!("{display}::{}", home.join("."))
            };
            let mut uses: Vec<crate::impact::LocalUse> = Vec::new();
            for r in blob.facts.iter().filter_map(|f| match f {
                Fact::Reference(r) => Some(r),
                _ => None,
            }) {
                if r.name.as_str() != name
                    || !r.qualifier.is_empty()
                    || matches!(r.receiver, Receiver::SelfType)
                {
                    continue;
                }
                let (at, at_callbacks) = scope_chain(&r.enclosing);
                let inside = at.starts_with(&home)
                    && (at.len() > home.len()
                        || home_callbacks.iter().all(|c| at_callbacks.contains(c)));
                let hidden = locals.iter().any(|l2| {
                    l2.name.as_str() == name && l2.site != local.site && {
                        let (own, _) = scope_chain(&l2.enclosing);
                        own.len() > home.len() && at.starts_with(&own)
                    }
                });
                if !inside || hidden {
                    continue;
                }
                let line = r.site.lines.0;
                // The inner local a use sits in: the nearest local declared
                // above it whose body, by indentation, still holds the line.
                let inner = if at == home {
                    locals
                        .iter()
                        .filter(|l2| {
                            l2.kind == LocalKind::Closure
                                && l2.site.lines.0 < line
                                && scope_chain(&l2.enclosing).0 == home
                                && {
                                    let base = indent(l2.site.lines.0);
                                    indent(line) > base
                                        && (l2.site.lines.0 + 1..line).all(|n| {
                                            lines.get(n as usize - 1).is_none_or(|l| {
                                                l.trim().is_empty() || indent(n) > base
                                            })
                                        })
                                }
                        })
                        .max_by_key(|l2| l2.site.lines.0)
                        .map(|l2| l2.name.as_str().to_string())
                } else {
                    None
                };
                uses.push(crate::impact::LocalUse {
                    inside: inner.or_else(|| at.last().map(|s| s.to_string())),
                    line,
                    call: matches!(r.use_kind, crate::facts::UseKind::Call),
                });
            }
            uses.sort_by_key(|u| u.line);
            found.push(crate::impact::LocalOf {
                path: display.clone(),
                name: name.to_string(),
                enclosing,
                line: local.site.lines.0,
                uses,
            });
        }
    }
    found
}

fn resolve_symbol_change_set(
    root_dir: &Path,
    scope: &[String],
    query: &str,
    run: &crate::resource::RunContext,
    t_entry: Instant,
) -> Result<ChangeSet> {
    let Some((path, segments)) = crate::impact::parse_symbol_query(query) else {
        return Err(malformed_symbol(query).into());
    };
    let full = segments.join(".");
    let name = *segments.last().expect("split yields one segment");
    let qualifier = segments.len().checked_sub(2).map(|i| segments[i]);
    // `PATH` is read from the path the run was given, as a shell user reads
    // it: `cd src && diffctx . --symbol a.rs:build` names `src/a.rs`, and
    // `diffctx sub/ --symbol x.py:f` stays inside `sub/` (#410).
    let pathspec: Vec<String> = match path {
        Some(p) => {
            let p = crate::paths::to_posix_display(std::borrow::Cow::Borrowed(p));
            if scope.is_empty() {
                vec![p]
            } else {
                scope
                    .iter()
                    .map(|s| format!("{}/{p}", s.trim_end_matches('/')))
                    .collect()
            }
        }
        None => scope.to_vec(),
    };
    let named = grep::grep_files_with_word(root_dir, &run.source(), name, &pathspec)?;
    let withheld = named.withheld;
    let mut files = named.rows;
    files.retain(|f| !is_lockfile_path(f));
    let named_in = files.len();
    if files.len() > MAX_SYMBOL_FILES {
        // The files that spell a declaration of the name go first, so the
        // cap drops callers, never the definition the answer starts from.
        let declaring: FxHashSet<PathBuf> =
            grep::grep_files_with_declaration(root_dir, &run.source(), name, &pathspec)
                .map(|g| g.rows)
                .unwrap_or_default()
                .into_iter()
                .collect();
        files.sort_by_key(|f| !declaring.contains(f));
        files.truncate(MAX_SYMBOL_FILES);
        run.note(crate::resource::LimitReason::CandidateLimit);
    }

    let mut seen: FxHashSet<FragmentId> = FxHashSet::default();
    let mut fragments =
        crate::fragmentation::process_files_for_fragments(&files, root_dir, &mut seen, true, run);
    crate::overlay::overlay_definitions(&mut fragments, &run.source());
    let indent = |f: &Fragment| {
        f.content
            .lines()
            .find(|l| !l.trim().is_empty() && !l.trim_start().starts_with('@'))
            .map_or(0, |l| l.len() - l.trim_start().len())
    };
    // The class a definition sits in: the innermost one spanning it, else
    // (a class fragmented as its header alone) the nearest shallower header
    // above it. Any shallower class anywhere above answered `Cart.total`
    // with `Ledger.total`.
    let owner_of = |def: &Fragment| -> Option<&Fragment> {
        let above = || {
            fragments.iter().filter(|c| {
                c.kind.is_container()
                    && c.path() == def.path()
                    && c.start_line() <= def.start_line()
            })
        };
        above()
            .filter(|c| c.end_line() >= def.end_line())
            .max_by_key(|c| c.start_line())
            .or_else(|| {
                above()
                    .filter(|c| indent(c) < indent(def))
                    .max_by_key(|c| c.start_line())
            })
    };
    // Kotlin states its owners and receivers in the facts: `Obj.f`,
    // `C.Companion.f` and an extension's `String.pad` are read there, not
    // guessed from indentation. An extension's receiver is the last of its
    // container, set apart by the `ImplOf` disambiguator.
    let kotlin_owned = |def: &Fragment, owner: &str| -> Option<bool> {
        use crate::facts::lang::kotlin::KotlinExtractor;
        use crate::facts::{Disambig, Extractor as _, Fact};
        if crate::languages::get_language_for_file(def.path()) != Some("kotlin") {
            return None;
        }
        let text = run.source().read_to_string(Path::new(def.path()))?;
        let blob = KotlinExtractor.extract(text.as_bytes(), &crate::snapshot::RunBudget::default());
        let (name, line) = (def.symbol_name.as_deref()?, def.start_line());
        let decl = blob
            .facts
            .iter()
            .filter_map(|fact| match fact {
                Fact::Definition(d)
                    if d.local.name.as_str() == name
                        && d.span.lines.0 <= line
                        && line <= d.span.lines.1 =>
                {
                    Some(d)
                }
                _ => None,
            })
            .min_by_key(|d| d.span.bytes.len())?;
        let container = &decl.local.container;
        let receiver = match &decl.local.disambig {
            Some(Disambig::ImplOf(receiver)) => Some(receiver.as_str()),
            _ => None,
        };
        let owners = &container[..container.len() - usize::from(receiver.is_some())];
        Some(owners.last().map(|o| o.as_str()) == Some(owner) || receiver == Some(owner))
    };
    let owned_by = |def: &Fragment, owner: &str| {
        if let Some(owned) = kotlin_owned(def, owner).or_else(|| {
            crate::overlay::stated_owner(def.path(), def.start_line(), owner, &run.source())
        }) {
            return owned;
        }
        Path::new(def.path())
            .file_stem()
            .is_some_and(|stem| stem.to_string_lossy() == owner)
            || owner_of(def).and_then(|c| c.symbol_name.as_deref()) == Some(owner)
    };
    // What diff mode names as a changed definition: a module-level `const`
    // or `let` is one, though its kind is a variable's (#420).
    let is_definition = |f: &&Fragment| {
        crate::container::named_container_of(f, &fragments).is_some_and(|c| c.id == f.id)
    };
    let mut definitions: Vec<&Fragment> = fragments
        .iter()
        .filter(|f| f.symbol_name.as_deref() == Some(full.as_str()))
        .filter(is_definition)
        .collect();
    if definitions.is_empty() {
        definitions = fragments
            .iter()
            .filter(|f| f.symbol_name.as_deref() == Some(name))
            .filter(is_definition)
            .filter(|f| qualifier.is_none_or(|q| owned_by(f, q)))
            .collect();
    }
    // A name the facts layer holds only as a local of a definition is no
    // definition, whatever the fragmenter named.
    definitions.retain(|f| !is_local_fragment(f, name, run));
    let container = segments[..segments.len() - 1].join(".");
    for stand_in in shadowed_definitions(&fragments, &definitions, name, &container, run) {
        definitions.push(stand_in);
    }
    if definitions.is_empty() {
        // Written without backticks, a name that is no identifier is a query to
        // correct, as it was before such names were looked up.
        if !query.contains('`') && !segments.iter().all(|s| is_identifier(s)) {
            return Err(malformed_symbol(query).into());
        }
        if segments.len() == 1 {
            let locals = local_uses(root_dir, &files, name, run);
            if !locals.is_empty() {
                return Err(SymbolQueryError {
                    message: format!("{name:?} is a local of a definition"),
                    malformed: false,
                    named: None,
                    locals,
                }
                .into());
            }
        }
        let capped = if named_in > MAX_SYMBOL_FILES {
            format!(" (searched {MAX_SYMBOL_FILES} of the {named_in} files naming it)")
        } else {
            String::new()
        };
        // A run that stopped looking has not found there is none.
        let stops = run.stops();
        let unread_files = unanalysed_namers(root_dir, &files, &fragments, run);
        let unread = format!(
            "{}{}",
            unanalysed_namers_text(&unread_files),
            if stops.is_empty() {
                String::new()
            } else {
                format!(
                    " (the run stopped ({}) before it finished looking: a definition may exist)",
                    crate::impact::reason_names(&stops)
                )
            }
        );
        // `Cart.total` names an owner's definition: the word `total` spelled
        // elsewhere is no answer to it.
        let named = (segments.len() == 1 && !files.is_empty())
            .then(|| grep::grep_word_first_lines(root_dir, &run.source(), name, &pathspec).ok())
            .flatten()
            .and_then(|found| {
                let lines = found
                    .rows
                    .into_iter()
                    .filter(|(p, _, _)| !is_lockfile_path(Path::new(p)))
                    .collect();
                crate::impact::symbol_named(
                    root_dir,
                    &run.source(),
                    lines,
                    unread_files,
                    crate::resource::classify_limits(run, &[], &|_| false).0,
                )
            });
        return Err(SymbolQueryError {
            message: format!(
                "no definition of {full:?} found{}{capped}{}",
                path.map(|p| format!(" in {p}")).unwrap_or_default(),
                unread
            ),
            malformed: false,
            named,
            locals: Vec::new(),
        }
        .into());
    }

    let mut hunks = Vec::with_capacity(definitions.len());
    let mut diff_text = String::new();
    let mut changed_files: Vec<PathBuf> = Vec::new();
    for def in &definitions {
        let (start, len) = (def.start_line(), def.line_count());
        hunks.push(crate::types::DiffHunk {
            path: Arc::from(def.path()),
            new_start: start,
            new_len: len,
            old_start: start,
            old_len: len,
        });
        let display = crate::paths::display_rel_or_abs(root_dir, Path::new(def.path()));
        let _ = write!(
            diff_text,
            "diff --git a/{display} b/{display}\n--- a/{display}\n+++ b/{display}\n@@ -{start},{len} +{start},{len} @@\n"
        );
        for line in def.content.lines() {
            diff_text.push('+');
            diff_text.push_str(line);
            diff_text.push('\n');
        }
        let file = PathBuf::from(def.path());
        if !changed_files.contains(&file) {
            changed_files.push(file);
        }
    }
    let named_files = files
        .into_iter()
        .filter(|f| !changed_files.contains(f))
        .collect();

    Ok(ChangeSet::Ready(Box::new(ChangeSetData {
        commit_count: 0,
        hunks,
        diff_text,
        changed_files,
        untracked_files: Vec::new(),
        deleted_display: Vec::new(),
        renamed_display: Vec::new(),
        lockfile_display: Vec::new(),
        ignored_display: Vec::new(),
        policy_excluded: withheld.len(),
        commit_message: None,
        commit_messages: Vec::new(),
        head_rev: None,
        history_rev: None,
        named_files,
        pre_phase_ms: t_entry.elapsed().as_secs_f64() * 1000.0,
    })))
}

/// Where the run looks: the repository root, and the pathspecs that narrow
/// it. Explicit `paths` are the scope as given; with none, a `root_dir`
/// below the repository root narrows the run to that subtree.
fn resolve_scope(root_dir: &Path, paths: &[String]) -> Result<(PathBuf, Vec<String>)> {
    let requested = dunce::canonicalize(root_dir).unwrap_or_else(|_| root_dir.to_path_buf());
    let repo_root = resolve_repo_root(&requested)?;
    if !paths.is_empty() {
        let mut scope = Vec::with_capacity(paths.len());
        for p in paths {
            let given = Path::new(p);
            let rel = if given.is_absolute() {
                let canon = dunce::canonicalize(given).unwrap_or_else(|_| given.to_path_buf());
                canon
                    .strip_prefix(&repo_root)
                    .map(|r| crate::paths::to_posix_display(r.to_string_lossy()))
                    .map_err(|_| snapshot::SnapshotError::InvalidPath(p.clone()))?
            } else {
                crate::paths::to_posix_display(std::borrow::Cow::Borrowed(p.as_str()))
            };
            let rel = rel.trim_end_matches('/').to_string();
            if rel.is_empty() || rel == "." {
                return Ok((repo_root, Vec::new()));
            }
            scope.push(rel);
        }
        snapshot::validate_pathspec(&scope)?;
        return Ok((repo_root, scope));
    }
    let subtree = requested
        .strip_prefix(&repo_root)
        .ok()
        .map(|r| crate::paths::to_posix_display(r.to_string_lossy()))
        .filter(|r| !r.is_empty());
    Ok((repo_root, subtree.into_iter().collect()))
}

pub fn compute_scored_state(
    root_dir: &Path,
    diff_range: Option<&str>,
    paths: &[String],
    alpha: f64,
    scoring_mode: ScoringMode,
    timeout: u64,
) -> Result<ScoredState> {
    compute_scored_state_anchored(
        root_dir,
        Anchor::Diff(diff_range),
        paths,
        alpha,
        scoring_mode,
        timeout,
        false,
    )
}

/// The range a diff anchor resolves to (a duration window becomes a
/// revision range) and the change set either anchor yields.
fn resolve_anchor(
    root_dir: &Path,
    anchor: Anchor<'_>,
    scope: &[String],
    run: &crate::resource::RunContext,
    t_entry: Instant,
) -> Result<(Option<String>, Option<snapshot::StagedSnapshot>, ChangeSet)> {
    let (opened, symbol) = match anchor {
        Anchor::Diff(diff_range) => (
            crate::source::Analysis::open(root_dir, diff_range, run),
            None,
        ),
        Anchor::Range(range) => (
            crate::source::Analysis::open_range(root_dir, range, run),
            None,
        ),
        Anchor::Symbol(query) => (
            crate::source::Analysis::at_working_tree(root_dir, run),
            Some(query),
        ),
    };
    let analysis = match opened {
        Ok(analysis) => analysis,
        // The deadline or a cancellation cut the snapshot's listing
        // short: nothing was read, so the answer is the partial one
        // that names the limit, never a clean tree (a `--symbol` run
        // included: no definition was looked for, none is absent).
        Err(e) if e.is_interruption() => {
            run.note(crate::resource::reason_for(&e));
            return Ok((
                None,
                None,
                ChangeSet::Empty {
                    changed_files: Vec::new(),
                    untracked_files: Vec::new(),
                    lockfile_changes: Vec::new(),
                    ignored_changes: Vec::new(),
                    policy_excluded_count: 0,
                },
            ));
        }
        Err(e) => return Err(e.into()),
    };
    run.set_source(analysis.head());
    if let Some(query) = symbol {
        return Ok((
            None,
            None,
            resolve_symbol_change_set(root_dir, scope, query, run, t_entry)?,
        ));
    }
    let range = analysis.range();
    let change_set = resolve_change_set(
        root_dir,
        range,
        analysis.staged(),
        scope,
        analysis.is_working_tree_diff(),
        run,
        t_entry,
    )?;
    Ok((
        range.map(str::to_string),
        analysis.staged().cloned(),
        change_set,
    ))
}

/// `reverse_references` widens discovery to the files that name what the
/// change defines. Impact mode alone asks for it: the wider universe moves
/// selection, and it is deliberately outside `PipelineConfig` so the
/// effective-config hash every evaluation row carries stays what it was.
#[allow(clippy::too_many_arguments)]
pub fn compute_scored_state_anchored(
    root_dir: &Path,
    anchor: Anchor<'_>,
    paths: &[String],
    alpha: f64,
    scoring_mode: ScoringMode,
    timeout: u64,
    reverse_references: bool,
) -> Result<ScoredState> {
    let t_entry = Instant::now();
    crate::effective_config::enforce_strict_env()?;
    crate::tokenizer::warm();
    snapshot::set_git_timeout(timeout);
    let run = crate::resource::RunContext::for_run(timeout);
    let _in_run = run.enter();
    let (root_dir, scope) = resolve_scope(root_dir, paths)?;
    // `!(a > 0 && a < 1)` rather than `a <= 0 || a >= 1`: every comparison
    // against NaN is false, so the negated form is the one that rejects it.
    // The old form let NaN through into the PPR damping factor, where it
    // silently turned every score into NaN and the run returned an ordering
    // that meant nothing.
    if !(alpha > 0.0 && alpha < 1.0) {
        anyhow::bail!("alpha must be in (0, 1), got {}", alpha);
    }

    let (resolved_range, staged, change_set) =
        resolve_anchor(&root_dir, anchor, &scope, &run, t_entry)?;
    let index_tree = staged.map(|s| s.tree);
    let diff_range = resolved_range.as_deref();

    let data = match change_set {
        ChangeSet::Empty {
            changed_files,
            untracked_files,
            lockfile_changes,
            ignored_changes,
            policy_excluded_count,
        } => {
            crate::profile::record("pre_phase", t_entry.elapsed().as_secs_f64() * 1000.0);
            let mut state =
                empty_scored_state_with_changes(root_dir, diff_range, &scope, timeout, run)?;
            state.change_classes = classify_changes(&state.root_dir, &changed_files, &[], &[], &[]);
            state.changed_files = changed_files;
            state.untracked_files = untracked_files;
            state.lockfile_changes = lockfile_changes;
            state.ignored_changes = ignored_changes;
            state.policy_excluded_count = policy_excluded_count;
            state.index_tree = index_tree;
            state.analysed_range = resolved_range.clone();
            state.envelope_tokens = envelope_tokens_of(&state);
            return Ok(state);
        }
        ChangeSet::Ready(data) => data,
    };
    let ChangeSetData {
        commit_count,
        hunks,
        diff_text,
        changed_files,
        untracked_files,
        deleted_display,
        renamed_display,
        lockfile_display,
        ignored_display,
        policy_excluded,
        commit_message,
        commit_messages,
        head_rev,
        history_rev,
        named_files,
        pre_phase_ms,
    } = *data;
    run.set_head_rev(history_rev);
    run.set_changed_lines(crate::changed_lines::ChangedLines::from_diff(&diff_text));

    let t0 = Instant::now();

    let mut seen_frag_ids: FxHashSet<FragmentId> = FxHashSet::default();
    let mut all_fragments = crate::fragmentation::fragment_files(
        &changed_files,
        &root_dir,
        &mut seen_frag_ids,
        true,
        &run,
        &hunks,
    );

    // The raw diff still carries the hunks the run withholds; the texts keep
    // only the files analysed, or impact would name what `.diffctx/ignore`
    // asked to keep out (a removed definition, its path).
    let analysed: FxHashSet<String> = hunks
        .iter()
        .filter_map(|h| rel_path_string(&root_dir, Path::new(&*h.path)))
        .collect();
    let mut hunk_texts: Vec<_> = crate::changed_lines::hunk_texts(&diff_text)
        .into_iter()
        .filter(|h| analysed.contains(&h.path))
        .collect();
    settle_layout(&root_dir, &mut hunk_texts, &run);
    let change_classes = classify_changes(
        &root_dir,
        &changed_files,
        &hunks,
        &hunk_texts,
        &all_fragments,
    );

    let t_parse_changed = Instant::now();

    let included_set: FxHashSet<PathBuf> =
        changed_files.iter().chain(&named_files).cloned().collect();
    // Past the deadline the changed files are the whole universe: no walk,
    // no discovery, a graph over the change alone — a valid, partial artifact
    // instead of an abort.
    let mut all_candidate_files = if run.check() {
        candidate_files::collect_candidate_files(&root_dir, &included_set, &scope, &run)
    } else {
        Vec::new()
    };
    if all_candidate_files.len() > run.budget().max_candidate_files {
        all_candidate_files.truncate(run.budget().max_candidate_files);
        run.note(crate::resource::LimitReason::CandidateLimit);
    }
    run.record_usage(|u| u.candidate_files = all_candidate_files.len() as u64);

    let t_universe = Instant::now();

    let file_cache = build_file_cache(&all_candidate_files, &run);
    let mode = scoring_mode;
    let mut config = PipelineConfig::from_mode(mode);
    config.ppr_alpha = alpha;
    let provenance = crate::run_provenance::RunProvenance::new(
        &root_dir,
        diff_range,
        crate::effective_config::EffectiveConfigV1::resolve(&config, timeout),
    )?;

    let mut expansion_concepts: FxHashSet<String> =
        crate::types::extract_identifiers(&diff_text, TOKENIZATION.query_min_identifier_length)
            .into_iter()
            .collect();

    if std::env::var("DIFFCTX_NO_COMMIT_SIGNAL").as_deref() != Ok("1") {
        // Every message in the range, not the last one alone: a bot commit
        // on top of a person's work must not be the only query signal (#263).
        let mut signal: Vec<String> = commit_messages.clone();
        if signal.is_empty() {
            if let Some(ref h) = head_rev {
                signal.push(unless_cut_short(
                    Some(&run),
                    snapshot::commit_message(&root_dir, h),
                )?);
            }
        }
        for text in &signal {
            for ident in
                crate::types::extract_identifiers(text, TOKENIZATION.query_min_identifier_length)
            {
                expansion_concepts.insert(ident);
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
        source: run.source(),
        run: run.clone(),
    };

    let (discovered_files, discovery_attribution) =
        create_discovery(&config, reverse_references).discover_attributed(&discovery_ctx);
    // `--symbol` read every file naming the symbol under the changed-file
    // cap to find its definitions. Its callers are in those files, so they
    // are read the same way: under the candidate cap a 3,000-line module
    // that subclasses it was only "not fully analysed" (#542).
    let named_files: Vec<PathBuf> = named_files
        .iter()
        .map(|p| candidate_files::normalize_path(p, &root_dir))
        .collect();
    let discovered_files: Vec<PathBuf> = discovered_files
        .into_iter()
        .map(|p| candidate_files::normalize_path(&p, &root_dir))
        .filter(|p| !named_files.contains(p))
        .collect();
    let discovery_source: FxHashMap<Arc<str>, &'static str> = discovery_attribution
        .into_iter()
        .map(|(path, source)| {
            let normalized = candidate_files::normalize_path(&path, &root_dir);
            (Arc::from(normalized.to_string_lossy().as_ref()), source)
        })
        .collect();

    drop(discovery_ctx);
    // The walk, the cache and every strategy end at the deadline: whatever
    // stopped them, the universe the selection saw was cut short.
    if run.tripped() {
        run.note(crate::resource::LimitReason::DiscoveryTruncated);
    }

    let t_discovery = Instant::now();

    all_fragments.extend(process_files_for_fragments(
        &named_files,
        &root_dir,
        &mut seen_frag_ids,
        true,
        &run,
    ));
    all_fragments.extend(process_files_for_fragments(
        &discovered_files,
        &root_dir,
        &mut seen_frag_ids,
        false,
        &run,
    ));
    if reverse_references {
        crate::overlay::overlay_definitions(&mut all_fragments, &run.source());
    }

    let t_parse_discovered = Instant::now();

    let discovered_path_set: FxHashSet<Arc<str>> = discovered_files
        .iter()
        .chain(&named_files)
        .map(|p| Arc::from(p.to_string_lossy().as_ref()))
        .collect();
    let ScoredFragments {
        all_fragments,
        core_ids,
        core_excerpts,
        scoring_result,
        needs,
        tokenization_ms,
        changed_lines,
    } = score_from_fragments(
        all_fragments,
        &hunks,
        &diff_text,
        &config,
        Some(root_dir.as_path()),
        &discovered_path_set,
        &run,
    );
    let t_tokenization = t_parse_discovered + Duration::from_secs_f64(tokenization_ms / 1000.0);

    let t_done = Instant::now();

    if scoring_result.graph.cap_stats.edges_dropped_by_cap > 0 {
        run.note(crate::resource::LimitReason::EdgeLimit);
    }

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

    for (name, ms) in [
        ("pre_phase", heavy_latency_ms.pre_phase),
        ("parse_changed", heavy_latency_ms.parse_changed),
        ("universe_walk", heavy_latency_ms.universe_walk),
        ("discovery", heavy_latency_ms.discovery),
        ("parse_discovered", heavy_latency_ms.parse_discovered),
        ("tokenization", heavy_latency_ms.tokenization),
        ("graph_build", heavy_latency_ms.graph_build),
        ("scoring", heavy_latency_ms.scoring),
    ] {
        crate::profile::record(name, ms);
    }
    let usage = run.usage();
    for (name, n) in [
        ("candidate_files", usage.candidate_files),
        ("fragments", all_fragments.len() as u64),
        ("edges", usage.final_edges),
    ] {
        crate::profile::count(name, n);
    }

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

    let mut state = ScoredState {
        commit_count,
        root_dir,
        config,
        all_fragments,
        core_ids,
        core_excerpts,
        scoring_result,
        needs,
        discovery_source,
        changed_files,
        untracked_files,
        deleted_files: deleted_display,
        renamed_files: renamed_display,
        lockfile_changes: lockfile_display,
        ignored_changes: ignored_display,
        policy_excluded_count: policy_excluded,
        commit_message,
        commit_messages,
        change_classes,
        heavy_latency_ms,
        provenance,
        run,
        envelope_tokens: 0,
        index_tree,
        analysed_range: resolved_range.clone(),
        hunk_texts,
        changed_lines,
        scope,
    };
    state.envelope_tokens = envelope_tokens_of(&state);
    Ok(state)
}

const MAX_COMMIT_MESSAGES: usize = 20;
const MAX_COMMIT_MESSAGE_CHARS: usize = 2_000;

/// Which runs only a formatter could have written, each changed file read
/// as the fragmenter reads it: in chunks, the reads in order and the parses
/// in parallel. Past the deadline a file is judged by its lines alone.
fn settle_layout(
    root_dir: &Path,
    hunk_texts: &mut [crate::changed_lines::HunkText],
    run: &crate::resource::RunContext,
) {
    let source = run.source();
    let mut files: Vec<&mut [crate::changed_lines::HunkText]> =
        hunk_texts.chunk_by_mut(|a, b| a.path == b.path).collect();
    for chunk in files.chunks_mut(rayon::current_num_threads().max(1)) {
        let texts: Vec<Option<String>> = chunk
            .iter()
            .map(|runs| {
                run.check()
                    .then(|| {
                        crate::fragmentation::changed_text(
                            &root_dir.join(&runs[0].path),
                            root_dir,
                            &source,
                        )
                    })
                    .flatten()
            })
            .collect();
        chunk
            .par_iter_mut()
            .zip(texts.par_iter())
            .for_each(|(runs, text)| crate::change_class::settle_layout(runs, text.as_deref()));
    }
}

/// The class of every changed file, keyed by its display path. Generated
/// files are told by their header, which the first fragment of the file
/// carries; the rest by the shape of their hunks and changed lines.
pub(crate) fn classify_changes(
    root_dir: &Path,
    changed_files: &[PathBuf],
    hunks: &[crate::types::DiffHunk],
    hunk_texts: &[crate::changed_lines::HunkText],
    fragments: &[Fragment],
) -> Vec<(String, crate::change_class::ChangeClass, &'static str)> {
    // A file is layout-only when every run of it is: lines that only moved
    // from one run to another are a reorder, not a reformat.
    let mut sides_by_file: FxHashMap<&str, (Vec<String>, Vec<String>, bool)> = FxHashMap::default();
    for h in hunk_texts {
        let (removed, added, layout) = sides_by_file
            .entry(h.path.as_str())
            .or_insert_with(|| (Vec::new(), Vec::new(), true));
        removed.extend(h.removed.iter().cloned());
        added.extend(h.added.iter().cloned());
        *layout &= h.layout;
    }
    let mut head_by_path: FxHashMap<&str, &Fragment> = FxHashMap::default();
    for f in fragments {
        let entry = head_by_path.entry(f.path()).or_insert(f);
        if f.start_line() < entry.start_line() {
            *entry = f;
        }
    }
    changed_files
        .iter()
        .map(|path| {
            let display = crate::paths::display_rel_or_abs(root_dir, path);
            let key = path.to_string_lossy();
            let file_hunks: Vec<&crate::types::DiffHunk> = hunks
                .iter()
                .filter(|h| h.path.as_ref() == key.as_ref())
                .collect();
            let generated = head_by_path
                .get(key.as_ref())
                .is_some_and(|f| crate::fragmentation::is_generated_file(path, &f.content));
            let (removed, added, runs_layout) = sides_by_file
                .get(display.as_str())
                .map_or((&[][..], &[][..], false), |(r, a, l)| {
                    (r.as_slice(), a.as_slice(), *l)
                });
            let lines: Vec<String> = removed.iter().chain(added).cloned().collect();
            let layout = !lines.is_empty() && runs_layout;
            let (class, reason) =
                crate::change_class::classify(&file_hunks, &lines, generated, layout);
            (display, class, reason)
        })
        .collect()
}

/// The heavy phase from fragments onward: token counts, cores and their
/// stand-ins, signature variants, seed weights, scoring, information needs.
pub struct ScoredFragments {
    pub all_fragments: Vec<Fragment>,
    pub core_ids: FxHashSet<FragmentId>,
    pub core_excerpts: FxHashMap<FragmentId, Fragment>,
    pub scoring_result: ScoringResult,
    pub needs: Vec<InformationNeed>,
    pub tokenization_ms: f64,
    /// Changed lines per core fragment: how much of it the change touched,
    /// which orders the changed code when the budget cannot hold all of it.
    pub changed_lines: FxHashMap<FragmentId, f64>,
}

/// One heavy phase for every caller. The product pipeline arrives here from
/// git and discovery; the corpus harness from an in-memory repository. Both
/// used to spell these steps out separately, which is how the harness once
/// measured a system nobody runs (#149) and why the fork survived a first
/// consolidation of the selection half (#232). `repo_root` is `None` for the
/// harness — the strategies that need a filesystem do without one there.
pub fn score_from_fragments(
    mut all_fragments: Vec<Fragment>,
    hunks: &[crate::types::DiffHunk],
    diff_text: &str,
    config: &PipelineConfig,
    repo_root: Option<&Path>,
    discovered_paths: &FxHashSet<Arc<str>>,
    run: &crate::resource::RunContext,
) -> ScoredFragments {
    let t0 = Instant::now();
    assign_token_counts(&mut all_fragments);

    let core_ids = identify_core_fragments(hunks, &all_fragments);

    let mut core_excerpts =
        crate::excerpt::generate_core_excerpts(&all_fragments, &core_ids, hunks);
    assign_excerpt_token_counts(&mut core_excerpts);

    let mut sig_frags = generate_signature_variants(&all_fragments);
    assign_token_counts(&mut sig_frags);
    all_fragments.extend(sig_frags);

    let tokenization_ms = t0.elapsed().as_secs_f64() * 1000.0;

    let seed_weights = compute_seed_weights(hunks, &core_ids, &all_fragments);
    let changed_lines = compute_changed_lines(hunks, &core_ids, &all_fragments);

    let strategy = create_scoring_strategy(config);
    let scoring_result = strategy.score_and_filter(
        &all_fragments,
        &core_ids,
        hunks,
        repo_root,
        Some(&seed_weights),
        Some(discovered_paths),
        run,
    );

    let mut needs = crate::utility::needs::needs_from_diff(&all_fragments, &core_ids, diff_text);
    if needs.len() > run.budget().max_needs {
        // Highest priority first, then a total order, so the cap keeps the
        // same needs on every machine.
        needs.sort_by(|a, b| {
            b.priority
                .total_cmp(&a.priority)
                .then_with(|| a.need_type.cmp(&b.need_type))
                .then_with(|| a.symbol.cmp(&b.symbol))
        });
        needs.truncate(run.budget().max_needs);
        run.note(crate::resource::LimitReason::NeedLimit);
    }

    ScoredFragments {
        all_fragments,
        core_ids,
        core_excerpts,
        scoring_result,
        needs,
        tokenization_ms,
        changed_lines,
    }
}

/// Tokens one inventory row (`changes[]`) costs beyond its path.
const INVENTORY_ENTRY_TOKENS: u32 = 8;

/// Every subject stays; bodies are kept, newest first, while the whole list
/// fits `cap_tokens`. A range's commit bodies are the reader's "why", but
/// twenty GitOps commits of prose must not crowd the change out of a small
/// budget.
pub(crate) fn trim_commit_messages(messages: &[String], cap_tokens: u32) -> Vec<String> {
    let subjects: Vec<String> = messages
        .iter()
        .map(|m| m.lines().next().unwrap_or("").trim().to_string())
        .collect();
    let mut trimmed = subjects.clone();
    let mut used: u32 = subjects.iter().map(|s| count_tokens(s) + 1).sum();
    for (i, message) in messages.iter().enumerate() {
        if message.trim() == subjects[i] {
            continue;
        }
        let extra = count_tokens(message).saturating_sub(count_tokens(&subjects[i]));
        if used + extra > cap_tokens {
            break;
        }
        used += extra;
        trimmed[i] = message.trim().to_string();
    }
    trimmed
}

/// Every changed file's evidence tier, keyed by the path its fragments carry.
pub(crate) fn evidence_priority_of(
    changed_files: &[PathBuf],
    change_classes: &[(String, crate::change_class::ChangeClass, &'static str)],
) -> FxHashMap<Arc<str>, u8> {
    changed_files
        .iter()
        .zip(change_classes)
        .map(|(path, (display, class, _))| {
            (
                Arc::from(path.to_string_lossy().as_ref()),
                evidence_tier(display, *class),
            )
        })
        .collect()
}

/// The order changed files earn budget in when not all of them fit, and the
/// renderer cuts them in reverse: production code (0), then its tests,
/// then config and docs, then mechanical or layout-only edits, then
/// generated files (4). A test stub printed whole while the migration it
/// exercises was omitted is the inversion (#311). Read on the repository
/// path: an absolute one under a `tests/` directory made every file a test.
pub(crate) fn evidence_tier(repo_path: &str, class: crate::change_class::ChangeClass) -> u8 {
    use crate::change_class::ChangeClass;
    let path = Path::new(repo_path);
    let ext = crate::languages::file_ext(path);
    match class {
        ChangeClass::Generated => 4,
        ChangeClass::Mechanical | ChangeClass::Layout => 3,
        _ if crate::testfiles::is_test_path(path) => 1,
        _ if crate::config::extensions::CONFIG_EXTENSIONS.contains(ext.as_str())
            || matches!(ext.as_str(), ".md" | ".rst" | ".txt" | ".adoc") =>
        {
            2
        }
        _ => 0,
    }
}

/// What `select_and_postpass` hands back: the selection plus the facts about
/// how it was made that the renderers and provenance report.
pub struct PostpassOutcome {
    pub selected: Vec<Fragment>,
    pub selection_iters: usize,
    pub stopping_certificate: f64,
    pub stand_in_ids: FxHashSet<FragmentId>,
    pub tau: f64,
    pub gated: bool,
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
    tau: Option<f64>,
    evidence_priority: &FxHashMap<Arc<str>, u8>,
    changed_lines: &FxHashMap<FragmentId, f64>,
) -> PostpassOutcome {
    // A scorer with no admission gate (BM25 builds no graph, so it has no
    // declared-related set to gate on) is bounded by the threshold alone, so
    // an unspecified tau resolves to the ungated operating point instead of
    // the default. Which is why tau travels as an `Option` all the way from
    // the CLI and the Python bridge: it used to arrive as a bare `f64` and the
    // raise was triggered by COMPARING it to the default, so an ungated sweep
    // cell asking for exactly 0.12 was silently given 0.30 and had no way to
    // ask for the value it had named. The effective value is logged: an
    // ablation cell that does not say whether it ran gated is not comparable
    // with one that did (#245).
    let ungated = scoring_result.admissible_files.is_none();
    let tau = match tau {
        Some(requested) => requested,
        None if ungated => crate::config::limits::UNGATED_STOPPING_THRESHOLD,
        None => crate::config::limits::DEFAULT_STOPPING_THRESHOLD,
    };
    tracing::info!(
        "diffctx: selection gate={} tau={:.4}",
        if ungated { "none" } else { "admission" },
        tau
    );
    let trace = std::env::var_os("DIFFCTX_TRACE_BUILDERS").is_some();
    let t_stage = Instant::now();
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
                Some(evidence_priority),
                Some(changed_lines),
            )
        }
    };

    if trace {
        eprintln!("selection greedy: {:.1}s", t_stage.elapsed().as_secs_f64());
    }
    let t_stage = Instant::now();
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

    if trace {
        eprintln!(
            "selection coherence: {:.1}s",
            t_stage.elapsed().as_secs_f64()
        );
    }
    let t_stage = Instant::now();
    postpass::rescue_nontrivial_context(
        &mut selected,
        all_fragments,
        &scoring_result.rel_scores,
        core_ids,
        effective_budget,
        scoring_result.admissible_files.as_ref(),
    );
    if trace {
        eprintln!("selection rescue: {:.1}s", t_stage.elapsed().as_secs_f64());
    }

    PostpassOutcome {
        selected,
        selection_iters,
        stopping_certificate,
        stand_in_ids,
        tau,
        gated: !ungated,
    }
}

/// What the change summary costs before a single fragment is selected.
///
/// `--budget` is documented as a cap on the output in three places, and until
/// #241 it bounded only the fragments: the changed-file list, the
/// deleted/renamed/lockfile/ignored lists and the commit message rendered for
/// free. On an 83-file range that envelope was ~1.3k tokens and `--budget 0`
/// still emitted 2.4k — the selection dutifully under budget while the artifact
/// was three times over it. Charging the envelope first is what makes the
/// number mean what the docs claim.
///
/// Shared by the product pipeline and the corpus harness on purpose: a budget
/// the harness spends differently is a harness measuring a system nobody runs
/// (#149).
pub fn envelope_tokens_of(state: &ScoredState) -> u32 {
    let mut listed: Vec<String> = state
        .changed_files
        .iter()
        .map(|p| crate::paths::display_rel_or_abs(&state.root_dir, p))
        .collect();
    listed.extend(state.deleted_files.iter().cloned());
    listed.extend(state.lockfile_changes.iter().cloned());
    listed.extend(state.ignored_changes.iter().cloned());
    listed.extend(
        state
            .renamed_files
            .iter()
            .map(|(from, to)| format!("{from} {to}")),
    );
    envelope_token_cost(state.commit_message.as_deref(), &listed)
}

pub fn envelope_token_cost(commit_message: Option<&str>, listed_paths: &[String]) -> u32 {
    let mut preview = String::new();
    if let Some(msg) = commit_message {
        preview.push_str(msg);
        preview.push('\n');
    }
    for line in listed_paths {
        preview.push_str(line);
        preview.push('\n');
    }
    count_tokens(&preview)
        + (listed_paths.len() as u32) * BUDGET.envelope_overhead_per_entry
        + BUDGET.envelope_overhead
}

/// Selection + the 3 post-passes, shared verbatim by the pack renderer
/// (`select_with_params`) and the locate renderer — extracting it is pure
/// code motion so both modes select identically by construction.
pub fn run_selection(
    state: &ScoredState,
    budget_tokens: Option<u32>,
    tau: Option<f64>,
) -> SelectionOutcome {
    let _selection = crate::profile::phase("selection");
    let t_start = Instant::now();
    // Selection runs after the heavy phase returned, often on another thread
    // (the Python bridge re-runs it per cell): publish the run context again
    // so the greedy can poll the same deadline.
    let _in_run = state.run.enter();
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

    // Beyond the path lists the artifact carries an inventory row per
    // changed file and the range's commit messages, the latter bounded to a
    // tenth of the budget (subjects always, bodies while they fit). Both are
    // charged here, per outcome, since the bound follows the budget.
    let commit_messages = trim_commit_messages(&state.commit_messages, effective_budget / 10);
    let messages_extra = count_tokens(&commit_messages.join("\n")).saturating_sub(
        state
            .commit_message
            .as_deref()
            .map(count_tokens)
            .unwrap_or(0),
    );
    let inventory = state.changed_files.len() as u32 * INVENTORY_ENTRY_TOKENS;
    let selection_budget =
        effective_budget.saturating_sub(state.envelope_tokens + messages_extra + inventory);

    let evidence_priority = evidence_priority_of(&state.changed_files, &state.change_classes);
    let PostpassOutcome {
        mut selected,
        selection_iters,
        stopping_certificate,
        mut stand_in_ids,
        tau: tau_effective,
        gated,
    } = select_and_postpass(
        &state.scoring_result,
        &state.all_fragments,
        &state.core_ids,
        &state.needs,
        &state.core_excerpts,
        state.config.objective,
        selection_budget,
        tau,
        &evidence_priority,
        &state.changed_lines,
    );

    let used: u32 = selected.iter().map(|f| f.token_count).sum();
    let remaining = selection_budget.saturating_sub(used);
    let t_ensure = Instant::now();
    postpass::ensure_changed_files_represented(
        &mut selected,
        &state.all_fragments,
        &state.changed_files,
        remaining,
        &state.root_dir,
        &state.run.source(),
        &state.core_ids,
        &state.core_excerpts,
        &stand_in_ids,
    );
    if std::env::var_os("DIFFCTX_TRACE_BUILDERS").is_some() {
        eprintln!(
            "selection ensure_changed: {:.1}s",
            t_ensure.elapsed().as_secs_f64()
        );
    }

    crate::provenance::maybe_dump(state, &selected);

    // A clipped witness is a stand-in the floor made on the spot; the
    // renderer learns it carries the change from this set.
    stand_in_ids.extend(
        selected
            .iter()
            .filter(|f| f.kind == crate::types::FragmentKind::Excerpt)
            .map(|f| f.id.clone()),
    );
    let represented: FxHashSet<&str> = selected.iter().map(|f| f.id.path.as_ref()).collect();
    let fragmented: FxHashSet<&str> = state.all_fragments.iter().map(|f| f.path()).collect();
    let mut limit_reasons = Vec::new();
    if state.changed_files.iter().any(|p| {
        let key = p.to_string_lossy();
        fragmented.contains(key.as_ref()) && !represented.contains(key.as_ref())
    }) {
        limit_reasons.push(crate::resource::LimitReason::EvidenceBudgetExceeded);
    }

    let select_ms = t_start.elapsed().as_secs_f64() * 1000.0;
    SelectionOutcome {
        limit_reasons,
        commit_messages,
        selected,
        effective_budget,
        selection_budget,
        selection_iters,
        stopping_certificate,
        select_ms,
        stand_in_ids,
        budget_requested: budget_tokens,
        tau: tau_effective,
        gated,
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
    tau: Option<f64>,
    no_content: bool,
) -> DiffContextOutput {
    let outcome = run_selection(state, budget_tokens, tau);
    let selection_provenance = outcome.selection_provenance();
    let fit = crate::answer::envelope::fit_of(outcome.budget_requested, outcome.effective_budget);
    let selection_limits = outcome.limit_reasons.clone();
    let commit_messages = outcome.commit_messages.clone();
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
        range_spec: state.provenance.range.clone(),
        index_tree: state.index_tree.clone(),
        commit_count: state.commit_count,
        commit_message: state.commit_message.clone(),
        commit_messages,
        changes: state.change_classes.clone(),
        fragmentless: fragmentless_changed_files(
            &state.root_dir,
            &state.changed_files,
            &state.all_fragments,
        ),
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
    output.provenance = Some(render::provenance_value(
        &state.provenance.finish(Some(selection_provenance)),
    ));
    output.fit = Some(fit);
    let mut limits = selection_limits;
    if output.redactions.is_some() {
        limits.push(crate::resource::LimitReason::SanitizationRedaction);
    }
    let about = answer_paths(state, selected.iter());
    output.coverage =
        crate::resource::CoverageReport::build(&state.run, &limits, &|p| about.contains(p));
    output
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

/// Whether a file may be shown: not secret by name, not inside git's own
/// directory, not excluded by `.gitignore` or `.diffctx/ignore` — one
/// admission, read where the run's snapshot is read (#507, #153).
pub(crate) fn is_admitted(root_dir: &Path, source: &Source, path: &Path) -> bool {
    rel_path_string(root_dir, path).is_none_or(|rel| source.admitted(&rel) == Admitted::Yes)
}

pub(crate) fn is_withheld(root_dir: &Path, source: &Source, path: &Path) -> bool {
    !is_admitted(root_dir, source, path)
}

/// Whether `path` is inside a repository, asked of git once per run: `Err`
/// when git could not answer, which is no verdict on the directory.
fn is_git_repo(path: &Path) -> snapshot::Result<bool> {
    crate::resource::memoized("is_git_repo", path, || snapshot::is_git_repo(path))
}

/// Which of `rel_paths` (repo-root-relative) the engine withholds — the
/// question the MCP fetch used to answer with a different engine (#228). A
/// directory that is no repository has no ignore rules to read, and the
/// secret names still apply; a repository whose policy cannot be read
/// withholds everything rather than publish what it asked to keep out.
pub fn withheld_paths(root_dir: &Path, rel_paths: &[String]) -> Vec<String> {
    let run = crate::resource::RunContext::unbounded();
    match crate::source::Analysis::at_working_tree(root_dir, &run) {
        Ok(analysis) => analysis.head().withheld_among(rel_paths),
        Err(_) if !is_git_repo(root_dir).unwrap_or(true) => rel_paths
            .iter()
            .filter(|rel| crate::source::is_secret_path(Path::new(rel)))
            .cloned()
            .collect(),
        Err(e) => {
            tracing::error!(
                "could not read the admission policy of {} ({e}); all {} queried paths are treated \
                 as withheld rather than risk publishing them",
                root_dir.display(),
                rel_paths.len()
            );
            rel_paths.to_vec()
        }
    }
}

/// The unified diff of `diff_range` as git prints it, minus the file sections
/// diff mode never discloses: secret-like paths, ignored paths, and lock files
/// (#112). Additive output for `--with-raw-diff` (#150) — it feeds no
/// selection state, so selection is bit-identical with and without it.
pub fn raw_diff_text(
    root_dir: &Path,
    diff_range: Option<&str>,
    paths: &[String],
    timeout: u64,
) -> Result<(String, crate::sanitize::Redactions)> {
    snapshot::set_git_timeout(timeout);
    let (root_dir, scope) = resolve_scope(root_dir, paths)?;
    let run = crate::resource::RunContext::for_run(timeout);
    let analysis = crate::source::Analysis::open(&root_dir, diff_range, &run)?;
    let diff_text = diff_parse::get_diff_text(&root_dir, analysis.range(), &scope)?;
    let mut disclosable = keep_disclosable_sections(&root_dir, &analysis.head(), &diff_text);
    let redactions = crate::sanitize::sanitize_in_place(&mut disclosable);
    Ok((disclosable, redactions))
}

fn keep_disclosable_sections(root_dir: &Path, source: &Source, diff_text: &str) -> String {
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

    let mut kept: Vec<&str> = Vec::new();
    for (path, range) in sections {
        // A section whose path cannot be resolved inside the repository is
        // dropped: the bundle must never widen what diff mode is willing to
        // show, and an unattributable section cannot be policy-checked.
        let Some(path) = path else {
            continue;
        };
        if is_lockfile_path(&path) || is_withheld(root_dir, source, &path) {
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
        match diff_parse::parse_path_line(line, root_dir) {
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
        return diff_parse::resolve_in_repo(root_dir, &unquote_c_style(quoted.trim()));
    }
    let rest = section.first()?.strip_prefix("diff --git ")?;
    let rel_path = rest.get("a/".len()..rest.find(" b/")?)?;
    if rest != format!("a/{rel_path} b/{rel_path}") {
        return None;
    }
    diff_parse::resolve_in_repo(root_dir, rel_path)
}

fn resolve_repo_root(root_dir: &Path) -> Result<PathBuf> {
    let root_dir = dunce::canonicalize(root_dir).unwrap_or_else(|e| {
        tracing::debug!("canonicalize failed for '{}': {}", root_dir.display(), e);
        root_dir.to_path_buf()
    });
    if !is_git_repo(&root_dir)? {
        return Err(snapshot::SnapshotError::NotARepo(root_dir).into());
    }
    // A repository with no work tree (a bare one) is read from where it was named.
    Ok(snapshot::find_toplevel(&root_dir)?.unwrap_or(root_dir))
}

/// What `--full` removed from its own answer, carried to whichever of the four
/// exits the run takes so no exit can forget to disclose it.
struct FullWithheld {
    policy_excluded_count: usize,
    ignored_changes: Vec<String>,
}

fn full_empty_output(
    root_dir: &Path,
    source: &Source,
    diff_range: Option<&str>,
    pathspec: &[String],
    changed_files: &[PathBuf],
    withheld: &FullWithheld,
) -> Result<DiffContextOutput> {
    let (deleted, renamed) = deletion_rename_displays(
        root_dir,
        source,
        &diff_parse::diff_names(root_dir, diff_range, pathspec)?,
    );
    let mut output = empty_output(root_dir);
    output.range = crate::answer::envelope::range_of(diff_range, None, 0, Vec::new());
    output.inventory = Inventory::new(
        Vec::new(),
        deleted,
        renamed,
        Vec::new(),
        withheld.ignored_changes.clone(),
        withheld.policy_excluded_count,
    );
    list_unrepresented_changes(
        &mut output,
        &classify_changes(root_dir, changed_files, &[], &[], &[]),
        &fragmentless_changed_files(root_dir, changed_files, &[]),
    );
    Ok(output)
}

/// The inventory of an output that selected nothing: every changed path, each
/// row saying it is not represented.
fn list_unrepresented_changes(
    output: &mut DiffContextOutput,
    change_classes: &[(String, crate::change_class::ChangeClass, &'static str)],
    fragmentless: &FxHashSet<String>,
) {
    output.inventory.changed = Listing::of(
        change_classes.iter().map(|(p, _, _)| p.clone()).collect(),
        0,
    );
    output.changes = change_classes
        .iter()
        .map(|(path, class, _)| render::ChangeEntry {
            path: path.clone(),
            class: *class,
            represented: false,
            no_fragments: fragmentless.contains(path),
        })
        .collect();
}

fn build_diff_context_full(
    root_dir: &Path,
    diff_range: Option<&str>,
    paths: &[String],
    no_content: bool,
    timeout: u64,
) -> Result<DiffContextOutput> {
    snapshot::set_git_timeout(timeout);
    let (root_dir, scope) = resolve_scope(root_dir, paths)?;
    // Reading the changed files is bounded like every other run: a deadline
    // that cuts it is named, not reported as files over the size cap (#439).
    let run = crate::resource::RunContext::for_run(timeout);
    let analysis = crate::source::Analysis::open(&root_dir, diff_range, &run)?;
    let source = analysis.head();
    run.set_source(source.clone());
    let diff_range = analysis.range();
    let (mut hunks, untracked_files) = collect_hunks_with_untracked(
        &root_dir,
        diff_range,
        &scope,
        analysis.is_working_tree_diff(),
        &source,
        None,
    )?;
    let secret_excluded_paths = withhold_secret_hunks(&root_dir, &source, &mut hunks);
    // What `--full` withholds, it must still own up to. It used to drop secret
    // and policy-excluded paths in silence, so a run that had removed files
    // from its answer was indistinguishable from one with nothing to remove —
    // the same misreading ("the diff did not touch this") that #188 fixed for
    // the default mode.
    let mut withheld = FullWithheld {
        policy_excluded_count: secret_excluded_paths.len(),
        ignored_changes: Vec::new(),
    };
    if hunks.is_empty() {
        let listed = hunkless_changed_files(
            &root_dir,
            &source,
            diff_range,
            &scope,
            untracked_files,
            None,
        )?;
        return full_empty_output(&root_dir, &source, diff_range, &scope, &listed, &withheld);
    }
    let (ignored_display, policy_excluded_paths) = ignore_disclosure(&root_dir, &source, &hunks);
    withheld.ignored_changes = ignored_display;
    withheld.policy_excluded_count += policy_excluded_paths.len();
    hunks.retain(|h| is_admitted(&root_dir, &source, Path::new(&*h.path)));
    if hunks.is_empty() {
        let listed = hunkless_changed_files(
            &root_dir,
            &source,
            diff_range,
            &scope,
            untracked_files,
            None,
        )?;
        return full_empty_output(&root_dir, &source, diff_range, &scope, &listed, &withheld);
    }
    let names = diff_parse::diff_names(&root_dir, diff_range, &scope)?;
    let mut changed_files = names.changed.clone();
    changed_files.extend(untracked_files);
    let gone = gone_at_head(&root_dir, &names);
    changed_files.retain(|f| !is_gone(&gone, f) && !is_withheld(&root_dir, &source, f));
    changed_files.dedup();
    if changed_files.is_empty() {
        return full_empty_output(&root_dir, &source, diff_range, &scope, &[], &withheld);
    }
    let head_rev = diff_range.and_then(|range| snapshot::split_diff_range(range).1);
    let mut seen_frag_ids: FxHashSet<FragmentId> = FxHashSet::default();
    let _in_run = run.enter();
    let mut all_fragments = crate::fragmentation::fragment_files(
        &changed_files,
        &root_dir,
        &mut seen_frag_ids,
        true,
        &run,
        &hunks,
    );
    assign_token_counts(&mut all_fragments);
    let mut sig_frags = generate_signature_variants(&all_fragments);
    assign_token_counts(&mut sig_frags);
    all_fragments.extend(sig_frags);
    changed_files.sort();
    let core_ids = identify_core_fragments(&hunks, &all_fragments);
    let selected = select_full_mode(&all_fragments, &changed_files);
    let commit_message = match head_rev.as_deref() {
        Some(h) => unless_cut_short(Some(&run), snapshot::commit_message(&root_dir, h))?,
        None => String::new(),
    };
    let commit_message = commit_message
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string);
    let (deleted_display, renamed_display) = deletion_rename_displays(&root_dir, &source, &names);
    let change = render::ChangeSummary {
        range_spec: diff_range.map(str::to_string),
        index_tree: None,
        commit_count: 0,
        commit_message,
        commit_messages: Vec::new(),
        changes: classify_changes(&root_dir, &changed_files, &hunks, &[], &all_fragments),
        fragmentless: fragmentless_changed_files(&root_dir, &changed_files, &all_fragments),
        changed_files: changed_files
            .iter()
            .map(|p| crate::paths::display_rel_or_abs(&root_dir, p))
            .collect(),
        deleted_files: deleted_display,
        renamed_files: renamed_display,
        // `--full` is the escape hatch that promises every fragment of the
        // changed files, so it keeps lockfile content instead of diverting it.
        lockfile_changes: Vec::new(),
        ignored_changes: withheld.ignored_changes,
        policy_excluded_count: withheld.policy_excluded_count,
    };
    let changed_display: FxHashSet<String> = change.changed_files.iter().cloned().collect();
    let mut output = render::build_diff_context_output(
        &root_dir,
        &selected,
        no_content,
        &core_ids,
        // `--full` emits whole files: nothing is substituted for a core.
        &FxHashSet::default(),
        &FxHashMap::default(),
        change,
    );
    output.coverage =
        crate::resource::CoverageReport::build(&run, &[], &|p| changed_display.contains(p));
    Ok(output)
}

fn deletion_rename_displays(
    root_dir: &Path,
    source: &Source,
    names: &diff_parse::DiffNames,
) -> (Vec<String>, Vec<(String, String)>) {
    let mut deleted: Vec<String> = names
        .deleted
        .iter()
        .map(|p| crate::paths::display_rel_or_abs(root_dir, p))
        .collect();
    deleted.sort();
    let mut renamed = names.rename_pairs();
    // A deletion or rename of a withheld file is no exception to withholding
    // it: the header would otherwise publish the path the policy hides.
    let named: Vec<String> = deleted
        .iter()
        .cloned()
        .chain(renamed.iter().flat_map(|(a, b)| [a.clone(), b.clone()]))
        .collect();
    let hidden: FxHashSet<String> = source.withheld(&named).into_iter().collect();
    if !hidden.is_empty() {
        deleted.retain(|p| !hidden.contains(p));
        renamed.retain(|(a, b)| !hidden.contains(a) && !hidden.contains(b));
    }
    (deleted, renamed)
}

/// Keeps the run that listed the change, not a fresh one: what it noted on
/// the way (a deadline that cut a listing short) is what the empty artifact
/// must still report.
fn empty_scored_state_with_changes(
    root_dir: PathBuf,
    diff_range: Option<&str>,
    pathspec: &[String],
    timeout: u64,
    run: crate::resource::RunContext,
) -> Result<ScoredState> {
    let names = unless_cut_short(
        Some(&run),
        diff_parse::diff_names(&root_dir, diff_range, pathspec),
    )?;
    let (deleted, renamed) = deletion_rename_displays(&root_dir, &run.source(), &names);
    let mut state = empty_scored_state(root_dir, diff_range, timeout, run)?;
    state.deleted_files = deleted;
    state.renamed_files = renamed;
    state.envelope_tokens = envelope_tokens_of(&state);
    Ok(state)
}

fn empty_scored_state(
    root_dir: PathBuf,
    diff_range: Option<&str>,
    timeout: u64,
    run: crate::resource::RunContext,
) -> Result<ScoredState> {
    let config = PipelineConfig::from_mode(ScoringMode::Ego);
    let effective = crate::effective_config::EffectiveConfigV1::resolve(&config, timeout);
    let provenance =
        match crate::run_provenance::RunProvenance::new(&root_dir, diff_range, effective.clone()) {
            Ok(provenance) => provenance,
            // The deadline stopped the run before its revisions were read: the
            // artifact is the partial one that names the limit, its record says no
            // revision was resolved.
            Err(e) if e.is_interruption() => {
                run.note(crate::resource::reason_for(&e));
                crate::run_provenance::RunProvenance::without_revisions(diff_range, effective)
            }
            Err(e) => return Err(e.into()),
        };
    Ok(ScoredState {
        commit_count: 0,
        root_dir,
        config,
        provenance,
        run,
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
        untracked_files: Vec::new(),
        deleted_files: Vec::new(),
        renamed_files: Vec::new(),
        commit_message: None,
        commit_messages: Vec::new(),
        change_classes: Vec::new(),
        heavy_latency_ms: HeavyLatencyMs::default(),
        envelope_tokens: 0,
        index_tree: None,
        analysed_range: None,
        hunk_texts: Vec::new(),
        changed_lines: FxHashMap::default(),
        scope: Vec::new(),
    })
}

fn empty_output(root_dir: &Path) -> DiffContextOutput {
    let resolved = dunce::canonicalize(root_dir).unwrap_or_else(|_| root_dir.to_path_buf());
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
    output.range = crate::answer::envelope::range_of(
        state.provenance.range.as_deref(),
        state.index_tree.as_deref(),
        state.commit_count,
        crate::answer::envelope::messages_of(
            state.commit_message.clone(),
            state.commit_messages.clone(),
        ),
    );
    output.inventory = Inventory::new(
        Vec::new(),
        state.deleted_files.clone(),
        state.renamed_files.clone(),
        state.lockfile_changes.clone(),
        state.ignored_changes.clone(),
        state.policy_excluded_count,
    );
    list_unrepresented_changes(
        &mut output,
        &state.change_classes,
        &fragmentless_changed_files(&state.root_dir, &state.changed_files, &state.all_fragments),
    );
    output.provenance = Some(render::provenance_value(&state.provenance.finish(None)));
    let limits: &[crate::resource::LimitReason] = if output.redactions.is_some() {
        &[crate::resource::LimitReason::SanitizationRedaction]
    } else {
        &[]
    };
    let about = answer_paths(state, std::iter::empty());
    output.coverage =
        crate::resource::CoverageReport::build(&state.run, limits, &|p| about.contains(p));
    output
}

pub(crate) fn create_discovery(
    config: &PipelineConfig,
    reverse_references: bool,
) -> Box<dyn DiscoveryStrategy> {
    let mut strategies: Vec<Box<dyn DiscoveryStrategy>> = vec![
        Box::new(DefaultDiscovery),
        Box::new(TestFileDiscovery),
        Box::new(BM25Discovery::new(config.bm25_top_k)),
    ];
    if reverse_references {
        strategies.push(Box::new(ReverseReferenceDiscovery));
    }
    Box::new(EnsembleDiscovery::new(strategies))
}

fn build_file_cache(
    candidate_files: &[PathBuf],
    run: &crate::resource::RunContext,
) -> FxHashMap<PathBuf, String> {
    let source = run.source();
    // Stream files one at a time to avoid materialising all content before the cap.
    // Previous par_iter().collect() allocated the full eligible corpus into an
    // intermediate Vec before truncating — on repos with thousands of files this
    // caused peak memory far above max_cache_bytes.
    let mut sorted = candidate_files.to_vec();
    sorted.sort();
    let mut cache: FxHashMap<PathBuf, String> = FxHashMap::default();
    let mut cache_bytes = 0usize;
    for path in run.bounded(sorted) {
        if cache_bytes > GRAPH_FILTERING.max_cache_bytes {
            break;
        }
        if let Some(content) = source
            .read_to_string(&path)
            .filter(|content| content.len() <= LIMITS.max_file_size)
        {
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
    /// second, independent copy of the one in `diff_parse::parse_path_line`, carrying
    /// the same lexical-prefix hole; both now share `diff_parse::resolve_in_repo`, so
    /// this pins that a section naming a path outside the root is dropped
    /// rather than bundled.
    #[test]
    fn a_raw_diff_section_escaping_the_root_resolves_to_nothing() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let base = dunce::canonicalize(tmp.path()).expect("canonical tempdir");
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
    use crate::source::is_secret_path;
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
