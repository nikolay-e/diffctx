// A process that outlives its child without waiting for it leaves a zombie,
// and under a PID 1 that never reaps (a CI pod's shell, a container's
// entrypoint) the zombies stay until the pod ends: four thousand of them from
// one run of tests (fj#168). The fault suite cannot see it, because it counts
// a zombie as gone.
//
// This process becomes the subreaper of everything the binary leaves behind:
// a git the binary neither waited for nor killed outlives it as an orphan,
// reparents to us, and is ours to reap. A clean run leaves no such child.
#![cfg(target_os = "linux")]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

mod support;
use support::fake_git::FAKE_GIT;
use support::{diffctx, git};

const TIMEOUT_SECS: &str = "3";

fn repo(dir: &Path) {
    git(dir, &["init", "-q", "."]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "diffctx tests"]);
    std::fs::write(
        dir.join("pricing.py"),
        "def total(items):\n    return sum(items)\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("checkout.py"),
        "from pricing import total\n\n\ndef charge(cart):\n    return total(cart)\n",
    )
    .unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "initial"]);
    std::fs::write(
        dir.join("pricing.py"),
        "def total(items):\n    return sum(items) * 2\n",
    )
    .unwrap();
    git(dir, &["commit", "-qam", "double"]);
}

/// Children this process still has after the binary exited, each reaped as it
/// is found. An orphan still running ends on its stdin's EOF, and the blocking
/// `waitpid` waits for it; no child at all is `ECHILD`.
fn orphans() -> Vec<libc::pid_t> {
    let mut found = Vec::new();
    loop {
        let mut status = 0;
        // SAFETY: waitpid(2) writes the status into the local it is given.
        #[allow(unsafe_code)]
        let pid = unsafe { libc::waitpid(-1, &raw mut status, 0) };
        if pid <= 0 {
            return found;
        }
        found.push(pid);
    }
}

#[test]
fn a_run_leaves_no_git_child_behind() {
    // SAFETY: prctl(2) with these arguments only changes this process's flag.
    #[allow(unsafe_code)]
    let set = unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) };
    assert_eq!(set, 0, "this kernel has no child subreaper");

    let tmp = TempDir::new().unwrap();
    let work = tmp.path().join("repo");
    std::fs::create_dir_all(&work).unwrap();
    repo(&work);
    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("git"), FAKE_GIT).unwrap();
    std::fs::set_permissions(bin.join("git"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let real = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    let real = String::from_utf8(real.stdout).unwrap().trim().to_string();
    let stalled = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let runs: [(&str, Option<&str>, &[&str]); 3] = [
        (
            "impact",
            None,
            &[".", "--diff", "HEAD~1..HEAD", "--mode", "impact", "-q"],
        ),
        ("symbol", None, &[".", "--symbol", "total", "-q"]),
        (
            // The deadline kills a cat-file that never answers.
            "stalled cat-file",
            Some(&stalled),
            &[
                ".",
                "--diff",
                "HEAD~1..HEAD",
                "--mode",
                "impact",
                "-q",
                "--timeout",
                TIMEOUT_SECS,
            ],
        ),
    ];
    for (name, path, args) in runs {
        let mut command = diffctx();
        command
            .current_dir(&work)
            .args(args)
            .env("DIFFCTX_NO_MARKER", "1");
        if let Some(path) = path {
            command
                .env("PATH", path)
                .env("REAL_GIT", &real)
                .env("FAKE_GIT_TARGET", "cat-file")
                .env("FAKE_GIT_MODE", "stall_before_read")
                .env("FAKE_GIT_PIDS", tmp.path().join("pids"))
                .env("FAKE_GIT_CALLS", tmp.path().join("calls"));
        }
        command.output().expect("run diffctx");
        let left = orphans();
        assert!(left.is_empty(), "{name}: left children behind: {left:?}");
    }
}
