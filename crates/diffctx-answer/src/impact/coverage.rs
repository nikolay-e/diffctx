//! What the search for the callers of a changed definition covered, and what it says when it did not.

use crate::decide::{self};

use super::output::{ChangedSymbol, ImpactOutput, Unknown};
use super::unanalysed::OUTSIDE_DEFINITIONS;

/// What the search for one definition's callers did not cover: the cell's own
/// gaps (which carry its occurrence account), or, for a definition no cell
/// answers for, the reasons the answer names for it (`unresolved`) and the
/// `outside` files that name it beyond the group (a script that embeds its code,
/// a file outside code). The one place that decides whether an absence may be
/// claimed for it.
pub(super) fn search_coverage(
    verdict: Option<&decide::verdict::Verdict>,
    unresolved: Option<&Unknown>,
    outside: usize,
) -> decide::coverage::Coverages {
    // What the cell's own gaps said is already in its coverage: only a reason it
    // did not give is a route nothing traces.
    let explained = verdict.is_some_and(|v| v.gap().is_some());
    let own_gap = match unresolved {
        Some(Unknown::Unfinished { stops }) if !explained => Some(decide::gap::Gap::Stopped {
            reasons: stops.clone(),
        }),
        Some(Unknown::Unfinished { .. }) | None => None,
        Some(_) => (!explained).then_some(decide::gap::Gap::NotTraced),
    };
    let named_outside = (outside > 0).then_some(decide::gap::Gap::Unaccounted {
        reason: decide::gap::Reason::OtherLanguage,
        files: outside,
    });
    match verdict {
        Some(verdict) => decide::coverage::Coverages::of_verdict(verdict)
            .with(own_gap)
            .with(named_outside),
        None => {
            decide::coverage::Coverages::unowned(own_gap.into_iter().chain(named_outside).collect())
        }
    }
}

/// Untracked directories the answer names in its coverage; the rest are counted.
const MAX_UNREAD_DIRECTORIES_NAMED: usize = 10;

/// What the run as a whole did not cover: a stop or limit that can have lost a
/// caller, files withheld by ignore rules. A definition's own gaps are its own.
pub(crate) fn scope_coverage(output: &ImpactOutput) -> decide::coverage::Coverage {
    let untracked = output
        .unanalysed_files
        .iter()
        .filter(|f| f.reason == OUTSIDE_DEFINITIONS)
        .count();
    let mut gaps = Vec::new();
    if !output.limits.is_empty() {
        gaps.push(decide::gap::Gap::Stopped {
            reasons: output.limits.clone(),
        });
    }
    if output.withheld_files > 0 {
        gaps.push(decide::gap::Gap::Withheld {
            files: output.withheld_files,
        });
    }
    if output.outside_scope_files > 0 {
        gaps.push(decide::gap::Gap::OutsideScope {
            files: output.outside_scope_files,
        });
    }
    if untracked > 0 {
        gaps.push(decide::gap::Gap::Untracked { files: untracked });
    }
    if !output.unread_directories.is_empty() {
        gaps.push(decide::gap::Gap::UnreadDirectories {
            directories: output
                .unread_directories
                .iter()
                .take(MAX_UNREAD_DIRECTORIES_NAMED)
                .cloned()
                .collect(),
            total: output.unread_directories.len(),
        });
    }
    decide::coverage::Coverage::of_run(gaps)
}

/// A definition the answer lists as one nothing calls: only a complete search
/// makes one.
pub(super) fn absence(named: String, _complete: decide::coverage::Complete) -> String {
    named
}

pub(super) fn unfinished_symbol(
    path: String,
    symbol: Option<String>,
    lines: String,
    kind: String,
    stops: &[diffctx_snapshot::resource::LimitReason],
) -> ChangedSymbol {
    ChangedSymbol {
        unresolved: Some(Unknown::Unfinished {
            stops: stops.to_vec(),
        }),
        ..ChangedSymbol::bare(
            path,
            symbol,
            lines,
            kind,
            decide::coverage::Coverages::partial(decide::gap::Gap::Stopped {
                reasons: stops.to_vec(),
            }),
        )
    }
}

/// A definition whose file the search could not read: the loss is its answer.
pub(super) fn uncovered_symbol(
    path: String,
    name: &str,
    lines: String,
    kind: String,
    reason: decide::gap::Reason,
) -> ChangedSymbol {
    let gap = decide::gap::Gap::Unaccounted { reason, files: 1 };
    ChangedSymbol {
        unresolved: Some(Unknown::Uncovered {
            why: gap.to_string(),
        }),
        ..ChangedSymbol::bare(
            path,
            Some(name.to_string()),
            lines,
            kind,
            decide::coverage::Coverages::partial(gap),
        )
    }
}
