//! A twin: a definition of another language family with the changed one's exact
//! name that spells a string literal the change removed. Neither call nor import
//! links them, and a pair of ports of one behaviour changes together, so the
//! answer names the twin as a counterpart with no static edge. The name alone or
//! the literal alone is noise, and a twin in the same family is an ordinary
//! duplicate that family's resolver already speaks for.

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::FxHashSet;

use diffctx_base::types::Fragment;
use diffctx_resolve::vocabulary::family::Group;

use super::class_of;
use crate::impact::ScoredState;
use crate::impact::callers::is_code_file;
use crate::impact::changes::{changed_rels, rel};
use crate::impact::output::PathReference;
use crate::impact::removed::MAX_NAMED_BY_PER_TARGET;
use crate::impact::unanalysed::spelled_name;

static STRING_LITERAL: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#""((?:[^"\\]|\\.)*)"|'((?:[^'\\]|\\.)*)'"#).expect("string literal regex")
});

/// The string literals a line spells, quotes off: a text with a letter or digit
/// in it, which is what a reader of the output could still look for.
fn literals<'a>(lines: impl IntoIterator<Item = &'a str>) -> FxHashSet<String> {
    lines
        .into_iter()
        .flat_map(|line| STRING_LITERAL.captures_iter(line))
        .filter_map(|c| c.get(1).or_else(|| c.get(2)))
        .map(|m| m.as_str().to_string())
        .filter(|text| text.chars().any(char::is_alphanumeric))
        .collect()
}

pub(in crate::impact) fn twin_references(
    state: &ScoredState,
    cores: &[&Fragment],
) -> Vec<PathReference> {
    let changed: FxHashSet<String> = changed_rels(state).collect();
    // (the changed definition, its name, the literals its change removed)
    let mut removed: Vec<(&Fragment, &str, FxHashSet<String>)> = Vec::new();
    for core in cores {
        let Some(name) = spelled_name(core) else {
            continue;
        };
        let path = rel(state, core.path());
        let hunks = state.hunk_texts.iter().filter(|h| {
            let (first, last) = h.new_range();
            h.path == path && core.start_line() <= last && core.end_line() >= first
        });
        let kept = literals(core.content.lines());
        let gone: FxHashSet<String> =
            literals(hunks.flat_map(|h| h.removed.iter()).map(String::as_str))
                .into_iter()
                .filter(|lit| !kept.contains(lit))
                .collect();
        if !gone.is_empty() {
            removed.push((core, name, gone));
        }
    }
    let source = state.run.source();
    let mut found: Vec<PathReference> = Vec::new();
    for (core, name, gone) in &removed {
        let target = rel(state, core.path());
        let group = Group::of_name(&target);
        let same_name = state
            .all_fragments
            .iter()
            .filter(|f| spelled_name(f) == Some(*name));
        for twin in same_name {
            let path = rel(state, twin.path());
            let other = Group::of_name(&path);
            if other.is_none()
                || other == group
                || changed.contains(&path)
                || !is_code_file(&path)
                || found.iter().filter(|r| r.target == target).count() >= MAX_NAMED_BY_PER_TARGET
                || found.iter().any(|r| r.target == target && r.path == path)
            {
                continue;
            }
            let Some((at, literal, text)) =
                twin.content.lines().enumerate().find_map(|(at, text)| {
                    let spelled = literals([text]);
                    gone.iter()
                        .find(|lit| spelled.contains(*lit))
                        .map(|lit| (at, lit, text))
                })
            else {
                continue;
            };
            found.push(PathReference {
                class: class_of(&state.root_dir, &source, &path, text, None),
                line: twin.start_line() + at as u32,
                path,
                target: target.clone(),
                key: Some(literal.clone()),
            });
        }
    }
    found
}
