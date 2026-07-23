use mimalloc::MiMalloc;
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

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

/// Mirrors `_UNLIMITED_BUDGET` in src/diffctx/diffctx/pipeline.py so `--budget -1`
/// means the same thing in both CLIs.
const UNLIMITED_BUDGET_TOKENS: u32 = 10_000_000;

#[derive(Parser)]
#[command(name = "diffctx", version, about = "Semantic diff context selector")]
struct Cli {
    /// Repository path to analyze
    #[arg(default_value = ".")]
    path: PathBuf,

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

    match cli.format.as_str() {
        "json" => {
            let json = serde_json::to_string_pretty(&output)?;
            println!("{}", json);
        }
        "yaml" => {
            let yaml = serde_yaml::to_string(&output)?;
            print!("{}", yaml);
        }
        other => {
            anyhow::bail!("diffctx: unsupported --format '{other}' (native binary: yaml, json)")
        }
    }

    Ok(())
}
