use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

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
        .and_then(Path::parent)
        .expect("repository root")
        .join("tests")
        .join("cases")
        .join("diff")
}

#[derive(Clone)]
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
            // The directory-qualified relative path (not just the file stem)
            // so `cargo test --test yaml_cases -- languages/rust/` can filter
            // a single stratum by substring — this is what the nightly
            // per-directory gate and local debugging rely on.
            let rel = path.strip_prefix(&dir).unwrap_or(&path);
            let name = rel
                .with_extension("")
                .components()
                .filter_map(|c| c.as_os_str().to_str())
                .collect::<Vec<_>>()
                .join("/");
            DiscoveredCase { name, path }
        })
        .collect();
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    entries
}

/// Groups a case by top-level category, splitting `languages/*` one level
/// deeper so each language gets its own stratum (`languages/rust`,
/// `languages/python`, ...) instead of being lumped under one `languages`
/// bucket.
fn stratum_of(cases_dir: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(cases_dir).unwrap_or(path);
    let mut components = rel.components();
    let first = components
        .next()
        .and_then(|c| c.as_os_str().to_str())
        .unwrap_or("root")
        .to_string();
    if first == "languages" {
        let second = components
            .next()
            .and_then(|c| c.as_os_str().to_str())
            .unwrap_or("misc");
        format!("languages/{second}")
    } else {
        first
    }
}

// Historic depth: before this sampler existed, DIFFCTX_YAML_CASES_LIMIT
// truncated the path-sorted case list, which deterministically selected
// algorithm/algorithm_001..020 (`algorithm` sorts first among the top-level
// directories and alone has far more than 20 cases). Keep that same depth of
// algorithm coverage as a floor so switching to stratified sampling doesn't
// silently regress it.
const ALGORITHM_STRATUM: &str = "algorithm";
const ALGORITHM_FLOOR: usize = 20;

/// Picks a directory-aware sample instead of truncating the path-sorted case
/// list: a flat `truncate(n)` always selected `algorithm/algorithm_001..020`,
/// so none of the 38 `languages/*` directories (768 cases) ever ran
/// pre-merge. Every stratum (each top-level category, and each
/// `languages/*` directory) gets at least one case; `algorithm` additionally
/// keeps its historic depth; any remaining budget is distributed round-robin
/// in stratum-name order, so growing the limit broadens coverage evenly
/// instead of piling more cases into one directory.
fn stratified_sample(cases: Vec<DiscoveredCase>, target: usize) -> Vec<DiscoveredCase> {
    let dir = cases_dir();
    let mut groups: BTreeMap<String, Vec<DiscoveredCase>> = BTreeMap::new();
    for case in cases {
        groups
            .entry(stratum_of(&dir, &case.path))
            .or_default()
            .push(case);
    }

    let mut cursor: BTreeMap<String, usize> = groups.keys().cloned().map(|k| (k, 0usize)).collect();
    let mut selected: Vec<DiscoveredCase> = Vec::new();

    // Round 0: one case from every stratum so no directory is ever dark.
    for (key, group) in groups.iter() {
        if selected.len() >= target {
            break;
        }
        if let Some(case) = group.first() {
            selected.push(case.clone());
            *cursor.get_mut(key).unwrap() = 1;
        }
    }

    // Top up `algorithm` toward its historic floor before spreading any
    // further budget round-robin.
    if let Some(group) = groups.get(ALGORITHM_STRATUM) {
        let floor = ALGORITHM_FLOOR.min(group.len());
        let c = cursor.get_mut(ALGORITHM_STRATUM).unwrap();
        while *c < floor && selected.len() < target {
            selected.push(group[*c].clone());
            *c += 1;
        }
    }

    // Remaining budget: round-robin over every stratum in name order.
    loop {
        if selected.len() >= target {
            break;
        }
        let mut progressed = false;
        for (key, group) in groups.iter() {
            if selected.len() >= target {
                break;
            }
            let c = cursor.get_mut(key).unwrap();
            if *c < group.len() {
                selected.push(group[*c].clone());
                *c += 1;
                progressed = true;
            }
        }
        if !progressed {
            break;
        }
    }

    selected
}

fn run_git(repo: &Path, args: &[&str]) -> Result<(), String> {
    let out = _diffctx::git::git_command(repo)
        .args(args)
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
    let out = _diffctx::git::git_command(repo)
        .args(["rev-parse", "HEAD"])
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

/// Case names whose oracle score is known to sit below the threshold. Without
/// this list a stratified sample cannot gate anything: ~17% of the corpus is
/// below threshold, so any directory-aware sample is red on arrival and the
/// only way to keep CI green is to sample the same handful of passing cases
/// forever — which is how 35 of 38 language directories went dark.
///
/// The list is enforced in BOTH directions. An unlisted case that fails is a
/// regression. A listed case that *passes* is also an error, because a stale
/// entry would silently re-absorb a future regression in that case.
fn known_below_threshold() -> &'static std::collections::BTreeSet<String> {
    static BASELINE: std::sync::OnceLock<std::collections::BTreeSet<String>> =
        std::sync::OnceLock::new();
    BASELINE.get_or_init(|| {
        // Set DIFFCTX_YAML_IGNORE_BASELINE=1 to surface the true below-threshold
        // set instead of the gated view. The nightly job uses it to track how
        // large the baseline still is, so shrinking it stays visible work rather
        // than something a green CI hides.
        if std::env::var("DIFFCTX_YAML_IGNORE_BASELINE").is_ok() {
            return std::collections::BTreeSet::new();
        }
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("known_below_threshold.txt");
        let raw = fs::read_to_string(&path).unwrap_or_default();
        raw.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(str::to_string)
            .collect()
    })
}

