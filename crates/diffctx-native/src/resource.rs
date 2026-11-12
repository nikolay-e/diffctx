//! The run's cooperative control surface: one wall-clock deadline, one set of
//! resource caps, one log of what limited the run.
//!
//! A deadline is an expected outcome, not an invariant violation, so it is
//! never a panic: every phase polls `RunContext::check`, stops what it is
//! doing at a bounded unit (a file, a builder, a push batch), and the run
//! goes on to produce a valid artifact that says it is partial. `catch_unwind`
//! survives only at the FFI boundary, for programming errors.
//!
//! Rayon cannot interrupt a closure that is already running, so the hot loops
//! inside edge builders poll the thread-local context published by `enter`
//! instead of threading a parameter through 51 `EdgeBuilder::build`
//! implementations — the same reason the old deadline lived in a thread-local.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Why a run did not do everything it could have. One vocabulary for every
/// phase; consumers read `coverage.limits` and never a per-subsystem
/// boolean.
#[derive(
    Serialize, Deserialize, JsonSchema, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum LimitReason {
    FileTooLarge,
    /// A file cut to `max_fragments_per_file`: what the diff touched survives
    /// the cut, the rest is the longest fragments only.
    FragmentLimit,
    /// A changed or context file that was not UTF-8 and was decoded lossily;
    /// `coverage.lossy_files` names them.
    NonUtf8Content,
    TotalByteLimit,
    CandidateLimit,
    EdgeContributionLimit,
    EdgeLimit,
    NeedLimit,
    DiscoveryTruncated,
    DiffusionTruncated,
    Deadline,
    /// The caller withdrew the request (an MCP cancellation, the MCP
    /// deadline): work stopped at the next poll instead of running on
    /// detached.
    Cancelled,
    /// A git read the answer depends on failed (`git grep`, a file
    /// listing): what it would have found is unknown, never none.
    ReadFailed,
    EvidenceBudgetExceeded,
    SelectionBudgetExceeded,
    SanitizationRedaction,
}

/// Test hook: the n-th blob read of every analysis fails on purpose, base and
/// head sides counted together, or one side's alone with `base:N` / `head:N`.
/// Semantic — it changes the artifact.
pub const TEST_READ_FAULT_ENV: &str = "DIFFCTX_TEST_READ_FAULT";

pub(crate) fn test_read_fault() -> Option<std::sync::Arc<crate::snapshot::ReadFault>> {
    crate::snapshot::ReadFault::parse(&std::env::var(TEST_READ_FAULT_ENV).ok()?)
}

/// Test hook: `<kind>:<n>` gives the n-th file, by path, of each cell's
/// interaction group one loss of that kind (`Loss::kind`, `Loss::samples`), as if
/// the run had lost it, so that a sweep can put every variant of `Loss` on every
/// family in turn. Semantic — it changes the artifact.
pub const TEST_LOSS_ENV: &str = "DIFFCTX_TEST_LOSS";

/// The kind and the place `TEST_LOSS_ENV` names; a malformed knob names none.
pub(crate) fn test_loss() -> Option<(String, usize)> {
    let spec = std::env::var(TEST_LOSS_ENV).ok()?;
    let (kind, at) = spec.rsplit_once(':')?;
    Some((kind.to_string(), at.parse().ok()?))
}

/// Every cap a run is held to. `max_wall_secs` is the `--timeout`; the rest
/// bound memory, which the wall clock alone never did — 38M edge
/// contributions took 11 GB before a single one was deduplicated (#196).
#[derive(Serialize, JsonSchema, Clone, Debug, PartialEq, Eq)]
pub struct ResourceBudget {
    pub max_wall_secs: u64,
    pub max_source_bytes: u64,
    pub max_file_bytes: usize,
    pub max_changed_file_bytes: usize,
    pub max_candidate_files: usize,
    pub max_fragments_per_file: usize,
    /// Edge emissions counted BEFORE deduplication: the number that owns the
    /// memory, and the one a builder cannot see past its own output.
    pub max_edge_contributions: u64,
    pub max_out_edges_per_node: usize,
    /// Information needs mined from the diff. Every candidate is scored
    /// against every need, so an unbounded need set from a repository-sized
    /// diff made selection quadratic (413 s on this repo's own history).
    pub max_needs: usize,
}

pub const DEFAULT_MAX_NEEDS: usize = 4_000;
pub const DEFAULT_MAX_SOURCE_BYTES: u64 = 256 * 1024 * 1024;
pub const DEFAULT_MAX_CANDIDATE_FILES: usize = 200_000;
pub const DEFAULT_MAX_EDGE_CONTRIBUTIONS: u64 = 20_000_000;

