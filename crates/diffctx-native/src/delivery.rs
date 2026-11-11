//! What every door delivers.
//!
//! A door — the native binary, the Python CLI, the MCP server, the hook — parses
//! its own arguments, asks for one [`Delivery`] and prints it: the text for
//! stdout, the notes for stderr, the status to exit with. What an empty diff
//! means and says, what an early stop discloses, the token summary, the
//! redactions, the share of the raw diff, and the ceiling a reply may not pass
//! are decided here, once. A door that wants any of them different asks the
//! engine, not itself.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Result;
use serde_json::{Value, json};

use crate::answer::envelope::fit_of;
use crate::artifact::{self, Format, UNLIMITED_BUDGET_TOKENS};
use crate::impact::{self, ImpactOutput};
use crate::mode::ScoringMode;
use crate::pipeline;
use crate::render::DiffContextOutput;
use crate::resource::{CoverageReport, CoverageStatus, LimitReason, ResourceUsage};
use crate::tokenizer::count_tokens;

/// A diff that yields no semantic context is an actionable result, not a
/// success: the status a script keys on.
pub const EXIT_EMPTY_DIFF: i32 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Pack,
    Locate,
    Impact,
}

impl Mode {
    pub fn named(name: &str) -> Option<Self> {
        match name {
            "pack" => Some(Self::Pack),
            "locate" => Some(Self::Locate),
            "impact" => Some(Self::Impact),
            _ => None,
        }
    }
}

/// How an impact answer is cut to size: for a reader asking, or for the hook
/// that adds it to a session unasked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    Cli,
    /// Candidates counted, not listed, and the whole text within
    /// [`impact::AUTOMATIC_TOKEN_CAP`]; nothing is recorded as reviewed.
    Hook,
}

/// One question to the engine, in the vocabulary every door shares.
pub struct Request {
    pub root: PathBuf,
    pub mode: Mode,
    /// `None` is the working tree against HEAD.
    pub range: Option<String>,
    /// Impact only: the definitions of a name, instead of a range's change.
    pub symbol: Option<String>,
    pub paths: Vec<String>,
    /// As `--budget`: `None` auto, `-1` unlimited, `0` no fragments.
    pub budget: Option<i64>,
    /// What the reply may not pass, the `envelope` included: the MCP's
    /// `max_tokens`. A budget above it is fitted to it, never refused.
    pub ceiling: Option<u32>,
    /// Text the door puts before the reply, charged to the ceiling with the
    /// rest (the MCP's note that its ceiling lowered the budget, or that the
    /// clipboard was unavailable).
    pub envelope: String,
    pub format: Format,
    pub unfold: bool,
    pub alpha: f64,
    pub tau: Option<f64>,
    pub scoring: ScoringMode,
    pub timeout: u64,
    pub no_content: bool,
    pub full: bool,
    pub with_raw_diff: bool,
    /// Locate only: list the ranked candidates the budget left behind (their
    /// count is always stated), charged to the budget when listed.
    pub overflow: bool,
    pub profile: Profile,
}

impl Request {
    pub fn new(root: PathBuf, mode: Mode) -> Self {
        Self {
            root,
            mode,
            range: None,
            symbol: None,
            paths: Vec::new(),
            budget: None,
            ceiling: None,
            envelope: String::new(),
            format: Format::Md,
            unfold: false,
            alpha: crate::config::limits::DEFAULT_PPR_ALPHA,
            tau: None,
            scoring: ScoringMode::from_str(crate::config::limits::DEFAULT_SCORING)
                .expect("the shipped default scoring mode parses"),
            timeout: crate::config::limits::DEFAULT_PIPELINE_TIMEOUT_SECONDS,
            no_content: false,
            full: false,
            with_raw_diff: false,
            overflow: false,
            profile: Profile::Cli,
        }
    }
}

/// A line for stderr, and how a door treats it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Note {
    /// Said always, after the door's `name: `.
    Warning(String),
    /// Said unless the door is quiet, after the door's `name: `.
    Detail(String),
    /// Said unless the door is quiet, as it stands.
    Figure(String),
}

