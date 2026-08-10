use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use once_cell::sync::Lazy;

/// Wall-clock ceiling for the compute phases, shared across rayon workers.
///
/// The `timeout` parameter only ever reached git subprocesses
/// (`set_git_timeout`); the native CLI is protected by a process-level
/// watchdog that exits 124, but the library path — pyo3, and through it the
/// MCP server — had no ceiling at all: a 420s timeout was observed to sit
/// through an 8-minute edge build without firing. The edge phase checks this
/// between builders, so the overshoot is bounded by the slowest single
/// builder rather than unbounded.
///
/// Expiry panics with a recognizable message on purpose: the phase runs deep
/// inside call chains that do not return `Result`, rayon propagates the
/// unwind to the caller, and pyo3 surfaces it as a Python exception — an
/// error after `timeout` seconds, where before there was a hang.
static ANCHOR: Lazy<Instant> = Lazy::new(Instant::now);
static DEADLINE_MS: AtomicU64 = AtomicU64::new(0);

pub fn set_compute_deadline(timeout_secs: u64) {
    let now_ms = ANCHOR.elapsed().as_millis() as u64;
    // +1 keeps a zero timeout distinct from the 0 = "no deadline" sentinel.
    // Saturating: an absurd caller-supplied timeout must clamp to "far future",
    // not wrap behind `now_ms` and fire instantly.
    let deadline = now_ms
        .saturating_add(timeout_secs.saturating_mul(1000))
        .saturating_add(1);
    DEADLINE_MS.store(deadline, Ordering::Relaxed);
}

pub fn check_compute_deadline(phase: &str) {
    let deadline = DEADLINE_MS.load(Ordering::Relaxed);
    if deadline == 0 {
        return;
    }
    let now_ms = ANCHOR.elapsed().as_millis() as u64;
    check_expired(now_ms, deadline, phase);
}

fn check_expired(now_ms: u64, deadline_ms: u64, phase: &str) {
    if now_ms > deadline_ms {
        panic!("diffctx compute deadline exceeded during {phase}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The expiry check is tested against the pure comparison, not the
    // process-global atomic: an expired global deadline is visible to every
    // rayon worker in the test process, so mutating it here made any
    // concurrently running graph test flakily panic mid-build.
    #[test]
    fn an_expired_deadline_panics_with_the_phase_name() {
        let err = std::panic::catch_unwind(|| check_expired(6, 1, "edge construction"))
            .expect_err("deadline did not fire");
        let msg = err.downcast_ref::<String>().cloned().unwrap_or_default();
        assert!(msg.contains("edge construction"), "message was: {msg}");
    }

    #[test]
    fn an_unexpired_deadline_does_not_fire() {
        check_expired(1, 6, "edge construction");
    }
}