/// Test hook: the compute deadline is spent before the first poll, so every
/// phase takes its cooperative exit on a deterministic, tiny input. The git
/// ceiling is untouched (`set_git_timeout` reads the CLI value), so the run
/// still reaches the phases instead of failing in `rev-parse`. Semantic — it
/// changes the artifact — and hashed through `resources.max_wall_secs`.
pub const TEST_DEADLINE_EXPIRED_ENV: &str = "DIFFCTX_TEST_DEADLINE_EXPIRED";

/// Test hook: the deadline fires at the `n`th poll, whatever the clock says,
/// so a test can stop a run at every point it can stop without racing the
/// wall clock (#510, #489, #502). Semantic, like the expired deadline.
pub const TEST_DEADLINE_AFTER_POLLS_ENV: &str = "DIFFCTX_TEST_DEADLINE_AFTER_POLLS";

/// Test hook: the file a stalled child leaves when its output stops. A git
/// read that has waited empty gives up as timed out once the file exists,
/// whatever the clock says, so a stall test ends its run on the stall itself,
/// not on a short `--timeout` or a count of waits (fj#172). Semantic, like the
/// expired deadline.
pub const TEST_GIT_STALL_FILE_ENV: &str = "DIFFCTX_TEST_GIT_STALL_FILE";

pub(crate) fn test_git_stall_file() -> Option<std::path::PathBuf> {
    std::env::var_os(TEST_GIT_STALL_FILE_ENV).map(Into::into)
}

/// Test hook: the hook's one wall-clock ceiling is spent before its analysis
/// starts, as when an engine does not return in time, so a test reaches what
/// the hook says then without waiting for a clock. Semantic, like the deadline.
pub const TEST_HOOK_CEILING_EXPIRED_ENV: &str = "DIFFCTX_TEST_HOOK_CEILING_EXPIRED";

impl ResourceBudget {
    pub fn resolve(timeout_secs: u64) -> Self {
        let limits = &*crate::config::limits::LIMITS;
        let max_wall_secs = if std::env::var(TEST_DEADLINE_EXPIRED_ENV).as_deref() == Ok("1") {
            0
        } else {
            timeout_secs
        };
        Self {
            max_wall_secs,
            max_source_bytes: env_u64("DIFFCTX_MAX_SOURCE_BYTES", DEFAULT_MAX_SOURCE_BYTES),
            max_file_bytes: limits.max_file_size,
            max_changed_file_bytes: limits.max_changed_file_size,
            max_candidate_files: env_u64(
                "DIFFCTX_MAX_CANDIDATE_FILES",
                DEFAULT_MAX_CANDIDATE_FILES as u64,
            ) as usize,
            max_fragments_per_file: limits.max_fragments,
            max_edge_contributions: env_u64(
                "DIFFCTX_MAX_EDGE_CONTRIBUTIONS",
                DEFAULT_MAX_EDGE_CONTRIBUTIONS,
            ),
            max_out_edges_per_node: crate::graph::read_max_out_edges_per_node(),
            max_needs: env_u64("DIFFCTX_MAX_NEEDS", DEFAULT_MAX_NEEDS as u64) as usize,
        }
    }

    /// No ceiling of any kind: the in-memory corpus harness, the project
    /// graph, unit tests.
    pub fn unbounded() -> Self {
        Self {
            max_wall_secs: u64::MAX,
            max_source_bytes: u64::MAX,
            max_file_bytes: usize::MAX,
            max_changed_file_bytes: usize::MAX,
            max_candidate_files: usize::MAX,
            max_fragments_per_file: usize::MAX,
            max_edge_contributions: u64::MAX,
            max_out_edges_per_node: usize::MAX,
            max_needs: usize::MAX,
        }
    }
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|&v| v > 0)
        .unwrap_or(default)
}

/// What the run consumed, for the coverage block. Deterministic for a given
/// input and configuration (counts, not clocks).
#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, Default, PartialEq, Eq)]
pub struct ResourceUsage {
    pub source_bytes: u64,
    pub parsed_files: u64,
    pub candidate_files: u64,
    pub edge_contributions: u64,
    pub final_edges: u64,
}

/// A limit that touched one file, by its display path.
#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct LimitedFile {
    pub path: String,
    pub reason: LimitReason,
}

impl LimitReason {
    /// Loses context wherever it fires: the run as a whole stopped short.
    fn is_run_wide_loss(self) -> bool {
        matches!(
            self,
            Self::Deadline
                | Self::Cancelled
                | Self::ReadFailed
                | Self::DiscoveryTruncated
                | Self::CandidateLimit
                | Self::TotalByteLimit
                | Self::EdgeContributionLimit
                | Self::EvidenceBudgetExceeded
                | Self::SelectionBudgetExceeded
                | Self::SanitizationRedaction
        )
    }

