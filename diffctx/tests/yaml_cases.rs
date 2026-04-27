//! Integration test runner for the YAML case corpus at
//! `<repo-root>/tests/cases/diff/**.yaml`.
//!
//! Replaces the deleted Python `tests/test_yaml_diff.py`. Each YAML case becomes
//! one libtest-mimic Trial. Default behavior runs every `.yaml` under the cases
//! directory; cap with `DIFFCTX_YAML_CASES_LIMIT=N` for fast pre-commit runs.
//!
//! `cargo test --release --test yaml_cases` from `diffctx/`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use _diffctx::mode::ScoringMode;
use _diffctx::pipeline::build_diff_context;
use _diffctx::render::DiffContextOutput;
use libtest_mimic::{Arguments, Failed, Trial};
use serde::Deserialize;
use tempfile::TempDir;
use walkdir::WalkDir;

#[derive(Debug, Deserialize, Default)]
struct YamlCase {
    #[serde(default)]
    repo: RepoSpec,
    #[serde(default)]
    fragments: Vec<DeclaredFragment>,
    #[serde(default)]
    oracle: Oracle,
    #[serde(default)]
    accept: Accept,
}

#[derive(Debug, Deserialize, Default)]
struct RepoSpec {
    #[serde(default)]
    initial_files: BTreeMap<String, String>,
    #[serde(default)]
    changed_files: BTreeMap<String, String>,
    #[serde(default = "default_commit_message")]
    commit_message: String,
}

#[derive(Debug, Deserialize, Default)]
struct DeclaredFragment {
    id: String,
    #[serde(default)]
    selector: Selector,
}

#[derive(Debug, Deserialize, Default, Clone)]
struct Selector {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    symbol: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    anchor: Option<String>,
    #[serde(default)]
    any_of: Option<Vec<Selector>>,
}

