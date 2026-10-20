//! Opt-in run telemetry for the test suites: with `DIFFCTX_PROFILE=<file>` set,
//! every run appends one JSON line saying where its time went, so a suite can
//! end with which tests spent the most time in diffctx and in what. It never
//! reaches the artifact; off, a run pays one environment lookup.
//!
//! A run's phases are recorded on the thread that orchestrates it. A run that
//! starts while another is active on the thread (the hook's impact, the CLI's
//! pipeline) is part of that one, and work handed to a spawned thread is
//! carried there with [`carry`].

use std::cell::RefCell;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Instant;

const PROFILE_ENV: &str = "DIFFCTX_PROFILE";
pub const TAG_ENV: &str = "DIFFCTX_PROFILE_TAG";

struct Record {
    mode: &'static str,
    cli: bool,
    started: Instant,
    phases: Vec<(&'static str, f64)>,
    counts: Vec<(&'static str, u64)>,
    written: bool,
}

type Shared = Arc<Mutex<Record>>;

thread_local! {
    static ACTIVE: RefCell<Option<Shared>> = const { RefCell::new(None) };
    static TAG: RefCell<Option<String>> = const { RefCell::new(None) };
}

fn active() -> Option<Shared> {
    ACTIVE.with(|a| a.borrow().clone())
}

fn sink() -> Option<std::ffi::OsString> {
    std::env::var_os(PROFILE_ENV).filter(|v| !v.is_empty())
}

/// The guard of a run; the outermost one writes the record when dropped.
pub struct Run(Option<Shared>);

/// Starts a run named `mode`, or names the active one when it is still the
/// CLI's unnamed run.
pub fn run(mode: &'static str) -> Run {
    start(mode, false)
}

/// The CLI process's run: the pipeline it starts names it, and its peak
/// memory is the run's own.
pub fn run_cli() -> Run {
    start("cli", true)
}

fn start(mode: &'static str, cli: bool) -> Run {
    if let Some(outer) = active() {
        let mut r = outer.lock().unwrap();
        if r.mode == "cli" {
            r.mode = mode;
        }
        return Run(None);
    }
    if sink().is_none() {
        return Run(None);
    }
    let shared = Arc::new(Mutex::new(Record {
        mode,
        cli,
        started: Instant::now(),
        phases: Vec::new(),
        counts: Vec::new(),
        written: false,
    }));
    ACTIVE.with(|a| *a.borrow_mut() = Some(shared.clone()));
    Run(Some(shared))
}

impl Drop for Run {
    fn drop(&mut self) {
        if let Some(shared) = self.0.take() {
            ACTIVE.with(|a| *a.borrow_mut() = None);
            write(&shared);
        }
    }
}

/// Writes the active run now: the CLI's `process::exit` skips destructors.
pub fn flush() {
    if let Some(shared) = active() {
        write(&shared);
    }
}

/// Times a phase of the active run until dropped.
pub struct Phase(Option<(Shared, &'static str, Instant)>);

pub fn phase(name: &'static str) -> Phase {
    Phase(active().map(|s| (s, name, Instant::now())))
}

impl Drop for Phase {
    fn drop(&mut self) {
        if let Some((shared, name, started)) = self.0.take() {
            let ms = started.elapsed().as_secs_f64() * 1000.0;
            shared.lock().unwrap().phases.push((name, ms));
        }
    }
}

/// A phase measured elsewhere, in milliseconds.
pub fn record(name: &'static str, ms: f64) {
    if let Some(shared) = active() {
        shared.lock().unwrap().phases.push((name, ms));
    }
}

/// A size that explains a run's time: files read, fragments, edges.
pub fn count(name: &'static str, n: u64) {
    if let Some(shared) = active() {
        shared.lock().unwrap().counts.push((name, n));
    }
}

/// `work`, part of the active run on the thread spawned to do it.
pub fn carry<T>(work: impl FnOnce() -> T) -> impl FnOnce() -> T {
    let run = active();
    move || {
        ACTIVE.with(|a| *a.borrow_mut() = run);
        work()
    }
}

/// Runs `f` with its runs attributed to `tag`: a test harness that runs many
/// cases on pooled threads names each one.
pub fn tagged<T>(tag: &str, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<String>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let prev = self.0.take();
            TAG.with(|t| *t.borrow_mut() = prev);
        }
    }
    let _restore = Restore(TAG.with(|t| t.borrow_mut().replace(tag.to_string())));
    f()
}

/// The harness's name for the run, else the environment's, else the test
/// thread's: libtest names each test's thread after it.
fn tag() -> String {
    TAG.with(|t| t.borrow().clone())
        .or_else(|| std::env::var(TAG_ENV).ok().filter(|t| !t.is_empty()))
        .or_else(|| {
            std::thread::current()
                .name()
                .filter(|n| *n != "main")
                .map(str::to_string)
        })
        .unwrap_or_default()
}

fn write(shared: &Shared) {
    let mut r = shared.lock().unwrap();
    if r.written {
        return;
    }
    r.written = true;
    let Some(path) = sink() else {
        return;
    };
    let mut phases = serde_json::Map::new();
    for (name, ms) in &r.phases {
        let total = phases
            .get(*name)
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0)
            + ms;
        phases.insert((*name).to_string(), serde_json::json!(round(total)));
    }
    let counts: serde_json::Map<String, serde_json::Value> = r
        .counts
        .iter()
        .map(|(name, n)| ((*name).to_string(), serde_json::json!(n)))
        .collect();
    let mut line = serde_json::json!({
        "tag": tag(),
        "mode": r.mode,
        "total_ms": round(r.started.elapsed().as_secs_f64() * 1000.0),
        "phases": phases,
        "counts": counts,
    });
    if r.cli {
        line["peak_rss_mb"] = serde_json::json!(round(
            crate::peak_rss::peak_rss_bytes() as f64 / 1_048_576.0
        ));
    }
    let mut text = line.to_string();
    text.push('\n');
    // One write per line: appends from parallel test processes stay whole.
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut f| f.write_all(text.as_bytes()));
}

fn round(ms: f64) -> f64 {
    (ms * 10.0).round() / 10.0
}
