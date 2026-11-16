//! A run that reaches its deadline answers with what it read, says so, and
//! exits 0: never an empty stdout and an error. Nothing here reads the clock — a
//! stalled git read ends on the stall itself, the layout pass on the logical
//! poll count.

use std::path::Path;

use tempfile::TempDir;

mod support;
use support::{commit_all, diffctx_unclocked, init_repo, sweep_polls, write};

/// One function of `runs` formatter-only edits (a trailing comma each, between
/// lines that do not change): `runs` changed runs in one file, one fragment.
fn reformatted_module(runs: usize) -> TempDir {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    let module = |comma: &str| {
        let mut text = String::from("def run(a, b):\n");
        for i in 0..runs {
            text.push_str(&format!("    v{i} = g(a, b{comma})\n    w{i} = {i}\n"));
        }
        text + "    return a\n"
    };
    write(repo, "m.py", &module(","));
    commit_all(repo, "base");
    write(repo, "m.py", &module(""));
    commit_all(repo, "format");
    tmp
}

fn answer_with_polls(repo: &Path, polls: u64) -> serde_json::Value {
    let out = diffctx_unclocked()
        .current_dir(repo)
        .env("DIFFCTX_TEST_DEADLINE_AFTER_POLLS", polls.to_string())
        .args(["--diff", "HEAD~1..HEAD", "-f", "json", "-q"])
        .output()
        .expect("run diffctx");
    assert!(
        out.status.success(),
        "poll {polls}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("json")
}

fn stopped(doc: &serde_json::Value) -> bool {
    doc["coverage"]["limits"].to_string().contains("deadline")
}

fn class_of(doc: &serde_json::Value, path: &str) -> String {
    doc["changes"]
        .as_array()
        .and_then(|all| all.iter().find(|c| c["path"] == path))
        .and_then(|c| c["class"].as_str())
        .unwrap_or("")
        .to_string()
}

/// The layout pass judges a file run by run and polls the run's deadline before
/// each: a deadline that falls inside one file's work stops it there, the runs
/// left are judged by their lines alone, and the file is not called layout. One
/// poll per file let a single huge module run on past any deadline.
#[test]
fn the_layout_pass_polls_the_deadline_for_each_run_of_a_file() {
    const RUNS: usize = 24;
    let tmp = reformatted_module(RUNS);
    let sweep = sweep_polls(|n| answer_with_polls(tmp.path(), n), |doc| !stopped(doc));
    let whole = &sweep.last().expect("a finished run").1;
    assert_eq!(class_of(whole, "m.py"), "layout", "{whole:#}");
    let stopped_inside = sweep
        .iter()
        .filter(|(_, doc)| stopped(doc) && class_of(doc, "m.py") != "layout")
        .count();
    assert!(
        stopped_inside >= RUNS,
        "{stopped_inside} stop points left the file unjudged, one per run is {RUNS}"
    );
}

#[cfg(unix)]
mod stalled_reads {
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::PathBuf;
    use std::process::Output;

    use super::*;

    /// A `git` that numbers its calls under `$SLOTS` and, for the one numbered
    /// `$STALL_INDEX`, leaves `$STALLED` and never answers: the event the run's
    /// stall knob waits for. Every other call is the real git.
    const STALLING_GIT: &str = r#"#!/bin/sh
until mkdir "$SLOTS/lock" 2>/dev/null; do :; done
read -r n < "$SLOTS/next"
echo $((n + 1)) > "$SLOTS/next"
echo "$n $*" >> "$SLOTS/calls"
rmdir "$SLOTS/lock"
if [ "$n" = "$STALL_INDEX" ]; then
  echo stalled >> "$STALLED"
  exec sleep 60
fi
exec "$REAL_GIT" "$@"
"#;

    struct Shim {
        dir: TempDir,
    }

    impl Shim {
        fn new() -> Self {
            let dir = TempDir::new().expect("tempdir");
            let bin = dir.path().join("bin");
            std::fs::create_dir_all(&bin).expect("mkdir");
            let git = bin.join("git");
            std::fs::write(&git, STALLING_GIT).expect("write shim");
            std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755)).expect("chmod");
            Self { dir }
        }

        /// One impact run on `repo` whose `stall`th git call never answers
        /// (`0`: none does), and the calls the run made, in order.
        fn run(&self, repo: &Path, stall: usize) -> (Output, Vec<String>) {
            let slots: PathBuf = self.dir.path().join(format!("slots-{stall}"));
            std::fs::create_dir_all(&slots).expect("mkdir");
            std::fs::write(slots.join("next"), "1\n").expect("write");
            std::fs::write(slots.join("calls"), "").expect("write");
            let stalled = slots.join("stalled");
            let real = support::scrubbed_sh(repo, "command -v git")
                .output()
                .expect("locate git");
            let path = format!(
                "{}:{}",
                self.dir.path().join("bin").display(),
                std::env::var("PATH").unwrap_or_default()
            );
            let out = diffctx_unclocked()
                .current_dir(repo)
                .env("PATH", path)
                .env("REAL_GIT", String::from_utf8_lossy(&real.stdout).trim())
                .env("SLOTS", &slots)
                .env("STALL_INDEX", stall.to_string())
                .env("STALLED", &stalled)
                .env("DIFFCTX_TEST_GIT_STALL_FILE", &stalled)
                .args([
                    "--diff",
                    "HEAD~1..HEAD",
                    "--mode",
                    "impact",
                    "-f",
                    "md",
                    "-q",
                ])
                .output()
                .expect("run diffctx");
            let calls = std::fs::read_to_string(slots.join("calls")).unwrap_or_default();
            (out, calls.lines().map(str::to_string).collect())
        }
    }

    /// Once the change is read, a revision git does not answer in time is a
    /// reason the answer is partial, not the end of the run. The provenance of the
    /// answer named both sides of the range with a read that failed the run on
    /// the deadline (exit 3, nothing on stdout), where a run past its deadline
    /// had always answered with what it held.
    #[test]
    fn a_revision_read_that_stalls_after_the_change_was_read_still_gets_an_answer() {
        let tmp = TempDir::new().expect("tempdir");
        let repo = tmp.path();
        init_repo(repo);
        write(repo, "util.py", "def add(a, b):\n    return a + b\n");
        write(
            repo,
            "app.py",
            "from util import add\n\n\ndef main():\n    return add(1, 2)\n",
        );
        commit_all(repo, "initial");
        write(
            repo,
            "util.py",
            "def add(a, b):\n    return a + b\n\n\ndef sub(a, b):\n    return a - b\n",
        );
        commit_all(repo, "add sub");

        let shim = Shim::new();
        let (whole, calls) = shim.run(repo, 0);
        assert!(whole.status.success(), "{whole:?}");
        let listed = calls
            .iter()
            .position(|c| c.contains(" diff "))
            .expect("the run read the change");
        let revisions: Vec<usize> = calls
            .iter()
            .enumerate()
            .filter(|(i, c)| *i > listed && c.contains("rev-parse --verify --quiet"))
            .map(|(i, _)| i + 1)
            .collect();
        assert!(revisions.len() >= 2, "{calls:#?}");
        for stall in revisions {
            let (out, _) = shim.run(repo, stall);
            let (stdout, stderr) = (
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr),
            );
            assert_eq!(out.status.code(), Some(0), "call {stall}: {stderr}");
            assert!(
                stdout.contains("partial: deadline"),
                "call {stall}: {stdout}"
            );
        }
    }
}