    /// A run that noted it did not finish looking: what it printed as a
    /// count of what it found is a lower bound.
    pub fn stops_a_search(self) -> bool {
        matches!(self, Self::Deadline | Self::Cancelled | Self::ReadFailed)
    }

    /// Loses context only in the file it names.
    fn is_per_file(self) -> bool {
        matches!(
            self,
            Self::FileTooLarge | Self::FragmentLimit | Self::NonUtf8Content
        )
    }
}

const MAX_LIMITED_FILES_LISTED: usize = 20;

/// What a run's limits mean for the reader (#306). `limits` are the
/// ones that can have cost this answer context: a run-wide stop, or a
/// per-file limit on a file the answer is about (changed, or holding a
/// selected fragment). Graph caps every repository trips, and a size cap on
/// a lockfile nobody reads, go to `capped` and leave the status `complete`.
#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug)]
pub struct CoverageReport {
    pub status: CoverageStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limits: Vec<LimitReason>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capped: Vec<LimitReason>,
    /// The files behind the per-file reasons in `limits`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limited_files: Vec<LimitedFile>,
    pub resources: ResourceUsage,
    /// Fields of this block nothing measured. `resources` when the block
    /// was added after the run — a fit that cut the answer, a redaction in
    /// the raw diff — and its counts read 0 only because the schema requires
    /// numbers there (#550).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unknown: Vec<String>,
    /// Files that were not UTF-8 and were decoded with replacement
    /// characters (`NonUtf8Content`): their symbols and identifiers may be
    /// truncated at the first non-ASCII byte.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lossy_files: Vec<String>,
}

/// How much of what the run was about it covered: `degraded` when a changed file
/// has no witness in the answer, `partial` when a limit cost context, `complete`
/// when only caps nothing needed were hit.
#[derive(Serialize, Deserialize, JsonSchema, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CoverageStatus {
    Complete,
    Partial,
    Degraded,
}

impl CoverageStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Partial => "partial",
            Self::Degraded => "degraded",
        }
    }
}

/// `limits`, `capped` and the relevant `limited_files` of a run, by
/// the rule on [`CoverageReport`]. `extra` are the selection's own reasons.
pub fn classify_limits(
    ctx: &RunContext,
    extra: &[LimitReason],
    relevant: &dyn Fn(&str) -> bool,
) -> (Vec<LimitReason>, Vec<LimitReason>, Vec<LimitedFile>) {
    let mut reasons: BTreeSet<LimitReason> = ctx.reasons().into_iter().collect();
    reasons.extend(extra.iter().copied());
    let limited: Vec<LimitedFile> = ctx
        .limited_files()
        .into_iter()
        .filter(|f| relevant(&f.path))
        .collect();
    let (mut headline, mut capped) = (Vec::new(), Vec::new());
    for reason in reasons {
        let loses = reason.is_run_wide_loss()
            || (reason.is_per_file() && limited.iter().any(|f| f.reason == reason));
        if loses {
            headline.push(reason);
        } else {
            capped.push(reason);
        }
    }
    let mut limited: Vec<LimitedFile> = limited
        .into_iter()
        .filter(|f| headline.contains(&f.reason))
        .collect();
    limited.truncate(MAX_LIMITED_FILES_LISTED);
    (headline, capped, limited)
}

impl CoverageReport {
    /// A run that could not give every changed file a witness is
    /// `degraded`, not merely partial. Absent when nothing limited the run.
    pub fn build(
        ctx: &RunContext,
        extra: &[LimitReason],
        relevant: &dyn Fn(&str) -> bool,
    ) -> Option<Self> {
        let (limit_reasons, capped, limited_files) = classify_limits(ctx, extra, relevant);
        if limit_reasons.is_empty() && capped.is_empty() {
            return None;
        }
        let status = if limit_reasons.contains(&LimitReason::EvidenceBudgetExceeded) {
            CoverageStatus::Degraded
        } else if limit_reasons.is_empty() {
            CoverageStatus::Complete
        } else {
            CoverageStatus::Partial
        };
        Some(Self {
            status,
            limits: limit_reasons,
            capped,
            limited_files,
            resources: ctx.usage(),
            unknown: Vec::new(),
            lossy_files: ctx.lossy_files(),
        })
    }
}

