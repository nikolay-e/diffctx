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
    DEFAULT_BUDGET_TOKENS, DEFAULT_PIPELINE_TIMEOUT_SECONDS, DEFAULT_PPR_ALPHA,
    DEFAULT_STOPPING_THRESHOLD,
};
use _diffctx::mode::ScoringMode;
use _diffctx::pipeline::build_diff_context;

#[derive(Parser)]
#[command(name = "diffctx", version, about = "Semantic diff context selector")]
struct Cli {
    #[arg(default_value = ".")]
    path: PathBuf,

    #[arg(long, default_value_t = DEFAULT_BUDGET_TOKENS)]
    budget: u32,

    #[arg(long, default_value = "yaml")]
    format: String,

    #[arg(long = "diff")]
    diff_ref: Option<String>,

    #[arg(long, default_value_t = DEFAULT_PPR_ALPHA)]
    alpha: f64,

    #[arg(long, default_value_t = DEFAULT_STOPPING_THRESHOLD)]
    tau: f64,

    #[arg(long)]
    no_content: bool,

    #[arg(long)]
    full: bool,

    #[arg(long, default_value = "ego")]
    scoring: String,

    #[arg(long, default_value_t = DEFAULT_PIPELINE_TIMEOUT_SECONDS)]
    timeout: u64,
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
    let budget = cli.budget;
    let alpha = cli.alpha;
    let tau = cli.tau;
    let no_content = cli.no_content;
    let full = cli.full;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let result = build_diff_context(
            &path,
            diff_ref.as_deref(),
            Some(budget),
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
        _ => {
            let yaml = serde_yaml::to_string(&output)?;
            print!("{}", yaml);
        }
    }

    Ok(())
}
