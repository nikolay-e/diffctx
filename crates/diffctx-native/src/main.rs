use mimalloc::MiMalloc;
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use anyhow::Result;
use clap::{Parser, ValueEnum};
use tracing_subscriber::EnvFilter;

use _diffctx::config::limits::{
    DEFAULT_PIPELINE_TIMEOUT_SECONDS, DEFAULT_PPR_ALPHA, DEFAULT_SCORING,
};
use _diffctx::mode::ScoringMode;
use _diffctx::pipeline::build_diff_context;
use _diffctx::render::DiffContextOutput;
use _diffctx::tokenizer::count_tokens;

/// Mirrors `_UNLIMITED_BUDGET` in src/diffctx/_native/pipeline.py so `--budget -1`
/// means the same thing in both CLIs.
const UNLIMITED_BUDGET_TOKENS: u32 = 10_000_000;

/// Mirrors `_EXIT_EMPTY_DIFF` in src/diffctx/_app.py: a diff that yields no
/// semantic context is an actionable result, not a success.
const EXIT_EMPTY_DIFF: i32 = 4;

/// Mirrors the Python CLI: a closed stdout pipe ends the run as SIGPIPE would.
const EXIT_BROKEN_PIPE: i32 = 141;

/// Same contract as the Python CLI: a malformed invocation is exit 2.
const EXIT_USAGE: i32 = 2;

#[derive(Clone, Copy, ValueEnum)]
enum OutputFormat {
    Yaml,
    Json,
}

#[derive(Parser)]
#[command(
    name = "diffctx",
    version,
    about = "Semantic diff context selector",
    disable_version_flag = true
)]
struct Cli {
    /// Repository path to analyze
    #[arg(default_value = ".")]
    path: PathBuf,

    /// Print version. `-v` matches the Python CLI, `-V` the clap convention.
    #[arg(short = 'v', short_alias = 'V', long, action = clap::ArgAction::Version)]
    version: Option<bool>,

    /// Token budget for the whole artifact — the change summary is charged
    /// first and the selection spends the remainder: omit = auto, N = cap,
    /// -1 = unlimited, 0 = no fragments (use --full for changed files only)
    #[arg(long, allow_negative_numbers = true, value_parser = parse_budget)]
    budget: Option<i64>,

    /// Output format
    #[arg(short = 'f', long, default_value = "yaml", value_enum)]
    format: OutputFormat,

    /// Git diff range (e.g. HEAD~1..HEAD, main..feature) or a duration window
    /// ending now (24h, 8d, 90min, 1h30m); omitted or bare --diff uses the
    /// working tree vs HEAD
    #[arg(long = "diff", num_args = 0..=1, default_missing_value = "HEAD")]
    diff_ref: Option<String>,

    /// PPR damping: how tightly context clusters around changes, 0-1 exclusive
    #[arg(long, default_value_t = DEFAULT_PPR_ALPHA, value_parser = parse_alpha)]
    alpha: f64,

    /// Relevance threshold for full fragment content; lower = more context.
    /// Omitted resolves per scorer: the default for a gated one, the ungated
    /// operating point for a scorer that builds no graph — which is why this
    /// carries no clap default, so naming the default value explicitly is a
    /// request the pipeline can still tell apart from silence.
    #[arg(long, value_parser = parse_tau)]
    tau: Option<f64>,

    /// Skip fragment contents (structure only)
    #[arg(long)]
    no_content: bool,

    /// Only the changed files, every fragment, no related-code context
    #[arg(long)]
    full: bool,

    /// Relevance scoring mode
    #[arg(long, default_value = DEFAULT_SCORING, value_parser = _diffctx::mode::SCORING_MODE_NAMES.to_vec())]
    scoring: String,

    /// Output mode: `pack` = context with source bodies; `locate` = ranked
    /// navigation list with provenance reasons, JSON only (--format ignored)
    #[arg(long, default_value = "pack", value_parser = ["pack", "locate"])]
    mode: String,