pub struct Delivery {
    /// What goes to stdout (or into the reply), newline-terminated unless the
    /// format ends otherwise.
    pub text: String,
    pub notes: Vec<Note>,
    /// `0`, or [`EXIT_EMPTY_DIFF`].
    pub exit: i32,
    /// Fragments (pack) or items (locate) the text lists.
    pub listed: usize,
    /// Telemetry beside the artifact, never inside it.
    pub latency: Option<Value>,
}

pub fn deliver(request: &Request) -> Result<Delivery> {
    let started = Instant::now();
    match request.mode {
        Mode::Impact => impact_delivery(request),
        Mode::Locate => locate_delivery(request),
        Mode::Pack => pack_delivery(request, started),
    }
}

/// `--budget` as the pipeline takes it.
fn resolved(budget: Option<i64>) -> Option<u32> {
    budget.map(|n| {
        if n < 0 {
            UNLIMITED_BUDGET_TOKENS
        } else {
            u32::try_from(n).unwrap_or(UNLIMITED_BUDGET_TOKENS)
        }
    })
}

fn within(budget: Option<u32>, ceiling: Option<u32>) -> Option<u32> {
    match (budget, ceiling) {
        (Some(b), Some(c)) => Some(b.min(c)),
        (b, _) => b,
    }
}

/// What a ceiling leaves for the answer once the envelope is paid for.
fn room_under(request: &Request) -> Option<u32> {
    request
        .ceiling
        .map(|c| c.saturating_sub(count_tokens(&request.envelope)).max(1))
}

fn impact_delivery(request: &Request) -> Result<Delivery> {
    let mut output = match &request.symbol {
        Some(symbol) => pipeline::build_symbol_impact(
            &request.root,
            symbol,
            &request.paths,
            request.alpha,
            request.scoring,
            request.timeout,
        )?,
        None => {
            let output = pipeline::build_diff_context_impact(
                &request.root,
                request.range.as_deref(),
                &request.paths,
                request.alpha,
                request.scoring,
                request.timeout,
            )?;
            // A scoped answer reviewed part of the change; the gate keys on all of it.
            if request.profile == Profile::Cli && request.paths.is_empty() {
                crate::hook::mark_range_reviewed(
                    &request.root,
                    request.range.as_deref().unwrap_or("HEAD"),
                    &output,
                );
            }
            output
        }
    };
    let mut text = render_impact(request, &mut output);
    if !text.ends_with('\n') {
        text.push('\n');
    }
    if request.profile == Profile::Cli {
        text.insert_str(0, &request.envelope);
    }
    // An empty impact is the answer ("nothing outside the diff depends on
    // this"), not the failure the other modes exit 4 on: the hook keys on it.
    Ok(Delivery {
        notes: vec![Note::Figure(token_summary(&text))],
        text,
        exit: 0,
        listed: 0,
        latency: None,
    })
}

/// The answer alone: the envelope, charged to the cap here, goes before it.
fn render_impact(request: &Request, output: &mut ImpactOutput) -> String {
    if request.profile == Profile::Hook {
        return impact::render_automatic(output, impact::AUTOMATIC_TOKEN_CAP);
    }
    let cap = within(
        Some(resolved(request.budget).unwrap_or(impact::IMPACT_TOKEN_CAP)),
        request.ceiling,
    )
    .unwrap_or(impact::IMPACT_TOKEN_CAP);
    impact::deliver_framed(
        output,
        request.format == Format::Md,
        cap,
        request.unfold,
        &request.envelope,
    )
}

fn locate_delivery(request: &Request) -> Result<Delivery> {
    let room = room_under(request);
    let mut output = pipeline::build_diff_context_locate(
        &request.root,
        request.range.as_deref(),
        &request.paths,
        within(resolved(request.budget), room),
        request.alpha,
        request.tau,
        request.scoring,
        request.timeout,
        request.overflow,
    )?;
    // An auto budget can exceed the ceiling the selection never saw.
    if let Some(room) = room {
        if room < output.fit.cap {
            output.fit.cap = room;
            output.fit_to(room, request.overflow);
        }
    }
    let text = format!("{}{}\n", request.envelope, serde_json::to_string(&output)?);
    let mut notes = Vec::new();
    let exit = judge(
        output.carries_nothing(),
        &output.coverage.limits,
        || empty_diff_hint(request, &output.inventory.changed.items),
        &mut notes,
    );
    notes.push(Note::Figure(token_summary(&text)));
    if let Some(redacted) = redactions_note(output.redactions.as_ref().map(|r| r.count)) {
        notes.push(redacted);
    }
    Ok(Delivery {
        listed: output.items.len(),
        text,
        notes,
        exit,
        latency: None,
    })
}

