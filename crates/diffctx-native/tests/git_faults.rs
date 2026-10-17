// Fault injection for the git boundary (#356). A `git` earlier on PATH
// misbehaves on one subcommand and execs the real git for every other one, so
// each case drives the shipped binary through a real pipeline with exactly one
// stalled, malformed or leaking child. The contract under test: the run ends
// at its deadline, owns up to what it lost, and leaves none of its children
// behind. No case reads the clock (#510): a stall never ends on its own, so a
// run its deadline does not end hangs, and CI fails it at the job's timeout.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

use tempfile::TempDir;

static BIN: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    std::env::var("DIFFCTX_NATIVE_BIN")
        .unwrap_or_else(|_| env!("CARGO_BIN_EXE_diffctx").to_string())
});

const TIMEOUT_SECS: u64 = 3;
/// The deadline for runs that must return the whole answer, where nothing
/// stalls: one no runner reaches. Under the 3 s one, a slow macOS runner cut
/// the true answer short and the test read the disclosed partial as a
/// changed answer (#502).
const ANSWER_TIMEOUT_SECS: u64 = 600;
/// How many times a recorded pid is looked at, a poll slice apart, before it
/// counts as having outlived the run.
const SURVIVOR_CHECKS: usize = 30;
const CHECK_PAUSE: Duration = Duration::from_millis(100);

