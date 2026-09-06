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
    // must auto-size like the Python CLI. The two-file repo the first version
    // used fit under 512 tokens whole, so auto and a 4096 cap were
    // byte-identical and the assertion could not go red on the very
    // regression it names; this repo's related context exceeds 4096 tokens,
    // so a fixed 4096 cap is strictly smaller than the auto budget.
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    std::fs::write(repo.join("util.py"), "def add(a, b):\n    return a + b\n").expect("write");
    for i in 0..40 {
        let body: String = (0..30)
            .map(|k| format!("def caller_{i}_{k}(x):\n    return add(x, {k}) + {i}\n\n"))
            .collect();
        std::fs::write(
            repo.join(format!("mod_{i}.py")),
            format!("from util import add\n\n{body}"),
        )
        .expect("write");
    }
    commit_all(repo, "initial");
    std::fs::write(
        repo.join("util.py"),
        "def add(a, b):\n    return a + b + 0\n",
    )
    .expect("write");
    commit_all(repo, "touch add");

    let auto = run(repo, &[".", "--diff", "HEAD~1..HEAD"]);
    let explicit = run(repo, &[".", "--diff", "HEAD~1..HEAD", "--budget", "4096"]);

    assert_eq!(auto.status.code(), Some(0));
    assert_eq!(explicit.status.code(), Some(0));
    assert!(
        auto.stdout.len() > explicit.stdout.len(),
        "auto budget ({} bytes) did not exceed a 4096-token cap ({} bytes) on a repo whose context outgrows it",
        auto.stdout.len(),
        explicit.stdout.len()
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

/// #239: on POSIX a backslash is an ordinary filename byte. Every path that
/// reached output — and, worse, the `rev:path` spec handed to `git show` —
/// rewrote it to `/`, so with both `src\utils.py` (changed) and `src/utils.py`
/// (untouched) present, the changed file was listed under the other's name
/// and rendered with the other's body.
#[cfg(not(windows))]
#[test]
fn a_backslash_filename_keeps_its_name_and_its_own_content() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path();
    init_repo(repo);
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/utils.py"), "def b():\n    return 2\n").unwrap();
    std::fs::write(repo.join("src\\utils.py"), "def a():\n    return 1\n").unwrap();
    commit_all(repo, "both files");
    std::fs::write(repo.join("src\\utils.py"), "def a():\n    return 11\n").unwrap();
    commit_all(repo, "change only the backslash one");

    let out = run(repo, &[".", "--diff", "HEAD~1..HEAD", "-f", "json"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();

    let changed: Vec<&str> = parsed["changed_files"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(
        changed,
        vec!["src\\utils.py"],
        "the changed file is named as it is spelled"
    );

    let changed_frag = parsed["fragments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["role"] == "changed")
        .expect("a changed fragment");
    assert_eq!(changed_frag["path"], "src\\utils.py");
    assert!(
        changed_frag["content"]
            .as_str()
            .unwrap()
            .contains("return 11"),
        "the changed fragment must carry the backslash file's own body, got {}",
        changed_frag["content"]
    );
}

/// The repository is the trust boundary. Both paths into the candidate
/// universe — `git ls-files` for tracked files, the untracked scan for the
/// working tree — used to hand out names that were only LEXICALLY inside the
/// root: `is_candidate_file` asked `is_file()`/`metadata()`, which follow a
/// symlink, and `get_untracked_files` returned `canonicalize()`, which IS the
/// link's target.
///
/// The untracked half is the one a CLI run can observe: the escaped file is
/// opened (a line count is taken from it) and its absolute out-of-repository
/// path is printed in the changed-file list. It takes a second, ordinary
/// change in the tree — with the link alone the run ends empty and the leak
/// stays invisible, which is why the first version of this test passed
/// against the vulnerable build.
#[test]
#[cfg(unix)]
fn a_symlink_out_of_the_repository_is_never_read_as_context() {
    let tmp = TempDir::new().expect("tempdir");
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(&outside).expect("outside");
    std::fs::write(
        outside.join("secret.py"),
        "def secret_work(value):\n    return value  # SYMLINK_ESCAPE_MARKER\n",
    )
    .expect("write secret");

    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).expect("repo");
    init_repo(&repo);
    std::fs::write(
        repo.join("app.py"),
        "from helper import work\n\n\ndef main():\n    return work(1)\n",
    )
    .expect("write app");
    std::fs::write(repo.join("helper.py"), "def work(x):\n    return x\n").expect("write helper");
    // Tracked: committed as a link, so `ls-files` reports it and it enters the
    // candidate universe.
    std::os::unix::fs::symlink(outside.join("secret.py"), repo.join("tracked_link.py"))
        .expect("tracked symlink");
    commit_all(&repo, "initial");

    // Untracked: the working tree carries the link plus one ordinary new file,
    // so the change set is non-empty and the untracked scan actually runs.
    std::os::unix::fs::symlink(outside.join("secret.py"), repo.join("untracked_link.py"))
        .expect("untracked symlink");
    // The escaped file is not merely present: the working-tree change calls the
    // symbol it defines, so ranking WANTS it. Without that pull the run emits
    // one fragment of an unrelated new file and the leak stays invisible —
    // which is how the first version of this test passed against the bug.
    std::fs::write(
        repo.join("app.py"),
        "from untracked_link import secret_work\n\n\ndef main():\n    return secret_work(1)\n",
    )
    .expect("rewrite app");

    let outside_marker = outside.to_string_lossy().to_string();
    for args in [
        vec![".", "--diff", "HEAD"],
        vec![".", "--diff", "HEAD", "--full"],
        vec![".", "--diff", "HEAD~1..HEAD"],
    ] {
        let out = run(&repo, &args);
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            !text.contains("SYMLINK_ESCAPE_MARKER"),
            "{args:?} read a file outside the repository"
        );
        assert!(
            !text.contains(outside_marker.as_str()),
            "{args:?} emitted a path outside the repository:\n{text}"
        );
    }
}

/// `--full` is the escape hatch that promises MORE than the default mode, and
/// it delivered less in two ways at once. It called `parse_diff` alone, so
/// every untracked file — a change git reports no diff for — was invisible to
/// it while the default mode listed them. And it dropped secret- and
/// policy-excluded paths in silence, which is the misreading #188 fixed
/// everywhere else: a run that removed files from its answer looked exactly
/// like one with nothing to remove.
#[test]
fn full_mode_sees_untracked_files_and_owns_up_to_what_it_withholds() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(repo.join(".diffctx")).expect("policy dir");
    init_repo(&repo);
    std::fs::write(repo.join(".diffctx/ignore"), "private/\n").expect("write policy");
    std::fs::create_dir_all(repo.join("private")).expect("private dir");
    std::fs::write(repo.join("private/conf.py"), "TOKEN = \"before\"\n").expect("write conf");
    std::fs::write(repo.join("helper.py"), "def work(x):\n    return x\n").expect("write helper");
    commit_all(&repo, "initial");

    std::fs::write(repo.join("helper.py"), "def work(x):\n    return x + 1\n").expect("edit");
    std::fs::write(repo.join("private/conf.py"), "TOKEN = \"after\"\n").expect("edit conf");
    std::fs::write(
        repo.join("brand_new.py"),
        "def brand_new():\n    return 7\n",
    )
    .expect("new");

    let out = run(&repo, &[".", "--diff", "HEAD", "--full", "-f", "yaml"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("brand_new"),
        "--full did not see the untracked file:\n{text}"
    );
    assert!(
        text.contains("policy_excluded_count: 1"),
        "--full withheld a changed file without saying so:\n{text}"
    );
    assert!(
        !text.contains("private/conf.py"),
        "--full published a path the policy withholds:\n{text}"
    );
}

/// `alpha <= 0.0 || alpha >= 1.0` is false for NaN — every comparison against
/// it is — so NaN sailed through validation into the damping factor and every
/// score came out NaN. Same for tau, where a NaN read as "not negative".
#[test]
fn nan_is_rejected_as_a_parameter() {
    let tmp = code_change_repo();
    for (flag, value) in [
        ("--alpha", "nan"),
        ("--alpha", "inf"),
        ("--tau", "nan"),
        ("--tau", "-inf"),
    ] {
        let out = run(tmp.path(), &[".", "--diff", "HEAD~1..HEAD", flag, value]);
        assert!(
            !out.status.success(),
            "{flag} {value} was accepted:\n{}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
}

/// The exit code is the deadline's contract. Two clocks watch it — this
/// receive and the pipeline's phase checks on the worker — and the worker's
/// panic closing the channel first is mapped to the same 124 in `main.rs`;
/// that branch is not reachable on demand from here (with equal timeouts the
/// receive always fires first), so this pins the reachable half only.
#[test]
fn an_expired_deadline_exits_124() {
    let tmp = code_change_repo();
    let out = run(
        tmp.path(),
        &[".", "--diff", "HEAD~1..HEAD", "--timeout", "0"],
    );
    assert_eq!(
        out.status.code(),
        Some(124),
        "stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
