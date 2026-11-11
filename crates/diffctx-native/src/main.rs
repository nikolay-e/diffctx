use mimalloc::MiMalloc;
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use tracing_subscriber::EnvFilter;

use _diffctx::artifact::Format;
use _diffctx::config::limits::{
    DEFAULT_PIPELINE_TIMEOUT_SECONDS, DEFAULT_PPR_ALPHA, DEFAULT_SCORING,
};
use _diffctx::delivery::{Delivery, Mode, Note, Request};
use _diffctx::mode::ScoringMode;

/// Mirrors the Python CLI: a closed stdout pipe ends the run as SIGPIPE would.
const EXIT_BROKEN_PIPE: i32 = 141;

/// Same contract as the Python CLI: a malformed invocation is exit 2.
const EXIT_USAGE: i32 = 2;

#[derive(Clone, Copy, ValueEnum)]
enum OutputFormat {
    Yaml,
    Json,
    /// Text for a reader, `--mode impact` only
    Md,
}

#[derive(Parser)]
#[command(
    name = "diffctx",
    version = _diffctx::run_provenance::BUILD_LABEL,
    about = "Semantic diff context selector",
    disable_version_flag = true,
    args_conflicts_with_subcommands = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

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

    /// Impact mode: list every row of a changed definition, each marked with the
    /// rule of the folding table that would fold it, instead of counting the rows
    /// the change cannot have affected in one line each
    #[arg(long)]
    unfold: bool,

    /// Output format
    #[arg(short = 'f', long, default_value = "yaml", value_enum)]
    format: OutputFormat,

    /// Git diff range (e.g. HEAD~1..HEAD, main..feature; A..B is what B changed
    /// since it forked from A), a duration window ending now (24h, 8d, 90min,
    /// 1h30m), or `staged` (also `--diff=--cached`) for the index alone;
    /// omitted or bare --diff uses the working tree vs HEAD
    #[arg(long = "diff", num_args = 0..=1, default_missing_value = "HEAD")]
    diff_ref: Option<String>,

    /// Impact without a diff: where NAME (or PATH:NAME) is defined, who
    /// calls it, and the tests that reach each caller
    #[arg(long, value_name = "NAME", conflicts_with = "diff_ref")]
    symbol: Option<String>,

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

    /// Locate mode: list the ranked candidates the budget left out (up to 50,
    /// without bodies, charged to the budget); their count is always stated
    #[arg(long)]
    overflow: bool,

    /// Skip fragment contents (structure only)
    #[arg(long)]
    no_content: bool,

    /// Pack mode: also embed the raw unified diff ahead of the fragments. It is
    /// not charged to --budget; the stderr token summary counts it. Lock-file,
    /// ignored and secret-like sections stay omitted
    #[arg(long)]
    with_raw_diff: bool,

    /// Only the changed files, every fragment, no related-code context
    #[arg(long)]
    full: bool,

    /// Relevance scoring mode
    #[arg(long, default_value = DEFAULT_SCORING, value_parser = _diffctx::mode::SCORING_MODE_NAMES.to_vec())]
    scoring: String,

    /// Output mode: `pack` = context with source bodies; `locate` = ranked
    /// navigation list with provenance reasons, JSON only (--format ignored);
    /// `impact` = callers outside the diff, their tests, cross-commit overlap
    /// (JSON, or text with `-f md`)
    /// `pack` when not given; a `--symbol` answers in `impact` and refuses
    /// any other mode named outright, as the Python CLI does (#410).
    #[arg(long, value_parser = ["pack", "locate", "impact"])]
    mode: Option<String>,

    /// Wall-clock deadline in seconds; on expiry diffctx exits 124
    #[arg(long, default_value_t = DEFAULT_PIPELINE_TIMEOUT_SECONDS, value_parser = clap::value_parser!(u64).range(1..))]
    timeout: u64,

    /// Suppress the token summary on stderr
    #[arg(short = 'q', long)]
    quiet: bool,

    /// Migration measurement: per changed file, the changed definitions the
    /// impact path reports beside the facts layer's, as JSON
    #[arg(long, hide = true, requires = "diff_ref", conflicts_with_all = ["symbol", "mode", "with_raw_diff"])]
    shadow: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Agent-harness hooks; each reads its event on stdin and never fails
    Hook {
        #[command(subcommand)]
        event: HookEvent,
    },
}

