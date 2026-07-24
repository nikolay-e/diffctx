use mimalloc::MiMalloc;
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

use _diffctx::config::limits::{
    DEFAULT_PIPELINE_TIMEOUT_SECONDS, DEFAULT_PPR_ALPHA, DEFAULT_STOPPING_THRESHOLD,
};
use _diffctx::mode::ScoringMode;
use _diffctx::pipeline::build_diff_context;
use _diffctx::render::DiffContextOutput;

/// Mirrors `_UNLIMITED_BUDGET` in src/diffctx/diffctx/pipeline.py so `--budget -1`
/// means the same thing in both CLIs.
const UNLIMITED_BUDGET_TOKENS: u32 = 10_000_000;

/// Mirrors `_EXIT_EMPTY_DIFF` in src/diffctx/main.py: a diff that yields no
/// semantic context is an actionable result, not a success.
const EXIT_EMPTY_DIFF: i32 = 4;

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

    /// Token budget: omit = auto, N = fixed cap, -1 = unlimited, 0 = strict-zero
    /// floor (empty selection; use --full for changed files only)
    #[arg(long, allow_negative_numbers = true)]
    budget: Option<i64>,

    /// Output format
    #[arg(short = 'f', long, default_value = "yaml", value_parser = ["yaml", "json"])]
    format: String,

    /// Git diff range (e.g. HEAD~1..HEAD, main..feature); bare --diff uses the
    /// working tree vs HEAD
    #[arg(long = "diff", num_args = 0..=1, default_missing_value = "HEAD")]
    diff_ref: Option<String>,

    /// PPR damping: how tightly context clusters around changes, 0-1 exclusive
    #[arg(long, default_value_t = DEFAULT_PPR_ALPHA)]
    alpha: f64,

    /// Relevance threshold for full fragment content; lower = more context
    #[arg(long, default_value_t = DEFAULT_STOPPING_THRESHOLD)]
    tau: f64,

    /// Skip fragment contents (structure only)
    #[arg(long)]
    no_content: bool,

    /// Only the changed files, every fragment, no related-code context
    #[arg(long)]
    full: bool,

    /// Relevance scoring mode
    #[arg(long, default_value = "ego", value_parser = ["ppr", "ego", "bm25"])]
    scoring: String,

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

// Mirrors `_diff_result_is_empty` in src/diffctx/main.py: deletions and renames
// are real signal even with zero fragments, so only a result carrying neither
// counts as empty.
fn diff_result_is_empty(output: &DiffContextOutput) -> bool {
    output.deleted_files.is_empty()
        && output.renamed_files.is_empty()
        && output.lockfile_changes.is_empty()
        && output.fragment_count == 0
}

fn empty_diff_hint(budget: Option<i64>, diff_ref: &str) -> String {
    match budget {
        Some(0) => "--budget 0 selects only the changed code itself; omit --budget for auto sizing"
            .to_string(),
        Some(n) if n > 0 => {
            format!(
                "--budget {n} may be too small to fit any fragment; raise it or omit for auto sizing"
            )
        }
        _ if diff_ref == "HEAD" => {
            "the working tree matches HEAD; try --diff HEAD~1 for the last commit".to_string()
        }
        _ => format!("check the range with: git diff --stat {diff_ref}"),
    }
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();

    let scoring_mode = ScoringMode::from_str(&cli.scoring).unwrap_or_else(|e| {
        eprintln!("error: {e}");
        std::process::exit(2);
    });

    // The per-phase git timeout does not bound in-process phases (parse, graph
    // build, scoring), so a pathological repo can hang far past `--timeout`
    // until OOM/SIGKILL (#70). Run the pipeline on a worker thread and enforce
    // `--timeout` as a true total wall-clock ceiling: on expiry, fail fast with
    // the actionable-error contract instead of hanging unbounded.
    let timeout = cli.timeout;
    let path = cli.path.clone();
    let diff_ref = cli.diff_ref.clone();
    let budget = resolve_budget(cli.budget);
    let alpha = cli.alpha;
    let tau = cli.tau;
    let no_content = cli.no_content;
    let full = cli.full;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let result = build_diff_context(
            &path,
            diff_ref.as_deref(),
            budget,
            alpha,
            tau,
            no_content,
            full,
            scoring_mode,
            timeout,
        );
        let _ = tx.send(result);
    });

    let output = match rx.recv_timeout(Duration::from_secs(timeout)) {
        Ok(result) => result?,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            eprintln!(
                "diffctx: pipeline exceeded {timeout}s wall-clock deadline; aborting before \
                 OOM/SIGKILL. Narrow the review with an explicit '--diff <from>..<to>' range or \
                 run on a smaller subtree, or raise '--timeout'."
            );
            std::process::exit(124);
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            anyhow::bail!("diffctx: pipeline worker terminated unexpectedly");
        }
    };

    let rendered = match cli.format.as_str() {
        "json" => format!("{}\n", serde_json::to_string_pretty(&output)?),
        "yaml" => serde_yaml::to_string(&output)?,
        other => {
            anyhow::bail!("diffctx: unsupported --format '{other}' (native binary: yaml, json)")
        }
    };

    if diff_result_is_empty(&output) {
        eprintln!(
            "diffctx: diff produced no semantic context (clean working tree, binary-only, or \
             files over the size cap); {}",
            empty_diff_hint(cli.budget, cli.diff_ref.as_deref().unwrap_or("HEAD"))
        );
    }
    if !cli.quiet {
        print_token_summary(&rendered);
    }
    print!("{rendered}");
    io::stdout().flush()?;

    if diff_result_is_empty(&output) {
        std::process::exit(EXIT_EMPTY_DIFF);
    }

    Ok(())
}