struct Inner {
    expires_at: Option<Instant>,
    /// The git ceiling: the CLI `--timeout` even when the test hook spends
    /// the compute deadline up front, so the run still reaches its phases.
    io_expires_at: Option<Instant>,
    cancel: Arc<AtomicBool>,
    budget: ResourceBudget,
    tripped: AtomicBool,
    contributions: AtomicU64,
    reasons: Mutex<BTreeSet<LimitReason>>,
    usage: Mutex<ResourceUsage>,
    limited_files: Mutex<BTreeSet<(String, LimitReason)>>,
    /// Files a read found and the withhold policy kept out, repo-relative.
    withheld: Mutex<BTreeSet<String>>,
    head_rev: Mutex<Option<String>>,
    source: Mutex<Option<crate::source::Source>>,
    changed_lines: Mutex<Option<Arc<crate::changed_lines::ChangedLines>>>,
    /// Polls left before the logical clock fires; `None` outside a test.
    polls_left: Option<AtomicU64>,
    /// Answers that depend on a path alone (is it a repository, what
    /// `.diffctx/ignore` declares), asked up to seven times a run: `memoized`.
    memo: Mutex<rustc_hash::FxHashMap<(&'static str, std::path::PathBuf), Arc<Memoized>>>,
}

#[derive(Clone)]
pub struct RunContext {
    inner: Arc<Inner>,
}

impl RunContext {
    pub fn new(budget: ResourceBudget) -> Self {
        let io_secs = budget.max_wall_secs;
        Self::with_io_deadline(budget, io_secs)
    }

    /// The pipeline's context: `timeout_secs` bounds every git child of the
    /// run, together, whatever the budget's compute deadline is.
    pub fn for_run(timeout_secs: u64) -> Self {
        Self::with_io_deadline(ResourceBudget::resolve(timeout_secs), timeout_secs)
    }

    fn with_io_deadline(budget: ResourceBudget, io_secs: u64) -> Self {
        // Saturating: an absurd timeout must clamp to "no ceiling", not wrap
        // behind `now` and fire instantly.
        let after = |secs: u64| {
            if secs == u64::MAX {
                None
            } else {
                Instant::now().checked_add(Duration::from_secs(secs))
            }
        };
        let expires_at = after(budget.max_wall_secs);
        let polls_left = expires_at
            .and_then(|_| std::env::var(TEST_DEADLINE_AFTER_POLLS_ENV).ok())
            .and_then(|n| n.parse().ok())
            .map(AtomicU64::new);
        let io_expires_at = after(io_secs);
        let cancel = PENDING_CANCEL
            .with(|c| c.borrow().clone())
            .unwrap_or_default();
        Self {
            inner: Arc::new(Inner {
                expires_at,
                io_expires_at,
                cancel,
                budget,
                tripped: AtomicBool::new(false),
                contributions: AtomicU64::new(0),
                reasons: Mutex::new(BTreeSet::new()),
                usage: Mutex::new(ResourceUsage::default()),
                limited_files: Mutex::new(BTreeSet::new()),
                withheld: Mutex::new(BTreeSet::new()),
                head_rev: Mutex::new(None),
                source: Mutex::new(None),
                changed_lines: Mutex::new(None),
                polls_left,
                memo: Mutex::default(),
            }),
        }
    }

    pub fn unbounded() -> Self {
        Self::new(ResourceBudget::unbounded())
    }

    pub fn budget(&self) -> &ResourceBudget {
        &self.inner.budget
    }

    /// A file decoded lossily: `NonUtf8Content` plus the path, so the reader
    /// knows which symbols to distrust.
    pub fn note_lossy(&self, display_path: String) {
        self.note_file(LimitReason::NonUtf8Content, display_path);
    }

    /// A limit that cost one file something, with the file: the coverage
    /// block can then say whether it touched what the answer is about.
    pub fn note_file(&self, reason: LimitReason, display_path: String) {
        self.note(reason);
        self.inner
            .limited_files
            .lock()
            .unwrap()
            .insert((display_path, reason));
    }

    /// The newest commit the history channels may read: the base of the
    /// analysed range, the commit a staged snapshot sits on; `None` for the
    /// working tree (its base is `HEAD`) and for an unborn branch.
    pub fn set_head_rev(&self, rev: Option<String>) {
        *self.inner.head_rev.lock().unwrap() = rev;
    }

    /// The current side of the analysis this run serves; a run that was
    /// given none reads nothing.
    pub fn source(&self) -> crate::source::Source {
        self.inner
            .source
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(crate::source::Source::empty)
    }

    /// What bounds a git child started for this run: its I/O deadline and
    /// its cancellation.
    pub fn io_limits(&self) -> (Option<Instant>, Arc<AtomicBool>) {
        (self.inner.io_expires_at, self.inner.cancel.clone())
    }

    pub fn set_source(&self, source: crate::source::Source) {
        source.report_to(RunNotes(Arc::downgrade(&self.inner)));
        *self.inner.source.lock().unwrap() = Some(source);
    }