fn pack_delivery(request: &Request, started: Instant) -> Result<Delivery> {
    let room = room_under(request);
    let mut pack = pipeline::build_diff_context_in(
        &request.root,
        request.range.as_deref(),
        &request.paths,
        within(resolved(request.budget), room),
        request.alpha,
        request.tau,
        request.no_content,
        request.full,
        request.scoring,
        request.timeout,
    )?;
    let latency = match &pack.latency {
        Some(breakdown) => serde_json::to_value(breakdown)?,
        None => json!({ "total_ms": (started.elapsed().as_secs_f64() * 10_000.0).round() / 10.0 }),
    };
    let raw_diff = if request.with_raw_diff {
        with_raw_diff(
            &mut pack,
            &request.root,
            request.range.as_deref(),
            &request.paths,
            request.timeout,
        )?
    } else {
        None
    };
    if let Some(room) = room {
        // The raw diff is not charged to `--budget`, but it is in the reply.
        let exempt = raw_diff.as_deref().map_or(0, count_tokens);
        lower_cap(
            &mut pack,
            room.saturating_sub(exempt).max(1),
            request.budget,
        );
    }
    let render = crate::profile::phase("render");
    let mut text = artifact::fit(&mut pack, request.format);
    drop(render);
    text.insert_str(0, &request.envelope);
    let mut notes = Vec::new();
    let limits = pack
        .coverage
        .as_ref()
        .map_or(&[][..], |c| c.limits.as_slice());
    let exit = judge(
        pack.carries_nothing(),
        limits,
        || empty_diff_hint(request, &pack.inventory.changed.items),
        &mut notes,
    );
    notes.push(Note::Figure(token_summary(&text)));
    if let Some(note) = redactions_note(pack.redactions.as_ref().map(|r| r.count)) {
        notes.push(note);
    }
    if request.with_raw_diff {
        notes.push(match &raw_diff {
            Some(patch) => Note::Figure(format!(
                "  of which {} tokens are the raw diff (not charged to --budget)",
                group_thousands(count_tokens(patch))
            )),
            None => Note::Detail(
                "--with-raw-diff produced no patch text (the range only touches untracked, \
                 lock, ignored, or secret-like files)"
                    .to_string(),
            ),
        });
    }
    Ok(Delivery {
        listed: pack.fragments.len(),
        text,
        notes,
        exit,
        latency: Some(latency),
    })
}

/// The cap the fit holds the document to: a ceiling below it lowers it, as the
/// selection would have run had it known. A document the run sized no budget for
/// (`--full`, unlimited) is given one; `--budget 0` asked for no fragments and
/// has nothing to cut.
fn lower_cap(pack: &mut DiffContextOutput, cap: u32, asked: Option<i64>) {
    match pack.fit.as_mut() {
        Some(fit) if fit.cap > cap => fit.cap = cap,
        Some(_) => {}
        None if asked == Some(0) => {}
        None => pack.fit = Some(fit_of(None, cap)),
    }
}

/// Whether the run is empty and says so. A run the deadline or a cancellation
/// stopped before it found anything is partial, not a clean tree, and exits 0
/// as every partial artifact does (#408).
fn judge(
    nothing_found: bool,
    limits: &[LimitReason],
    hint: impl FnOnce() -> String,
    notes: &mut Vec<Note>,
) -> i32 {
    if !nothing_found {
        return 0;
    }
    let stopped: Vec<String> = limits
        .iter()
        .filter(|r| matches!(r, LimitReason::Deadline | LimitReason::Cancelled))
        .filter_map(|r| serde_json::to_value(r).ok()?.as_str().map(str::to_string))
        .collect();
    if !stopped.is_empty() {
        notes.push(Note::Warning(format!(
            "the run stopped early ({}) before it found any context; the output is partial, \
             not a clean tree: raise --timeout or narrow the range",
            stopped.join(", ")
        )));
        return 0;
    }
    notes.push(Note::Warning(format!(
        "diff produced no semantic context (clean working tree, binary-only, or files over the \
         size cap); {}",
        hint()
    )));
    EXIT_EMPTY_DIFF
}

