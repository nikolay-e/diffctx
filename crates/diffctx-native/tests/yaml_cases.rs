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
use common::{TestCase, XFail, calculate_budget, evaluate_oracle, garbage_files};

// The impact and symbol corpus runs the shipped binary, which `support` finds.
#[path = "common/oracle/mod.rs"]
mod oracle;
mod support;
use oracle::case::Mode;

fn cases_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("repository root")
        .join("tests")
        .join("cases")
}

fn cases_dir() -> PathBuf {
    cases_root().join("diff")
}

/// Which corpus a case belongs to: `diff` cases select context through the
/// library; `impact` and `symbol` cases run the shipped binary.
#[derive(Clone, Copy)]
enum Corpus {
    Diff,
    Oracle(Mode),
}

#[derive(Clone)]
struct DiscoveredCase {
    name: String,
    path: PathBuf,
    stratum: String,
    corpus: Corpus,
    /// The language directory of an impact or symbol case.
    language: String,
}

fn discover_cases() -> Vec<DiscoveredCase> {
    let mut cases = discover_in(&cases_dir(), Corpus::Diff);
    for mode in [Mode::Impact, Mode::Symbol] {
        cases.extend(discover_in(
            &cases_root().join(mode.name()),
            Corpus::Oracle(mode),
        ));
    }
    cases
}

fn discover_in(dir: &Path, corpus: Corpus) -> Vec<DiscoveredCase> {
    if !dir.exists() {
        return Vec::new();
    }
    let mut entries: Vec<_> = WalkDir::new(dir)
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
            let rel = path.strip_prefix(dir).unwrap_or(&path);
            let relative_name = rel
                .with_extension("")
                .components()
                .filter_map(|c| c.as_os_str().to_str())
                .collect::<Vec<_>>()
                .join("/");
            match corpus {
                Corpus::Diff => DiscoveredCase {
                    name: relative_name,
                    stratum: stratum_of(dir, &path),
                    corpus,
                    language: String::new(),
                    path,
                },
                Corpus::Oracle(mode) => {
                    let language = relative_name.split('/').next().unwrap_or("").to_string();
                    DiscoveredCase {
                        name: format!("{}/{relative_name}", mode.name()),
                        stratum: format!("{}/{language}", mode.name()),
                        corpus,
                        language,
                        path,
                    }
                }
            }
        })
        .collect();
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    entries
}

/// Groups a `diff` case by top-level category, splitting `languages/*` one
/// level deeper so each language gets its own stratum (`languages/rust`,
/// `languages/python`, ...) instead of being lumped under one `languages`
/// bucket. An impact or symbol case's stratum is `<mode>/<language>`.
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
    let mut groups: BTreeMap<String, Vec<DiscoveredCase>> = BTreeMap::new();
    for case in cases {
        groups.entry(case.stratum.clone()).or_default().push(case);
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
    // Reported, not scored. `score = recall * (1 - forbidden_rate)` carries no
    // term for over-selection, so a case that finds everything and buries it
    // reads the same as one that finds everything cleanly. The share of emitted
    // fragments that actually carry the diff separates those two at a glance,
    // and it is the number a reader wants first when recall is 100% and the
    // case still failed. Diagnostic only — changing the formula changes the
    // gate, which is not a debugging decision.
    let changed = output
        .fragments
        .iter()
        .filter(|f| f.role.as_deref() == Some("changed"))
        .count();
    if !output.fragments.is_empty() {
        msg.push_str(&format!(
            "changed-fragment share: {:.2} ({} of {})\n",
            changed as f64 / output.fragments.len() as f64,
            changed,
            output.fragments.len()
        ));
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
        // A name may carry a trailing `# gh#N open` or a reason.
        raw.lines()
            .map(|l| l.split('#').next().unwrap_or("").trim())
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect()
    })
}