    /// Wall-clock deadline in seconds; on expiry diffctx exits 124
    #[arg(long, default_value_t = DEFAULT_PIPELINE_TIMEOUT_SECONDS, value_parser = clap::value_parser!(u64).range(1..))]
    timeout: u64,

    /// Suppress the token summary on stderr
    #[arg(short = 'q', long)]
    quiet: bool,
}

// Both bounds are checked by clap, before any git call, so a typo exits 2
// as a usage error instead of 1 after the whole git phase has run.
fn parse_alpha(raw: &str) -> Result<f64, String> {
    let alpha: f64 = raw.parse().map_err(|e| format!("{e}"))?;
    if alpha > 0.0 && alpha < 1.0 {
        Ok(alpha)
    } else {
        Err(format!("must be in (0, 1), got {raw}"))
    }
}

fn parse_tau(raw: &str) -> Result<f64, String> {
    let tau: f64 = raw.parse().map_err(|e| format!("{e}"))?;
    if tau.is_finite() && tau >= 0.0 {
        Ok(tau)
    } else {
        Err(format!("must be a finite value >= 0, got {raw}"))
    }
}

fn parse_budget(raw: &str) -> Result<i64, String> {
    let budget: i64 = raw.parse().map_err(|e| format!("{e}"))?;
    if budget >= -1 {
        Ok(budget)
    } else {
        Err(format!(
            "--budget must be >= -1 (-1 = unlimited, 0 = strict-zero floor; use --full for \
             changed files only), got {raw}"
        ))
    }
}

fn resolve_budget(budget: Option<i64>) -> Option<u32> {
    match budget {
        None => None,
        Some(n) if n < 0 => Some(UNLIMITED_BUDGET_TOKENS),
        Some(n) => Some(u32::try_from(n).unwrap_or(UNLIMITED_BUDGET_TOKENS)),
    }
}

fn group_thousands(n: u32) -> String {
    let digits = n.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    grouped
}

fn format_size(byte_size: usize) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    if byte_size < 1024 {
        format!("{byte_size} B")
    } else if byte_size < 1024 * 1024 {
        format!("{:.1} KB", byte_size as f64 / KB)
    } else {
        format!("{:.1} MB", byte_size as f64 / MB)
    }
}

fn print_token_summary(rendered: &str) {
    eprintln!(
        "{} tokens (o200k_base), {}",
        group_thousands(_diffctx::tokenizer::count_tokens(rendered)),
        format_size(rendered.len())
    );
}

const WATCHDOG_GRACE_SECS: u64 = 30;

/// Test hook: the whole watchdog wait, deadline included, so its exit 124 is
/// reachable on a small repository (`0` fires before the worker can answer).
/// Non-semantic: it decides only when to abort, never what is selected.
fn test_watchdog_secs() -> Option<u64> {
    std::env::var("DIFFCTX_TEST_WATCHDOG_GRACE_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
}

fn run_with_deadline<T, F>(timeout: u64, work: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        let _ = tx.send(work());
    });
    // The pipeline's own deadline is cooperative: at `timeout` it stops the
    // phase it is in and renders a partial artifact (`coverage.status`). This
    // watchdog is the last resort behind it — a phase that cannot poll (a
    // git subprocess that ignores its own timeout) — so it fires a grace
    // period later, after the cooperative path had its chance to return.
    let (grace_secs, wait) = match test_watchdog_secs() {
        Some(secs) => (secs, secs),
        None => (
            WATCHDOG_GRACE_SECS,
            timeout.saturating_add(WATCHDOG_GRACE_SECS),
        ),
    };
    let grace = Duration::from_secs(wait);
    match rx.recv_timeout(grace) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            eprintln!(
                "diffctx: pipeline exceeded {timeout}s wall-clock deadline and did not stop \
                 cooperatively within {grace_secs}s more; aborting before OOM/SIGKILL. \
                 Narrow the review with an explicit '--diff <from>..<to>' range or run on a \
                 smaller subtree, or raise '--timeout'."
            );
            std::process::exit(124);
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            let _ = worker.join();
            anyhow::bail!("diffctx: pipeline worker terminated unexpectedly")
        }
    }
}

