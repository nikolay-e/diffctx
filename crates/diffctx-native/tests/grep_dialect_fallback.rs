//! A git built without PCRE refuses `-P`. Each refused search runs again in
//! git's own dialect (`-E` for patterns, `-F` for needles), and the answer is
//! the one the PCRE search gives.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use tempfile::TempDir;

mod support;
use support::{commit_all, diffctx_unclocked, init_repo, scrubbed_sh, write};

const GIT_WITHOUT_PCRE: &str = r#"#!/bin/sh
echo "$*" >> "$FAKE_GIT_CALLS"
for a in "$@"; do
  if [ "$a" = "-P" ]; then
    echo 'fatal: cannot use Perl-compatible regexes when not compiled with USE_LIBPCRE' >&2
    exit 128
  fi
done
exec "$REAL_GIT" "$@"
"#;

fn impact(repo: &Path, env: &[(&str, &str)]) -> serde_json::Value {
    let out = diffctx_unclocked()
        .current_dir(repo)
        .envs(env.iter().copied())
        .args(["--diff", "HEAD~1..HEAD", "--mode", "impact", "-q"])
        .output()
        .expect("run diffctx");
    assert!(
        out.status.success(),
        "impact failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("impact JSON")
}

#[test]
fn a_git_without_pcre_gives_the_answer_the_pcre_search_gives() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    write(
        repo,
        "app/rates.py",
        "def rate_total(items):\n    return sum(items)\n",
    );
    write(
        repo,
        "conf/app.yaml",
        "handlers:\n  total: \"rate_total\"\n  dynamic: \"rate_%s\"\n",
    );
    write(repo, "web/page.html", "<a onclick=\"rate_total(this)\">\n");
    commit_all(repo, "initial");
    write(
        repo,
        "app/rates.py",
        "def rate_total(items):\n    return sum(items) + 1\n",
    );
    commit_all(repo, "change");

    let pcre = impact(repo, &[]);

    let dir = TempDir::new().expect("shim dir");
    let shim = dir.path().join("git");
    std::fs::write(&shim, GIT_WITHOUT_PCRE).expect("write shim");
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let real = scrubbed_sh(repo, "command -v git").output().expect("git");
    let real = String::from_utf8(real.stdout).expect("utf8");
    let path = format!(
        "{}:{}",
        dir.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let log = dir.path().join("calls");
    let posix = impact(
        repo,
        &[
            ("PATH", path.as_str()),
            ("REAL_GIT", real.trim()),
            ("FAKE_GIT_CALLS", log.to_str().expect("utf8")),
        ],
    );

    let calls = std::fs::read_to_string(&log).expect("the shim's calls");
    let refused = calls
        .lines()
        .filter(|l| l.contains("-n -z -I -P -e"))
        .count();
    let retried = calls
        .lines()
        .filter(|l| l.contains("-n -z -I -E -e") || l.contains("-n -z -I -F -e"))
        .count();
    assert!(refused > 0, "no pattern search reached the shim:\n{calls}");
    assert_eq!(refused, retried, "{calls}");
    assert_eq!(posix["definitions"], pcre["definitions"]);
}
