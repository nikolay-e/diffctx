#![allow(dead_code)]

use std::io::{Write, stderr};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use clap::Parser;
use rayon::prelude::*;
use rustc_hash::FxHashMap;
use serde::Serialize;

use _diffctx::memory_pipeline::{MemoryRepo, build_diff_context_in_memory};
use _diffctx::mode::ScoringMode;

#[path = "../tests/common/mod.rs"]
mod common;
use common::{TestCase, calculate_budget, evaluate_oracle, garbage_files};

#[derive(Parser)]
#[command(name = "diffctx-test", about = "Run YAML diff test cases in-memory")]
struct Cli {
    #[arg(default_value = "../tests/cases/diff")]
    cases_dir: PathBuf,

    #[arg(short, long)]
    filter: Option<String>,

    #[arg(long)]
    tag: Option<String>,

    #[arg(long)]
    verbose: bool,

    #[arg(long)]
    xfail_only: bool,

    #[arg(long)]
    json: bool,
}

#[derive(Debug, Serialize)]
struct TestResult {
    name: String,
    passed: bool,
    xfail: bool,
    xfail_category: Option<String>,
    score: f64,
    missing_required: Vec<String>,
    hit_forbidden: Vec<String>,
    elapsed_secs: f64,
}

#[derive(Debug, Serialize)]
struct ScoreBuckets {
    score_100: usize,
    score_90_99: usize,
    score_70_89: usize,
    score_50_69: usize,
    score_below_50: usize,
}

#[derive(Debug, Serialize)]
struct JsonReport {
    total: usize,
    passed: usize,
    failed: usize,
    xfailed: usize,
    xpassed: usize,
    score_distribution: ScoreBuckets,
    wall_time_secs: f64,
    results: Vec<TestResult>,
}

static STDERR_LOCK: Mutex<()> = Mutex::new(());

fn locked_eprintln(msg: &str) {
    let _guard = STDERR_LOCK.lock().unwrap();
    let _ = writeln!(stderr(), "{msg}");
}

fn build_memory_repo(case: &TestCase) -> MemoryRepo {
    let mut initial: FxHashMap<String, String> = case
        .repo
        .initial_files
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let mut changed: FxHashMap<String, String> = case
        .repo
        .changed_files
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    for (k, v) in &initial.clone() {
        changed.entry(k.clone()).or_insert_with(|| v.clone());
    }

    for (k, v) in &case.fixtures.distractors {
        initial.insert(k.clone(), v.clone());
        changed.insert(k.clone(), v.clone());
    }

    if case.fixtures.auto_garbage {
        for (k, v) in garbage_files() {
            initial.insert(k.clone(), v.clone());
            changed.insert(k.clone(), v.clone());
        }
    }

    MemoryRepo {
        name: case.name.clone(),
        initial_files: initial,
        changed_files: changed,
    }
}

fn run_single_test(case: &TestCase) -> TestResult {
    let t0 = Instant::now();
    let repo = build_memory_repo(case);
    let budget = calculate_budget(case);

    let output =
        build_diff_context_in_memory(&repo, Some(budget), 0.60, 0.05, false, ScoringMode::Hybrid);

    let oracle = evaluate_oracle(case, &output);
    let xfail_active = case.xfail.as_ref().map(|x| x.is_active()).unwrap_or(false);
    let is_xfail = xfail_active && !oracle.passed;
    let xfail_category = case.xfail.as_ref().and_then(|x| x.category.clone());

    TestResult {
        name: case.name.clone(),
        passed: oracle.passed,
        xfail: is_xfail,
        xfail_category,
        score: oracle.score,
        missing_required: oracle.missing_required,
        hit_forbidden: oracle.hit_forbidden,
        elapsed_secs: t0.elapsed().as_secs_f64(),
    }
}

#[derive(Debug, serde::Deserialize)]
struct TestsWrapper {
    tests: Vec<TestCase>,
}

fn load_all_cases(dir: &Path) -> Vec<(PathBuf, TestCase)> {
    let pattern = format!("{}/**/*.yaml", dir.display());
    let mut cases = Vec::new();

    for entry in glob::glob(&pattern).expect("invalid glob pattern") {
        let path = match entry {
            Ok(p) => p,
            Err(_) => continue,
        };

        let file_name_str = path.file_name().map(|n| n.to_string_lossy().to_string());
        if let Some(ref n) = file_name_str {
            if n.starts_with('.') || n == "SCHEMA.md" {
                continue;
            }
        }

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                locked_eprintln(&format!("WARN: cannot read {}: {}", path.display(), e));
                continue;
            }
        };

        if let Ok(case) = serde_yaml::from_str::<TestCase>(&content) {
            let mut c = case;
            if c.name.is_empty() {
                c.name = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "unnamed".into());
            }
            cases.push((path, c));
            continue;
        }

        if let Ok(list) = serde_yaml::from_str::<Vec<TestCase>>(&content) {
            for case in list {
                cases.push((path.clone(), case));
            }
            continue;
        }

        if let Ok(wrapper) = serde_yaml::from_str::<TestsWrapper>(&content) {
            for case in wrapper.tests {
                cases.push((path.clone(), case));
            }
            continue;
        }

        panic!(
            "test fixture {} did not match TestCase | Vec<TestCase> | TestsWrapper schema",
            path.display()
        );
    }

    cases.sort_by(|a, b| a.1.name.cmp(&b.1.name));
    cases
}