// Both entry points end the same way, and the exit code is part of the CLI
// contract: an empty diff must still print, still summarize, and still exit
// EXIT_EMPTY_DIFF rather than 0.
fn emit(cli: &Cli, rendered: &str, is_empty: bool, changed_files: &[String]) -> Result<()> {
    if is_empty {
        eprintln!(
            "diffctx: diff produced no semantic context (clean working tree, binary-only, or \
             files over the size cap); {}",
            empty_diff_hint(
                &cli.path,
                cli.budget,
                cli.diff_ref.as_deref().unwrap_or("HEAD"),
                changed_files
            )
        );
    }
    if !cli.quiet {
        print_token_summary(rendered);
    }
    write_stdout(rendered)?;
    if is_empty {
        std::process::exit(EXIT_EMPTY_DIFF);
    }
    Ok(())
}

/// A reader that stopped reading (`| head`) is not an error: exit the way a
/// shell pipeline expects (128 + SIGPIPE) instead of panicking in `print!`.
fn write_stdout(rendered: &str) -> Result<()> {
    let mut stdout = io::stdout().lock();
    match stdout
        .write_all(rendered.as_bytes())
        .and_then(|()| stdout.flush())
    {
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => std::process::exit(EXIT_BROKEN_PIPE),
        other => Ok(other?),
    }
}

// Mirrors `_diff_result_is_empty` in src/diffctx/_app.py: deletions and renames
// are real signal even with zero fragments, so only a result carrying neither
// counts as empty.
fn diff_result_is_empty(output: &DiffContextOutput) -> bool {
    output.deleted_files.is_empty()
        && output.renamed_files.is_empty()
        && output.lockfile_changes.is_empty()
        && output.ignored_changes.is_empty()
        && output.policy_excluded_count == 0
        && output.fragment_count == 0
}

fn empty_diff_hint(
    root: &Path,
    budget: Option<i64>,
    diff_ref: &str,
    changed_files: &[String],
) -> String {
    match budget {
        Some(0) => {
            "--budget 0 emits no fragments (changed files are listed as omitted); use --full \
                    for the changed code, or omit --budget for auto sizing"
                .to_string()
        }
        Some(n) if n > 0 => {
            format!(
                "--budget {n} may be too small to fit any fragment; raise it or omit for auto sizing"
            )
        }
        _ if !changed_files.is_empty() => format!(
            "{} changed file(s) carry no text hunk (a mode change, a submodule pointer, a binary \
             or a '-diff' attribute); they are listed in changed_files",
            changed_files.len()
        ),
        _ if diff_ref == "HEAD" && working_tree_is_clean(root) => {
            "the working tree matches HEAD; try --diff HEAD~1 for the last commit".to_string()
        }
        _ if is_duration_window(root, diff_ref) => {
            format!("nothing changed in the last {diff_ref}; widen the window (e.g. --diff 7d)")
        }
        _ => format!("check the range with: git diff --stat {diff_ref}"),
    }
}

fn working_tree_is_clean(root: &Path) -> bool {
    _diffctx::git::run_git(root, &["status", "--porcelain"]).is_ok_and(|out| out.trim().is_empty())
}

fn is_duration_window(root: &Path, diff_ref: &str) -> bool {
    _diffctx::git::resolve_duration_range(root, Some(diff_ref))
        .map(|resolved| resolved.from_duration)
        .unwrap_or(false)
}

#[allow(clippy::too_many_arguments)]
fn run_locate(
    cli: &Cli,
    path: PathBuf,
    diff_ref: Option<String>,
    budget: Option<u32>,
    alpha: f64,
    tau: Option<f64>,
    scoring_mode: ScoringMode,
    timeout: u64,
) -> Result<()> {
    let output = run_with_deadline(timeout, move || {
        _diffctx::pipeline::build_diff_context_locate(
            &path,
            diff_ref.as_deref(),
            &[],
            budget,
            alpha,
            tau,
            scoring_mode,
            timeout,
        )
    })?;

    let rendered = format!("{}\n", serde_json::to_string(&output)?);
    let is_empty = output.item_count == 0
        && output.deleted_files.is_empty()
        && output.renamed_files.is_empty()
        && output.lockfile_changes.is_empty()
        && output.ignored_changes.is_empty()
        && output.policy_excluded_count == 0;
    emit(cli, &rendered, is_empty, &[])
}