    /// The lines the change touched, for the readers that tell a changed
    /// line from an untouched one in a changed file.
    pub fn set_changed_lines(&self, lines: crate::changed_lines::ChangedLines) {
        *self.inner.changed_lines.lock().unwrap() = Some(Arc::new(lines));
    }

    pub fn changed_lines(&self) -> Option<Arc<crate::changed_lines::ChangedLines>> {
        self.inner.changed_lines.lock().unwrap().clone()
    }

    pub fn lossy_files(&self) -> Vec<String> {
        self.limited_files()
            .into_iter()
            .filter(|f| f.reason == LimitReason::NonUtf8Content)
            .map(|f| f.path)
            .collect()
    }

    /// The files this run noted for `reason`, repo-relative.
    pub fn limited_by(&self, reason: LimitReason) -> rustc_hash::FxHashSet<String> {
        self.inner
            .limited_files
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, r)| *r == reason)
            .map(|(path, _)| path.clone())
            .collect()
    }

    pub fn limited_files(&self) -> Vec<LimitedFile> {
        self.inner
            .limited_files
            .lock()
            .unwrap()
            .iter()
            .map(|(path, reason)| LimitedFile {
                path: path.clone(),
                reason: *reason,
            })
            .collect()
    }

    /// `true` while the phase may go on. The first expiry records `Deadline`;
    /// every later poll is a cheap atomic load.
    pub fn check(&self) -> bool {
        if self.inner.tripped.load(Ordering::Relaxed) {
            return false;
        }
        if self.cancelled() {
            self.note(LimitReason::Cancelled);
            self.inner.tripped.store(true, Ordering::Relaxed);
            return false;
        }
        #[allow(deprecated, reason = "try_update needs Rust 1.95; MSRV is 1.85")]
        let logical = self.inner.polls_left.as_ref().is_some_and(|left| {
            left.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1))
                .is_err()
        });
        match self.inner.expires_at {
            _ if logical => {
                self.note(LimitReason::Deadline);
                self.inner.tripped.store(true, Ordering::Relaxed);
                false
            }
            Some(expires_at) if Instant::now() >= expires_at => {
                self.note(LimitReason::Deadline);
                self.inner.tripped.store(true, Ordering::Relaxed);
                false
            }
            _ => true,
        }
    }

    pub fn cancelled(&self) -> bool {
        self.inner.cancel.load(Ordering::Relaxed)
    }

    /// `items`, ending at the first poll past the deadline, which `check`
    /// notes: a loop over an input-sized list is bounded by being one, not
    /// by remembering to poll — the stale-reference loop that had no poll
    /// ran a one-second run to 3.2 s (#510).
    pub fn bounded<'a, I>(&'a self, items: I) -> impl Iterator<Item = I::Item> + 'a
    where
        I: IntoIterator,
        I::IntoIter: 'a,
    {
        items.into_iter().take_while(move |_| self.check())
    }

    /// `bounded` for a rayon loop: each item is taken after one poll, from
    /// any worker, so no item starts past the deadline. Without one, `collect`
    /// keeps the input's order.
    pub fn bounded_par<'a, I>(
        &'a self,
        items: I,
    ) -> impl rayon::iter::ParallelIterator<Item = I::Item> + 'a
    where
        I: rayon::iter::IntoParallelIterator,
        I::Iter: 'a,
    {
        use rayon::iter::ParallelIterator;
        items.into_par_iter().take_any_while(move |_| self.check())
    }

    /// Whether the deadline has fired, without re-checking the clock.
    pub fn tripped(&self) -> bool {
        self.inner.tripped.load(Ordering::Relaxed)
    }

    pub fn note(&self, reason: LimitReason) {
        self.inner.reasons.lock().unwrap().insert(reason);
    }

    /// Files a read left out under the withhold policy: the answer counts
    /// them, since what they say was never read (#392).
    pub fn note_withheld(&self, paths: Vec<String>) {
        self.inner.withheld.lock().unwrap().extend(paths);
    }

    pub fn withheld_count(&self) -> usize {
        self.inner.withheld.lock().unwrap().len()
    }

    /// What limited the run: the reasons noted on it, and a failed read the
    /// snapshot layer recorded even if its caller dropped the error.
    pub fn reasons(&self) -> Vec<LimitReason> {
        let mut all: BTreeSet<LimitReason> = self.inner.reasons.lock().unwrap().clone();
        if self.source().lost_a_read() {
            all.insert(LimitReason::ReadFailed);
        }
        all.into_iter().collect()
    }

    /// The reasons that make a count of what the run searched for unfinished:
    /// the deadline, a cancellation, a read that failed. Empty while nothing
    /// of the kind has been noted; once it is not, a caller count the run
    /// prints is what it reached, not what there is.
    pub fn stops(&self) -> Vec<LimitReason> {
        let mut stops = self.reasons();
        stops.retain(|r| r.stops_a_search());
        stops
    }

    /// Records `n` more edge emissions. `false` once the run is over its
    /// contribution cap: the caller stops emitting and the reason is logged.
    pub fn add_contributions(&self, n: u64) -> bool {
        let total = self.inner.contributions.fetch_add(n, Ordering::Relaxed) + n;
        if total > self.inner.budget.max_edge_contributions {
            self.note(LimitReason::EdgeContributionLimit);
            return false;
        }
        true
    }

    pub fn over_contributions(&self) -> bool {
        self.inner.contributions.load(Ordering::Relaxed) > self.inner.budget.max_edge_contributions
    }

    pub fn record_usage(&self, f: impl FnOnce(&mut ResourceUsage)) {
        f(&mut self.inner.usage.lock().unwrap());
    }

    pub fn usage(&self) -> ResourceUsage {
        let mut usage = self.inner.usage.lock().unwrap().clone();
        usage.edge_contributions = self.inner.contributions.load(Ordering::Relaxed);
        usage
    }

    /// Publishes this context to the current thread for the guard's lifetime,
    /// so hot loops deep inside edge builders can poll it. Each rayon worker
    /// publishes its own copy, so concurrent runs never see each other's.
    pub fn enter(&self) -> ScopedContext {
        let prev = CURRENT.with(|c| c.replace(Some(self.clone())));
        SELF_REPORTED.with(|r| r.set(0));
        let git = crate::snapshot::enter_run(crate::snapshot::RunBudget {
            deadline: self.inner.io_expires_at,
            cancel: Some(self.inner.cancel.clone()),
            read_fault: None,
        });
        ScopedContext { prev, _git: git }
    }

    /// Charges a finished builder's total minus what its own loops already
    /// reported on this thread.
    pub fn charge_remaining(&self, total: u64) -> bool {
        let already = SELF_REPORTED.with(|r| r.replace(0));
        let remaining = total.saturating_sub(already);
        remaining == 0 || self.add_contributions(remaining)
    }
}

