//! Helm, for the fragments of a chart's template: a named template (`define
//! "chart.name"`) is a definition, which a pattern over the lines does not see.
//! Who includes it is the facts layer's (`facts::lang::helm`, `resolve::chart`).

use crate::facts::lang::helm::defined;
use diffctx_base::types::FragmentKind;

pub(super) use crate::facts::lang::helm::is_template;

/// The named templates of `text` as `(name, lines, kind)`.
pub(super) fn declarations(text: &str) -> Vec<(Option<String>, (u32, u32), FragmentKind)> {
    defined(text)
        .into_iter()
        .map(|(name, lines)| (Some(name), lines, FragmentKind::Function))
        .collect()
}