fn real_git() -> String {
    let out = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .expect("locate git");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

const FAKE_GIT: &str = r#"#!/bin/sh
echo "$*" >> "$FAKE_GIT_CALLS"
for a in "$@"; do
  if [ "$a" = "$FAKE_GIT_TARGET" ]; then hit=1; fi
done
if [ -z "$hit" ]; then exec "$REAL_GIT" "$@"; fi
echo $$ >> "$FAKE_GIT_PIDS"
stall() { while :; do sleep 60; done; }
case "$FAKE_GIT_MODE" in
  stall_before_read) stall ;;
  partial_header) read -r _; printf 'abc'; stall ;;
  stall_in_body) read -r _; printf '%s blob 100\n0123456789' 0123456789012345678901234567890123456789; stall ;;
  stall_in_oversized) read -r _; printf '%s blob 99000000\n0123456789' 0123456789012345678901234567890123456789; stall ;;
  malformed) read -r _; echo 'this is not a header'; stall ;;
  early_eof) exit 0 ;;
  fail) echo 'fatal: simulated failure' >&2; exit 128 ;;
  fail_after_first) [ $(wc -l < "$FAKE_GIT_PIDS") -gt 1 ] || exec "$REAL_GIT" "$@"; echo 'fatal: detected dubious ownership in repository' >&2; exit 128 ;;
  descendant_holds_stdout) stall & echo $! >> "$FAKE_GIT_PIDS"; exec "$REAL_GIT" "$@" ;;
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

    fn run(&self, target: &str, mode: &str, args: &[&str]) -> Output {
        let bin_dir = self.tmp.path().join("bin");
        let path = format!(
            "{}:{}",
            bin_dir.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        Command::new(&*BIN)
            .current_dir(self.repo())
            .args(args)
            .env("PATH", path)
            .env("REAL_GIT", real_git())
            .env("FAKE_GIT_TARGET", target)
            .env("FAKE_GIT_MODE", mode)
            .env("FAKE_GIT_PIDS", self.pids_file())
            .env("FAKE_GIT_CALLS", self.tmp.path().join("calls"))
            .env("DIFFCTX_NO_MARKER", "1")
            .output()
            .expect("run diffctx")
    }

    fn impact(&self, target: &str, mode: &str) -> Output {
        self.impact_within(target, mode, TIMEOUT_SECS)
    }

    fn impact_within(&self, target: &str, mode: &str, secs: u64) -> Output {
        let timeout = secs.to_string();
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

    fn pack(&self, target: &str, mode: &str, extra: &[&str]) -> Output {
        let timeout = TIMEOUT_SECS.to_string();
        let mut args = vec![
            ".",
            "--diff",
            &self.range,
            "-f",
            "json",
            "-q",
            "--timeout",
            &timeout,
        ];
        args.extend_from_slice(extra);
        self.run(target, mode, &args)
    }

    /// The fake git reached its target, and every pid it recorded is gone:
    /// killed and reaped, or re-parented and killed with its group — not left
    /// stalling.
    fn assert_no_survivors(&self) {
        let pids = std::fs::read_to_string(self.pids_file()).unwrap_or_default();
        assert!(
            !pids.trim().is_empty(),
            "the fake git never reached its target: the test measured nothing"
        );
        for pid in pids.split_whitespace() {
            let gone = (0..SURVIVOR_CHECKS).any(|_| {
                let dead = !running(pid);
                if !dead {
                    std::thread::sleep(CHECK_PAUSE);
                }
                dead
            });
            assert!(gone, "pid {pid} outlived the run");
        }
    }
}

/// A stall never ends on its own, so whatever a failed run left behind is
/// killed here with its group: a regression leaks nothing onto the machine.
impl Drop for Fixture {
    fn drop(&mut self) {
        let fake = self.tmp.path().join("bin").display().to_string();
        let pids = std::fs::read_to_string(self.pids_file()).unwrap_or_default();
        for pid in pids.split_whitespace() {
            if running(pid) && command_of(pid).contains(&fake) {
                let _ = Command::new("sh")
                    .args(["-c", &format!("kill -9 -{pid} {pid} 2>/dev/null")])
                    .status();
            }
        }
    }
}

fn command_of(pid: &str) -> String {
    let out = Command::new("ps")
        .args(["-o", "command=", "-p", pid])
        .output()
        .expect("ps");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Running, not merely present: a zombie has exited and waits for its reaper.
fn running(pid: &str) -> bool {
    let out = Command::new("ps")
        .args(["-o", "stat=", "-p", pid])
        .output()
        .expect("ps");
    let stat = String::from_utf8_lossy(&out.stdout);
    !stat.trim().is_empty() && !stat.trim_start().starts_with('Z')
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
    let out = fx.impact("cat-file", mode);
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
    let out = fx.impact_within("cat-file", mode, ANSWER_TIMEOUT_SECS);
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
    let _ = fx.impact("ls-files", "descendant_holds_stdout");
    fx.assert_no_survivors();
}

#[test]
fn a_flood_on_stderr_neither_deadlocks_nor_changes_the_answer() {
    let fx = fixture();
    let out = fx.impact_within("ls-files", "stderr_flood", ANSWER_TIMEOUT_SECS);
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
        let _ = fx.impact("cat-file", "stall_before_read");
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
    let out = fx.run(
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

/// The working tree's one change is a file git does not track, so all the
/// run can say about it comes from `ls-files --others` (#408).
fn untracked_change() -> Fixture {
    let mut fx = fixture();
    std::fs::write(
        fx.repo().join("refund.py"),
        "from pricing import total\n\n\ndef refund(cart):\n    return -total(cart)\n",
    )
    .unwrap();
    fx.range = "HEAD".to_string();
    fx
}

/// An untracked listing git refused is an error in git's own words, never
/// the clean tree an empty listing reads as.
#[test]
fn a_failing_untracked_scan_is_an_error_not_a_clean_tree() {
    let fx = untracked_change();
    for (mode, out) in [
        ("pack", fx.pack("--others", "fail", &[])),
        ("full", fx.pack("--others", "fail", &["--full"])),
        ("impact", fx.impact("--others", "fail")),
    ] {
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            out.status.code(),
            Some(3),
            "{mode}: stderr={stderr} stdout={}",
            String::from_utf8_lossy(&out.stdout)
        );
        assert!(
            stderr.contains("ls-files") && stderr.contains("simulated failure"),
            "{mode}: {stderr}"
        );
    }
}

/// An untracked listing the deadline cut short leaves an answer that names
/// the deadline: `empty`, or a coverage claiming completeness, would vouch
/// for a change the run never saw. `--full` has no partial form, so there
/// the cut is an error.
#[test]
fn a_stalled_untracked_scan_is_disclosed_as_partial() {
    let fx = untracked_change();
    let out = fx.impact("--others", "stall_before_read");
    let doc = json(&out);
    assert_eq!(doc["empty"], false, "{doc}");
    assert!(doc["limits"].to_string().contains("deadline"), "{doc}");

    let out = fx.pack("--others", "stall_before_read", &[]);
    let doc = json(&out);
    assert_eq!(doc["coverage"]["status"], "partial", "{doc}");
    assert!(
        doc["coverage"]["limit_reasons"]
            .to_string()
            .contains("deadline"),
        "{doc}"
    );

    let out = fx.pack("--others", "stall_before_read", &["--full"]);
    assert_eq!(
        out.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    fx.assert_no_survivors();
}

/// The last commit edits `vault.py`, which `.diffctx/ignore` withholds.
fn withheld_change() -> Fixture {
    let fx = fixture();
    let repo = fx.repo();
    std::fs::create_dir_all(repo.join(".diffctx")).unwrap();
    std::fs::write(repo.join(".diffctx/ignore"), "vault.py\n").unwrap();
    std::fs::write(repo.join("vault.py"), "TOKEN = 1\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "vault"]);
    std::fs::write(repo.join("vault.py"), "TOKEN = 2\n").unwrap();
    git(&repo, &["commit", "-qam", "rotate"]);
    fx
}

/// A listing the deadline cut short leaves the ignore check that follows
/// unable to ask git anything either. The path the policy withholds stays
/// withheld; it used to be published beside the count saying it was not.
#[test]
fn a_policy_the_deadline_left_unread_still_withholds() {
    let fx = withheld_change();
    for (mode, out) in [
        ("impact", fx.impact("--diff-filter=R", "stall_before_read")),
        ("pack", fx.pack("--diff-filter=R", "stall_before_read", &[])),
    ] {
        let doc = json(&out).to_string();
        assert!(!doc.contains("vault.py"), "{mode}: {doc}");
    }
    fx.assert_no_survivors();
}

/// git refusing to say whether this is a repository once the run is under
/// way — dubious ownership, a broken config, git gone — leaves the policy
/// unread just as the deadline does, and the path it withholds stays
/// withheld.
#[test]
fn a_policy_git_refused_to_read_still_withholds() {
    for mode in ["impact", "pack"] {
        let fx = withheld_change();
        let out = if mode == "impact" {
            fx.impact("--git-dir", "fail_after_first")
        } else {
            fx.pack("--git-dir", "fail_after_first", &[])
        };
        let doc = json(&out).to_string();
        assert!(!doc.contains("vault.py"), "{mode}: {doc}");
        let refusals = std::fs::read_to_string(fx.pids_file()).unwrap_or_default();
        assert!(
            refusals.lines().count() > 1,
            "{mode}: git never refused, the test measured nothing"
        );
    }
}

/// A run the deadline stopped before it found anything has not found a
/// clean tree: it exits 0 with its partial artifact and names the limit,
/// where it exited 4 with "check the range with: git diff --stat".
#[test]
fn a_deadline_that_found_nothing_names_the_limit_not_a_clean_tree() {
    let fx = untracked_change();
    for (mode, extra) in [("pack", &[][..]), ("locate", &["--mode", "locate"][..])] {
        let out = fx.pack("--others", "stall_before_read", extra);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(0), "{mode}: {stderr}");
        assert!(stderr.contains("deadline"), "{mode}: {stderr}");
        assert!(!stderr.contains("no semantic context"), "{mode}: {stderr}");
    }
    fx.assert_no_survivors();
}

/// A change with all three kinds of answer impact reads through git: a
/// caller of a changed definition, a caller of a removed one, and a line
/// still naming a deleted path.
fn sweep_fixture() -> Fixture {
    let fx = fixture();
    let repo = fx.repo();
    std::fs::write(
        repo.join("pricing.py"),
        "def total(items):\n    return sum(items) * 2\n\n\ndef legacy(items):\n    return items\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("old.py"),
        "from pricing import legacy\n\n\ndef use_legacy(cart):\n    return legacy(cart)\n",
    )
    .unwrap();
    std::fs::create_dir_all(repo.join("scripts")).unwrap();
    std::fs::write(repo.join("scripts/rotate.sh"), "echo rotate\n").unwrap();
    std::fs::write(repo.join("ops.md"), "Run scripts/rotate.sh weekly.\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "legacy and the script"]);
    std::fs::write(
        repo.join("pricing.py"),
        "def total(items):\n    return sum(items) * 3\n",
    )
    .unwrap();
    std::fs::remove_file(repo.join("scripts/rotate.sh")).unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "drop legacy and the script"]);
    // The range is the whole question: a working tree that lost every caller
    // and gained junk answers it only for a run that read the disk instead
    // of the snapshot, the way a failed `ls-tree` once did (#443).
    for gone in ["checkout.py", "old.py", "ops.md"] {
        std::fs::remove_file(repo.join(gone)).unwrap();
    }
    std::fs::write(repo.join("junk.py"), "def total():\n    return 0\n").unwrap();
    fx
}

/// What the run claims: the callers of each changed or removed symbol, and
/// the lines still naming a deleted path.
fn claims(doc: &serde_json::Value) -> Vec<String> {
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

/// Whether the run said it did not see everything.
fn disclosed(doc: &serde_json::Value) -> bool {
    let listed = |k: &str| doc[k].as_array().is_some_and(|a| !a.is_empty());
    listed("limits") || listed("unanalysed_files") || doc["withheld_files"].as_u64() > Some(0)
}

/// Every git read impact makes, failed in turn: the answer is an error, a
/// named loss, or the whole truth, never a smaller answer that looks
/// complete. A failed `git grep` used to drop the removed definition's
/// caller and the stale reference with exit 0 and no limit.
#[test]
fn every_failing_git_read_is_an_error_or_a_named_loss() {
    let fx = sweep_fixture();
    let calls = fx.tmp.path().join("calls");
    let _ = std::fs::remove_file(&calls);
    let out = fx.impact_within("no-such-verb", "fail", ANSWER_TIMEOUT_SECS);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let truth = claims(&json(&out));
    for expected in ["legacy <- old.py", "stale ops.md", "total <- checkout.py"] {
        assert!(truth.iter().any(|c| c == expected), "{expected}: {truth:?}");
    }
    let log = std::fs::read_to_string(&calls).expect("the fake git logged its calls");
    let mut verbs: Vec<String> = log
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            while let Some(w) = words.next() {
                match w {
                    "-c" | "-C" | "--git-dir" | "--work-tree" => {
                        words.next();
                    }
                    w if w.starts_with('-') => {}
                    w => return Some(w.to_string()),
                }
            }
            None
        })
        .collect();
    verbs.sort();
    verbs.dedup();
    assert!(verbs.iter().any(|v| v == "grep"), "{verbs:?}");
    for verb in &verbs {
        let out = fx.impact(verb, "fail");
        if !out.status.success() {
            continue;
        }
        let doc = json(&out);
        assert!(
            disclosed(&doc) || claims(&doc) == truth,
            "failing `git {verb}` gave a smaller answer that looks complete: {:?} vs {truth:?}\n{doc:#}",
            claims(&doc)
        );
    }
    fx.assert_no_survivors();
}

/// #439: `--full` reads only the changed files, and a stalled read of them
/// is a deadline the answer names — not "files over the size cap".
#[test]
fn a_stalled_full_read_names_the_deadline() {
    let fx = fixture();
    let out = fx.pack("cat-file", "stall_before_read", &["--full"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("over the size cap"), "{stderr}");
    let named = stderr.contains("deadline")
        || stderr.contains("timeout")
        || serde_json::from_slice::<serde_json::Value>(&out.stdout).is_ok_and(|doc| {
            doc["coverage"]["limit_reasons"]
                .to_string()
                .contains("deadline")
        });
    assert!(
        named,
        "stdout={} stderr={stderr}",
        String::from_utf8_lossy(&out.stdout)
    );
    fx.assert_no_survivors();
}