thread_local! {
    static CURRENT: RefCell<Option<RunContext>> = const { RefCell::new(None) };
    /// The cancellation flag the next context created on this thread adopts:
    /// the FFI entry sets it around the pipeline call, so a withdrawn MCP
    /// request reaches every phase and every git child of that run.
    static PENDING_CANCEL: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) };
    /// What the loops of the builder running on this thread have already
    /// charged through `poll_emissions`, so the orchestrator charges only the
    /// remainder when the builder returns — never the same edge twice.
    static SELF_REPORTED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// The change's lines on a thread the run's context is not published to
    /// (the discovery strategies run on rayon workers).
    static CHANGED_LINES: RefCell<Option<Arc<crate::changed_lines::ChangedLines>>> =
        const { RefCell::new(None) };
    /// The analysis' source on a thread the run's context is not published to.
    static SOURCE: RefCell<Option<crate::source::Source>> = const { RefCell::new(None) };
}

pub struct ScopedContext {
    prev: Option<RunContext>,
    /// The run's limits as the git children of this thread see them, put back
    /// with the context.
    _git: crate::snapshot::RunScope,
}

impl Drop for ScopedContext {
    fn drop(&mut self) {
        let prev = self.prev.take();
        CURRENT.with(|c| *c.borrow_mut() = prev);
    }
}

fn with_current<T>(f: impl FnOnce(&RunContext) -> T) -> Option<T> {
    CURRENT.with(|c| c.borrow().as_ref().map(f))
}

type Memoized = dyn std::any::Any + Send + Sync;

/// The current run's first answer to `kind` for `key`, for an answer that
/// depends on that path alone; anything read off a revision or a source
/// belongs to that source instead. Outside a run every call computes, and a
/// failure is never kept, so the next ask tries again.
pub fn memoized<T, E>(
    kind: &'static str,
    key: &std::path::Path,
    f: impl FnOnce() -> Result<T, E>,
) -> Result<T, E>
where
    T: Clone + Send + Sync + 'static,
{
    let Some(run) = with_current(RunContext::clone) else {
        return f();
    };
    let slot = (kind, key.to_path_buf());
    let known = run.inner.memo.lock().unwrap().get(&slot).cloned();
    if let Some(value) = known.as_deref().and_then(|v| v.downcast_ref::<T>()) {
        return Ok(value.clone());
    }
    let value = f()?;
    run.inner
        .memo
        .lock()
        .unwrap()
        .insert(slot, Arc::new(value.clone()));
    Ok(value)
}

