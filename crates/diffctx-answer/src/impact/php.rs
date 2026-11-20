//! What registers a PHP method with a framework that no caller names: a class
//! handed to the router, the container, the event or queue dispatcher by
//! `X::class` or `dispatch(new X)`, and an attribute that subscribes a method.
//! The framework calls the method by convention, so a changed one nothing calls
//! is a use the engine cannot trace, never an absence.

use super::changes::head_of;
use super::dispatch::{base_symbol, enclosing_class};
use super::removed::read_or_note;
use super::{Fragment, ScoredState};

/// The methods a Laravel router, observer, queue or listener calls by name.
const CONVENTIONAL: &[&str] = &[
    "index",
    "create",
    "store",
    "show",
    "edit",
    "update",
    "destroy",
    "handle",
    "creating",
    "created",
    "updating",
    "updated",
    "saving",
    "saved",
    "deleting",
    "deleted",
    "restoring",
    "restored",
    "retrieved",
    "forcedeleted",
];

/// Attributes that subscribe a method to a dispatcher.
const SUBSCRIBING: &[&str] = &["AsEventListener", "AsMessageHandler"];

/// Lines that name a class without handing it to a framework.
const NOT_REGISTERING: &[&str] = &["instanceof", "assert", "mock", "expects", "catch"];

/// The text that registers `core`, a PHP method, with a framework.
pub(super) fn registered_by(
    state: &ScoredState,
    diff_range: Option<&str>,
    core: &Fragment,
    file: &[&Fragment],
) -> Option<String> {
    if !matches!(
        super::callers::extension(core.path()).as_str(),
        "php" | "phtml" | "inc"
    ) {
        return None;
    }
    let method = base_symbol(core.symbol_name.as_deref()?);
    if let Some(attribute) = subscribing_attribute(state, core) {
        return Some(attribute);
    }
    if !CONVENTIONAL.contains(&method.to_lowercase().as_str()) {
        return None;
    }
    let class = enclosing_class(core, file)?.symbol_name.as_deref()?;
    let class = base_symbol(class).rsplit('.').next()?;
    let own = regex::escape(class);
    let pattern = format!(
        r"(^|[^[:alnum:]_]){own}::class([^[:alnum:]_]|$)|(dispatch|event|dispatchSync|dispatchNow|push|send)[[:space:]]*\([^)]*new[[:space:]]+\\?([A-Za-z_\\]*\\)?{own}([^[:alnum:]_]|$)"
    );
    let source = state.run.source();
    let grepped = read_or_note(
        state,
        diffctx_snapshot::grep::grep_lines_matching(
            &state.root_dir,
            &source,
            &pattern,
            head_of(diff_range).as_deref(),
            &[":(glob,icase)**/*.php"],
        ),
    );
    state.run.note_withheld(grepped.withheld);
    let own_file = super::changes::rel(state, core.path());
    state
        .run
        .bounded(&grepped.rows)
        .into_iter()
        .filter(|(path, _, text)| {
            *path != own_file
                && !path
                    .split('/')
                    .any(|part| part == "tests" || part == "test")
                && !super::mentions::commented(text)
                && !NOT_REGISTERING
                    .iter()
                    .any(|word| text.to_lowercase().contains(word))
        })
        .map(|(_, _, text)| text.trim().to_string())
        .min()
}

/// The attribute above `core` that subscribes it to a dispatcher.
fn subscribing_attribute(state: &ScoredState, core: &Fragment) -> Option<String> {
    let text = state
        .run
        .source()
        .read_to_string(std::path::Path::new(core.path()))?;
    let lines: Vec<&str> = text.lines().collect();
    let first = (core.start_line() as usize).saturating_sub(1);
    lines[..first.min(lines.len())]
        .iter()
        .rev()
        .map(|l| l.trim())
        .take_while(|l| l.starts_with("#[") || l.is_empty())
        .chain(lines.get(first).map(|l| l.trim()))
        .find(|l| {
            SUBSCRIBING
                .iter()
                .any(|a| l.trim_start_matches("#[").starts_with(a))
        })
        .map(str::to_string)
}