/// The gate runs the SHIPPED default and nothing else — see the header of
/// `known_below_threshold.txt` for why that matters (#175: three harnesses each
/// scored a private tau nobody ships).
///
/// `DIFFCTX_PROBE_MODE` exists to compare a candidate scoring mode against that
/// default on the same corpus, which is how RRF was measured at 450 and PIT at
/// 410 against EGO's 371. It is a measurement knob, deliberately NOT wired into
/// the baseline file: a probe run is only meaningful with
/// `DIFFCTX_YAML_IGNORE_BASELINE=1`, since the baseline enumerates EGO's
/// failures and a different mode fails a different set.
fn run_case(case_name: &str, case_path: &Path) -> Result<(), Failed> {
    let mode = match std::env::var("DIFFCTX_PROBE_MODE").as_deref() {
        Ok("rrf") => ScoringMode::Rrf,
        Ok("pit") => ScoringMode::Pit,
        Ok("ppr") => ScoringMode::Ppr,
        Ok("bm25") => ScoringMode::Bm25,
        _ => ScoringMode::Ego,
    };
    run_case_with_scoring(case_name, case_path, mode, false)
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

    // An active `xfail:` marks a case as known-broken. It used to `return
    // Ok(())` here — never running the case at all — so the marker was an
    // unconditional silent skip and an XPASS was unobservable: the day the
    // underlying bug got fixed, the case still reported a pass and nobody
    // learned the marker was stale. `known_below_threshold.txt` is enforced
    // bidirectionally for exactly this reason; the two suppression mechanisms
    // now agree. The case runs, and a passing xfail is a failure telling you to
    // drop the marker.
    let xfail_active = case.xfail.as_ref().is_some_and(XFail::is_active);

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

    // `DIFFCTX_PROBE_TAU` is the tau counterpart of `DIFFCTX_PROBE_MODE`: a
    // measurement knob for comparing a candidate stopping threshold on the
    // same corpus, deliberately NOT wired into the baseline file, and only
    // meaningful with `DIFFCTX_YAML_IGNORE_BASELINE=1`. The default run passes
    // `None` — the shipped configuration is "unspecified, resolved per
    // scorer", and naming the constant here would pin the ungated scorers to
    // the gated operating point instead of the one they ship with (#175).
    let tau = std::env::var("DIFFCTX_PROBE_TAU")
        .ok()
        .and_then(|s| s.parse::<f64>().ok());
    let output = build_diff_context(
        repo,
        Some(&diff_range),
        Some(budget),
        _diffctx::config::limits::DEFAULT_PPR_ALPHA,
        tau,
        false,
        false,
        scoring,
        60,
    )
    .map_err(|e| Failed::from(format!("pipeline: {e}")))?;
    // A run its deadline stopped answered a smaller question: under a loaded
    // full `cargo test` that is a timing failure, and it says so instead of
    // failing on a score with nothing to read (#390).
    if let Some(stop) = output.coverage.as_ref().and_then(|c| {
        c.limit_reasons.iter().find(|r| {
            matches!(
                r,
                _diffctx::resource::LimitReason::Deadline
                    | _diffctx::resource::LimitReason::Cancelled
            )
        })
    }) {
        return Err(Failed::from(format!(
            "the run stopped at {stop:?} before scoring: a timing failure under load, not a selection result"
        )));
    }

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

    // SCHEMA.md promises that with `auto_garbage: true` the runner verifies the
    // injected files' markers stay out of the output; until now only the
    // injection half existed.
    if case.fixtures.auto_garbage {
        let leaked: Vec<String> = output
            .fragments
            .iter()
            .filter(|f| {
                f.path.starts_with("unrelated_dir/")
                    || f.content.as_deref().is_some_and(|c| c.contains("GARBAGE_"))
            })
            .map(|f| f.path.clone())
            .collect();
        if !leaked.is_empty() {
            return Err(Failed::from(format!(
                "auto_garbage leaked into the output: {}",
                leaked.join(", ")
            )));
        }
    }

    let oracle = evaluate_oracle(&case, &output);

    let min_score = case.min_score.unwrap_or_else(|| {
        std::env::var("DIFFCTX_YAML_MIN_SCORE")
            .ok()
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(10.0)
    });

    let passing = oracle.score >= min_score;
    if xfail_active {
        return if passing {
            Err(Failed::from(format!(
                "xfail case now scores {:.1}% >= min {min_score:.1}% — drop the `xfail:` \
                 block from the case, otherwise a future regression here would be \
                 silently absorbed",
                oracle.score
            )))
        } else {
            Ok(())
        };
    }

    let baselined = known_below_threshold().contains(case_name);
    match (passing, baselined) {
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

/// Cases running right now, with when they started. A hung case cannot be
/// stopped from another thread, so the supervisor ends the whole process,
/// naming it: a bounded red run instead of seven silent hours (#356).
static IN_FLIGHT: std::sync::LazyLock<
    std::sync::Mutex<std::collections::BTreeMap<String, std::time::Instant>>,
> = std::sync::LazyLock::new(Default::default);

const DEFAULT_CASE_LIMIT_SECS: u64 = 600;

fn supervised(name: &str, work: impl FnOnce() -> Result<(), Failed>) -> Result<(), Failed> {
    IN_FLIGHT
        .lock()
        .unwrap()
        .insert(name.to_string(), std::time::Instant::now());
    let result = _diffctx::profile::tagged(&format!("yaml_cases::{name}"), work);
    IN_FLIGHT.lock().unwrap().remove(name);
    result
}

fn start_supervisor() {
    let limit = std::time::Duration::from_secs(
        std::env::var("DIFFCTX_YAML_CASE_LIMIT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(DEFAULT_CASE_LIMIT_SECS),
    );
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(std::time::Duration::from_secs(5));
            let hung: Vec<String> = IN_FLIGHT
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, started)| started.elapsed() > limit)
                .map(|(name, started)| format!("{name} ({}s)", started.elapsed().as_secs()))
                .collect();
            if !hung.is_empty() {
                eprintln!(
                    "yaml_cases: case(s) exceeded {}s and cannot be stopped in-process, \
                     aborting the run: {}",
                    limit.as_secs(),
                    hung.join(", ")
                );
                dump_thread_stacks();
                std::process::exit(101);
            }
        }
    });
}

