//! How a caller reaches its callee through a type: merged symbols, dispatch routes, supertypes and the
//! class that encloses a fragment.

use std::path::Path;

use crate::run_provenance::state::ScoredState;

use diffctx_base::types::Fragment;

use super::callers::is_member;
use super::output::Caller;

/// The chunks of one long function (`main[88]`, `main[120]`) are one caller
/// with several call sites, not several callers, and so is a file's
/// module-level code split into unnamed chunks (#349); distinct functions
/// stay distinct, two same-named methods of one file among them (their spans
/// are apart), and so do evidence levels and stated kinds.
#[allow(
    clippy::disallowed_methods,
    reason = "`Caller.lines` is `a-b`, written by this module: a field that is no number has no call site to add"
)]
pub(super) fn merge_by_symbol(callers: Vec<Caller>) -> Vec<Caller> {
    let span = |l: &str| -> Option<(u32, u32)> {
        let (a, b) = l.split_once('-')?;
        Some((a.parse().ok()?, b.parse().ok()?))
    };
    let mut merged: Vec<Caller> = Vec::new();
    for mut c in callers {
        if c.sites.is_empty() {
            c.sites = c
                .lines
                .split('-')
                .next()
                .and_then(|l| l.parse().ok())
                .into_iter()
                .collect();
        }
        let one_definition = |m: &Caller| match (m.symbol.as_deref(), c.symbol.as_deref()) {
            _ if m.within.is_some() || c.within.is_some() => m.within == c.within,
            (None, None) => true,
            (Some(a), Some(b)) if base_symbol(a) == base_symbol(b) => {
                a != b // chunks of one function: `main`, `main[88]`
                    || matches!((span(&m.lines), span(&c.lines)), (Some((a0, a1)), Some((b0, b1))) if a0 <= b1 && b0 <= a1)
            }
            _ => false,
        };
        let same = |m: &&mut Caller| {
            m.path == c.path
                && m.evidence == c.evidence
                && m.kind == c.kind
                && m.folded_by == c.folded_by
                && m.fold_kind == c.fold_kind
                && m.fold_share == c.fold_share
                && one_definition(m)
        };
        match merged.iter_mut().find(|m| same(m)) {
            Some(m) => {
                // The caller spans every chunk it was split into, or a call
                // site in a later chunk sits outside the range shown (#472).
                if let (Some((a, b)), Some((c2, d))) = (span(&m.lines), span(&c.lines)) {
                    m.lines = format!("{}-{}", a.min(c2), b.max(d));
                }
                m.symbol = m.symbol.as_deref().map(|s| base_symbol(s).to_string());
                m.weight = m.weight.max(c.weight);
                m.sites.extend(c.sites);
                m.sites.sort_unstable();
                m.sites.dedup();
                if c.relation == "call" {
                    m.relation = "call";
                }
                if m.tests.is_empty() {
                    m.tests = c.tests;
                }
                m.unread_test = m.unread_test.take().or(c.unread_test);
                m.file_tested = m.tests.is_empty() && (m.file_tested || c.file_tested);
                m.tests_open = m.tests.is_empty() && (m.tests_open || c.tests_open);
                m.tests_partial = !m.tests.is_empty() && (m.tests_partial || c.tests_partial);
            }
            None => merged.push(c),
        }
    }
    merged
}

pub(crate) fn base_symbol(symbol: &str) -> &str {
    symbol.split('[').next().unwrap_or(symbol)
}

/// What can call a member nothing calls directly, in a language whose model
/// does not trace dispatch: the supertype it overrides or implements, the
/// runtime for a Python operator or a library's `_sunder_` hook, any interface a Go
/// method set satisfies.
/// Read as "nothing calls them", an override of `Runnable.run` or an
/// `__eq__` a test exercises was reported as dead (#566). A private or
/// static member is reached only directly, so its zero stands.
pub(super) fn dispatch_route(
    state: &ScoredState,
    core: &Fragment,
    file: &[&Fragment],
) -> Option<String> {
    let name = base_symbol(core.symbol_name.as_deref()?);
    let ext = Path::new(core.path())
        .extension()?
        .to_str()?
        .to_ascii_lowercase();
    match ext.as_str() {
        // Traits are the Rust cell's (`Gap::Dispatched`).
        "rs" => return None,
        "go" => {
            return core
                .content
                .trim_start()
                .starts_with("func (")
                .then(|| "any interface its type's method set satisfies".to_string());
        }
        "py" | "pyi" if name.len() > 2 && name.starts_with('_') && name.ends_with('_') => {
            return Some(format!(
                "the operator or protocol the runtime maps to `{name}`"
            ));
        }
        "py" | "pyi" if name == "close" && is_member(core, file) => {
            return Some("`contextlib.closing`, which calls `close` on what it wraps".to_string());
        }
        "py" | "pyi" if name.starts_with('_') => return None,
        _ => {}
    }
    if !is_member(core, file) {
        return None;
    }
    let text = state.run.source().read_to_string(Path::new(core.path()))?;
    let lines: Vec<&str> = text.lines().collect();
    let first = core.start_line().saturating_sub(1) as usize;
    let above = lines[..first.min(lines.len())]
        .iter()
        .rev()
        .take_while(|l| l.trim_start().starts_with('@'))
        .copied();
    let declaration = lines.get(first).copied().unwrap_or("");
    let head: String = above.chain([declaration]).collect::<Vec<_>>().join(" ");
    let words: Vec<&str> = head
        .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '@'))
        .collect();
    if words
        .iter()
        .any(|w| matches!(*w, "private" | "static" | "@staticmethod"))
    {
        return None;
    }
    let supers =
        enclosing_class(core, file).map_or_else(Vec::new, |c| supertypes(&c.content, &ext));
    let quoted = || format!("`{}`", supers[..supers.len().min(2)].join("`, `"));
    if words.iter().any(|w| matches!(*w, "@Override" | "override")) {
        return Some(if supers.is_empty() {
            "the supertype it overrides".to_string()
        } else {
            quoted()
        });
    }
    (!supers.is_empty()).then(quoted)
}