fn redactions_note(count: Option<usize>) -> Option<Note> {
    count
        .filter(|&n| n > 0)
        .map(|n| Note::Detail(format!("{n} credential-shaped string(s) redacted")))
}

fn empty_diff_hint(request: &Request, changed_files: &[String]) -> String {
    let root = request.root.as_path();
    let diff_ref = request.range.as_deref().unwrap_or("HEAD");
    match request.budget {
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
             or a '-diff' attribute); they are listed in inventory.changed",
            changed_files.len()
        ),
        _ if diff_ref == "HEAD" && working_tree_is_clean(root) => {
            "the working tree matches HEAD; try --diff HEAD~1 for the last commit".to_string()
        }
        _ if crate::snapshot::is_staged_keyword(diff_ref) => {
            "check the staged changes with: git diff --cached --stat".to_string()
        }
        _ if is_duration_window(root, diff_ref) => {
            format!("nothing changed in the last {diff_ref}; widen the window (e.g. --diff 7d)")
        }
        // `A..B` is read from the fork point, which is git's `A...B`.
        _ if diff_ref.contains("..") && !diff_ref.contains("...") => format!(
            "check the range with: git diff --stat {}",
            diff_ref.replacen("..", "...", 1)
        ),
        _ => format!("check the range with: git diff --stat {diff_ref}"),
    }
}

fn working_tree_is_clean(root: &Path) -> bool {
    crate::snapshot::run_git(root, &["status", "--porcelain"])
        .is_ok_and(|out| out.trim().is_empty())
}

fn is_duration_window(root: &Path, diff_ref: &str) -> bool {
    crate::snapshot::duration_base(root, diff_ref).is_ok_and(|base| base.is_some())
}

/// The summary a door prints on stderr: what the reader's context pays.
pub fn token_summary(text: &str) -> String {
    format!(
        "{} tokens (o200k_base), {}",
        group_thousands(count_tokens(text)),
        format_size(text.len())
    )
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

/// Attaches the disclosable patch to a pack document, ahead of the fragments it
/// explains, and counts what the sanitizer replaced in it: a secret on a removed
/// line exists only in the patch. Additive: selection has already run and is
/// never perturbed. `None` when the range leaves no patch to show.
pub fn with_raw_diff(
    pack: &mut DiffContextOutput,
    root: &Path,
    range: Option<&str>,
    paths: &[String],
    timeout: u64,
) -> Result<Option<String>> {
    let (patch, redactions) = pipeline::raw_diff_text(root, range, paths, timeout)?;
    if !redactions.is_empty() {
        count_patch_redactions(pack, redactions);
    }
    if patch.is_empty() {
        return Ok(None);
    }
    pack.raw_diff = Some(patch.clone());
    Ok(Some(patch))
}

fn count_patch_redactions(pack: &mut DiffContextOutput, found: crate::sanitize::Redactions) {
    let block = pack.redactions.get_or_insert_with(Default::default);
    block.count += found.count;
    for category in found.categories {
        if !block.categories.iter().any(|c| c.as_str() == category) {
            block.categories.push(category.to_string());
        }
    }
    let coverage = pack.coverage.get_or_insert_with(|| CoverageReport {
        status: CoverageStatus::Partial,
        limits: Vec::new(),
        capped: Vec::new(),
        limited_files: Vec::new(),
        resources: ResourceUsage::default(),
        unknown: vec!["resources".to_string()],
        lossy_files: Vec::new(),
    });
    let reason = LimitReason::SanitizationRedaction;
    if !coverage.limits.contains(&reason) {
        coverage.limits.push(reason);
    }
}
