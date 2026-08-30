use std::cell::Cell;
use std::time::{Duration, Instant};

/// Per-run wall-clock ceiling for the compute phases.
///
/// The `timeout` parameter only ever reached git subprocesses
/// (`set_git_timeout`); the native CLI is protected by a process-level
/// watchdog that exits 124, but the library path — pyo3, and through it the
/// MCP server — had no ceiling at all: a 420s timeout was observed to sit
/// through an 8-minute edge build without firing.
///
/// A per-run value rather than a process global (#210): the MCP server runs
/// overlapping pipelines on worker threads, and a shared atomic meant the
/// last request to arrive overwrote every in-flight request's ceiling — and
/// an expired ceiling was never cleared, so unrelated later runs in the same
/// process (the yaml harness above all) inherited it.
///
/// Expiry panics with a recognizable message on purpose: the phase runs deep
/// inside call chains that do not return `Result`, rayon propagates the
/// unwind to the caller, and pyo3 surfaces it as a Python exception — an
/// error after `timeout` seconds, where before there was a hang.
#[derive(Clone, Copy, Debug)]
pub struct Deadline {
    expires_at: Option<Instant>,
}

impl Deadline {
    /// Saturating: an absurd caller-supplied timeout must clamp to "no
    /// ceiling", not wrap behind `now` and fire instantly.
    pub fn from_timeout_secs(timeout_secs: u64) -> Self {
        Deadline {
            expires_at: Instant::now().checked_add(Duration::from_secs(timeout_secs)),
        }
    }

    pub fn none() -> Self {
        Deadline { expires_at: None }
    }

    pub fn check(&self, phase: &str) {
        if let Some(expires_at) = self.expires_at {
            check_expired(Instant::now(), expires_at, phase);
        }
    }

    /// Publishes this deadline to the current thread for the guard's
    /// lifetime, so hot loops deep inside edge builders can poll it via
    /// `check_current_every` without threading a parameter through 51
    /// `EdgeBuilder::build` implementations. Each rayon worker publishes its
    /// own copy, so concurrent runs never see each other's ceiling.
    pub fn enter(&self) -> ScopedDeadline {
        let prev = CURRENT.with(|c| c.replace(self.expires_at));
        ScopedDeadline { prev }
    }
}

thread_local! {
    static CURRENT: Cell<Option<Instant>> = const { Cell::new(None) };
}

pub struct ScopedDeadline {
    prev: Option<Instant>,
}

impl Drop for ScopedDeadline {
    fn drop(&mut self) {
        let prev = self.prev;
        CURRENT.with(|c| c.set(prev));
    }
}

/// Cheap intra-loop poll: costs a branch on all but every `every`-th
/// iteration. For the builder loops whose single invocation can outrun the
/// whole timeout (the envoy 520-`config` cross product, the sentry-scale
/// config-key scan), where the between-builders check cannot help.
pub fn check_current_every(i: usize, every: usize, phase: &str) {
    if i % every != 0 {
        return;
    }
    if let Some(expires_at) = CURRENT.with(|c| c.get()) {
        check_expired(Instant::now(), expires_at, phase);
    }
}

/// The panic payload's prefix. `pybridge` matches on it to turn the unwind
/// back into an ordinary Python `TimeoutError` — the deadline is a routine
/// outcome for a caller, not a crash, and it must not look like one.
pub const PANIC_PREFIX: &str = "diffctx compute deadline exceeded";

fn check_expired(now: Instant, expires_at: Instant, phase: &str) {
    if now > expires_at {
        panic!("{PANIC_PREFIX} during {phase}");
    }
}

/// The message inside a caught panic payload, when it is one of ours.
#[cfg(feature = "python")]
pub fn deadline_panic_message(payload: &(dyn std::any::Any + Send)) -> Option<String> {
    let msg = payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| payload.downcast_ref::<&'static str>().copied())?;
    msg.starts_with(PANIC_PREFIX).then(|| msg.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_expired_deadline_panics_with_the_phase_name() {
        let deadline = Deadline::from_timeout_secs(0);
        std::thread::sleep(Duration::from_millis(5));
        let err = std::panic::catch_unwind(|| deadline.check("edge construction"))
            .expect_err("deadline did not fire");
        let msg = err.downcast_ref::<String>().cloned().unwrap_or_default();
        assert!(msg.contains("edge construction"), "message was: {msg}");
    }

    #[test]
    fn an_unexpired_deadline_does_not_fire() {
        Deadline::from_timeout_secs(1000).check("edge construction");
        Deadline::none().check("edge construction");
    }

    #[test]
    fn concurrent_deadlines_do_not_affect_each_other() {
        // The #210 defect: request B's short ceiling used to overwrite
        // request A's. Per-run values make each check see only its own.
        let short = Deadline::from_timeout_secs(0);
        let long = Deadline::from_timeout_secs(1000);
        std::thread::sleep(Duration::from_millis(5));
        long.check("edge construction");
        std::panic::catch_unwind(|| short.check("edge construction"))
            .expect_err("short deadline did not fire");
        long.check("edge construction");
    }

    #[test]
    fn scoped_deadline_clears_on_drop_and_nests() {
        let outer = Deadline::from_timeout_secs(1000);
        let guard = outer.enter();
        check_current_every(0, 1, "edge construction");
        {
            let expired = Deadline::from_timeout_secs(0);
            let inner = expired.enter();
            std::thread::sleep(Duration::from_millis(5));
            std::panic::catch_unwind(|| check_current_every(0, 1, "edge construction"))
                .expect_err("inner deadline did not fire");
            drop(inner);
        }
        check_current_every(0, 1, "edge construction");
        drop(guard);
        // No deadline published: must never fire.
        check_current_every(0, 1, "edge construction");
    }
}