/// The types a class header names as its supertypes: `extends`,
/// `implements` and `with` clauses, a Python base list, a Kotlin, Swift or
/// C# `: Base` after the primary constructor. The root types every class
/// has are not routes.
pub(crate) fn supertypes(class: &str, ext: &str) -> Vec<String> {
    let Some(at) = class.find("class ") else {
        return Vec::new();
    };
    let header = &class[at..];
    let python = matches!(ext, "py" | "pyi");
    let header = header
        .split(if python { ":\n" } else { "{" })
        .next()
        .unwrap_or(header);
    let clause: String = if python {
        header
            .split_once('(')
            .and_then(|(_, rest)| rest.rsplit_once(')'))
            .map_or_else(String::new, |(bases, _)| bases.to_string())
    } else {
        // A primary constructor's `(x: Int)` holds colons of its own.
        let mut depth = 0usize;
        let flat: String = header
            .chars()
            .filter(|c| {
                match c {
                    '(' | '<' => depth += 1,
                    ')' | '>' => depth = depth.saturating_sub(1),
                    _ => return depth == 0,
                }
                false
            })
            .collect();
        let after = ["extends ", "implements ", " with ", ":"]
            .iter()
            .filter_map(|k| flat.find(k).map(|i| i + k.len()))
            .min();
        after.map_or_else(String::new, |i| flat[i..].to_string())
    };
    clause
        .split([',', ' ', '\n', '\t'])
        .map(|t| t.trim().trim_end_matches(':'))
        .filter(|t| {
            !t.is_empty()
                && !t.contains('=')
                && !matches!(
                    *t,
                    "extends" | "implements" | "with" | "object" | "Object" | "Any" | "AnyObject"
                )
                && t.chars()
                    .next()
                    .is_some_and(|c| c.is_alphabetic() || c == '_')
        })
        .map(|t| t.split(['[', '<', '(']).next().unwrap_or(t))
        .filter(|t| !t.is_empty())
        .map(|t| t.rsplit('.').next().unwrap_or(t))
        // Python's typing constructs declare no member a call reaches a subclass's
        // own through: their classes' methods are reached directly or not at all.
        .filter(|t| !(python && matches!(*t, "NamedTuple" | "TypedDict" | "Generic")))
        .map(str::to_string)
        .collect()
}

/// The class a fragment sits in, among `file`, the fragments of its own
/// file in fragment order.
/// The indentation of a text's first non-blank, non-decorator line.
pub(super) fn indent_of(text: &str) -> usize {
    text.lines()
        .find(|l| !l.trim().is_empty() && !l.trim_start().starts_with('@'))
        .map_or(0, |l| l.len() - l.trim_start().len())
}

pub(crate) fn enclosing_class<'a>(f: &Fragment, file: &[&'a Fragment]) -> Option<&'a Fragment> {
    let own = indent_of(&f.content);
    let containers = || {
        file.iter().copied().filter(|c| {
            c.path() == f.path()
                && c.id != f.id
                && c.kind.is_container()
                && c.start_line() <= f.start_line()
        })
    };
    // A class fragment may be its header alone, the methods fragmented on
    // their own: an indented definition then belongs to the nearest class
    // above it at a shallower indentation.
    containers()
        .filter(|c| c.end_line() >= f.end_line())
        .min_by_key(|c| c.line_count())
        .or_else(|| {
            (own > 0)
                .then(|| {
                    containers()
                        .filter(|c| indent_of(&c.content) < own)
                        .max_by_key(|c| c.start_line())
                })
                .flatten()
        })
}
