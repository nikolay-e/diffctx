// Contract tests for the standalone binary. Five of the six release channels
// ship this binary rather than the Python CLI, and nothing else in the suite
// goes through its clap parser: `yaml_cases` and the pybridge both pass every
// parameter explicitly. The 4096-token hard cap, the missing `-v`, the absent
// token summary and the missing empty-diff exit code all reached users through
// that gap.

use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_diffctx");

fn git(repo: &Path, args: &[&str]) {
    // Scrub the same vars git_command() scrubs: under `git commit -a` the
    // pre-commit hook runs with GIT_INDEX_FILE pointing at the MAIN repo's
    // in-progress index, and an unscrubbed child git in a temp repo locks
    // that index instead ("index.lock.lock: File exists").
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

fn init_repo(repo: &Path) {
    git(repo, &["init", "-q", "."]);
    git(repo, &["config", "user.email", "test@example.com"]);
    git(repo, &["config", "user.name", "diffctx tests"]);
}

fn commit_all(repo: &Path, message: &str) {
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", message]);
}

fn run(repo: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .current_dir(repo)
        .args(args)
        .output()
        .expect("run diffctx")
}

fn code_change_repo() -> TempDir {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    std::fs::write(repo.join("util.py"), "def add(a, b):\n    return a + b\n").expect("write");
    std::fs::write(
        repo.join("app.py"),
        "from util import add\n\n\ndef main():\n    print(add(1, 2))\n",
    )
    .expect("write");
    commit_all(repo, "initial");
    std::fs::write(
        repo.join("util.py"),
        "def add(a, b):\n    return a + b\n\n\ndef sub(a, b):\n    return a - b\n",
    )
    .expect("write");
    commit_all(repo, "add sub");
    tmp
}

fn binary_only_repo() -> TempDir {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    std::fs::write(repo.join("readme"), "x\n").expect("write");
    commit_all(repo, "initial");
    std::fs::write(repo.join("blob.bin"), [0u8, 159, 146, 150, 0, 1, 2, 3]).expect("write");
    commit_all(repo, "binary only");
    tmp
}

#[test]
fn both_version_short_flags_print_the_version() {
    let tmp = code_change_repo();
    for flag in ["-v", "-V", "--version"] {
        let out = run(tmp.path(), &[flag]);
        assert!(
            out.status.success(),
            "{flag} exited {:?}",
            out.status.code()
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.starts_with("diffctx "),
            "{flag} printed {stdout:?}, expected a version line"
        );
    }
}

#[test]
fn a_diff_with_context_exits_zero_and_reports_tokens_on_stderr() {
    let tmp = code_change_repo();
    let out = run(tmp.path(), &[".", "--diff", "HEAD~1..HEAD"]);

    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("type: diff_context"), "got {stdout:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("tokens (o200k_base)"),
        "expected a token summary on stderr, got {stderr:?}"
    );
}

#[test]
fn quiet_suppresses_the_token_summary_but_keeps_the_output() {
    let tmp = code_change_repo();
    let out = run(tmp.path(), &[".", "--diff", "HEAD~1..HEAD", "-q"]);

    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).contains("type: diff_context"));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("tokens (o200k_base)"),
        "--quiet must silence the summary, got {stderr:?}"
    );
}

#[test]
fn an_empty_diff_exits_four_with_an_actionable_message() {
    let tmp = binary_only_repo();
    let out = run(tmp.path(), &[".", "--diff", "HEAD~1..HEAD"]);

    assert_eq!(
        out.status.code(),
        Some(4),
        "binary-only diff must use the empty-diff exit code"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no semantic context"),
        "expected the empty-diff warning, got {stderr:?}"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("type: diff_context"),
        "the skeleton is still written to stdout"
    );
}

#[test]
fn json_format_emits_parsable_json() {
    let tmp = code_change_repo();
    let out = run(tmp.path(), &[".", "--diff", "HEAD~1..HEAD", "-f", "json"]);

    assert_eq!(out.status.code(), Some(0));
    let parsed: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stdout must be valid JSON");
    assert_eq!(parsed["type"], "diff_context");
}

#[test]
fn unknown_format_is_a_usage_error() {
    let tmp = code_change_repo();
    let out = run(tmp.path(), &[".", "--diff", "HEAD~1..HEAD", "-f", "md"]);

    assert_eq!(out.status.code(), Some(2), "clap rejects unknown formats");
}

#[test]
fn budget_minus_one_is_unlimited_and_below_minus_one_is_a_usage_error() {
    let tmp = code_change_repo();

    let unlimited = run(
        tmp.path(),
        &[".", "--diff", "HEAD~1..HEAD", "--budget", "-1"],
    );
    assert_eq!(unlimited.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&unlimited.stdout).contains("type: diff_context"));

    let invalid = run(
        tmp.path(),
        &[".", "--diff", "HEAD~1..HEAD", "--budget", "-5"],
    );
    assert_eq!(invalid.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("--budget must be >= -1"));
}

#[test]
fn an_omitted_budget_is_not_capped_at_a_fixed_default() {
    // Regression for the shipped 4096-token clap default: the native binary
    // must auto-size like the Python CLI, so omitting --budget cannot produce
    // less context than an explicitly huge budget on the same diff.
    let tmp = code_change_repo();

    let auto = run(tmp.path(), &[".", "--diff", "HEAD~1..HEAD"]);
    let explicit = run(
        tmp.path(),
        &[".", "--diff", "HEAD~1..HEAD", "--budget", "4096"],
    );

    assert_eq!(auto.status.code(), Some(0));
    assert_eq!(explicit.status.code(), Some(0));
    assert!(
        auto.stdout.len() >= explicit.stdout.len(),
        "auto budget produced less than a 4096-token cap"
    );
}

#[test]
fn locate_mode_emits_the_versioned_schema_with_reasons() {
    let tmp = code_change_repo();
    let out = run(
        tmp.path(),
        &[".", "--diff", "HEAD~1..HEAD", "--mode", "locate", "-q"],
    );
    assert_eq!(out.status.code(), Some(0));
    let doc: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("locate output must be valid JSON");
    assert_eq!(doc["schema"], "diffctx.locate.v1");
    let items = doc["items"].as_array().expect("items array");
    assert_eq!(items.len(), doc["item_count"].as_u64().unwrap() as usize);
    assert!(!items.is_empty());
    for item in items {
        assert!(item["path"].is_string());
        assert!(item["lines"].is_string());
        assert!(item["score"].is_number());
        let reasons = item["reasons"].as_array().expect("reasons array");
        assert!(!reasons.is_empty(), "every item carries >=1 reason");
        for reason in reasons {
            assert!(reason["type"].is_string());
        }
    }
    // No source bodies anywhere in the payload.
    assert!(!String::from_utf8_lossy(&out.stdout).contains("def add"));
}

#[test]
fn locate_mode_rejects_full_and_survives_empty_diffs() {
    let tmp = code_change_repo();
    let conflict = run(
        tmp.path(),
        &[".", "--diff", "HEAD~1..HEAD", "--mode", "locate", "--full"],
    );
    assert_eq!(conflict.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&conflict.stderr).contains("--mode locate"));

    // Clean working tree (bare --diff): the empty-diff contract holds in
    // locate mode too — exit 4 and a parseable, empty item list.
    let empty = run(tmp.path(), &[".", "--diff", "--mode", "locate", "-q"]);
    assert_eq!(empty.status.code(), Some(4));
    let doc: serde_json::Value = serde_json::from_slice(&empty.stdout).expect("valid JSON");
    assert_eq!(doc["item_count"], 0);
}
