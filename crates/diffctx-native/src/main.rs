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

/// Mirrors `_UNLIMITED_BUDGET` in src/diffctx/_native/pipeline.py so `--budget -1`
/// means the same thing in both CLIs.
const UNLIMITED_BUDGET_TOKENS: u32 = 10_000_000;

/// Mirrors `_EXIT_EMPTY_DIFF` in src/diffctx/_app.py: a diff that yields no
/// semantic context is an actionable result, not a success.
const EXIT_EMPTY_DIFF: i32 = 4;

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
    #[arg(long, allow_negative_numbers = true)]
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
    #[arg(long, default_value_t = DEFAULT_PPR_ALPHA)]
    alpha: f64,

    /// Relevance threshold for full fragment content; lower = more context.
    /// Omitted resolves per scorer: the default for a gated one, the ungated
    /// operating point for a scorer that builds no graph — which is why this
    /// carries no clap default, so naming the default value explicitly is a
    /// request the pipeline can still tell apart from silence.
    #[arg(long)]
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
    #[arg(long, default_value_t = DEFAULT_PIPELINE_TIMEOUT_SECONDS)]
    timeout: u64,

    /// Suppress the token summary on stderr
    #[arg(short = 'q', long)]
    quiet: bool,
}

fn resolve_budget(budget: Option<i64>) -> Option<u32> {
    match budget {
        None => None,
        Some(n) if n < -1 => {
            eprintln!(
                "error: --budget must be >= -1 (-1 = unlimited, 0 = strict-zero floor; use --full \
                 for changed files only), got {n}"
            );
            std::process::exit(2);
        }
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

fn run_with_deadline<T, F>(timeout: u64, work: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        let _ = tx.send(work());
    });
    let exit_on_deadline = || -> ! {
        eprintln!(
            "diffctx: pipeline exceeded {timeout}s wall-clock deadline; aborting before \
             OOM/SIGKILL. Narrow the review with an explicit '--diff <from>..<to>' range or \
             run on a smaller subtree, or raise '--timeout'."
        );
        std::process::exit(124);
    };
    match rx.recv_timeout(Duration::from_secs(timeout)) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => exit_on_deadline(),
        // Two clocks watch the same deadline: this receive and the pipeline's
        // own phase checks, which panic on the worker. When the worker's fires
        // first the channel closes instead of timing out, and that used to
        // surface as "terminated unexpectedly" with a generic exit code — the
        // same overrun, reported two ways depending on which clock won by a
        // millisecond. A deadline panic is the deadline, whichever side saw it.
        Err(mpsc::RecvTimeoutError::Disconnected) => match worker.join() {
            Err(payload) if _diffctx::deadline::deadline_panic_message(&*payload).is_some() => {
                exit_on_deadline()
            }
            _ => anyhow::bail!("diffctx: pipeline worker terminated unexpectedly"),
        },
    }
}

// Both entry points end the same way, and the exit code is part of the CLI
// contract: an empty diff must still print, still summarize, and still exit
// EXIT_EMPTY_DIFF rather than 0.
fn emit(cli: &Cli, rendered: &str, is_empty: bool) -> Result<()> {
    if is_empty {
        eprintln!(
            "diffctx: diff produced no semantic context (clean working tree, binary-only, or \
             files over the size cap); {}",
            empty_diff_hint(
                &cli.path,
                cli.budget,
                cli.diff_ref.as_deref().unwrap_or("HEAD")
            )
        );
    }
    if !cli.quiet {
        print_token_summary(rendered);
    }
    print!("{rendered}");
    io::stdout().flush()?;
    if is_empty {
        std::process::exit(EXIT_EMPTY_DIFF);
    }
    Ok(())
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

fn empty_diff_hint(root: &Path, budget: Option<i64>, diff_ref: &str) -> String {
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
        _ if diff_ref == "HEAD" => {
            "the working tree matches HEAD; try --diff HEAD~1 for the last commit".to_string()
        }
        _ if is_duration_window(root, diff_ref) => {
            format!("nothing changed in the last {diff_ref}; widen the window (e.g. --diff 7d)")
        }
        _ => format!("check the range with: git diff --stat {diff_ref}"),
    }
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
    emit(cli, &rendered, is_empty)
}

fn main() {
    if let Err(err) = real_main() {
        // The Python CLI and README promise exit 3 for git/environment
        // failures (not a repo, unknown revision, no commits); anyhow's
        // default is 1, which made the two binaries disagree on the one code
        // a wrapper script keys on.
        if err.downcast_ref::<_diffctx::git::GitError>().is_some() {
            eprintln!("diffctx: {err}");
            std::process::exit(3);
        }
        eprintln!("Error: {err:?}");
        std::process::exit(1);
    }
}

fn real_main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();

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

    let rendered = match cli.format {
        OutputFormat::Json => format!("{}\n", serde_json::to_string_pretty(&output)?),
        OutputFormat::Yaml => serde_yaml::to_string(&output)?,
    };

    emit(&cli, &rendered, diff_result_is_empty(&output))
}
