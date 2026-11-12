// Fault injection for the git boundary (#356). A `git` earlier on PATH
// misbehaves on one subcommand and execs the real git for every other one, so
// each case drives the shipped binary through a real pipeline with exactly one
// stalled, malformed or leaking child. The contract under test: the run ends
// at its deadline, owns up to what it lost, and leaves none of its children
// behind. No case reads the clock (#510): a stall never ends on its own, so a
// run its deadline does not end hangs, and CI fails it at the job's timeout.
#![cfg(unix)]

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

use tempfile::TempDir;

mod support;
use support::answer::{
    changed_rows, coverage_disclosed, fragments, impact_disclosed, items, relations,
};
use support::fake_git::FAKE_GIT;
use support::seeds::{Edit, Knobs, Lang, RANGE, seed};
use support::{claims, diffctx, git};

/// The `--timeout` of every run: one no runner reaches. Under a 3 s one, a
/// slow macOS runner cut the true answer short and the test read the
/// disclosed partial as a changed answer (#502).
const ANSWER_TIMEOUT_SECS: u64 = 600;
/// The knob a stalled run ends by: a blocked git read gives up after this many
/// empty waits, counted, not timed (fj#172).
const STALL_KNOB: (&str, &str) = ("DIFFCTX_TEST_GIT_STALL_AFTER_WAITS", "10");