#[derive(Subcommand)]
enum HookEvent {
    /// Claude Code PreToolUse: record the refs a git commit, merge,
    /// cherry-pick, revert or push may move; before a PR is created, print
    /// its impact as additionalContext
    Pretooluse {
        /// Strict gate: deny a plain git commit with the index's impact
        /// attached until it has been reviewed once (then the retry passes);
        /// a commit line that stages for itself is asked to stage first
        #[arg(long)]
        gate: bool,
    },
    /// Claude Code PostToolUse: after a commit, merge, pick, revert or push,
    /// the impact of what it recorded; after a git diff or git status, the
    /// pending change's; after a search for a name the change edits, its callers
    Posttooluse,
    /// Claude Code PostToolUseFailure: the same, for a line whose last
    /// command exited non-zero; what it recorded landed all the same
    Posttoolusefailure,
}

/// The hook contract is exit 0 with either the JSON answer or nothing: a
/// non-zero exit blocks the agent's tool call, and a hook that blocks
/// commits because diffctx hit a corner is a hook the user removes.
fn run_hook(event: &HookEvent) {
    let (kind, gate) = match event {
        HookEvent::Pretooluse { gate } => (_diffctx::hook::Event::PreToolUse, *gate),
        HookEvent::Posttooluse => (_diffctx::hook::Event::PostToolUse { failed: false }, false),
        HookEvent::Posttoolusefailure => {
            (_diffctx::hook::Event::PostToolUse { failed: true }, false)
        }
    };
    let mut input = String::new();
    let _ = io::Read::read_to_string(&mut io::stdin().lock(), &mut input);
    let answer = std::panic::catch_unwind(move || {
        _diffctx::hook::respond_within_deadline(kind, input, gate)
    })
    .ok()
    .flatten();
    if let Some(json) = answer {
        let _ = write_stdout(&format!("{json}\n"));
    }
    std::process::exit(0);
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

const WATCHDOG_GRACE_SECS: u64 = 30;

/// Test hook: the whole watchdog wait, deadline included, so its exit 124 is
/// reachable on a small repository (`0` fires before the worker can answer).
/// Non-semantic: it decides only when to abort, never what is selected.
/// ASCII digits within u64, as the Python CLI reads it: `+5`, `²` and an
/// overflowing value are no override (#410).
fn test_watchdog_secs() -> Option<u64> {
    std::env::var("DIFFCTX_TEST_WATCHDOG_GRACE_SECS")
        .ok()
        .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|v| v.parse().ok())
}

fn run_with_deadline<T, F>(timeout: u64, work: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    let worker = thread::spawn(_diffctx::profile::carry(move || {
        let _ = tx.send(work());
    }));
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
            exit(124);
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            let _ = worker.join();
            anyhow::bail!("diffctx: pipeline worker terminated unexpectedly")
        }
    }
}

/// Every diff mode ends the same way, and the engine decided it: the notes, the
/// text, the status. An empty diff still prints and still summarizes before it
/// exits non-zero.
fn finish(cli: &Cli, delivery: Delivery) -> Result<()> {
    for note in &delivery.notes {
        match note {
            Note::Warning(text) => eprintln!("diffctx: {text}"),
            Note::Detail(text) if !cli.quiet => eprintln!("diffctx: {text}"),
            Note::Figure(text) if !cli.quiet => eprintln!("{text}"),
            Note::Detail(_) | Note::Figure(_) => {}
        }
    }
    write_stdout(&delivery.text)?;
    if delivery.exit != 0 {
        exit(delivery.exit);
    }
    Ok(())
}

fn deliver(cli: &Cli, request: Request) -> Result<()> {
    let timeout = request.timeout;
    let delivery = run_with_deadline(timeout, move || _diffctx::delivery::deliver(&request))?;
    finish(cli, delivery)
}

/// An exit in the middle of a run: `process::exit` skips the profile's guard.
fn exit(code: i32) -> ! {
    _diffctx::profile::flush();
    std::process::exit(code)
}

/// A reader that stopped reading (`| head`) is not an error: exit the way a
/// shell pipeline expects (128 + SIGPIPE) instead of panicking in `print!`.
fn write_stdout(rendered: &str) -> Result<()> {
    let mut stdout = io::stdout().lock();
    match stdout
        .write_all(rendered.as_bytes())
        .and_then(|()| stdout.flush())
    {
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => exit(EXIT_BROKEN_PIPE),
        other => Ok(other?),
    }
}