fn run_case(case_name: &str, case_path: &Path) -> Result<(), Failed> {
    run_case_with_scoring(case_name, case_path, ScoringMode::Ego, false)
}

/// `contributes_context_only` skips the oracle and asserts only that the
/// mode produced related context at all. The oracle thresholds were
/// calibrated for Ego, so holding PPR and BM25 to them would just encode
/// Ego's answer as the expectation; what actually needs guarding is the
/// failure mode where a selectable mode silently degrades to
/// changed-files-only while still exiting 0.
fn run_case_with_scoring(
    case_name: &str,
    case_path: &Path,
    scoring: ScoringMode,
    contributes_context_only: bool,
) -> Result<(), Failed> {
    let raw = fs::read_to_string(case_path).map_err(|e| Failed::from(format!("read: {e}")))?;
    let case: TestCase =
        serde_yaml::from_str(&raw).map_err(|e| Failed::from(format!("parse YAML: {e}")))?;

    if case.repo.initial_files.is_empty()
        && case.repo.changed_files.is_empty()
        && case.repo.deleted_files.is_empty()
        && case.repo.renamed_files.is_empty()
    {
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

    // Renames first (an explicit changed_files entry for the destination
    // then overwrites the moved content — rename+edit), deletions last so a
    // case cannot resurrect a deleted path by also listing it in
    // changed_files. `git add -A` records all three change kinds.
    for (from, to) in &case.repo.renamed_files {
        let src = repo.join(from);
        let dst = repo.join(to);
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent).map_err(|e| Failed::from(format!("mkdir: {e}")))?;
        }
        fs::rename(&src, &dst).map_err(|e| Failed::from(format!("rename {from} -> {to}: {e}")))?;
    }
    write_files(repo, &changed).map_err(Failed::from)?;
    for rel in &case.repo.deleted_files {
        fs::remove_file(repo.join(rel)).map_err(|e| Failed::from(format!("delete {rel}: {e}")))?;
    }
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
        scoring,
        60,
    )
    .map_err(|e| Failed::from(format!("pipeline: {e}")))?;

    if contributes_context_only {
        if output.fragments.is_empty() {
            return Err(Failed::from(format!("{scoring:?} selected nothing at all")));
        }
        let context = output
            .fragments
            .iter()
            .filter(|f| f.role.as_deref() != Some("changed"))
            .count();
        if context == 0 {
            return Err(Failed::from(format!(
                "{scoring:?} degraded to changed-files-only: {} fragments, none of them context",
                output.fragments.len()
            )));
        }
        return Ok(());
    }

    let oracle = evaluate_oracle(&case, &output);

    let min_score = case.min_score.unwrap_or_else(|| {
        std::env::var("DIFFCTX_YAML_MIN_SCORE")
            .ok()
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(10.0)
    });

    let baselined = known_below_threshold().contains(case_name);
    match (oracle.score >= min_score, baselined) {
        (true, false) => Ok(()),
        (false, true) => Ok(()),
        (true, true) => Err(Failed::from(format!(
            "case now scores {:.1}% >= min {min_score:.1}% but is still listed in \
             tests/known_below_threshold.txt — remove it from that file, otherwise a \
             future regression here would be silently absorbed",
            oracle.score
        ))),
        (false, false) => Err(Failed::from(format_failure(&oracle, &output, min_score))),
    }
}

fn main() {
    let args = Arguments::from_args();

    let limit: Option<usize> = std::env::var("DIFFCTX_YAML_CASES_LIMIT")
        .ok()
        .and_then(|s| s.parse().ok());

    let mut cases = discover_cases();
    if let Some(n) = limit {
        cases = stratified_sample(cases, n);
    }

    let mut trials: Vec<Trial> = cases
        .iter()
        .map(|c| {
            let (name, path) = (c.name.clone(), c.path.clone());
            Trial::test(c.name.clone(), move || run_case(&name, &path)).with_kind("yaml")
        })
        .collect();

    // Every oracle case above runs Ego. `--scoring ppr`, `bm25` and `rrf` are
    // documented, user-selectable modes whose only other coverage asserts an
    // exit code, so any of them could degrade to changed-files-only and ship green.
    for c in cases.iter().filter(|c| c.name.starts_with("selection/")) {
        for scoring in [ScoringMode::Ppr, ScoringMode::Bm25, ScoringMode::Rrf] {
            let (name, path) = (c.name.clone(), c.path.clone());
            let label = format!("scoring/{scoring:?}/{}", c.name).to_lowercase();
            trials.push(
                Trial::test(label, move || {
                    run_case_with_scoring(&name, &path, scoring, true)
                })
                .with_kind("yaml"),
            );
        }
    }

    libtest_mimic::run(&args, trials).exit();
}
