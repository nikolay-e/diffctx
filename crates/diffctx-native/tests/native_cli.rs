// Contract tests for the standalone binary. Five of the six release channels
// ship this binary rather than the Python CLI, and nothing else in the suite
// goes through its clap parser: `yaml_cases` and the pybridge both pass every
// parameter explicitly. The 4096-token hard cap, the missing `-v`, the absent
// token summary and the missing empty-diff exit code all reached users through
// that gap.

use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

/// The binary under test: the one cargo just built, or — set by the release
/// job — the shipped artifact for a target, so every standalone binary that
/// reaches a user passes this suite on its own platform.
static BIN: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    std::env::var("DIFFCTX_NATIVE_BIN")
        .unwrap_or_else(|_| env!("CARGO_BIN_EXE_diffctx").to_string())
});

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
    Command::new(&*BIN)
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
/// Every out-of-range number is a usage error (2) decided by the parser,
/// before git runs — NaN included, and a zero `--timeout`, which used to
/// reach git and come back as exit 3.
#[test]
fn an_out_of_range_parameter_is_a_usage_error() {
    let tmp = code_change_repo();
    for (flag, value) in [
        ("--alpha", "nan"),
        ("--alpha", "inf"),
        ("--alpha", "0"),
        ("--alpha", "1"),
        ("--tau", "nan"),
        ("--tau", "-inf"),
        ("--tau", "inf"),
        ("--tau", "-1"),
        ("--budget", "-2"),
        ("--timeout", "0"),
    ] {
        let out = run(tmp.path(), &[".", "--diff", "HEAD~1..HEAD", flag, value]);
        assert_eq!(
            out.status.code(),
            Some(2),
            "{flag} {value}: stdout {:?} stderr {:?}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.stdout.is_empty(), "{flag} {value} wrote an artifact");
    }
}

/// An unset `$RANGE` expands to `--diff ""`; the binary must not guess.
#[test]
fn an_empty_range_is_a_usage_error() {
    let tmp = code_change_repo();
    for range in ["", "  "] {
        let out = run(tmp.path(), &[".", "--diff", range]);
        assert_eq!(out.status.code(), Some(2), "range {range:?}");
        assert!(out.stdout.is_empty());
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("--diff requires a non-empty range"),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// A duration-shaped typo is a usage error naming the duration grammar,
/// never "unknown git revision" — `git log` cannot show what is wrong.
#[test]
fn a_malformed_duration_names_the_duration_syntax() {
    let tmp = code_change_repo();
    for range in ["1.5h", "99999999999w"] {
        let out = run(tmp.path(), &[".", "--diff", range]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{range}: {stderr}");
        assert!(stderr.contains("invalid duration"), "{range}: {stderr}");
        assert!(!stderr.contains("unknown revision"), "{range}: {stderr}");
    }
    let out = run(tmp.path(), &[".", "--diff", " 24h "]);
    assert!(
        matches!(out.status.code(), Some(0 | 4)),
        "a padded duration is still a duration: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// "Not a repository" and "does not exist" are environment failures (3),
/// like every other git refusal.
#[test]
fn a_directory_outside_git_is_an_environment_error() {
    let plain = TempDir::new().expect("tempdir");
    let out = run(plain.path(), &[".", "--diff", "HEAD"]);
    assert_eq!(
        out.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let missing = plain.path().join("does-not-exist");
    let out = run(plain.path(), &[missing.to_str().unwrap(), "--diff", "HEAD"]);
    assert_eq!(
        out.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// git refusing to start is reported in git's own words, not as "not a git
/// repository": the `fatal:` line is the diagnosis.
#[test]
fn a_broken_git_config_surfaces_gits_own_fatal_line() {
    let tmp = code_change_repo();
    let config = tmp.path().join(".git/config");
    let mut text = std::fs::read_to_string(&config).expect("config");
    text.push_str("[core\n");
    std::fs::write(&config, text).expect("write config");
    let out = run(tmp.path(), &[".", "--diff", "HEAD~1..HEAD"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(3), "{stderr}");
    assert!(stderr.contains("fatal:"), "{stderr}");
    assert!(!stderr.contains("not a git repository"), "{stderr}");
}

/// The known-bad probe for the resource contract: a contribution cap the
/// smallest graph exceeds must yield a valid artifact that names the limit,
/// not an error and not a silent full result.
#[test]
fn a_bound_resource_yields_a_partial_artifact_that_says_so() {
    let tmp = code_change_repo();
    let out = Command::new(&*BIN)
        .current_dir(tmp.path())
        .env("DIFFCTX_MAX_EDGE_CONTRIBUTIONS", "1")
        .args([".", "--diff", "HEAD~1..HEAD", "--format", "json", "--quiet"])
        .output()
        .expect("run diffctx");
    assert!(
        out.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json output");
    assert_eq!(doc["coverage"]["status"], "partial");
    let reasons = doc["coverage"]["limit_reasons"]
        .as_array()
        .expect("limit_reasons")
        .iter()
        .map(|r| r.as_str().unwrap_or_default().to_string())
        .collect::<Vec<_>>();
    assert!(
        reasons.contains(&"edge_contribution_limit".to_string()),
        "reasons: {reasons:?}"
    );
    assert_eq!(
        doc["provenance"]["effective_config_hash"]
            .as_str()
            .map(str::len),
        Some(16)
    );
    assert!(!doc["changed_files"].as_array().unwrap().is_empty());
}

fn wide_repo() -> TempDir {
    // Sixty changed modules: the changed-file inventory alone is a few
    // thousand tokens, so a budget of 2000–3000 binds on the envelope the
    // engine only estimates, and the renderer has to do the trimming.
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    for i in 0..60 {
        let dir = repo.join(format!("src/package_{i}"));
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(
            dir.join(format!("module_with_a_long_name_{i}.py")),
            format!("def fn_{i}(x):\n    return x + {i}\n"),
        )
        .expect("write");
    }
    commit_all(repo, "initial");
    for i in 0..60 {
        std::fs::write(
            repo.join(format!("src/package_{i}/module_with_a_long_name_{i}.py")),
            format!("def fn_{i}(x):\n    y = x + {i}\n    return y * 2\n"),
        )
        .expect("write");
    }
    commit_all(repo, "widen every module");
    tmp
}

#[test]
fn the_written_artifact_stays_within_the_budget_in_both_formats() {
    // #277: the binary documented `--budget` as a cap on the whole artifact
    // and enforced nothing on the document it wrote — 9121 tokens at 8000 on
    // a real range. The Python CLI trims at render; the binary now does too.
    let tmp = wide_repo();
    let repo = tmp.path();
    for format in ["yaml", "json"] {
        // The inventory alone is ~2.7k tokens in YAML and ~3.4k in JSON, so
        // these cells bind on fragments rather than on the envelope.
        for budget in ["4000", "6000"] {
            let out = run(
                repo,
                &[
                    ".",
                    "--diff",
                    "HEAD~1..HEAD",
                    "--budget",
                    budget,
                    "-f",
                    format,
                ],
            );
            assert_eq!(out.status.code(), Some(0), "{format} at {budget}");
            let text = String::from_utf8(out.stdout).expect("utf-8");
            let tokens = _diffctx::tokenizer::count_tokens(&text);
            let cap: u32 = budget.parse().expect("budget");
            assert!(
                tokens <= cap,
                "{format} at --budget {budget} wrote {tokens} tokens"
            );
            assert!(
                text.contains("selection_budget_exceeded"),
                "{format} at {budget}: a trimmed document must say so in coverage"
            );
        }
        // Control: a budget the whole artifact fits under is not trimmed.
        let out = run(
            repo,
            &[
                ".",
                "--diff",
                "HEAD~1..HEAD",
                "--budget",
                "16000",
                "-f",
                format,
            ],
        );
        let text = String::from_utf8(out.stdout).expect("utf-8");
        assert!(
            !text.contains("selection_budget_exceeded"),
            "{format} at 16000 was trimmed although it fits"
        );
    }
}

#[test]
fn a_budget_below_the_inventory_yields_the_inventory_alone() {
    // A changed path is never dropped to fit, so the document can exceed a
    // budget that cannot hold the inventory — but nothing else is spent.
    let tmp = wide_repo();
    let out = run(
        tmp.path(),
        &[
            ".",
            "--diff",
            "HEAD~1..HEAD",
            "--budget",
            "100",
            "-f",
            "json",
        ],
    );
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json document");
    // Same as the Python CLI: nothing selected is exit 4, with the inventory
    // still written so the omission is stated rather than implied.
    assert_eq!(out.status.code(), Some(4));
    assert_eq!(doc["fragment_count"], 0);
    assert_eq!(doc["changed_files"].as_array().map(Vec::len), Some(60));
}

fn json_doc(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout is not JSON ({e}); stderr:\n{}",
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

fn limit_reasons(doc: &serde_json::Value) -> Vec<String> {
    doc["coverage"]["limit_reasons"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|r| r.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Two changed files in different directories; a path argument, and running
/// from inside a subdirectory, both narrow the change set to that subtree.
#[test]
fn a_subtree_argument_or_working_directory_narrows_the_diff() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    for dir in ["pkg", "other"] {
        std::fs::create_dir_all(repo.join(dir)).expect("mkdir");
        std::fs::write(repo.join(dir).join("m.py"), "def f():\n    return 1\n").expect("write");
    }
    commit_all(repo, "initial");
    for dir in ["pkg", "other"] {
        std::fs::write(repo.join(dir).join("m.py"), "def f():\n    return 2\n").expect("write");
    }
    commit_all(repo, "edit both");

    let whole = json_doc(&run(
        repo,
        &[".", "--diff", "HEAD~1..HEAD", "-f", "json", "-q"],
    ));
    assert_eq!(
        whole["changed_files"],
        serde_json::json!(["other/m.py", "pkg/m.py"])
    );
    let by_arg = json_doc(&run(
        repo,
        &["pkg", "--diff", "HEAD~1..HEAD", "-f", "json", "-q"],
    ));
    assert_eq!(by_arg["changed_files"], serde_json::json!(["pkg/m.py"]));
    let by_cwd = json_doc(&run(
        &repo.join("pkg"),
        &[".", "--diff", "HEAD~1..HEAD", "-f", "json", "-q"],
    ));
    assert_eq!(by_cwd["changed_files"], serde_json::json!(["pkg/m.py"]));
    assert!(
        by_cwd["fragments"]
            .as_array()
            .unwrap()
            .iter()
            .all(|f| f["path"].as_str().unwrap().starts_with("pkg/")),
        "{by_cwd}"
    );
}

/// `RUST_LOG` must not turn the artifact into a log: tracing goes to stderr.
#[test]
fn tracing_goes_to_stderr_and_the_artifact_still_parses() {
    let tmp = code_change_repo();
    for format in ["json", "yaml"] {
        let out = Command::new(&*BIN)
            .current_dir(tmp.path())
            .env("RUST_LOG", "debug")
            .args([".", "--diff", "HEAD~1..HEAD", "-f", format, "-q"])
            .output()
            .expect("run diffctx");
        assert_eq!(out.status.code(), Some(0), "{format}");
        let stdout = String::from_utf8(out.stdout).expect("utf-8");
        let doc: serde_json::Value = if format == "json" {
            serde_json::from_str(&stdout).expect("stdout is JSON")
        } else {
            serde_yaml::from_str(&stdout).expect("stdout is YAML")
        };
        assert_eq!(doc["type"], "diff_context", "{format}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("DEBUG"),
            "{format}: debug tracing should reach stderr"
        );
    }
}

/// A reader that closes the pipe early (`| head`) ends the run with the
/// SIGPIPE code, never a panic. The output must exceed the 64 KiB pipe
/// buffer, or the write completes before the reader leaves.
#[test]
fn a_closed_stdout_pipe_exits_quietly() {
    use std::io::Read;
    use std::process::Stdio;
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    std::fs::write(repo.join("seed.py"), "x = 0\n").expect("write");
    commit_all(repo, "initial");
    let body: String = (0..3000)
        .map(|i| format!("def function_number_{i}(value):\n    return value + {i}\n\n\n"))
        .collect();
    std::fs::write(repo.join("big.py"), body).expect("write");
    commit_all(repo, "big");

    let mut child = Command::new(&*BIN)
        .current_dir(repo)
        .args([".", "--diff", "HEAD~1..HEAD", "--full", "-q"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn diffctx");
    let mut stdout = child.stdout.take().expect("stdout");
    let mut first = [0u8; 10];
    stdout.read_exact(&mut first).expect("read the head");
    drop(stdout);
    let out = child.wait_with_output().expect("wait");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("panicked"), "{stderr}");
    if cfg!(unix) {
        assert_eq!(out.status.code(), Some(141), "{stderr}");
    } else {
        assert!(matches!(out.status.code(), Some(141 | 0)), "{stderr}");
    }
}

/// The compute deadline's cooperative exit, forced from the first poll: a
/// valid, partial artifact that names the reason.
#[test]
fn an_expired_deadline_yields_a_partial_artifact_naming_it() {
    let tmp = code_change_repo();
    let out = Command::new(&*BIN)
        .current_dir(tmp.path())
        .env("DIFFCTX_TEST_DEADLINE_EXPIRED", "1")
        .args([".", "--diff", "HEAD~1..HEAD", "-f", "json", "-q"])
        .output()
        .expect("run diffctx");
    let doc = json_doc(&out);
    assert_eq!(doc["coverage"]["status"], "partial");
    assert!(
        limit_reasons(&doc).contains(&"deadline".to_string()),
        "{:?}",
        limit_reasons(&doc)
    );
    assert_eq!(doc["changed_files"], serde_json::json!(["util.py"]));
}

/// The watchdog behind the cooperative deadline: when the worker does not
/// answer in time the binary exits 124 with the actionable message.
#[test]
fn the_watchdog_exits_124_when_the_worker_does_not_answer() {
    let tmp = code_change_repo();
    let out = Command::new(&*BIN)
        .current_dir(tmp.path())
        .env("DIFFCTX_TEST_WATCHDOG_GRACE_SECS", "0")
        .args([".", "--diff", "HEAD~1..HEAD", "-q"])
        .output()
        .expect("run diffctx");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(124), "{stderr}");
    assert!(stderr.contains("wall-clock deadline"), "{stderr}");
    assert!(out.stdout.is_empty());
}

/// Locate prints the commit message; it goes through the same sanitizer as
/// pack.
#[test]
fn locate_redacts_the_commit_message() {
    let tmp = code_change_repo();
    std::fs::write(
        tmp.path().join("util.py"),
        "def add(a, b):\n    return b + a\n",
    )
    .expect("write");
    git(tmp.path(), &["add", "-A"]);
    git(
        tmp.path(),
        &[
            "commit",
            "-q",
            "-m",
            "wire client; temp key AKIAIOSFODNN7EXAMPLE", // pragma: allowlist secret
        ],
    );
    let out = run(
        tmp.path(),
        &[".", "--diff", "HEAD~1..HEAD", "--mode", "locate", "-q"],
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(!text.contains("AKIAIOSFODNN7EXAMPLE"), "{text}"); // pragma: allowlist secret
    let doc = json_doc(&out);
    assert!(
        doc["commit_message"]
            .as_str()
            .unwrap_or_default()
            .contains("[REDACTED:aws_access_key]"),
        "{doc}"
    );
}

/// A Latin-1 source file is text: fragmented, and named in the coverage
/// block as decoded lossily.
#[test]
fn a_non_utf8_changed_file_is_decoded_and_disclosed() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    std::fs::write(repo.join("caf.py"), b"def cafe():\n    return 1\n").expect("write");
    commit_all(repo, "initial");
    std::fs::write(repo.join("caf.py"), b"def cafe():\n    return '\xe9'\n").expect("write");
    commit_all(repo, "latin-1");
    let doc = json_doc(&run(
        repo,
        &[".", "--diff", "HEAD~1..HEAD", "-f", "json", "-q"],
    ));
    assert!(
        limit_reasons(&doc).contains(&"non_utf8_content".to_string()),
        "{doc}"
    );
    assert_eq!(
        doc["coverage"]["lossy_files"],
        serde_json::json!(["caf.py"])
    );
    assert!(doc["fragment_count"].as_u64().unwrap() >= 1, "{doc}");
}

/// A file with more fragments than the per-file cap keeps the function the
/// diff edited as the changed fragment, not a neighbour the cap happened to
/// keep.
#[test]
fn the_fragment_cap_never_drops_the_edited_function() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    let body = |edited: bool| -> String {
        (0..4000)
            .map(|i| {
                if edited && i == 2500 {
                    format!("def f{i}():\n    return {i} + 1\n\n\n")
                } else {
                    format!("def f{i}():\n    return {i}\n\n\n")
                }
            })
            .collect()
    };
    std::fs::write(repo.join("big.py"), body(false)).expect("write");
    commit_all(repo, "initial");
    std::fs::write(repo.join("big.py"), body(true)).expect("write");
    commit_all(repo, "edit f2500");
    let doc = json_doc(&run(
        repo,
        &[".", "--diff", "HEAD~1..HEAD", "-f", "json", "-q"],
    ));
    let changed: Vec<&serde_json::Value> = doc["fragments"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["role"] == "changed")
        .collect();
    assert!(
        changed.iter().any(|f| f["symbol"] == "f2500"),
        "changed fragments: {changed:?}"
    );
    assert!(
        changed.iter().all(|f| f["symbol"] != "f2499"),
        "a neighbour was labelled as the change: {changed:?}"
    );
    assert!(limit_reasons(&doc).contains(&"fragment_limit".to_string()));
}

/// A mode-only change has no text hunk; the artifact lists it as changed and
/// unrepresented, and the hint does not claim the tree matches HEAD.
#[cfg(unix)]
#[test]
fn a_hunkless_change_is_listed_and_the_hint_stays_truthful() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = code_change_repo();
    let path = tmp.path().join("app.py");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let out = run(tmp.path(), &[".", "--diff", "-f", "json", "-q"]);
    let doc = json_doc(&out);
    assert_eq!(doc["changed_files"], serde_json::json!(["app.py"]));
    assert_eq!(doc["changes"][0]["represented"], false);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("matches HEAD"), "{stderr}");
}

/// Budget trimming that leaves a changed file without a fragment is
/// `degraded`, and every inventory row says whether it is represented.
#[test]
fn a_trimmed_artifact_is_degraded_with_exact_representation() {
    let tmp = wide_repo();
    let out = run(
        tmp.path(),
        &[
            ".",
            "--diff",
            "HEAD~1..HEAD",
            "--budget",
            "4000",
            "-f",
            "json",
            "-q",
        ],
    );
    let doc = json_doc(&out);
    assert_eq!(doc["coverage"]["status"], "degraded");
    let fragments = doc["fragments"].as_array().unwrap();
    let represented: std::collections::BTreeSet<&str> = fragments
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    let changes = doc["changes"].as_array().unwrap();
    assert_eq!(changes.len(), 60);
    for change in changes {
        let path = change["path"].as_str().unwrap();
        assert_eq!(
            change["represented"].as_bool(),
            Some(represented.contains(path)),
            "{path}"
        );
    }
    let unrepresented = changes.iter().filter(|c| c["represented"] == false).count();
    assert_eq!(unrepresented, 60 - represented.len());
    assert!(unrepresented > 0);
}

#[test]
fn a_larger_budget_never_shows_less_of_the_change() {
    let tmp = TempDir::new().expect("tempdir");
    init_repo(tmp.path());
    let list = |k: usize| {
        let body: String = (0..300).map(|i| format!("    {},\n", i * k)).collect();
        format!("VALUES = [\n{body}]\n")
    };
    std::fs::write(tmp.path().join("vals.py"), list(1)).unwrap();
    commit_all(tmp.path(), "base");
    std::fs::write(tmp.path().join("vals.py"), list(7)).unwrap();
    commit_all(tmp.path(), "rewrite every value");

    let mut shown = Vec::new();
    for budget in ["600", "900", "1200", "1500", "2000", "3000"] {
        let out = run(
            tmp.path(),
            &[
                ".",
                "--diff",
                "HEAD~1..HEAD",
                "--budget",
                budget,
                "-f",
                "json",
                "-q",
            ],
        );
        let doc: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
        assert_eq!(
            doc["changes"][0]["represented"], true,
            "budget {budget}: {}",
            doc["coverage"]
        );
        let lines = doc["fragments"][0]["lines"].as_str().unwrap();
        let (start, end) = lines.split_once('-').unwrap();
        shown.push(end.parse::<u32>().unwrap() - start.parse::<u32>().unwrap() + 1);
    }
    let mut sorted = shown.clone();
    sorted.sort_unstable();
    assert_eq!(shown, sorted, "evidence shrank as the budget grew");
}

#[test]
fn a_long_range_says_how_many_commits_the_list_left_out() {
    let tmp = TempDir::new().expect("tempdir");
    init_repo(tmp.path());
    std::fs::write(tmp.path().join("app.py"), "def f():\n    return 0\n").unwrap();
    commit_all(tmp.path(), "base");
    for i in 1..=25 {
        std::fs::write(
            tmp.path().join("app.py"),
            format!("def f():\n    return {i}\n"),
        )
        .unwrap();
        commit_all(tmp.path(), &format!("step {i}"));
    }
    for mode in ["pack", "locate"] {
        let out = run(
            tmp.path(),
            &[
                ".",
                "--diff",
                "HEAD~25..HEAD",
                "--mode",
                mode,
                "-f",
                "json",
                "-q",
            ],
        );
        let doc: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
        assert_eq!(
            doc["commit_messages"].as_array().unwrap().len(),
            20,
            "{mode}"
        );
        assert_eq!(doc["commit_count"], 25, "{mode}");
    }
}

/// #306: an oversized file nobody selected costs the answer nothing — the
/// run is complete and the cap goes to `capped`; a limit on a changed file
/// raises the headline and names the file.
#[test]
fn a_per_file_limit_raises_the_headline_only_where_the_answer_is() {
    let tmp = code_change_repo();
    let repo = tmp.path();
    let big: String = (0..6000)
        .map(|i| format!("def f{i}(x):\n    return x + {i}\n"))
        .collect();
    std::fs::write(repo.join("generated_tables.py"), big).expect("write");
    commit_all(repo, "a big unrelated module");
    std::fs::write(
        repo.join("util.py"),
        "def add(a, b):\n    return a + b\n\n\ndef sub(a, b):\n    return b - a\n",
    )
    .expect("write");
    commit_all(repo, "flip sub");
    let json = |range: &str| -> serde_json::Value {
        let out = run(repo, &[".", "--diff", range, "--format", "json", "--quiet"]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).expect("json")
    };
    let doc = json("HEAD~1..HEAD");
    assert_eq!(
        doc["coverage"]["status"], "complete",
        "{:#}",
        doc["coverage"]
    );
    assert!(
        doc["coverage"]["capped"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r == "file_too_large")
    );
    assert!(doc["coverage"].get("limited_files").is_none());

    let mut latin1 = b"def greet():\n    return '".to_vec();
    latin1.extend_from_slice(&[0xe9, 0xe8]);
    latin1.extend_from_slice(b"'\n");
    std::fs::write(repo.join("util.py"), latin1).expect("write");
    commit_all(repo, "latin-1 util");
    let doc = json("HEAD~1..HEAD");
    assert_eq!(
        doc["coverage"]["status"], "partial",
        "{:#}",
        doc["coverage"]
    );
    assert_eq!(
        doc["coverage"]["limited_files"],
        serde_json::json!([{"path": "util.py", "reason": "non_utf8_content"}])
    );
}

/// #300: `--budget` bounds what the reader receives — the serialized locate
/// document — not only the bodies the items stand for.
#[test]
fn a_locate_answer_fits_its_budget() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    let workflow = |condition: &str| -> String {
        let mut text = String::from("spec:\n  dependencies:\n    - name: push\n  triggers:\n");
        for i in 0..120 {
            text.push_str(&format!(
                "    - template:\n        name: build-{i}\n        conditions: {condition}\n        k8s:\n          operation: create\n          source:\n            resource:\n              kind: Workflow\n              metadata:\n                generateName: build-{i}-\n"
            ));
        }
        text
    };
    std::fs::write(repo.join("sensor.yaml"), workflow("push")).expect("write");
    commit_all(repo, "initial");
    std::fs::write(repo.join("sensor.yaml"), workflow("push && tag")).expect("write");
    commit_all(repo, "gate every trigger");
    let out = run(
        repo,
        &[
            ".",
            "--diff",
            "HEAD~1..HEAD",
            "--mode",
            "locate",
            "--budget",
            "2000",
            "-q",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut doc: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    // What an MCP client receives: the overflow pointer list is stripped there.
    doc.as_object_mut().unwrap().remove("overflow");
    let tokens = _diffctx::tokenizer::count_tokens(&doc.to_string());
    assert!(
        tokens <= 2000,
        "locate answer is {tokens} tokens for --budget 2000"
    );
    assert!(doc["item_count"].as_u64().unwrap() >= 1);
}

/// #330: a language this build has no grammar for is read as text by design;
/// listing it as unparsed dropped confidence to 0 on every SQL or Kotlin
/// change. A file with a grammar and no structure still is a blind spot.
#[test]
fn a_file_without_a_grammar_is_not_a_blind_spot() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    std::fs::write(repo.join("schema.sql"), "CREATE TABLE t (id int);\n").expect("write");
    std::fs::write(repo.join("App.kt"), "fun main() {}\n").expect("write");
    commit_all(repo, "initial");
    std::fs::write(
        repo.join("schema.sql"),
        "CREATE TABLE t (id int, name text);\n",
    )
    .expect("write");
    std::fs::write(repo.join("App.kt"), "fun main() { println(1) }\n").expect("write");
    commit_all(repo, "change");
    let out = run(
        repo,
        &[".", "--diff", "HEAD~1..HEAD", "--mode", "locate", "-q"],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    let unparsed = &doc["coverage"]["unparsed_files"];
    assert!(unparsed.is_null(), "{:#}", doc["coverage"]);
}

/// #340: co-change history is read before the range's head, not before the
/// checkout — later commits must not leak into a historical range.
#[test]
fn a_historical_range_ignores_history_after_its_head() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    std::fs::write(repo.join("ledger.py"), "def post(x):\n    return x\n").expect("write");
    std::fs::write(
        repo.join("audit.py"),
        "from ledger import post\n\n\ndef trail(y):\n    return post(y)\n",
    )
    .expect("write");
    commit_all(repo, "base");
    std::fs::write(repo.join("ledger.py"), "def post(x):\n    return x + 1\n").expect("write");
    commit_all(repo, "the change");
    let range = {
        let out = Command::new("git")
            .current_dir(repo)
            .args(["rev-parse", "HEAD~1", "HEAD"])
            .output()
            .expect("rev-parse");
        let revs: Vec<String> = String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect();
        format!("{}..{}", revs[0], revs[1])
    };
    let history_edges = |repo: &Path| -> usize {
        let out = Command::new(&*BIN)
            .current_dir(repo)
            .env("DIFFCTX_TRACE_BUILDERS", "1")
            .args([".", "--diff", &range, "--format", "json", "--quiet"])
            .output()
            .expect("run diffctx");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stderr)
            .lines()
            .filter(|l| l.starts_with("builder history"))
            .filter_map(|l| {
                l.rsplit(", ")
                    .next()?
                    .strip_suffix(" edges")?
                    .parse::<usize>()
                    .ok()
            })
            .sum()
    };
    assert_eq!(history_edges(repo), 0);
    for i in 0..6 {
        std::fs::write(
            repo.join("ledger.py"),
            format!("def post(x):\n    return x + {i}\n"),
        )
        .expect("write");
        std::fs::write(
            repo.join("audit.py"),
            format!("from ledger import post\n\n\ndef trail(y):\n    return post(y) + {i}\n"),
        )
        .expect("write");
        commit_all(repo, "later, together");
    }
    assert_eq!(
        history_edges(repo),
        0,
        "commits after the range's head reached its co-change graph"
    );
}

/// #340: a range's own commits do not vouch for its files, and history that
/// does links the two files once, representative to representative.
#[test]
fn co_change_reads_history_before_the_range_and_links_files_once() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    let ledger = |i: u32| {
        format!(
            "def post(x):\n    return x + {i}\n\n\ndef void(x):\n    return -x\n\n\ndef total(xs):\n    return sum(xs)\n"
        )
    };
    let audit = |i: u32| {
        format!(
            "from ledger import post\n\n\ndef trail(y):\n    return post(y) + {i}\n\n\ndef flag(y):\n    return y > 0\n\n\ndef note(y):\n    return str(y)\n"
        )
    };
    std::fs::write(repo.join("ledger.py"), ledger(0)).expect("write");
    std::fs::write(repo.join("audit.py"), audit(0)).expect("write");
    std::fs::write(repo.join("other.py"), "def unrelated():\n    return 1\n").expect("write");
    commit_all(repo, "base");
    let history_edges = |range: &str| -> usize {
        let out = Command::new(&*BIN)
            .current_dir(repo)
            .env("DIFFCTX_TRACE_BUILDERS", "1")
            .args([".", "--diff", range, "--format", "json", "--quiet"])
            .output()
            .expect("run diffctx");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stderr)
            .lines()
            .filter(|l| l.starts_with("builder history"))
            .filter_map(|l| {
                l.rsplit(", ")
                    .next()?
                    .strip_suffix(" edges")?
                    .parse::<usize>()
                    .ok()
            })
            .sum()
    };
    for i in 1..=6 {
        std::fs::write(repo.join("ledger.py"), ledger(i)).expect("write");
        std::fs::write(repo.join("audit.py"), audit(i)).expect("write");
        commit_all(repo, "together, inside the range");
    }
    assert_eq!(
        history_edges("HEAD~6..HEAD"),
        0,
        "the range's own commits reached its co-change graph"
    );
    // A bare revision and a time window diff against the working tree: their
    // commits run up to HEAD and are the range's own just the same.
    for range in ["HEAD~6", "1d"] {
        assert_eq!(history_edges(range), 0, "{range}");
    }
    std::fs::write(repo.join("other.py"), "def unrelated():\n    return 2\n").expect("write");
    commit_all(repo, "the change");
    let linked = history_edges("HEAD~1..HEAD");
    assert!(
        (1..=2).contains(&linked),
        "one file pair is one edge (each way at most), not every fragment pair: {linked}"
    );
}

/// #296, #327: under a tight budget the rewritten functions are placed whole
/// once every changed file has its witness, not left as one-line stubs while
/// the budget goes to one-line constants.
#[test]
fn rewritten_functions_are_placed_whole_before_the_budget_goes_elsewhere() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    let consts = |bump: u32| -> String {
        (0..30)
            .map(|i| format!("LIMIT_{i} = {}\n", i + bump))
            .collect()
    };
    let function = |name: &str, k: u32, op: &str| -> String {
        let body: String = (0..14)
            .map(|j| format!("    total = total + item.weight {op} {}\n", j + k))
            .collect();
        format!(
            "def {name}(items):\n    total = 0\n    for item in items:\n{body}    return total\n\n\n"
        )
    };
    let file = |bump: u32, op: &str| {
        format!(
            "{}\n\n{}{}",
            consts(bump),
            function("step", 0, op),
            function("push", 100, op)
        )
    };
    std::fs::write(repo.join("worker.py"), file(0, "*")).expect("write");
    commit_all(repo, "base");
    std::fs::write(repo.join("worker.py"), file(1, "//")).expect("write");
    commit_all(repo, "change");
    let out = Command::new(&*BIN)
        .current_dir(repo)
        .args([
            ".",
            "--diff",
            "HEAD~1..HEAD",
            "--mode",
            "locate",
            "--budget",
            "900",
            "-q",
        ])
        .output()
        .expect("run diffctx");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).expect("locate json");
    let spans: Vec<(String, String)> = doc["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|i| {
            (
                i["symbol"].as_str().unwrap_or("").to_string(),
                i["lines"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect();
    for name in ["step", "push"] {
        let (_, lines) = spans
            .iter()
            .find(|(s, _)| s == name)
            .unwrap_or_else(|| panic!("{name} missing: {spans:?}"));
        let (a, b) = lines.split_once('-').expect("range");
        let len = b.parse::<u32>().unwrap() - a.parse::<u32>().unwrap() + 1;
        assert!(len > 10, "{name} placed as {lines}, a stub: {spans:?}");
    }
}

/// #308, #326: an added module is one hunk over every fragment in it, locals
/// included. Under a budget that cannot hold the module, its exported
/// functions are shown — whole or as signatures — and a one-line local never
/// stands in for a function that is itself left out.
#[test]
fn an_added_module_shows_its_functions_not_their_locals() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    std::fs::create_dir_all(repo.join("src")).expect("mkdir");
    std::fs::write(
        repo.join("src/graph.ts"),
        "export interface Graph { edges: Map<number, number[]>; }\n",
    )
    .expect("write");
    commit_all(repo, "base");
    let function = |i: usize| {
        format!(
            "export function order{i}(graph: Graph, sink: number, limit: number): number[] {{\n  const queue = [sink];\n  const seen = new Set<number>();\n  const order: number[] = [];\n  let depth = 0;\n  while (queue.length > 0 && depth < limit) {{\n    const v = queue.shift()!;\n    if (seen.has(v)) continue;\n    seen.add(v);\n    const deps = graph.edges.get(v) ?? [];\n    for (const d of deps) {{\n      queue.push(d + {i});\n    }}\n    order.push(v);\n    depth += 1;\n  }}\n  return order;\n}}\n"
        )
    };
    let functions: Vec<String> = (0..8).map(function).collect();
    let module = format!(
        "import type {{ Graph }} from './graph';\n\n{}",
        functions.join("\n")
    );
    std::fs::write(repo.join("src/engine.ts"), &module).expect("write");
    commit_all(repo, "add engine");

    let mut spans: Vec<(u32, u32)> = Vec::new();
    for (n, line) in module.lines().enumerate() {
        let n = n as u32 + 1;
        if line.starts_with("export function") {
            spans.push((n, n));
        } else if line == "}" {
            spans.last_mut().expect("a function is open").1 = n;
        }
    }
    let range = |lines: &str| -> (u32, u32) {
        let (a, b) = lines.split_once('-').expect("a line range");
        (a.parse().expect("start"), b.parse().expect("end"))
    };
    for (mode, list) in [("pack", "fragments"), ("locate", "items")] {
        let out = run(
            repo,
            &[
                ".",
                "--diff",
                "HEAD~1..HEAD",
                "--mode",
                mode,
                "--budget",
                "1200",
                "-f",
                "json",
                "-q",
            ],
        );
        let doc = json_doc(&out);
        let shown: Vec<(String, (u32, u32))> = doc[list]
            .as_array()
            .expect("a list")
            .iter()
            .filter(|f| f["path"] == "src/engine.ts")
            .map(|f| {
                (
                    f["kind"].as_str().unwrap_or("").to_string(),
                    range(f["lines"].as_str().unwrap_or("")),
                )
            })
            .collect();
        let represented =
            |(start, _): (u32, u32)| shown.iter().any(|(_, (a, b))| *a <= start && start <= *b);
        let functions_shown = spans.iter().filter(|s| represented(**s)).count();
        assert!(
            functions_shown >= 3,
            "{mode}: {functions_shown} of 8 exported functions shown: {shown:?}"
        );
        for (kind, (a, b)) in &shown {
            let around = spans.iter().find(|(s, e)| s < a && b <= e);
            if let Some(function) = around {
                assert!(
                    represented(*function),
                    "{mode}: a {kind} at {a}-{b} stands in for the function at {function:?}: {shown:?}"
                );
            }
        }
    }
}

/// #327: a budget that cannot hold the whole change shows the rewritten
/// production function whole, not as its signature beside whole test
/// functions. The first pass had stubbed it to leave room, and the sweep spent
/// the rest on the tests before the stub's upgrade ran.
#[test]
fn a_rewritten_production_function_is_whole_before_its_tests() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    let write = |path: &str, text: &str| {
        let full = repo.join(path);
        std::fs::create_dir_all(full.parent().expect("a parent")).expect("mkdir");
        std::fs::write(full, text).expect("write");
    };
    let tests = |decimal: bool| -> String {
        let cases: Vec<String> = (0..12)
            .map(|i| {
                let (quantity, price) = (i + 1, i + 10);
                if decimal {
                    let discountable = if i % 2 == 0 { "True" } else { "False" };
                    format!(
                        "def test_invoice_total_case_{i}():\n    class Line:\n        quantity = {quantity}\n        unit_price = {price}\n        discountable = {discountable}\n    assert invoice_total([Line()], Decimal('0.19'), discount=Decimal('0.1')) > 0\n"
                    )
                } else {
                    format!(
                        "def test_invoice_total_case_{i}():\n    class Line:\n        quantity = {quantity}\n        unit_price = {price}\n    assert invoice_total([Line()], 0.0) == {}\n",
                        quantity * price
                    )
                }
            })
            .collect();
        let imports = if decimal {
            "from decimal import Decimal\nfrom shop.billing import invoice_total\n"
        } else {
            "from shop.billing import invoice_total\n"
        };
        format!("{imports}\n{}", cases.join("\n"))
    };
    write("shop/__init__.py", "");
    write("shop/migrations/__init__.py", "");
    write(
        "shop/migrations/0001_initial.py",
        "from django.db import migrations, models\n\n\nclass Migration(migrations.Migration):\n    initial = True\n    dependencies = []\n    operations = [\n        migrations.CreateModel(\n            name=\"Invoice\",\n            fields=[\n                (\"id\", models.AutoField(primary_key=True)),\n                (\"total\", models.DecimalField(max_digits=12, decimal_places=2)),\n            ],\n        ),\n    ]\n",
    );
    write(
        "shop/billing.py",
        "def invoice_total(lines, tax_rate):\n    subtotal = 0\n    for line in lines:\n        subtotal += line.quantity * line.unit_price\n    return subtotal * (1 + tax_rate)\n",
    );
    write(
        "docs/billing.md",
        "# Billing\n\nInvoices sum their lines and apply the tax rate.\n\n## Rounding\n\nTotals are not rounded.\n",
    );
    write("tests/test_billing.py", &tests(false));
    commit_all(repo, "base");
    write(
        "shop/billing.py",
        "from decimal import Decimal, ROUND_HALF_UP\n\n\ndef invoice_total(lines, tax_rate, discount=None, currency=\"EUR\"):\n    subtotal = Decimal(\"0\")\n    for line in lines:\n        amount = Decimal(str(line.quantity)) * Decimal(str(line.unit_price))\n        if getattr(line, \"discountable\", True) and discount is not None:\n            amount = amount * (Decimal(\"1\") - Decimal(str(discount)))\n        subtotal += amount\n    taxed = subtotal * (Decimal(\"1\") + Decimal(str(tax_rate)))\n    quantum = Decimal(\"1\") if currency == \"JPY\" else Decimal(\"0.01\")\n    return taxed.quantize(quantum, rounding=ROUND_HALF_UP)\n",
    );
    write(
        "shop/migrations/0002_invoice_currency_discount.py",
        "from django.db import migrations, models\n\n\nclass Migration(migrations.Migration):\n    dependencies = [(\"shop\", \"0001_initial\")]\n    operations = [\n        migrations.AddField(\n            model_name=\"invoice\",\n            name=\"currency\",\n            field=models.CharField(default=\"EUR\", max_length=3),\n        ),\n        migrations.AddField(\n            model_name=\"invoice\",\n            name=\"discount\",\n            field=models.DecimalField(null=True, max_digits=5, decimal_places=4),\n        ),\n    ]\n",
    );
    write("tests/test_billing.py", &tests(true));
    write(
        "docs/billing.md",
        "# Billing\n\nInvoices sum their lines, apply an optional discount to discountable lines and then the tax rate.\n\n## Rounding\n\nTotals are rounded half-up to the currency's minor unit (whole yen for JPY).\n",
    );
    commit_all(repo, "decimal invoice totals with discount and currency");

    let out = run(
        repo,
        &[
            ".",
            "--diff",
            "HEAD~1..HEAD",
            "--budget",
            "1500",
            "-f",
            "json",
            "-q",
        ],
    );
    let doc = json_doc(&out);
    let shown: Vec<(String, String)> = doc["fragments"]
        .as_array()
        .expect("fragments")
        .iter()
        .map(|f| {
            (
                f["path"].as_str().unwrap_or("").to_string(),
                f["lines"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect();
    let covers = |path: &str, from: u32, to: u32| {
        shown.iter().any(|(p, lines)| {
            p == path
                && lines.split_once('-').is_some_and(|(a, b)| {
                    a.parse::<u32>().is_ok_and(|a| a <= from)
                        && b.parse::<u32>().is_ok_and(|b| b >= to)
                })
        })
    };
    assert!(
        covers("shop/billing.py", 4, 13),
        "invoice_total (4-13) is not whole: {shown:?}"
    );
    assert!(
        shown.iter().any(|(p, _)| p == "tests/test_billing.py"),
        "the changed tests stay in: {shown:?}"
    );
}

/// #411: fitting the document to its budget cuts docs before tests and tests
/// before production code. By position alone `zsrc/x.py` went first, since
/// it sorts after `README.md` and `tests/`, and the rewritten production
/// function was the one change missing from the artifact.
#[test]
fn fitting_the_budget_cuts_docs_and_tests_before_production() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    let steps = |field: &str| -> String {
        (0..12)
            .map(|k| format!("    step_{k} = ledger.lookup(order.item_{k}, order.{field}_{k})\n"))
            .collect()
    };
    let docs = |what: &str| -> String {
        let lines: String = (0..12)
            .map(|k| format!("Line {k} of the documentation for the ledger {what} process.\n"))
            .collect();
        format!("# Ledger\n\n{lines}")
    };
    let write = |field: &str, what: &str| {
        for dir in ["zsrc", "tests"] {
            std::fs::create_dir_all(repo.join(dir)).expect("mkdir");
        }
        std::fs::write(
            repo.join("zsrc/x.py"),
            format!(
                "def settle_order(order, ledger):\n{}    return step_0\n",
                steps(field)
            ),
        )
        .expect("write");
        std::fs::write(
            repo.join("tests/test_x.py"),
            format!(
                "from zsrc.x import settle_order\n\n\ndef test_settle_order():\n{}    return step_0\n",
                steps(field)
            ),
        )
        .expect("write");
        std::fs::write(repo.join("README.md"), docs(what)).expect("write");
    };
    write("qty", "settlement");
    commit_all(repo, "A");
    write("quantity", "settlement and reconciliation");
    commit_all(repo, "B");

    let out = run(
        repo,
        &[
            ".",
            "--diff",
            "HEAD~1..HEAD",
            "--budget",
            "900",
            "-f",
            "json",
            "-q",
        ],
    );
    let doc = json_doc(&out);
    let shown: Vec<(String, String)> = doc["fragments"]
        .as_array()
        .expect("fragments")
        .iter()
        .map(|f| {
            (
                f["path"].as_str().unwrap_or("").to_string(),
                f["lines"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect();
    assert!(
        shown.contains(&("zsrc/x.py".to_string(), "1-14".to_string())),
        "the production function is not whole: {shown:?}"
    );
}

/// #311: a Django migration says "Generated by Django" in its header, but it
/// is the range's schema change, not generated output to rank last.
#[test]
fn a_migration_is_not_a_generated_file_and_protoc_output_still_is() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    std::fs::write(repo.join("README.md"), "x\n").expect("write");
    commit_all(repo, "base");
    std::fs::create_dir_all(repo.join("app/migrations")).expect("mkdir");
    std::fs::write(
        repo.join("app/migrations/0002_add.py"),
        "# Generated by Django 4.2 on 2026-01-01\nfrom django.db import migrations\n\n\nclass Migration(migrations.Migration):\n    operations = []\n",
    )
    .expect("write");
    std::fs::write(
        repo.join("app/msg.pb.go"),
        "// Code generated by protoc-gen-go. DO NOT EDIT.\npackage pb\n",
    )
    .expect("write");
    commit_all(repo, "migration");
    let out = Command::new(&*BIN)
        .current_dir(repo)
        .args([".", "--diff", "HEAD~1..HEAD", "-f", "json", "-q"])
        .output()
        .expect("run diffctx");
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    let class = |p: &str| {
        doc["changes"]
            .as_array()
            .expect("changes")
            .iter()
            .find(|c| c["path"] == p)
            .map(|c| c["class"].as_str().unwrap_or("").to_string())
            .unwrap_or_default()
    };
    assert_ne!(class("app/migrations/0002_add.py"), "generated", "{doc}");
    assert_eq!(class("app/msg.pb.go"), "generated", "{doc}");
}