fn main() {
    if let Err(err) = real_main() {
        // The Python CLI and README promise exit 3 for git/environment
        // failures (not a repo, unknown revision, no commits); anyhow's
        // default is 1, which made the two binaries disagree on the one code
        // a wrapper script keys on.
        if let Some(git_err) = err.downcast_ref::<_diffctx::snapshot::SnapshotError>() {
            eprintln!("diffctx: {err}");
            std::process::exit(if git_err.is_usage() { EXIT_USAGE } else { 3 });
        }
        if let Some(q) = err.downcast_ref::<_diffctx::pipeline::SymbolQueryError>() {
            eprintln!("diffctx: {q}");
            std::process::exit(if q.malformed { EXIT_USAGE } else { 1 });
        }
        eprintln!("Error: {err:?}");
        std::process::exit(1);
    }
}

/// Only pack carries source, so only pack has a patch to bundle.
fn refuse_raw_diff_without_source(cli: &Cli, mode: &str) {
    if cli.with_raw_diff {
        eprintln!(
            "error: --mode {mode} emits no source; --with-raw-diff applies to pack mode only"
        );
        std::process::exit(EXIT_USAGE);
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
    if let Some(Commands::Hook { event }) = &cli.command {
        run_hook(event);
    }
    if cli.diff_ref.as_deref().is_some_and(|r| r.trim().is_empty()) {
        // An unset `$RANGE` in CI expands to this; guessing a meaning for it
        // would publish some other diff as the PR's context.
        eprintln!("error: --diff requires a non-empty range");
        std::process::exit(EXIT_USAGE);
    }

    let scoring_mode =
        ScoringMode::from_str(&cli.scoring).expect("clap value_parser already validated --scoring");
    let _run = _diffctx::profile::run_cli();

    // The per-phase git timeout does not bound in-process phases (parse, graph
    // build, scoring), so a pathological repo can hang far past `--timeout`
    // until OOM/SIGKILL (#70). Run the pipeline on a worker thread and enforce
    // `--timeout` as a true total wall-clock ceiling: on expiry, fail fast with
    // the actionable-error contract instead of hanging unbounded.
    let timeout = cli.timeout;
    // No `--diff` at all used to reach git as a bare `git diff` — index vs
    // worktree, staged edits invisible — while the pipeline's untracked-file
    // rule read the same `None` as "vs HEAD". One meaning: HEAD.
    let diff_ref = Some(cli.diff_ref.clone().unwrap_or_else(|| "HEAD".to_string()));

    let mode = cli.mode.as_deref().unwrap_or("pack");
    if cli.overflow && mode != "locate" {
        eprintln!("error: --overflow applies to --mode locate only");
        std::process::exit(EXIT_USAGE);
    }
    let mut request = Request::new(cli.path.clone(), Mode::named(mode).unwrap_or(Mode::Pack));
    request.range = diff_ref.clone();
    request.budget = cli.budget;
    request.unfold = cli.unfold;
    request.alpha = cli.alpha;
    request.tau = cli.tau;
    request.scoring = scoring_mode;
    request.timeout = timeout;
    request.no_content = cli.no_content;
    request.full = cli.full;
    request.with_raw_diff = cli.with_raw_diff;
    request.overflow = cli.overflow;
    request.format = match cli.format {
        OutputFormat::Yaml => Format::Yaml,
        OutputFormat::Json => Format::Json,
        OutputFormat::Md => Format::Md,
    };
    if let Some(symbol) = cli.symbol.clone() {
        if cli.mode.as_deref().is_some_and(|m| m != "impact") {
            eprintln!("error: --symbol answers in --mode impact");
            std::process::exit(EXIT_USAGE);
        }
        request.mode = Mode::Impact;
        request.symbol = Some(symbol);
        refuse_raw_diff_without_source(&cli, "impact");
        return deliver(&cli, request);
    }
    if cli.shadow {
        let path = cli.path.clone();
        let (alpha, scoring) = (cli.alpha, scoring_mode);
        let report = run_with_deadline(timeout, move || {
            _diffctx::shadow::report(&path, diff_ref.as_deref(), alpha, scoring, timeout)
        })?;
        return write_stdout(&format!("{report}\n"));
    }
    if request.mode != Mode::Pack {
        refuse_raw_diff_without_source(&cli, mode);
    }
    if request.mode != Mode::Impact && matches!(cli.format, OutputFormat::Md) {
        eprintln!("error: --format md is only available with --mode impact");
        std::process::exit(EXIT_USAGE);
    }
    if request.mode == Mode::Locate && cli.full {
        eprintln!(
            "error: --mode locate is incompatible with --full (locate ranks the selection; --full bypasses it)"
        );
        std::process::exit(2);
    }
    deliver(&cli, request)
}
