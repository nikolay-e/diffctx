// Fault injection for the git boundary (#356). A `git` earlier on PATH
// misbehaves on one subcommand and execs the real git for every other one, so
// each case drives the shipped binary through a real pipeline with exactly one
// stalled, malformed or leaking child. The contract under test: the run ends
// within its deadline plus a cleanup allowance, owns up to what it lost, and
// leaves none of its children behind.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use tempfile::TempDir;

static BIN: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    std::env::var("DIFFCTX_NATIVE_BIN")
        .unwrap_or_else(|_| env!("CARGO_BIN_EXE_diffctx").to_string())
});

const TIMEOUT_SECS: u64 = 3;
/// Deadline plus the cleanup allowance, plus process start-up on a loaded CI
/// runner; a hang is minutes, so the margin cannot hide one.
const BOUND: Duration = Duration::from_secs(TIMEOUT_SECS + 2 + 3);

fn real_git() -> String {
    let out = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .expect("locate git");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

const FAKE_GIT: &str = r#"#!/bin/sh
for a in "$@"; do
  if [ "$a" = "$FAKE_GIT_TARGET" ]; then hit=1; fi
done
if [ -z "$hit" ]; then exec "$REAL_GIT" "$@"; fi
echo $$ >> "$FAKE_GIT_PIDS"
case "$FAKE_GIT_MODE" in
  stall_before_read) exec sleep 600 ;;
  partial_header) read -r _; printf 'abc'; exec sleep 600 ;;
  stall_in_body) read -r _; printf '%s blob 100\n0123456789' 0123456789012345678901234567890123456789; exec sleep 600 ;;
  stall_in_oversized) read -r _; printf '%s blob 99000000\n0123456789' 0123456789012345678901234567890123456789; exec sleep 600 ;;
  malformed) read -r _; echo 'this is not a header'; exec sleep 600 ;;
  early_eof) exit 0 ;;
  descendant_holds_stdout) sleep 600 & echo $! >> "$FAKE_GIT_PIDS"; exec "$REAL_GIT" "$@" ;;
  stderr_flood) head -c 4000000 /dev/zero | tr '\0' 'e' >&2; exec "$REAL_GIT" "$@" ;;
esac
exec "$REAL_GIT" "$@"
"#;

struct Fixture {
    tmp: TempDir,
    range: String,
}

impl Fixture {
    fn repo(&self) -> PathBuf {
        self.tmp.path().join("repo")
    }

    fn pids_file(&self) -> PathBuf {
        self.tmp.path().join("pids")
    }