fn buckets(results: &[TestResult]) -> ScoreBuckets {
    ScoreBuckets {
        score_100: results
            .iter()
            .filter(|r| r.score >= 100.0 - f64::EPSILON)
            .count(),
        score_90_99: results
            .iter()
            .filter(|r| r.score >= 90.0 && r.score < 100.0 - f64::EPSILON)
            .count(),
        score_70_89: results
            .iter()
            .filter(|r| r.score >= 70.0 && r.score < 90.0)
            .count(),
        score_50_69: results
            .iter()
            .filter(|r| r.score >= 50.0 && r.score < 70.0)
            .count(),
        score_below_50: results.iter().filter(|r| r.score < 50.0).count(),
    }
}

fn print_human_summary(results: &[TestResult], wall_time: f64) {
    let passed = results.iter().filter(|r| r.passed && !r.xfail).count();
    let failed = results.iter().filter(|r| !r.passed && !r.xfail).count();
    let xfailed = results.iter().filter(|r| r.xfail).count();
    let xpassed = results
        .iter()
        .filter(|r| r.passed && r.xfail_category.is_some())
        .count();
    let b = buckets(results);

    eprintln!();
    eprintln!("{}", "=".repeat(50));
    eprintln!(
        "Results: {passed} passed, {failed} failed, {xfailed} xfailed{}",
        if xpassed > 0 {
            format!(", {xpassed} xpassed")
        } else {
            String::new()
        }
    );
    eprintln!("Score distribution:");
    eprintln!("  100%:  {}", b.score_100);
    eprintln!("  >=90%: {}", b.score_90_99);
    eprintln!("  >=70%: {}", b.score_70_89);
    eprintln!("  >=50%: {}", b.score_50_69);
    eprintln!("  <50%:  {}", b.score_below_50);
    eprintln!("Wall time: {wall_time:.1}s");
    eprintln!("{}", "=".repeat(50));
}

fn print_json_report(results: Vec<TestResult>, wall_time: f64) {
    let passed = results.iter().filter(|r| r.passed && !r.xfail).count();
    let failed = results.iter().filter(|r| !r.passed && !r.xfail).count();
    let xfailed = results.iter().filter(|r| r.xfail).count();
    let xpassed = results
        .iter()
        .filter(|r| r.passed && r.xfail_category.is_some())
        .count();
    let score_distribution = buckets(&results);

    let report = JsonReport {
        total: results.len(),
        passed,
        failed,
        xfailed,
        xpassed,
        score_distribution,
        wall_time_secs: wall_time,
        results,
    };
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
}

fn main() {
    let cli = Cli::parse();

    let all_cases = load_all_cases(&cli.cases_dir);
    let cases: Vec<&(PathBuf, TestCase)> = all_cases
        .iter()
        .filter(|(_, case)| {
            if let Some(ref f) = cli.filter {
                if !case.name.contains(f.as_str()) {
                    return false;
                }
            }
            if let Some(ref t) = cli.tag {
                if !case.tags.iter().any(|tag| tag.contains(t.as_str())) {
                    return false;
                }
            }
            if cli.xfail_only && case.xfail.is_none() {
                return false;
            }
            true
        })
        .collect();

    let total = cases.len();
    if total == 0 {
        locked_eprintln(&format!(
            "No test cases found in {}",
            cli.cases_dir.display()
        ));
        std::process::exit(1);
    }

    let n_workers = rayon::current_num_threads();
    if !cli.json {
        locked_eprintln(&format!("Running {total} tests ({n_workers} workers)..."));
    }

    let wall_start = Instant::now();
    let completed = AtomicUsize::new(0);

    let results: Vec<TestResult> = cases
        .par_iter()
        .map(|(_, case)| {
            let result = run_single_test(case);
            let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
            if !cli.json && (cli.verbose || !result.passed) {
                let icon = if result.xfail {
                    "\u{2298}"
                } else if result.passed {
                    "\u{2713}"
                } else {
                    "\u{2717}"
                };
                let mut line = format!(
                    "[{:5.1}s] {icon} {} ({:.1}%)",
                    result.elapsed_secs, result.name, result.score
                );
                if !result.missing_required.is_empty() {
                    line.push_str(&format!(
                        " [missing: {}]",
                        result.missing_required.join(", ")
                    ));
                }
                if !result.hit_forbidden.is_empty() {
                    line.push_str(&format!(
                        " [forbidden: {}]",
                        result.hit_forbidden.join(", ")
                    ));
                }
                if result.xfail {
                    if let Some(ref cat) = result.xfail_category {
                        line.push_str(&format!(" (xfail: {cat})"));
                    } else {
                        line.push_str(" (xfail)");
                    }
                }
                locked_eprintln(&line);
            }
            if !cli.json && cli.verbose && done % 500 == 0 {
                locked_eprintln(&format!("  ... {done}/{total} completed"));
            }
            result
        })
        .collect();

    let wall_time = wall_start.elapsed().as_secs_f64();
    let failed = results.iter().filter(|r| !r.passed && !r.xfail).count();

    if cli.json {
        print_json_report(results, wall_time);
    } else {
        print_human_summary(&results, wall_time);
    }

    if failed > 0 {
        std::process::exit(1);
    }
}