fn main() {
    if let Err(err) = real_main() {
        // The Python CLI and README promise exit 3 for git/environment
        // failures (not a repo, unknown revision, no commits); anyhow's
        // default is 1, which made the two binaries disagree on the one code
        // a wrapper script keys on.
        if let Some(git_err) = err.downcast_ref::<_diffctx::git::GitError>() {
            eprintln!("diffctx: {err}");
            std::process::exit(if git_err.is_usage() { EXIT_USAGE } else { 3 });
        }
        eprintln!("Error: {err:?}");
        std::process::exit(1);
    }
}

fn real_main() -> Result<()> {
    // stdout is the artifact; a log line there corrupts the JSON/YAML a
    // pipeline parses.
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(io::stderr)
        .init();

    let cli = Cli::parse();
    if cli.diff_ref.as_deref().is_some_and(|r| r.trim().is_empty()) {
        // An unset `$RANGE` in CI expands to this; guessing a meaning for it
        // would publish some other diff as the PR's context.
        eprintln!("error: --diff requires a non-empty range");
        std::process::exit(EXIT_USAGE);
    }

    let scoring_mode =
        ScoringMode::from_str(&cli.scoring).expect("clap value_parser already validated --scoring");

    // The per-phase git timeout does not bound in-process phases (parse, graph
    // build, scoring), so a pathological repo can hang far past `--timeout`
    // until OOM/SIGKILL (#70). Run the pipeline on a worker thread and enforce
    // `--timeout` as a true total wall-clock ceiling: on expiry, fail fast with
    // the actionable-error contract instead of hanging unbounded.
    let timeout = cli.timeout;
    let path = cli.path.clone();
    // No `--diff` at all used to reach git as a bare `git diff` — index vs
    // worktree, staged edits invisible — while the pipeline's untracked-file
    // rule read the same `None` as "vs HEAD". One meaning: HEAD.
    let diff_ref = Some(cli.diff_ref.clone().unwrap_or_else(|| "HEAD".to_string()));
    let budget = resolve_budget(cli.budget);
    let alpha = cli.alpha;
    let tau = cli.tau;
    let no_content = cli.no_content;
    let full = cli.full;

    if cli.mode == "locate" {
        if full {
            eprintln!(
                "error: --mode locate is incompatible with --full (locate ranks the selection; --full bypasses it)"
            );
            std::process::exit(2);
        }
        return run_locate(
            &cli,
            path,
            diff_ref,
            budget,
            alpha,
            tau,
            scoring_mode,
            timeout,
        );
    }

    let output = run_with_deadline(timeout, move || {
        build_diff_context(
            &path,
            diff_ref.as_deref(),
            budget,
            alpha,
            tau,
            no_content,
            full,
            scoring_mode,
            timeout,
        )
    })?;

    let mut output = output;
    let mut rendered = render(cli.format, &output)?;
    // `--budget` bounds the document, and the resolved budget (auto included)
    // is what provenance recorded; an unlimited or zero budget renders as is.
    let bound = output
        .provenance
        .as_ref()
        .and_then(|p| p.selection.as_ref())
        .map(|s| s.budget_tokens)
        .filter(|&b| b > 0 && b < UNLIMITED_BUDGET_TOKENS);
    if let Some(bound) = bound {
        while count_tokens(&rendered) > bound && output.drop_one_fragment() {
            rendered = render(cli.format, &output)?;
        }
    }

    emit(
        &cli,
        &rendered,
        diff_result_is_empty(&output),
        &output.changed_files,
    )
}

fn render(format: OutputFormat, output: &DiffContextOutput) -> Result<String> {
    Ok(match format {
        OutputFormat::Json => format!("{}\n", serde_json::to_string_pretty(output)?),
        OutputFormat::Yaml => serde_yaml::to_string(output)?,
    })
}