/// Every thread's stack, printed before the supervisor ends the run, so a
/// hang names the wait it sat in instead of only the cases (#544). A process
/// cannot walk its other threads' stacks itself; a debugger attached to it
/// can.
fn dump_thread_stacks() {
    let pid = std::process::id().to_string();
    let dumpers: &[(&str, &[&str])] = if cfg!(target_os = "macos") {
        &[("sample", &[&pid, "1"])]
    } else {
        &[
            ("gdb", &["-p", &pid, "-batch", "-ex", "thread apply all bt"]),
            ("eu-stack", &["-p", &pid]),
        ]
    };
    allow_a_debugger_to_attach();
    for (tool, args) in dumpers {
        match std::process::Command::new(tool).args(*args).output() {
            Ok(out) if out.status.success() && !out.stdout.is_empty() => {
                eprintln!(
                    "yaml_cases: thread stacks ({tool}):\n{}",
                    String::from_utf8_lossy(&out.stdout)
                );
                return;
            }
            Ok(out) => eprintln!(
                "yaml_cases: {tool} could not dump the threads: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
            Err(e) => eprintln!("yaml_cases: {tool} could not run: {e}"),
        }
    }
}

/// Yama's default lets a process be traced by its ancestors only, and the
/// dumper is a child.
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
fn allow_a_debugger_to_attach() {
    // SAFETY: PR_SET_PTRACER takes plain integers and touches no memory.
    unsafe {
        libc::prctl(libc::PR_SET_PTRACER, libc::PR_SET_PTRACER_ANY, 0, 0, 0);
    }
}

#[cfg(not(target_os = "linux"))]
const fn allow_a_debugger_to_attach() {}

/// An impact or symbol case: the shipped binary on a real repository, judged
/// against the case's relations, under the same bidirectional baseline as the
/// `diff` corpus. A case that fails to parse or build is an error whatever
/// the baseline says; only a wrong answer is baselined.
fn run_oracle_case(
    case_name: &str,
    case_path: &Path,
    mode: Mode,
    language: &str,
) -> Result<(), Failed> {
    let mut outcome =
        oracle::evaluate(case_name, case_path, mode, language).map_err(Failed::from)?;
    if outcome.disputed {
        outcome.report.state = "disputed";
        oracle::report::record(outcome.report);
        return Ok(());
    }
    let red = !outcome.failures.is_empty();
    let baselined = known_below_threshold().contains(case_name);
    let (state, verdict) = match (red, baselined) {
        (false, false) => ("pass", Ok(())),
        (true, true) => ("baselined_red", Ok(())),
        (false, true) => (
            "stale_baseline",
            Err(Failed::from(
                "case now passes but is still listed in tests/known_below_threshold.txt — \
                 remove it from that file, otherwise a future regression here would be \
                 silently absorbed",
            )),
        ),
        (true, false) => ("fail", Err(Failed::from(outcome.failures.join("\n\n")))),
    };
    outcome.report.state = state;
    oracle::report::record(outcome.report);
    verdict
}

/// The modes the binary ships, read from `src/main.rs`: the `--mode` values
/// and the `--symbol` lookup.
fn shipped_modes() -> Result<Vec<String>, String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("main.rs");
    let source = fs::read_to_string(&path).map_err(|e| format!("read {path:?}: {e}"))?;
    let field = source
        .find("mode: Option<String>")
        .ok_or("main.rs no longer declares `mode: Option<String>`")?;
    let attribute = "value_parser = [";
    let start = source[..field]
        .rfind(attribute)
        .ok_or("the `--mode` option no longer lists its values in `value_parser = [..]`")?;
    let list = &source[start + attribute.len()..field];
    let end = list
        .find(']')
        .ok_or("the `--mode` value list does not close")?;
    let mut modes: Vec<String> = list[..end]
        .split('"')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect();
    if source.contains("symbol: Option<String>") {
        modes.push("symbol".to_string());
    }
    Ok(modes)
}