#[derive(Debug, Deserialize, Default)]
struct Oracle {
    #[serde(default)]
    required: Vec<String>,
    #[serde(default)]
    forbidden: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Accept {
    #[serde(default = "default_symbol_match")]
    symbol_match: String,
    #[serde(default)]
    kind_must_match: bool,
}

impl Default for Accept {
    fn default() -> Self {
        Self {
            symbol_match: default_symbol_match(),
            kind_must_match: false,
        }
    }
}

fn default_symbol_match() -> String {
    "exact".into()
}

fn default_commit_message() -> String {
    "Update files".into()
}

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
            e.file_type().is_file()
                && e.path()
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

fn match_path(candidate: &str, target: &str) -> bool {
    candidate == target
        || candidate.ends_with(&format!("/{target}"))
        || candidate
            .replace('\\', "/")
            .ends_with(&format!("/{target}"))
}

fn symbol_matches(frag_symbol: &str, expected: &str, mode: &str) -> bool {
    match mode {
        "prefix" => frag_symbol.starts_with(expected),
        "substring" => frag_symbol.contains(expected),
        _ => frag_symbol == expected,
    }
}

fn matches_selector(
    fragment: &_diffctx::render::FragmentEntry,
    selector: &Selector,
    accept: &Accept,
) -> bool {
    if let Some(any) = &selector.any_of {
        return any.iter().any(|s| matches_selector(fragment, s, accept));
    }

    if let Some(path) = &selector.path {
        if !match_path(&fragment.path, path) {
            return false;
        }
    }

    if let Some(symbol) = &selector.symbol {
        let frag_symbol = fragment.symbol.as_deref().unwrap_or("");
        if !symbol_matches(frag_symbol, symbol, &accept.symbol_match) {
            return false;
        }
    }

    if let Some(kind) = &selector.kind {
        if accept.kind_must_match && fragment.kind != *kind {
            return false;
        }
    }

    if let Some(anchor) = &selector.anchor {
        let content = fragment.content.as_deref().unwrap_or("");
        if !content.contains(anchor) && !fragment.path.contains(anchor) {
            return false;
        }
    }

    true
}

fn evaluate_oracle(
    output: &DiffContextOutput,
    case: &YamlCase,
    min_score: f64,
) -> Result<(), String> {
    let mut matched_ids: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for entry in &output.fragments {
        for decl in &case.fragments {
            if matches_selector(entry, &decl.selector, &case.accept) {
                matched_ids.insert(decl.id.as_str());
            }
        }
    }

    let required_total = case.oracle.required.len();
    let missing_required: Vec<&str> = case
        .oracle
        .required
        .iter()
        .filter(|id| !matched_ids.contains(id.as_str()))
        .map(String::as_str)
        .collect();
    let required_hits = required_total - missing_required.len();

    let forbidden_total = case.oracle.forbidden.len();
    let present_forbidden: Vec<&str> = case
        .oracle
        .forbidden
        .iter()
        .filter(|id| matched_ids.contains(id.as_str()))
        .map(String::as_str)
        .collect();
    let forbidden_hits = present_forbidden.len();

    let required_recall = if required_total == 0 {
        1.0
    } else {
        required_hits as f64 / required_total as f64
    };
    let forbidden_rate = if forbidden_total == 0 {
        0.0
    } else {
        forbidden_hits as f64 / forbidden_total as f64
    };
    let score = 100.0 * required_recall * (1.0 - forbidden_rate);

    if score >= min_score {
        return Ok(());
    }

    let mut msg = format!(
        "score {score:.1}% < min {min_score:.1}% (recall={:.0}%, forbidden_rate={:.0}%)\n",
        required_recall * 100.0,
        forbidden_rate * 100.0,
    );
    if !missing_required.is_empty() {
        msg.push_str(&format!("missing required: {missing_required:?}\n"));
    }
    if !present_forbidden.is_empty() {
        msg.push_str(&format!("present forbidden: {present_forbidden:?}\n"));
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
    Err(msg)
}

fn calculate_budget(case: &YamlCase) -> u32 {
    let mut content_tokens: u32 = 0;
    for content in case
        .repo
        .initial_files
        .values()
        .chain(case.repo.changed_files.values())
    {
        content_tokens = content_tokens.saturating_add(_diffctx::tokenizer::count_tokens(content));
    }
    let estimated_fragments =
        (case.repo.initial_files.len() + case.repo.changed_files.len()).max(2) as u32;
    let overhead: u32 = estimated_fragments.saturating_mul(20);
    let total = content_tokens.saturating_add(overhead);
    let scaled = (f64::from(total) * 2.5) as u32;
    scaled.max(500)
}

fn run_case(case_path: &Path) -> Result<(), Failed> {
    let raw = fs::read_to_string(case_path).map_err(|e| Failed::from(format!("read: {e}")))?;
    let case: YamlCase =
        serde_yaml::from_str(&raw).map_err(|e| Failed::from(format!("parse YAML: {e}")))?;

    if case.repo.initial_files.is_empty() && case.repo.changed_files.is_empty() {
        return Err(Failed::from(
            "case has no initial_files and no changed_files",
        ));
    }

    let tmp = TempDir::new().map_err(|e| Failed::from(format!("tempdir: {e}")))?;
    let repo = tmp.path();
    run_git(repo, &["init", "--quiet"]).map_err(Failed::from)?;
    run_git(repo, &["config", "user.email", "test@example.com"]).map_err(Failed::from)?;
    run_git(repo, &["config", "user.name", "test"]).map_err(Failed::from)?;
    run_git(repo, &["config", "commit.gpgsign", "false"]).map_err(Failed::from)?;

    write_files(repo, &case.repo.initial_files).map_err(Failed::from)?;
    run_git(repo, &["add", "-A"]).map_err(Failed::from)?;
    run_git(
        repo,
        &["commit", "--quiet", "-m", "Initial commit", "--allow-empty"],
    )
    .map_err(Failed::from)?;
    let base_sha = rev_parse_head(repo).map_err(Failed::from)?;

    write_files(repo, &case.repo.changed_files).map_err(Failed::from)?;
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

    let min_score = std::env::var("DIFFCTX_YAML_MIN_SCORE")
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(10.0);

    evaluate_oracle(&output, &case, min_score).map_err(Failed::from)
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