/// A run that something it owns can note into without keeping it alive: the
/// source a run holds reports its failed reads here, from any thread.
#[derive(Clone)]
pub struct RunNotes(std::sync::Weak<Inner>);

impl RunNotes {
    pub fn note(&self, reason: LimitReason) {
        if let Some(inner) = self.0.upgrade() {
            inner.reasons.lock().unwrap().insert(reason);
        }
    }
}

/// `true` while the current run may go on; always `true` on a thread that
/// serves no run. The first expiry is noted as the deadline, as `check` does.
pub fn current_check() -> bool {
    with_current(RunContext::check).unwrap_or(true)
}

/// What bounds a read made for the current run: its I/O deadline and its
/// cancellation. `None` outside a run.
pub fn current_io_limits() -> Option<(Option<Instant>, Arc<AtomicBool>)> {
    with_current(RunContext::io_limits)
}

/// The current run's note, when this thread runs one.
pub fn note_current(reason: LimitReason) {
    with_current(|ctx| ctx.note(reason));
}

/// What a failed git read leaves the answer missing, as a limit: out of time
/// or cancelled is the deadline, an oversized blob its size, anything else a
/// read that failed.
pub fn reason_for(e: &crate::snapshot::SnapshotError) -> LimitReason {
    use crate::snapshot::SnapshotError as E;
    match e {
        E::Timeout(_) | E::Cancelled => LimitReason::Deadline,
        E::TooLarge { .. } => LimitReason::FileTooLarge,
        _ => LimitReason::ReadFailed,
    }
}

/// Runs `f` with `flag` as the cancellation of every run it starts.
pub fn with_cancel<T>(flag: Arc<AtomicBool>, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<Arc<AtomicBool>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let prev = self.0.take();
            PENDING_CANCEL.with(|c| *c.borrow_mut() = prev);
        }
    }
    let _restore = Restore(PENDING_CANCEL.with(|c| c.replace(Some(flag))));
    f()
}

/// Runs `f` with the change's lines published to this thread.
pub fn with_changed_lines<T>(
    lines: Option<Arc<crate::changed_lines::ChangedLines>>,
    f: impl FnOnce() -> T,
) -> T {
    let prev = CHANGED_LINES.with(|c| c.replace(lines));
    struct Restore(Option<Arc<crate::changed_lines::ChangedLines>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let prev = self.0.take();
            CHANGED_LINES.with(|c| *c.borrow_mut() = prev);
        }
    }
    let _restore = Restore(prev);
    f()
}

/// The lines the current run's change touched; `None` outside a run that
/// knows them (then every line counts as changed).
pub fn current_changed_lines() -> Option<Arc<crate::changed_lines::ChangedLines>> {
    CHANGED_LINES
        .with(|c| c.borrow().clone())
        .or_else(|| with_current(RunContext::changed_lines).flatten())
}

/// The source of the run this thread serves, or of the analysis a worker was
/// handed with `with_source`. Outside both it reads nothing: there is no
/// working-tree default to fall back on.
pub fn current_source() -> crate::source::Source {
    SOURCE
        .with(|c| c.borrow().clone())
        .or_else(|| with_current(RunContext::source))
        .unwrap_or_else(crate::source::Source::empty)
}

/// A file's text: the caller's cache when it holds the file, else the run's source.
pub fn read_file_cached(
    path: &std::path::Path,
    cache: Option<&rustc_hash::FxHashMap<std::path::PathBuf, String>>,
) -> Option<String> {
    cache
        .and_then(|c| c.get(path).cloned())
        .or_else(|| current_source().read_to_string(path))
}

/// Runs `f` with `source` published to this thread: for rayon workers the
/// run's context is not published to.
pub fn with_source<T>(source: &crate::source::Source, f: impl FnOnce() -> T) -> T {
    let prev = SOURCE.with(|c| c.replace(Some(source.clone())));
    struct Restore(Option<crate::source::Source>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let prev = self.0.take();
            SOURCE.with(|c| *c.borrow_mut() = prev);
        }
    }
    let _restore = Restore(prev);
    f()
}

/// Where history ends for the current run: the same `--diff A..B` must see
/// the same past whatever is checked out, and none of its own commits (#340).
pub fn current_head_rev() -> Option<String> {
    with_current(|ctx| ctx.inner.head_rev.lock().unwrap().clone()).flatten()
}