fn real_git() -> String {
    let out = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .expect("locate git");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

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

    /// A FIFO every run holds open for writing from its first instruction, and
    /// so does everything the run starts: git, the fake's shell, its sleeps.
    fn sentinel(&self) -> std::ffi::CString {
        let path = self.tmp.path().join("sentinel");
        std::ffi::CString::new(path.into_os_string().into_encoded_bytes()).unwrap()
    }

    fn run(&self, target: &str, mode: &str, args: &[&str]) -> Output {
        self.run_with(target, mode, args, &[])
    }

    fn run_with(
        &self,
        target: &str,
        mode: &str,
        args: &[&str],
        extra_env: &[(&str, &str)],
    ) -> Output {
        let bin_dir = self.tmp.path().join("bin");
        let path = format!(
            "{}:{}",
            bin_dir.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let sentinel = self.sentinel();
        // SAFETY: mkfifo(2) only creates the path; an existing one is the
        // earlier run's.
        #[allow(unsafe_code)]
        unsafe {
            libc::mkfifo(sentinel.as_ptr(), 0o600);
        }
        let mut command = diffctx();
        // SAFETY: between fork and exec only open(2), which is async-signal
        // safe, runs. O_RDWR, because a write-only open of a FIFO with no
        // reader blocks; the descriptor is not close-on-exec, so it follows
        // every descendant.
        #[allow(unsafe_code)]
        unsafe {
            use std::os::unix::process::CommandExt as _;
            command.pre_exec(move || {
                libc::open(sentinel.as_ptr(), libc::O_RDWR);
                Ok(())
            });
        }
        command
            .current_dir(self.repo())
            .args(args)
            .env("PATH", path)
            .env("REAL_GIT", real_git())
            .env("FAKE_GIT_TARGET", target)
            .env("FAKE_GIT_MODE", mode)
            .env("FAKE_GIT_PIDS", self.pids_file())
            .env("FAKE_GIT_CALLS", self.tmp.path().join("calls"))
            .env("DIFFCTX_NO_MARKER", "1");
        for (key, value) in extra_env {
            command.env(key, value);
        }
        command.output().expect("run diffctx")
    }

    /// The run a stall ends: the knob stops the blocked read.
    fn impact(&self, target: &str, mode: &str) -> Output {
        self.impact_with(target, mode, &[STALL_KNOB])
    }

    /// A run nothing stalls and nothing cuts short.
    fn impact_whole(&self, target: &str, mode: &str) -> Output {
        self.impact_with(target, mode, &[])
    }

    fn impact_with(&self, target: &str, mode: &str, env: &[(&str, &str)]) -> Output {
        let timeout = ANSWER_TIMEOUT_SECS.to_string();
        self.run_with(
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
            env,
        )
    }

    fn pack(&self, target: &str, mode: &str, extra: &[&str]) -> Output {
        self.pack_with(target, mode, extra, &[STALL_KNOB])
    }

    fn pack_whole(&self, target: &str, mode: &str, extra: &[&str]) -> Output {
        self.pack_with(target, mode, extra, &[])
    }

    fn pack_with(&self, target: &str, mode: &str, extra: &[&str], env: &[(&str, &str)]) -> Output {
        let timeout = ANSWER_TIMEOUT_SECS.to_string();
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
        self.run_with(target, mode, &args, env)
    }

    /// The fake git reached its target, and nothing the run started is left
    /// running: killed and reaped, or re-parented and killed with its group —
    /// not left stalling. The sentinel is held open by the run and all its
    /// descendants, so the read ends exactly when the last of them has exited,
    /// however long that takes; a stall the run left behind never ends, and the
    /// job's own timeout fails the test (#510).
    fn assert_no_survivors(&self) {
        let pids = std::fs::read_to_string(self.pids_file()).unwrap_or_default();
        assert!(
            !pids.trim().is_empty(),
            "the fake git never reached its target: the test measured nothing"
        );
        let sentinel = self.sentinel();
        // SAFETY: opens a path we created, then reads into a local buffer.
        #[allow(unsafe_code)]
        unsafe {
            let fd = libc::open(sentinel.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK);
            assert!(fd >= 0, "the sentinel cannot be opened");
            libc::fcntl(fd, libc::F_SETFL, 0);
            let mut byte = 0u8;
            loop {
                let n = libc::read(fd, (&raw mut byte).cast(), 1);
                let interrupted =
                    std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted;
                if n > 0 || (n < 0 && interrupted) {
                    continue;
                }
                break;
            }
            libc::close(fd);
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

/// An empty `repo` beside a `bin` whose `git` is the fake one.
fn scratch() -> TempDir {
    let tmp = TempDir::new().expect("tempdir");
    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(tmp.path().join("repo")).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    let fake = bin.join("git");
    std::fs::write(&fake, FAKE_GIT).unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    tmp
}

fn fixture() -> Fixture {
    let tmp = scratch();
    let repo = tmp.path().join("repo");
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
    doc["definitions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["symbol"] == "total")
        .flat_map(|c| c["rows"].as_array().cloned().unwrap_or_default())
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
        let limits = doc["coverage"]["limits"].to_string();
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

/// A batch that breaks on the same object twice leaves no bytes to answer
/// from: the run ends in an error or names the loss, never a smaller answer
/// that looks complete, and git is not asked another way.
fn assert_error_or_named_loss(mode: &str) {
    let fx = fixture();
    let clean = fx.impact_whole("none", "none");
    let truth = Query::Impact.claims(&json(&clean));
    assert!(!truth.is_empty(), "the clean run claimed nothing");
    let out = fx.impact_whole("cat-file", mode);
    verdict(Query::Impact, &truth, &out).unwrap_or_else(|why| panic!("{mode}: {why}"));
    fx.assert_no_survivors();
}

#[test]
fn a_malformed_header_is_an_error_or_a_named_loss() {
    assert_error_or_named_loss("malformed");
}

#[test]
fn a_cat_file_that_exits_at_once_is_an_error_or_a_named_loss() {
    assert_error_or_named_loss("early_eof");
}

#[test]
fn a_cat_file_that_fails_is_an_error_or_a_named_loss() {
    assert_error_or_named_loss("fail");
}

#[test]
fn a_descendant_holding_stdout_cannot_hold_the_run() {
    let fx = fixture();
    let _ = fx.impact("ls-tree", "descendant_holds_stdout");
    fx.assert_no_survivors();
}

#[test]
fn a_flood_on_stderr_neither_deadlocks_nor_changes_the_answer() {
    let fx = fixture();
    let out = fx.impact_whole("ls-tree", "stderr_flood");
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
        ("pack", fx.pack_whole("--others", "fail", &[])),
        ("full", fx.pack_whole("--others", "fail", &["--full"])),
        ("impact", fx.impact_whole("--others", "fail")),
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

/// The reads that title a range, count its commits and find its work tree are
/// the run's own: git refusing one ends the run in git's own words (exit 3),
/// where a missing title, a count of zero or the checkout's own path would
/// have read as a change with no history. Each target is the argument only
/// that call carries.
#[test]
fn a_refused_range_read_is_an_error_not_a_range_with_no_history() {
    for (target, subcommand) in [
        ("--format=%B%x1e", "log"),
        ("--format=%s%n%b", "log"),
        ("--count", "rev-list"),
        ("--show-toplevel", "rev-parse"),
    ] {
        let fx = fixture();
        for (mode, out) in [
            ("pack", fx.pack_whole(target, "fail", &[])),
            ("impact", fx.impact_whole(target, "fail")),
        ] {
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert_eq!(
                out.status.code(),
                Some(3),
                "{target} {mode}: stderr={stderr} stdout={}",
                String::from_utf8_lossy(&out.stdout)
            );
            assert!(
                stderr.contains(subcommand) && stderr.contains("simulated failure"),
                "{target} {mode}: {stderr}"
            );
        }
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
    assert!(
        doc["coverage"]["limits"].to_string().contains("deadline"),
        "{doc}"
    );

    let out = fx.pack("--others", "stall_before_read", &[]);
    let doc = json(&out);
    assert_eq!(doc["coverage"]["status"], "partial", "{doc}");
    assert!(
        doc["coverage"]["limits"].to_string().contains("deadline"),
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
        ("impact", fx.impact("--name-status", "stall_before_read")),
        ("pack", fx.pack("--name-status", "stall_before_read", &[])),
    ] {
        let doc = json(&out).to_string();
        assert!(!doc.contains("vault.py"), "{mode}: {doc}");
    }
    fx.assert_no_survivors();
}

/// git refusing to list the snapshot the policy is read from — dubious
/// ownership, a broken config, git gone — leaves the policy unread just as
/// the deadline does, and the path it withholds is not published: the run
/// ends in git's own words instead of answering from a snapshot it could not
/// admit (fj#130). It asks once: the listing is an answer the run keeps.
#[test]
fn a_policy_git_refused_to_read_still_withholds() {
    for mode in ["impact", "pack"] {
        let fx = withheld_change();
        let out = if mode == "impact" {
            fx.impact_whole("ls-tree", "fail")
        } else {
            fx.pack_whole("ls-tree", "fail", &[])
        };
        assert_eq!(out.status.code(), Some(3), "{mode}");
        let published = String::from_utf8_lossy(&out.stdout);
        assert!(!published.contains("vault.py"), "{mode}: {published}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("simulated failure"), "{mode}: {stderr}");
        let refusals = std::fs::read_to_string(fx.pids_file()).unwrap_or_default();
        assert!(
            !refusals.trim().is_empty(),
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

/// Every git read impact makes, failed in turn: the answer is an error, a
/// named loss, or the whole truth, never a smaller answer that looks
/// complete. A failed `git grep` used to drop the removed definition's
/// caller and the stale reference with exit 0 and no limit.
#[test]
fn every_failing_git_read_is_an_error_or_a_named_loss() {
    let fx = sweep_fixture();
    let calls = fx.tmp.path().join("calls");
    let _ = std::fs::remove_file(&calls);
    let out = fx.impact_whole("no-such-verb", "fail");
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
        let out = fx.impact_whole(verb, "fail");
        if !out.status.success() {
            continue;
        }
        let doc = json(&out);
        assert!(
            impact_disclosed(&doc) || claims(&doc) == truth,
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
        || serde_json::from_slice::<serde_json::Value>(&out.stdout)
            .is_ok_and(|doc| doc["coverage"]["limits"].to_string().contains("deadline"));
    assert!(
        named,
        "stdout={} stderr={stderr}",
        String::from_utf8_lossy(&out.stdout)
    );
    fx.assert_no_survivors();
}

// ---- every call of a run, failed in turn (arch migration, stage 1.5) ----

// Sweeps that fail today, test name -> the issue that tracks it (`gh#N` or
// `fj#N`). Checked both ways: a listed sweep that passes fails until it leaves
// the list.
const KNOWN_RED: &[(&str, &str)] = &[];

static SWEEPS: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy, Debug)]
enum Query {
    Impact,
    Symbol,
    Pack,
    Locate,
}

impl Query {
    fn args<'a>(self, range: &'a str, symbol: &'a str) -> Vec<&'a str> {
        let mut args = vec!["."];
        match self {
            Self::Impact => args.extend(["--diff", range, "--mode", "impact", "-f", "json"]),
            Self::Symbol => args.extend(["--symbol", symbol, "-f", "json"]),
            Self::Pack => args.extend(["--diff", range, "-f", "json"]),
            Self::Locate => args.extend(["--diff", range, "--mode", "locate"]),
        }
        args.push("-q");
        args
    }

    /// What the answer claims: callers, stale references and the definitions
    /// for impact and symbol, the fragments for pack, the items for locate.
    fn claims(self, doc: &serde_json::Value) -> BTreeSet<String> {
        match self {
            Self::Impact | Self::Symbol => {
                let mut all = relations(doc);
                all.extend(changed_rows(doc));
                all
            }
            Self::Pack => fragments(doc),
            Self::Locate => items(doc),
        }
    }

    fn disclosed(self, doc: &serde_json::Value) -> bool {
        match self {
            Self::Impact | Self::Symbol => impact_disclosed(doc),
            Self::Pack | Self::Locate => coverage_disclosed(doc),
        }
    }
}

impl Fixture {
    /// The query, with the call numbered `fail_at` failed (none: a clean run),
    /// and the directory the fake git logged the calls it saw in, in order.
    fn query_failing(
        &self,
        query: Query,
        symbol: &str,
        fail_at: Option<usize>,
    ) -> (Output, PathBuf) {
        let slots = self
            .tmp
            .path()
            .join(format!("slots-{}", SWEEPS.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&slots).unwrap();
        std::fs::write(slots.join("next"), "0\n").unwrap();
        let fail = fail_at.map_or_else(|| "none".to_string(), |n| n.to_string());
        let args = query.args(&self.range, symbol);
        let out = self.run_with(
            "no-such-verb",
            "none",
            &args,
            &[
                ("FAKE_GIT_SLOTS", slots.to_str().expect("utf-8 path")),
                ("FAKE_GIT_FAIL_INDEX", &fail),
            ],
        );
        (out, slots)
    }
}

/// A failed read ends the run with git's words and a code, or leaves an
/// answer that names its loss, or leaves the answer the clean run gave.
fn verdict(query: Query, truth: &BTreeSet<String>, out: &Output) -> Result<(), String> {
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        return match out.status.code() {
            Some(1..=4) if !stderr.trim().is_empty() => Ok(()),
            code => Err(format!(
                "exit {code:?} with stderr {stderr:?}: an error names what failed and exits with its code"
            )),
        };
    }
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| format!("exit 0 with stdout that is not JSON ({e})"))?;
    let claims = query.claims(&doc);
    if query.disclosed(&doc) || &claims == truth {
        return Ok(());
    }
    Err(format!(
        "a smaller answer that looks complete\nmissing: {:?}\nextra: {:?}",
        truth.difference(&claims).collect::<Vec<_>>(),
        claims.difference(truth).collect::<Vec<_>>()
    ))
}

/// Every git call the clean run makes, failed in turn, for one language's
/// seed and one kind of query (#623, #443, #505).
fn sweep(id: &str, lang: Lang, query: Query) {
    let knobs = Knobs {
        stale: true,
        extra_callers: 2,
        ..Knobs::default()
    };
    let planted = seed(lang, Edit::Signature, &knobs);
    let tmp = scratch();
    planted.commit_into(&tmp.path().join("repo"));
    let fx = Fixture {
        tmp,
        range: RANGE.to_string(),
    };
    let symbol = format!("{}:{}", planted.target_path, planted.target);
    let (clean, slots) = fx.query_failing(query, &symbol, None);
    assert!(
        clean.status.success(),
        "{lang:?} {query:?}: the clean run failed: {}",
        String::from_utf8_lossy(&clean.stderr)
    );
    let truth = query.claims(&json(&clean));
    assert!(
        !truth.is_empty(),
        "{lang:?} {query:?}: the clean run claimed nothing, so the sweep measures nothing"
    );
    let log = std::fs::read_to_string(slots.join("calls")).expect("the fake git logged its calls");
    let calls: Vec<&str> = log.lines().collect();
    assert!(!calls.is_empty(), "the fake git saw no call");
    let broken: Vec<String> = calls
        .iter()
        .enumerate()
        .filter_map(|(index, call)| {
            let (out, _) = fx.query_failing(query, &symbol, Some(index));
            verdict(query, &truth, &out)
                .err()
                .map(|why| format!("call {index} (`git {call}`): {why}"))
        })
        .collect();
    let known = KNOWN_RED.iter().find(|(name, _)| *name == id);
    match known {
        None => assert!(
            broken.is_empty(),
            "{lang:?} {query:?}: {} of {} failed calls left an answer that is neither an error nor a named loss\n{}",
            broken.len(),
            calls.len(),
            broken.join("\n")
        ),
        Some((_, issue)) => assert!(
            !broken.is_empty(),
            "{id} is listed as known red ({issue}) but passes: remove it from KNOWN_RED"
        ),
    }
    fx.assert_no_survivors();
}

macro_rules! sweeps {
    ($($name:ident: $lang:ident, $query:ident;)+) => {
        mod every_git_call_failed_in_turn {
            use super::{Lang, Query, sweep};

            $(
                #[test]
                fn $name() {
                    sweep(stringify!($name), Lang::$lang, Query::$query);
                }
            )+
        }
    };
}

sweeps! {
    python_impact: Python, Impact;
    python_symbol: Python, Symbol;
    python_pack: Python, Pack;
    python_locate: Python, Locate;
    typescript_impact: TypeScript, Impact;
    typescript_symbol: TypeScript, Symbol;
    typescript_pack: TypeScript, Pack;
    typescript_locate: TypeScript, Locate;
    rust_impact: Rust, Impact;
    rust_symbol: Rust, Symbol;
    rust_pack: Rust, Pack;
    rust_locate: Rust, Locate;
    kotlin_impact: Kotlin, Impact;
    kotlin_symbol: Kotlin, Symbol;
    kotlin_pack: Kotlin, Pack;
    kotlin_locate: Kotlin, Locate;
}

// ---- every blob read of a run, failed in turn (arch migration, wave 2.5) ----

// Read sweeps that fail today, test name -> the issue that tracks it. Checked
// both ways, as `KNOWN_RED`.
const READ_KNOWN_RED: &[(&str, &str)] = &[];

/// Every blob the snapshot layer reads for one language's seed and one kind
/// of query, failed in turn on either side of the range
/// (`DIFFCTX_TEST_READ_FAULT=n` fails the n-th read): the answer is an error,
/// a named loss or the whole truth, never a smaller answer that looks
/// complete. The git sweep fails calls; this one fails the reads a caller can
/// swallow (m3).
fn read_sweep(id: &str, lang: Lang, query: Query) {
    let knobs = Knobs {
        stale: true,
        extra_callers: 2,
        ..Knobs::default()
    };
    let planted = seed(lang, Edit::Signature, &knobs);
    let tmp = scratch();
    planted.commit_into(&tmp.path().join("repo"));
    let fx = Fixture {
        tmp,
        range: RANGE.to_string(),
    };
    let symbol = format!("{}:{}", planted.target_path, planted.target);
    let args = query.args(&fx.range, &symbol);
    let run = |fail: Option<usize>| {
        let at = fail.map(|n| n.to_string());
        let env: Vec<(&str, &str)> = at
            .as_deref()
            .map(|n| ("DIFFCTX_TEST_READ_FAULT", n))
            .into_iter()
            .collect();
        fx.run_with("no-such-verb", "none", &args, &env)
    };
    let clean = run(None);
    assert!(
        clean.status.success(),
        "{lang:?} {query:?}: the clean run failed: {}",
        String::from_utf8_lossy(&clean.stderr)
    );
    let truth = query.claims(&json(&clean));
    assert!(
        !truth.is_empty(),
        "{lang:?} {query:?}: the clean run claimed nothing, so the sweep measures nothing"
    );
    let mut broken: Vec<String> = Vec::new();
    let mut struck = 0;
    loop {
        let out = run(Some(struck));
        if !String::from_utf8_lossy(&out.stderr).contains("injected read fault") {
            break;
        }
        if let Err(why) = verdict(query, &truth, &out) {
            broken.push(format!("blob read {struck}: {why}"));
        }
        struck += 1;
        assert!(struck < 5_000, "{lang:?} {query:?}: the sweep never ended");
    }
    assert!(
        struck > 0,
        "{lang:?} {query:?}: no read was struck, so the sweep measures nothing"
    );
    match READ_KNOWN_RED.iter().find(|(name, _)| *name == id) {
        None => assert!(
            broken.is_empty(),
            "{lang:?} {query:?}: {} of {struck} failed reads left an answer that is neither an error nor a named loss\n{}",
            broken.len(),
            broken.join("\n")
        ),
        Some((_, issue)) => assert!(
            !broken.is_empty(),
            "{id} is listed as known red ({issue}) but passes: remove it from READ_KNOWN_RED"
        ),
    }
}

macro_rules! read_sweeps {
    ($($name:ident: $lang:ident, $query:ident;)+) => {
        mod every_blob_read_failed_in_turn {
            use super::{Lang, Query, read_sweep};

            $(
                #[test]
                fn $name() {
                    read_sweep(stringify!($name), Lang::$lang, Query::$query);
                }
            )+
        }
    };
}

read_sweeps! {
    python_impact: Python, Impact;
    python_symbol: Python, Symbol;
    python_pack: Python, Pack;
    python_locate: Python, Locate;
    typescript_impact: TypeScript, Impact;
    typescript_symbol: TypeScript, Symbol;
    typescript_pack: TypeScript, Pack;
    typescript_locate: TypeScript, Locate;
    rust_impact: Rust, Impact;
    rust_symbol: Rust, Symbol;
    rust_pack: Rust, Pack;
    rust_locate: Rust, Locate;
    kotlin_impact: Kotlin, Impact;
    kotlin_symbol: Kotlin, Symbol;
    kotlin_pack: Kotlin, Pack;
    kotlin_locate: Kotlin, Locate;
}

// ---- a read of the base side that failed under a removed definition (m3) ----

/// Files that still call the removed method.
const CALLERS: usize = 6;

/// A method the range removes, and `CALLERS` files that still call it through a
/// variable typed by an assignment: the base side reads each file's text, one
/// blob read each, to know that `gadget.run()` was a call of `Gadget.run`. Those
/// reads are the base reads the cell makes beyond the run's own, which is what a
/// fault aimed at base reads alone has to reach.
fn removed_method() -> Fixture {
    let tmp = scratch();
    let repo = tmp.path().join("repo");
    git(&repo, &["init", "-q", "."]);
    git(&repo, &["config", "user.email", "test@example.com"]);
    git(&repo, &["config", "user.name", "diffctx tests"]);
    std::fs::write(
        repo.join("gadget.py"),
        "class Gadget:\n    def run(self):\n        return 1\n\n    def stop(self):\n        return 0\n",
    )
    .unwrap();
    for k in 1..=CALLERS {
        std::fs::write(
            repo.join(format!("caller{k}.py")),
            format!(
                "from gadget import Gadget\n\n\ndef drive{k}():\n    gadget = Gadget()\n    return gadget.run()\n"
            ),
        )
        .unwrap();
    }
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "initial"]);
    std::fs::write(
        repo.join("gadget.py"),
        "class Gadget:\n    def stop(self):\n        return 0\n",
    )
    .unwrap();
    git(&repo, &["commit", "-qam", "drop run"]);
    Fixture {
        tmp,
        range: "HEAD~1..HEAD".to_string(),
    }
}

/// The row of the removed method, when the answer lists one.
fn removed_run(doc: &serde_json::Value) -> Option<&serde_json::Value> {
    doc["definitions"]
        .as_array()?
        .iter()
        .find(|c| c["change"]["kind"] == "removed" && c["symbol"] == "run")
}

/// How many files the row names as still calling the method.
fn callers_named(row: &serde_json::Value) -> usize {
    row["rows"].as_array().map_or(0, |callers| {
        callers
            .iter()
            .filter(|c| c["path"].as_str().is_some_and(|p| p.starts_with("caller")))
            .count()
    })
}

/// A blob read of the base side that fails is no reason to say less of who still
/// calls a removed method without saying why: every base read after the run's
/// own is struck in turn (`DIFFCTX_TEST_READ_FAULT=base:N` counts base reads
/// alone), and the answer names all the callers, or says why it could not, or is
/// an error or a disclosed partial run.
///
/// The reads beyond the run's own are the base resolver's text reads of the
/// caller files. A struck one is a `Loss::ReadFailed` of the base resolver: the
/// head's `stale_uses` turns it into a gap that names the stop, and a build that
/// drops it (`(Vec::new(), None)`, the swallow) answers with the callers whose
/// text was read, no gap and no limit, which this fails.
#[test]
fn a_removed_method_is_never_unreferenced_when_a_read_of_the_base_side_failed() {
    let fx = removed_method();
    // No disk cache to serve a blob's facts: every blob is read.
    let no_cache = fx.tmp.path().join("no-cache");
    std::fs::write(&no_cache, "").unwrap();
    let no_cache = no_cache.to_str().expect("utf-8 path").to_string();
    let json_args = Query::Impact.args(&fx.range, "");
    let md_args = [
        ".",
        "--diff",
        fx.range.as_str(),
        "--mode",
        "impact",
        "-f",
        "md",
        "-q",
    ];
    let run = |args: &[&str], fail: Option<usize>| {
        let at = fail.map(|n| format!("base:{n}"));
        let mut env: Vec<(&str, &str)> = vec![("DIFFCTX_CACHE_DIR", no_cache.as_str())];
        if let Some(spec) = at.as_deref() {
            env.push(("DIFFCTX_TEST_READ_FAULT", spec));
        }
        fx.run_with("no-such-verb", "none", args, &env)
    };

    let clean = run(&json_args, None);
    assert!(
        clean.status.success(),
        "the clean run failed: {}",
        String::from_utf8_lossy(&clean.stderr)
    );
    let clean = json(&clean);
    assert_eq!(
        removed_run(&clean).map(callers_named),
        Some(CALLERS),
        "the clean run must name every file that still calls `run`: {clean:#}"
    );

    let mut broken: Vec<String> = Vec::new();
    // A run the base resolver's own stop was named in, with the run itself clean.
    let mut named_by_the_cell = false;
    let mut struck = 0;
    loop {
        let out = run(&json_args, Some(struck));
        let strikes = String::from_utf8_lossy(&out.stderr)
            .matches("failed on purpose")
            .count();
        if strikes == 0 {
            break;
        }
        if out.status.success() {
            let doc = json(&out);
            let disclosed = impact_disclosed(&doc);
            let row = removed_run(&doc);
            let says_why = row.is_some_and(|r| r["gaps"].as_array().is_some_and(|g| !g.is_empty()));
            let told = disclosed || says_why || row.is_some_and(|r| callers_named(r) == CALLERS);
            if !told {
                broken.push(format!("base read {struck}: {doc}"));
            }
            named_by_the_cell |= !disclosed
                && row.is_some_and(|r| {
                    r["gaps"].as_array().is_some_and(|gaps| {
                        gaps.iter().any(|g| {
                            g["kind"] == "stopped"
                                && g["reasons"] == serde_json::json!(["read_failed"])
                        })
                    })
                });
            let md = run(&md_args, Some(struck));
            let md = String::from_utf8_lossy(&md.stdout);
            if md.contains("nothing references it") {
                broken.push(format!("base read {struck}: {md}"));
            }
        }
        struck += 1;
        assert!(struck < 5_000, "the sweep never ended");
    }
    assert!(
        struck > 0,
        "no base read was struck, so the sweep measures nothing"
    );
    assert!(
        named_by_the_cell,
        "no struck base read left the run itself clean and the row naming the stop: the sweep never reached the base resolver's own reads"
    );
    assert!(
        broken.is_empty(),
        "{} of {struck} failed base reads left an answer that names fewer callers of `run` and says nothing\n{}",
        broken.len(),
        broken.join("\n")
    );
}

/// The changed file is past the size cap, so what it defines is not read: the
/// answer says so in every mode, and never reads as a change nothing depends
/// on (#505).
fn big_changed_file() -> Fixture {
    let tmp = scratch();
    let repo = tmp.path().join("repo");
    git(&repo, &["init", "-q", "."]);
    git(&repo, &["config", "user.email", "test@example.com"]);
    git(&repo, &["config", "user.name", "diffctx tests"]);
    let mut big = String::from("def total(items):\n    return sum(items)\n\n\n");
    let mut n = 0;
    while big.len() < 5 * 1024 * 1024 {
        let _ = write!(big, "def filler_{n}(x):\n    return x + {n}\n\n\n");
        n += 1;
    }
    std::fs::write(repo.join("big.py"), &big).unwrap();
    std::fs::write(
        repo.join("checkout.py"),
        "from big import total\n\n\ndef charge(cart):\n    return total(cart)\n",
    )
    .unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "initial"]);
    let doubled = big.replacen("return sum(items)\n", "return sum(items) * 2\n", 1);
    std::fs::write(repo.join("big.py"), doubled).unwrap();
    git(&repo, &["commit", "-qam", "double"]);
    Fixture {
        tmp,
        range: "HEAD~1..HEAD".to_string(),
    }
}

#[test]
fn a_five_megabyte_changed_file_is_a_named_loss() {
    let fx = big_changed_file();
    let size = std::fs::metadata(fx.repo().join("big.py")).unwrap().len();
    assert!(size >= 5 * 1024 * 1024, "the fixture is {size} bytes");
    let out = fx.impact_whole("no-such-verb", "none");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc = json(&out);
    assert_eq!(doc["empty"], false, "{doc}");
    assert!(
        impact_disclosed(&doc) || claims(&doc) == ["total <- checkout.py".to_string()],
        "an unread changed file must be disclosed, got {doc}"
    );
    for (mode, extra) in [("pack", &[][..]), ("locate", &["--mode", "locate"][..])] {
        let out = fx.pack("no-such-verb", "none", extra);
        let named = serde_json::from_slice::<serde_json::Value>(&out.stdout)
            .is_ok_and(|doc| coverage_disclosed(&doc))
            || (!out.status.success() && !out.stderr.is_empty());
        assert!(
            named,
            "{mode}: stdout={} stderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
