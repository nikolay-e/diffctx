use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

use _diffctx::mode::ScoringMode;
use _diffctx::pipeline::build_diff_context;

#[derive(Parser)]
#[command(name = "diffctx", version, about = "Semantic diff context selector")]
struct Cli {
    #[arg(default_value = ".")]
    path: PathBuf,

    #[arg(long, default_value = "4096")]
    budget: u32,

    #[arg(long, default_value = "yaml")]
    format: String,

    #[arg(long)]
    diff_ref: Option<String>,

    #[arg(long, default_value = "0.60")]
    alpha: f64,

    #[arg(long, default_value = "0.08")]
    tau: f64,

    #[arg(long)]
    no_content: bool,

    #[arg(long)]
    full: bool,

    #[arg(long, default_value = "hybrid")]
    scoring: String,

    #[arg(long, default_value = "300")]
    timeout: u64,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();

    let scoring_mode = ScoringMode::from_str(&cli.scoring);
    let output = build_diff_context(
        &cli.path,
        cli.diff_ref.as_deref(),
        Some(cli.budget),
        cli.alpha,
        cli.tau,
        cli.no_content,
        cli.full,
        scoring_mode,
        cli.timeout,
    )?;

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
