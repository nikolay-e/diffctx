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

/// What `/proc` says a child of ours is: its pid, name and argument vector (a
/// zombie has none, only the name).
fn describe(pid: libc::pid_t) -> String {
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default();
    let argv = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
    let argv = String::from_utf8_lossy(&argv).replace('\0', " ");
    format!("{pid} {} [{}]", comm.trim(), argv.trim())
}

/// The git children this process still has after the binary exited, each named
/// and then every child reaped. Only git is judged: what a killed git left
/// behind, such as a fake's `sleep`, lost its parent with the kill and is
/// nobody's to reap but an init's. An orphan still running ends on its stdin's
/// EOF, and the blocking `waitpid` waits for it; no child at all is `ECHILD`.
fn orphans() -> Vec<String> {
    let me = i64::from(std::process::id());
    let mut found = Vec::new();
    for entry in std::fs::read_dir("/proc").unwrap().flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<libc::pid_t>() else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        let after_name = stat.rsplit_once(')').map_or("", |(_, rest)| rest);
        if after_name
            .split_whitespace()
            .nth(1)
            .and_then(|p| p.parse().ok())
            == Some(me)
        {
            let named = describe(pid);
            if named.split(' ').nth(1) == Some("git") {
                found.push(named);
            }
        }
    }
    loop {
        let mut status = 0;
        // SAFETY: waitpid(2) writes the status into the local it is given.
        #[allow(unsafe_code)]
        let pid = unsafe { libc::waitpid(-1, &raw mut status, 0) };
        if pid <= 0 {
            return found;
        }
    }
}

#[test]
fn a_run_leaves_no_git_child_behind() {
    let tmp = TempDir::new().unwrap();
    let work = tmp.path().join("repo");
    std::fs::create_dir_all(&work).unwrap();
    repo(&work);
    // Only now: `git commit` detaches `git maintenance` and leaves it to
    // whoever adopts orphans, so a subreaper set before the setup adopts git's
    // own daemons, which no diffctx run left.
    // SAFETY: prctl(2) with these arguments only changes this process's flag.
    #[allow(unsafe_code)]
    let set = unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) };
    assert_eq!(set, 0, "this kernel has no child subreaper");
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
            // The stall kills a cat-file that never answers.
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
                "600",
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
                .env("FAKE_GIT_STALLED", tmp.path().join("stalled"))
                .env("DIFFCTX_TEST_GIT_STALL_FILE", tmp.path().join("stalled"))
                .env("FAKE_GIT_CALLS", tmp.path().join("calls"));
        }
        command.output().expect("run diffctx");
        let left = orphans();
        assert!(left.is_empty(), "{name}: left children behind: {left:?}");
    }
}
