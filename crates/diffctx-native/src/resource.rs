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
use serde::Serialize;

/// Why a run did not do everything it could have. One vocabulary for every
/// phase; consumers read `coverage.limit_reasons` and never a per-subsystem
/// boolean.
#[derive(Serialize, JsonSchema, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
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
    EvidenceBudgetExceeded,
    SelectionBudgetExceeded,
    SanitizationRedaction,
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
#[derive(Serialize, JsonSchema, Clone, Debug, Default, PartialEq, Eq)]
pub struct ResourceUsage {
    pub source_bytes: u64,
    pub parsed_files: u64,
    pub candidate_files: u64,
    pub edge_contributions: u64,
    pub final_edges: u64,
}

/// A limit that touched one file, by its display path.
#[derive(Serialize, JsonSchema, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
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
                | Self::DiscoveryTruncated
                | Self::CandidateLimit
                | Self::TotalByteLimit
                | Self::EdgeContributionLimit
                | Self::EvidenceBudgetExceeded
                | Self::SelectionBudgetExceeded
                | Self::SanitizationRedaction
        )
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

/// What a run's limits mean for the reader (#306). `limit_reasons` are the
/// ones that can have cost this answer context: a run-wide stop, or a
/// per-file limit on a file the answer is about (changed, or holding a
/// selected fragment). Graph caps every repository trips, and a size cap on
/// a lockfile nobody reads, go to `capped` and leave the status `complete`.
#[derive(Serialize, JsonSchema, Clone, Debug)]
pub struct CoverageReport {
    pub status: &'static str,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limit_reasons: Vec<LimitReason>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capped: Vec<LimitReason>,
    /// The files behind the per-file reasons in `limit_reasons`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limited_files: Vec<LimitedFile>,
    pub resources: ResourceUsage,
    /// Files that were not UTF-8 and were decoded with replacement
    /// characters (`NonUtf8Content`): their symbols and identifiers may be
    /// truncated at the first non-ASCII byte.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lossy_files: Vec<String>,
}

/// `limit_reasons`, `capped` and the relevant `limited_files` of a run, by
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
            "degraded"
        } else if limit_reasons.is_empty() {
            "complete"
        } else {
            "partial"
        };
        Some(Self {
            status,
            limit_reasons,
            capped,
            limited_files,
            resources: ctx.usage(),
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
    head_rev: Mutex<Option<String>>,
    source: Mutex<Option<crate::source::Source>>,
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
                head_rev: Mutex::new(None),
                source: Mutex::new(None),
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

    pub fn source(&self) -> crate::source::Source {
        self.inner
            .source
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_default()
    }

    pub fn set_source(&self, source: crate::source::Source) {
        *self.inner.source.lock().unwrap() = Some(source);
    }

    pub fn lossy_files(&self) -> Vec<String> {
        self.limited_files()
            .into_iter()
            .filter(|f| f.reason == LimitReason::NonUtf8Content)
            .map(|f| f.path)
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
        match self.inner.expires_at {
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

    /// Whether the deadline has fired, without re-checking the clock.
    pub fn tripped(&self) -> bool {
        self.inner.tripped.load(Ordering::Relaxed)
    }

    pub fn note(&self, reason: LimitReason) {
        self.inner.reasons.lock().unwrap().insert(reason);
    }

    pub fn reasons(&self) -> Vec<LimitReason> {
        self.inner.reasons.lock().unwrap().iter().copied().collect()
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
        ScopedContext { prev }
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
}

pub struct ScopedContext {
    prev: Option<RunContext>,
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

pub fn current_source() -> Option<crate::source::Source> {
    with_current(RunContext::source)
}

/// What bounds a git child started now: the run's I/O deadline and its
/// cancellation. `None` outside a run.
pub fn current_io_limits() -> Option<(Option<Instant>, Arc<AtomicBool>)> {
    with_current(|ctx| (ctx.inner.io_expires_at, ctx.inner.cancel.clone()))
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
