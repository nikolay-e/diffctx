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
    DEADLINE_MS.store(now_ms + timeout_secs * 1000 + 1, Ordering::Relaxed);
}

pub fn check_compute_deadline(phase: &str) {
    let deadline = DEADLINE_MS.load(Ordering::Relaxed);
    if deadline == 0 {
        return;
    }
    let now_ms = ANCHOR.elapsed().as_millis() as u64;
    if now_ms > deadline {
        panic!("diffctx compute deadline exceeded during {phase}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_expired_deadline_panics_with_the_phase_name() {
        set_compute_deadline(0);
        std::thread::sleep(std::time::Duration::from_millis(5));
        let err = std::panic::catch_unwind(|| check_compute_deadline("edge construction"))
            .expect_err("deadline did not fire");
        let msg = err.downcast_ref::<String>().cloned().unwrap_or_default();
        assert!(msg.contains("edge construction"), "message was: {msg}");
        // Reset so other tests in the process are unaffected.
        DEADLINE_MS.store(0, Ordering::Relaxed);
    }
}
