//! The binary the suites drive, as `DIFFCTX_NATIVE_BIN` names it or cargo
//! built it, with each run's profile line named after the test that ran it;
//! and the git fixtures they build their repositories with.

// Every suite compiles this module and uses a part of it.
#![allow(dead_code)]

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

pub fn diffctx() -> Command {
    let mut cmd = Command::new(
        std::env::var("DIFFCTX_NATIVE_BIN")
            .unwrap_or_else(|_| env!("CARGO_BIN_EXE_diffctx").to_string()),
    );
    let suite = env!("CARGO_CRATE_NAME");
    let tag = match std::thread::current().name() {
        Some(test) if test != "main" => format!("{suite}::{test}"),
        _ => suite.to_string(),
    };
    cmd.env(_diffctx::profile::TAG_ENV, tag);
    cmd
}

pub fn git(repo: &Path, args: &[&str]) {
    // Scrub the same vars git_command() scrubs: under `git commit -a` the
    // pre-commit hook runs with GIT_INDEX_FILE pointing at the MAIN repo's
    // in-progress index, and an unscrubbed child git in a temp repo locks
    // that index instead ("index.lock.lock: File exists").
    let out = Command::new("git")
        .current_dir(repo)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .args(args)
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

pub fn init_repo(repo: &Path) {
    git(repo, &["init", "-q", "."]);
    git(repo, &["config", "user.email", "test@example.com"]);
    git(repo, &["config", "user.name", "diffctx tests"]);
}

pub fn commit_all(repo: &Path, message: &str) {
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", message]);
}

pub fn write(repo: &Path, rel: &str, content: &str) {
    let path = repo.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(path, content).expect("write");
}

/// A repository at `root` with one commit per non-empty step.
pub fn fill_steps(root: &Path, steps: &[&[(&str, &str)]]) {
    init_repo(root);
    for (step, files) in steps.iter().filter(|f| !f.is_empty()).enumerate() {
        for (rel, content) in *files {
            write(root, rel, content);
        }
        commit_all(root, &format!("step {step}"));
    }
}

/// A repository at `root` whose last commit applies `change` on top of `base`.
pub fn fill(root: &Path, base: &[(&str, &str)], change: &[(&str, &str)]) {
    fill_steps(root, &[base, change]);
}

pub fn repo(base: &[(&str, &str)], change: &[(&str, &str)]) -> TempDir {
    let tmp = TempDir::new().expect("tempdir");
    fill(tmp.path(), base, change);
    tmp
}

pub fn repo_steps(steps: &[&[(&str, &str)]]) -> TempDir {
    let tmp = TempDir::new().expect("tempdir");
    fill_steps(tmp.path(), steps);
    tmp
}

pub fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

/// Every caller and stale reference an impact answer claims, sorted.
pub fn claims(doc: &serde_json::Value) -> Vec<String> {
    let mut out: Vec<String> = doc["changed"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|c| {
            let symbol = c["symbol"].as_str().unwrap_or("").to_string();
            c["callers"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(move |k| format!("{symbol} <- {}", k["path"].as_str().unwrap_or("")))
        })
        .chain(
            doc["stale_references"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|r| format!("stale {}", r["path"].as_str().unwrap_or(""))),
        )
        .collect();
    out.sort();
    out
}

/// The answers of `run(n)` for n = 0, 1, 2, … in order, up to and including
/// the first one `finished` accepts: the #510 sweeps, run a batch of polls at
/// a time. One process at a time, three of them were most of the suite's wall
/// clock. Each run is named after the test that sweeps, as `diffctx()` names
/// its profile line.
pub fn sweep_polls<T: Send>(
    run: impl Fn(u64) -> T + Sync,
    finished: impl Fn(&T) -> bool,
) -> Vec<(u64, T)> {
    let width = std::thread::available_parallelism().map_or(4, |n| n.get().clamp(2, 8)) as u64;
    let test = std::thread::current().name().map(str::to_string);
    let run = &run;
    let mut answers = Vec::new();
    let mut start = 0;
    loop {
        let batch: Vec<(u64, T)> = std::thread::scope(|scope| {
            let runs: Vec<_> = (start..start + width)
                .map(|n| {
                    let mut thread = std::thread::Builder::new();
                    if let Some(name) = &test {
                        thread = thread.name(name.clone());
                    }
                    (
                        n,
                        thread.spawn_scoped(scope, move || run(n)).expect("spawn"),
                    )
                })
                .collect();
            runs.into_iter()
                .map(|(n, handle)| (n, handle.join().expect("sweep run")))
                .collect()
        });
        for (n, answer) in batch {
            let done = finished(&answer);
            answers.push((n, answer));
            if done {
                return answers;
            }
        }
        assert!(start < 5_000, "the run never finished");
        start += width;
    }
}
