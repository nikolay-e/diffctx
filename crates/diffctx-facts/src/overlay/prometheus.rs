//! Prometheus rule files, for the fragments of a YAML file: an alerting or recording
//! rule is a definition named by its `alert` or `record`, which a pattern over the
//! lines does not see (`facts::lang::config` states the same definitions). Who reads
//! the name (a `promtool` test, a dashboard, a route) is read by no cell.

use diffctx_base::types::FragmentKind;
use diffctx_snapshot::snapshot::RunBudget;

use crate::facts::config::{Dialect, Options, read};
use crate::facts::lang::config::rules_of;

pub(super) fn is_rule_file(path: &str) -> bool {
    diffctx_base::languages::get_language_for_file(path) == Some("yaml")
}

/// The rules of `text` as `(name, lines, kind)`.
pub(super) fn declarations(text: &str) -> Vec<(Option<String>, (u32, u32), FragmentKind)> {
    if !text.contains("alert:") && !text.contains("record:") {
        return Vec::new();
    }
    let reading = read(
        Dialect::Yaml,
        text.as_bytes(),
        Options::default(),
        &RunBudget::default(),
    );
    reading
        .documents
        .iter()
        .flat_map(rules_of)
        .map(|(name, rule)| {
            (
                name.as_str().map(str::to_string),
                rule.span.lines,
                FragmentKind::Definition,
            )
        })
        .collect()
}