fn yaml_cases_under(dir: &Path) -> usize {
    WalkDir::new(dir)
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
        .count()
}

/// Every mode the binary ships has a corpus with at least one case. `locate`
/// ranks the selection `pack` prints, so the selection corpus is its corpus
/// until locate gets cases of its own.
fn modes_shipped_have_cases() -> Result<(), Failed> {
    let mut missing = Vec::new();
    for mode in shipped_modes().map_err(Failed::from)? {
        let corpus = match mode.as_str() {
            "pack" | "locate" => "diff",
            "impact" => "impact",
            "symbol" => "symbol",
            other => {
                return Err(Failed::from(format!(
                    "the binary ships the mode `{other}` and the harness knows no corpus for it: \
                     add its cases under tests/cases and name them in modes_shipped_have_cases"
                )));
            }
        };
        if yaml_cases_under(&cases_root().join(corpus)) == 0 {
            missing.push(format!("{mode} (tests/cases/{corpus})"));
        }
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(Failed::from(format!(
            "shipped modes with no case: {}",
            missing.join(", ")
        )))
    }
}

fn main() {
    let args = Arguments::from_args();
    start_supervisor();

    let limit: Option<usize> = std::env::var("DIFFCTX_YAML_CASES_LIMIT")
        .ok()
        .and_then(|s| s.parse().ok());

    let mut cases = discover_cases();
    // DIFFCTX_CASE_TAG=<tag>: only the impact and symbol cases carrying the tag
    // run, against the binary DIFFCTX_BIN names; the in-process corpus is skipped.
    let tag = std::env::var("DIFFCTX_CASE_TAG")
        .ok()
        .filter(|t| !t.is_empty());
    if let Some(tag) = &tag {
        cases.retain(|c| {
            matches!(c.corpus, Corpus::Oracle(_)) && oracle::case::has_tag(&c.path, tag)
        });
    }
    if let Some(n) = limit {
        cases = stratified_sample(cases, n);
    }

    let mut trials: Vec<Trial> = cases
        .iter()
        .map(|c| {
            let (name, path) = (c.name.clone(), c.path.clone());
            match c.corpus {
                Corpus::Diff => Trial::test(c.name.clone(), move || {
                    supervised(&name, || run_case(&name, &path))
                })
                .with_kind("yaml"),
                Corpus::Oracle(mode) => {
                    let language = c.language.clone();
                    Trial::test(c.name.clone(), move || {
                        supervised(&name, || run_oracle_case(&name, &path, mode, &language))
                    })
                    .with_kind("yaml")
                }
            }
        })
        .collect();
    if tag.is_none() {
        trials.push(
            Trial::test("modes_shipped_have_cases", modes_shipped_have_cases).with_kind("oracle"),
        );
    }

    // Every oracle case above runs Ego. `--scoring ppr`, `bm25` and `rrf` are
    // documented, user-selectable modes whose only other coverage asserts an
    // exit code, so any of them could degrade to changed-files-only and ship green.
    for c in cases.iter().filter(|c| c.name.starts_with("selection/")) {
        for scoring in [ScoringMode::Ppr, ScoringMode::Bm25, ScoringMode::Rrf] {
            let (name, path) = (c.name.clone(), c.path.clone());
            let label = format!("scoring/{scoring:?}/{}", c.name).to_lowercase();
            trials.push(
                Trial::test(label.clone(), move || {
                    supervised(&label, || {
                        run_case_with_scoring(&name, &path, scoring, true)
                    })
                })
                .with_kind("yaml"),
            );
        }
    }

    let conclusion = libtest_mimic::run(&args, trials);
    if let Err(e) = oracle::report::write_if_requested() {
        eprintln!("yaml_cases: could not write DIFFCTX_ORACLE_REPORT: {e}");
    }
    conclusion.exit();
}
