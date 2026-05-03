use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use _diffctx::mode::ScoringMode;
use _diffctx::pipeline::build_diff_context;
use _diffctx::render::DiffContextOutput;
use libtest_mimic::{Arguments, Failed, Trial};
use tempfile::TempDir;
use walkdir::WalkDir;

mod common;
use common::{TestCase, calculate_budget, evaluate_oracle, garbage_files};

fn cases_dir() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .expect("diffctx parent")
        .join("tests")
        .join("cases")
        .join("diff")
}

struct DiscoveredCase {
    name: String,
    path: PathBuf,
}

fn discover_cases() -> Vec<DiscoveredCase> {
    let dir = cases_dir();
    if !dir.exists() {
        return Vec::new();
    }
    let mut entries: Vec<_> = WalkDir::new(&dir)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| {
            if !e.file_type().is_file() {
                return false;
            }
            let name = match e.path().file_name().and_then(|n| n.to_str()) {
                Some(n) => n,
                None => return false,
            };
            if name.starts_with('.') || name == "SCHEMA.md" {
                return false;
            }
            e.path()
                .extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| x == "yaml" || x == "yml")
        })
        .map(|e| {
            let path = e.path().to_path_buf();
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "unnamed".into());
            DiscoveredCase { name: stem, path }
        })
        .collect();
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    entries
}

fn run_git(repo: &Path, args: &[&str]) -> Result<(), String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .map_err(|e| format!("spawn git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(())
}

fn write_files(repo: &Path, files: &BTreeMap<String, String>) -> Result<(), String> {
    for (rel, content) in files {
        let full = repo.join(rel);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("mkdir {parent:?}: {e}"))?;
        }
        fs::write(&full, content).map_err(|e| format!("write {full:?}: {e}"))?;
    }
    Ok(())
}

fn rev_parse_head(repo: &Path) -> Result<String, String> {
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo)
        .output()
        .map_err(|e| format!("spawn git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "rev-parse HEAD failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn assemble_initial(case: &TestCase) -> BTreeMap<String, String> {
    let mut initial = case.repo.initial_files.clone();
    for (k, v) in &case.fixtures.distractors {
        initial.insert(k.clone(), v.clone());
    }
    if case.fixtures.auto_garbage {
        for (k, v) in garbage_files() {
            initial.insert(k.clone(), v.clone());
        }
    }
    initial
}

fn assemble_changed(case: &TestCase) -> BTreeMap<String, String> {
    let mut changed = case.repo.changed_files.clone();
    for (k, v) in &case.fixtures.distractors {
        changed.entry(k.clone()).or_insert_with(|| v.clone());
    }
    if case.fixtures.auto_garbage {
        for (k, v) in garbage_files() {
            changed.entry(k.clone()).or_insert_with(|| v.clone());
        }
    }
    changed
}

fn format_failure(
    oracle: &common::OracleResult,
    output: &DiffContextOutput,
    min_score: f64,
) -> String {
    let mut msg = format!(
        "score {:.1}% < min {:.1}% (recall={:.0}%, forbidden_rate={:.0}%)\n",
        oracle.score,
        min_score,
        oracle.recall * 100.0,
        oracle.forbidden_rate * 100.0,
    );
    if !oracle.missing_required.is_empty() {
        msg.push_str(&format!(
            "missing required: {:?}\n",
            oracle.missing_required
        ));
    }
    if !oracle.hit_forbidden.is_empty() {
        msg.push_str(&format!("present forbidden: {:?}\n", oracle.hit_forbidden));
    }
    msg.push_str(&format!(
        "selected fragments ({}):\n",
        output.fragments.len()
    ));
    for f in &output.fragments {
        msg.push_str(&format!(
            "  {}:{} [{}]{}\n",
            f.path,
            f.lines,
            f.kind,
            f.symbol
                .as_deref()
                .map(|s| format!(" {s}"))
                .unwrap_or_default()
        ));
    }
    msg
}

fn run_case(case_path: &Path) -> Result<(), Failed> {
    let raw = fs::read_to_string(case_path).map_err(|e| Failed::from(format!("read: {e}")))?;
    let case: TestCase =
        serde_yaml::from_str(&raw).map_err(|e| Failed::from(format!("parse YAML: {e}")))?;

    if case.repo.initial_files.is_empty() && case.repo.changed_files.is_empty() {
        return Err(Failed::from(
            "case has no initial_files and no changed_files",
        ));
    }

    if case.xfail.as_ref().map(|x| x.is_active()).unwrap_or(false) {
        return Ok(());
    }

    let initial = assemble_initial(&case);
    let changed = assemble_changed(&case);

    let tmp = TempDir::new().map_err(|e| Failed::from(format!("tempdir: {e}")))?;
    let repo = tmp.path();
    run_git(repo, &["init", "--quiet"]).map_err(Failed::from)?;
    run_git(repo, &["config", "user.email", "test@example.com"]).map_err(Failed::from)?;
    run_git(repo, &["config", "user.name", "test"]).map_err(Failed::from)?;
    run_git(repo, &["config", "commit.gpgsign", "false"]).map_err(Failed::from)?;

    write_files(repo, &initial).map_err(Failed::from)?;
    run_git(repo, &["add", "-A"]).map_err(Failed::from)?;
    run_git(
        repo,
        &["commit", "--quiet", "-m", "Initial commit", "--allow-empty"],
    )
    .map_err(Failed::from)?;
    let base_sha = rev_parse_head(repo).map_err(Failed::from)?;

    write_files(repo, &changed).map_err(Failed::from)?;
    run_git(repo, &["add", "-A"]).map_err(Failed::from)?;
    run_git(
        repo,
        &[
            "commit",
            "--quiet",
            "-m",
            &case.repo.commit_message,
            "--allow-empty",
        ],
    )
    .map_err(Failed::from)?;

    let budget = calculate_budget(&case);
    let diff_range = format!("{base_sha}..HEAD");

    let output = build_diff_context(
        repo,
        Some(&diff_range),
        Some(budget),
        0.85,
        0.0,
        false,
        false,
        ScoringMode::Hybrid,
        60,
    )
    .map_err(|e| Failed::from(format!("pipeline: {e}")))?;

    let oracle = evaluate_oracle(&case, &output);

    let min_score = case.min_score.unwrap_or_else(|| {
        std::env::var("DIFFCTX_YAML_MIN_SCORE")
            .ok()
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(10.0)
    });

    if oracle.score >= min_score {
        Ok(())
    } else {
        Err(Failed::from(format_failure(&oracle, &output, min_score)))
    }
}

fn main() {
    let args = Arguments::from_args();

    let limit: Option<usize> = std::env::var("DIFFCTX_YAML_CASES_LIMIT")
        .ok()
        .and_then(|s| s.parse().ok());

    let mut cases = discover_cases();
    if let Some(n) = limit {
        cases.truncate(n);
    }

    let trials: Vec<Trial> = cases
        .into_iter()
        .map(|c| Trial::test(c.name, move || run_case(&c.path)).with_kind("yaml"))
        .collect();

    libtest_mimic::run(&args, trials).exit();
}