    fn run(&self, target: &str, mode: &str, args: &[&str]) -> (Output, Duration) {
        let bin_dir = self.tmp.path().join("bin");
        let path = format!(
            "{}:{}",
            bin_dir.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let started = Instant::now();
        let out = Command::new(&*BIN)
            .current_dir(self.repo())
            .args(args)
            .env("PATH", path)
            .env("REAL_GIT", real_git())
            .env("FAKE_GIT_TARGET", target)
            .env("FAKE_GIT_MODE", mode)
            .env("FAKE_GIT_PIDS", self.pids_file())
            .env("DIFFCTX_NO_MARKER", "1")
            .output()
            .expect("run diffctx");
        (out, started.elapsed())
    }

    fn impact(&self, target: &str, mode: &str) -> (Output, Duration) {
        let timeout = TIMEOUT_SECS.to_string();
        self.run(
            target,
            mode,
            &[
                ".",
                "--diff",
                &self.range,
                "--mode",
                "impact",
                "-f",
                "json",
                "-q",
                "--timeout",
                &timeout,
            ],
        )
    }

    /// Every pid the fake git recorded is gone: killed and reaped, or
    /// re-parented and killed with its group — not left sleeping.
    fn assert_no_survivors(&self) {
        let pids = std::fs::read_to_string(self.pids_file()).unwrap_or_default();
        let deadline = Instant::now() + Duration::from_secs(3);
        for pid in pids.split_whitespace() {
            loop {
                let alive = Command::new("kill")
                    .args(["-0", pid])
                    .stderr(std::process::Stdio::null())
                    .status()
                    .expect("kill -0")
                    .success();
                if !alive {
                    break;
                }
                assert!(Instant::now() < deadline, "pid {pid} outlived the run");
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(repo)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .args(args)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

fn fixture() -> Fixture {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path().join("repo");
    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    let fake = bin.join("git");
    std::fs::write(&fake, FAKE_GIT).unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    git(&repo, &["init", "-q", "."]);
    git(&repo, &["config", "user.email", "test@example.com"]);
    git(&repo, &["config", "user.name", "diffctx tests"]);
    std::fs::write(
        repo.join("pricing.py"),
        "def total(items):\n    return sum(items)\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("checkout.py"),
        "from pricing import total\n\n\ndef charge(cart):\n    return total(cart)\n",
    )
    .unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "initial"]);
    std::fs::write(
        repo.join("pricing.py"),
        "def total(items):\n    return sum(items) * 2\n",
    )
    .unwrap();
    git(&repo, &["commit", "-qam", "double"]);
    Fixture {
        tmp,
        range: "HEAD~1..HEAD".to_string(),
    }
}

fn json(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "not JSON ({e}): stdout={} stderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

fn callers_of_total(doc: &serde_json::Value) -> Vec<String> {
    doc["changed"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["symbol"] == "total")
        .flat_map(|c| c["callers"].as_array().cloned().unwrap_or_default())
        .filter_map(|c| c["symbol"].as_str().map(str::to_string))
        .collect()
}

/// A stalled reader: the run is bounded, says it hit the deadline, and makes
/// no claim about the callers it could not read.
fn assert_stall_is_bounded_and_disclosed(mode: &str) {
    let fx = fixture();
    let (out, elapsed) = fx.impact("cat-file", mode);
    assert!(elapsed < BOUND, "{mode}: took {elapsed:?}");
    // Either an error exit or a partial artifact that names the deadline;
    // never a clean answer about callers it could not read.
    if out.status.success() {
        let doc = json(&out);
        let limits = doc["limits"].to_string();
        assert!(
            limits.contains("deadline"),
            "{mode}: a stalled read must be disclosed, got {doc}"
        );
        assert!(callers_of_total(&doc).is_empty(), "{mode}: {doc}");
    }
    fx.assert_no_survivors();
}

#[test]
fn a_cat_file_that_never_reads_its_request_is_bounded() {
    assert_stall_is_bounded_and_disclosed("stall_before_read");
}

#[test]
fn a_cat_file_stalled_mid_header_is_bounded() {
    assert_stall_is_bounded_and_disclosed("partial_header");
}

#[test]
fn a_cat_file_stalled_mid_body_is_bounded() {
    assert_stall_is_bounded_and_disclosed("stall_in_body");
}

#[test]
fn a_cat_file_stalled_while_draining_an_oversized_blob_is_bounded() {
    assert_stall_is_bounded_and_disclosed("stall_in_oversized");
}

/// A broken stream is discarded and the file asked through argv: the answer
/// is the one the real git gives, not a guess and not a hang.
fn assert_recovers_the_true_answer(mode: &str) {
    let fx = fixture();
    let (out, elapsed) = fx.impact("cat-file", mode);
    assert!(elapsed < BOUND, "{mode}: took {elapsed:?}");
    assert!(
        out.status.success(),
        "{mode}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        callers_of_total(&json(&out)),
        vec!["charge".to_string()],
        "{mode}"
    );
    fx.assert_no_survivors();
}

#[test]
fn a_malformed_header_discards_the_stream_and_recovers() {
    assert_recovers_the_true_answer("malformed");
}

#[test]
fn a_cat_file_that_exits_at_once_is_recovered_from() {
    assert_recovers_the_true_answer("early_eof");
}

#[test]
fn a_descendant_holding_stdout_cannot_hold_the_run() {
    let fx = fixture();
    let (out, elapsed) = fx.impact("ls-files", "descendant_holds_stdout");
    assert!(elapsed < BOUND, "took {elapsed:?}");
    let _ = out;
    fx.assert_no_survivors();
}

#[test]
fn a_flood_on_stderr_neither_deadlocks_nor_changes_the_answer() {
    let fx = fixture();
    let (out, elapsed) = fx.impact("ls-files", "stderr_flood");
    assert!(elapsed < BOUND, "took {elapsed:?}");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(callers_of_total(&json(&out)), vec!["charge".to_string()]);
}

#[test]
fn repeated_stalls_are_each_bounded_and_cleaned_up() {
    let fx = fixture();
    for _ in 0..3 {
        let (_, elapsed) = fx.impact("cat-file", "stall_before_read");
        assert!(elapsed < BOUND, "took {elapsed:?}");
    }
    fx.assert_no_survivors();
}

#[test]
fn a_control_character_path_still_reads_its_own_content() {
    let fx = fixture();
    let repo = fx.repo();
    let odd = "we\nird.py";
    std::fs::write(
        repo.join(odd),
        "from pricing import total\n\n\ndef weird(c):\n    return total(c)\n",
    )
    .unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "odd name"]);
    std::fs::write(
        repo.join("pricing.py"),
        "def total(items):\n    return sum(items) * 3\n",
    )
    .unwrap();
    git(&repo, &["commit", "-qam", "triple"]);
    let (out, _) = fx.run(
        "none",
        "none",
        &[
            ".",
            "--diff",
            "HEAD~1..HEAD",
            "--mode",
            "impact",
            "-f",
            "json",
            "-q",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut callers = callers_of_total(&json(&out));
    callers.sort();
    assert_eq!(callers, vec!["charge".to_string(), "weird".to_string()]);
}