/// Intra-loop poll for the builder loops whose single invocation can outrun
/// the whole timeout (the envoy 520-`config` cross product, the sentry-scale
/// config-key scan, the python identifier fan-out). Costs a branch on all but
/// every `every`-th iteration; `produced` is the emission count since the
/// previous poll, so the contribution cap binds inside the loop and not only
/// after it returns. `true` means keep going.
pub fn poll_current(i: usize, every: usize, produced: u64) -> bool {
    if i % every != 0 {
        return true;
    }
    with_current(|ctx| ctx.check() && (produced == 0 || ctx.add_contributions(produced)))
        .unwrap_or(true)
}

/// `poll_current` for a loop that accumulates into one map: `emitted` is the
/// map's size so far, `reported` what the last poll already charged. Updated
/// only on the poll cadence — advancing it every iteration charged one
/// fragment's worth per poll and a 38M-edge builder never tripped the cap.
pub fn poll_emissions(i: usize, every: usize, emitted: u64, reported: &mut u64) -> bool {
    if i % every != 0 {
        return true;
    }
    let produced = emitted.saturating_sub(*reported);
    *reported = emitted;
    SELF_REPORTED.with(|r| r.set(r.get() + produced));
    poll_current(i, every, produced)
}

/// The deadline half alone, for loops that emit nothing (diffusion pushes).
pub fn poll_current_every(i: usize, every: usize) -> bool {
    poll_current(i, every, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn budget(secs: u64, contributions: u64) -> ResourceBudget {
        ResourceBudget {
            max_wall_secs: secs,
            max_edge_contributions: contributions,
            ..ResourceBudget::unbounded()
        }
    }

    #[test]
    fn an_expired_deadline_stops_the_phase_and_records_the_reason() {
        let ctx = RunContext::new(budget(0, u64::MAX));
        std::thread::sleep(Duration::from_millis(5));
        assert!(!ctx.check());
        assert!(ctx.tripped());
        assert_eq!(ctx.reasons(), vec![LimitReason::Deadline]);
    }

    #[test]
    fn an_unexpired_or_unbounded_context_never_stops() {
        assert!(RunContext::new(budget(1000, u64::MAX)).check());
        assert!(RunContext::unbounded().check());
        assert!(RunContext::unbounded().add_contributions(u64::MAX / 2));
    }

    #[test]
    fn the_contribution_cap_trips_once_crossed_and_is_recorded() {
        let ctx = RunContext::new(budget(1000, 100));
        assert!(ctx.add_contributions(60));
        assert!(ctx.add_contributions(40));
        assert!(!ctx.add_contributions(1));
        assert!(ctx.over_contributions());
        assert_eq!(ctx.reasons(), vec![LimitReason::EdgeContributionLimit]);
        assert_eq!(ctx.usage().edge_contributions, 101);
    }

    #[test]
    fn concurrent_contexts_do_not_affect_each_other() {
        // The #210 defect: request B's short ceiling used to overwrite
        // request A's. Per-run values make each check see only its own.
        let short = RunContext::new(budget(0, u64::MAX));
        let long = RunContext::new(budget(1000, u64::MAX));
        std::thread::sleep(Duration::from_millis(5));
        assert!(long.check());
        assert!(!short.check());
        assert!(long.check());
        assert!(long.reasons().is_empty());
    }

    #[test]
    fn the_published_context_is_scoped_and_nests() {
        let outer = RunContext::new(budget(1000, u64::MAX));
        let guard = outer.enter();
        assert!(poll_current_every(0, 1));
        {
            let expired = RunContext::new(budget(0, u64::MAX));
            let inner = expired.enter();
            std::thread::sleep(Duration::from_millis(5));
            assert!(!poll_current_every(0, 1));
            drop(inner);
        }
        assert!(poll_current_every(0, 1));
        let capped = RunContext::new(budget(1000, 10));
        let g2 = capped.enter();
        assert!(!poll_current(0, 1, 11));
        assert!(poll_current(1, 2, 1_000_000), "off-cycle polls never check");
        drop(g2);
        let capped = RunContext::new(budget(1000, 10));
        let g3 = capped.enter();
        let mut reported = 0;
        assert!(poll_emissions(0, 4, 3, &mut reported));
        assert!(
            poll_emissions(1, 4, 8, &mut reported),
            "off-cycle: nothing charged"
        );
        assert_eq!(reported, 3);
        assert!(
            !poll_emissions(4, 4, 12, &mut reported),
            "12 emitted > cap 10"
        );
        assert_eq!(capped.usage().edge_contributions, 12);
        assert!(!capped.charge_remaining(15), "3 more, still over the cap");
        assert_eq!(
            capped.usage().edge_contributions,
            15,
            "self-reported emissions charged once"
        );
        drop(g3);
        drop(guard);
        // Nothing published: must never stop.
        assert!(poll_current(0, 1, u64::MAX));
    }
}
