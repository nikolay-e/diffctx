//! What the actions of a Helm template say: the named templates it defines, the
//! named templates it includes and the values it reads.
//!
//! This is the text of Go template actions (`super::template` keeps them), read
//! for the three forms a chart's cells need and for nothing else:
//!
//! - `define "name"` (and `block "name"`) up to its matching `end`;
//! - `include "name" .` and `template "name" .`, with a literal name;
//! - `.Values.a.b` and `index .Values "a" "b"`, with literal keys.
//!
//! A read through a variable, a computed name or a `tpl` is not read: it is a
//! gap the template's own [`super::Reading::templates`] already say.

use serde::{Deserialize, Serialize};

use crate::facts::{Scope, Span};

use super::template::Template;

/// What an action uses.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum UseOf {
    /// A value of the chart: `.Values.image.tag` is the name `image.tag`.
    Values,
    /// A named template: `include "app.labels" .`.
    Include,
}

/// An action that reads a value or includes a named template.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ConfigUse {
    /// The action in the file.
    pub site: Span,
    /// The named template the action sits in, else the module.
    pub enclosing: Scope,
    pub of: UseOf,
    pub name: String,
}

/// A named template: from its `define` action to the matching `end`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Define {
    pub name: String,
    pub span: Span,
    /// The `define` action alone.
    pub name_span: Span,
}

/// A use, and the named template it sits in (an index into the defines).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Found {
    pub site: Span,
    pub of: UseOf,
    pub name: String,
    pub inside: Option<usize>,
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Analysis {
    pub defines: Vec<Define>,
    pub uses: Vec<Found>,
}

/// The defines and uses of a template's actions, in source order.
pub fn analyse(templates: &[Template]) -> Analysis {
    let mut analysis = Analysis::default();
    // The open blocks: the define an `if`, `range` or `with` is not, `None`.
    let mut open: Vec<Option<usize>> = Vec::new();
    for action in templates {
        let body = body_of(&action.text);
        if body.starts_with("/*") {
            continue;
        }
        let keyword = body.split_whitespace().next().unwrap_or("");
        match keyword {
            "end" => {
                if let Some(Some(index)) = open.pop() {
                    let define = &mut analysis.defines[index];
                    define.span.bytes.end = action.span.bytes.end;
                    define.span.lines.1 = action.span.lines.1;
                }
                continue;
            }
            "if" | "range" | "with" => open.push(None),
            "define" | "block" => {
                let named = first_string(&body[keyword.len()..]);
                match named {
                    Some(name) => {
                        analysis.defines.push(Define {
                            name,
                            span: action.span.clone(),
                            name_span: action.span.clone(),
                        });
                        open.push(Some(analysis.defines.len() - 1));
                    }
                    None => open.push(None),
                }
                // A `block` also invokes the template it defines; its name is
                // no other use.
                if keyword == "define" {
                    continue;
                }
            }
            _ => {}
        }
        let inside = open.iter().rev().find_map(|block| *block);
        for (of, name) in uses_in(body) {
            analysis.uses.push(Found {
                site: action.span.clone(),
                of,
                name,
                inside,
            });
        }
    }
    analysis
}

/// The text of an action without its delimiters, trim markers and blanks.
fn body_of(action: &str) -> &str {
    let inner = action
        .strip_prefix("{{")
        .and_then(|rest| rest.strip_suffix("}}"))
        .unwrap_or(action);
    let inner = inner.trim();
    let inner = inner
        .strip_prefix('-')
        .filter(|rest| rest.starts_with(char::is_whitespace))
        .unwrap_or(inner);
    let inner = inner
        .strip_suffix('-')
        .filter(|rest| rest.ends_with(char::is_whitespace))
        .unwrap_or(inner);
    inner.trim()
}

/// The first quoted string of `text` (`"` or a backtick), unescaped.
fn first_string(text: &str) -> Option<String> {
    let (quote, rest) = {
        let trimmed = text.trim_start();
        let quote = trimmed.chars().next().filter(|c| matches!(c, '"' | '`'))?;
        (quote, &trimmed[1..])
    };
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            c if c == quote => return Some(out),
            '\\' if quote == '"' => out.push(chars.next()?),
            other => out.push(other),
        }
    }
    None
}

fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// The values read and the named templates included in one action's body.
fn uses_in(body: &str) -> Vec<(UseOf, String)> {
    let mut found = Vec::new();
    for (at, _) in body.match_indices(".Values") {
        let before = body[..at].chars().next_back();
        if before.is_some_and(is_ident) {
            continue;
        }
        let rest = &body[at + ".Values".len()..];
        let mut keys: Vec<String> = Vec::new();
        if rest.starts_with('.') {
            let mut tail = rest;
            while let Some(after) = tail.strip_prefix('.') {
                let key: String = after.chars().take_while(|c| is_ident(*c)).collect();
                if key.is_empty() || key.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                    break;
                }
                tail = &after[key.len()..];
                keys.push(key);
            }
        } else if rest.starts_with(char::is_whitespace) && body[..at].trim_end().ends_with("index")
        {
            let mut tail = rest;
            while let Some(name) = first_string_at_start(tail) {
                keys.push(name.0);
                tail = &tail[name.1..];
            }
        }
        if !keys.is_empty() {
            found.push((UseOf::Values, keys.join(".")));
        }
    }
    for word in ["include", "template"] {
        for (at, _) in body.match_indices(word) {
            let before = body[..at].chars().next_back();
            let rest = &body[at + word.len()..];
            if before.is_some_and(is_ident) || !rest.starts_with(char::is_whitespace) {
                continue;
            }
            if let Some(name) = first_string_at_start(rest) {
                found.push((UseOf::Include, name.0));
            }
        }
    }
    found
}

/// A quoted string at the start of `text` (after blanks), and how many bytes of
/// `text` it takes.
fn first_string_at_start(text: &str) -> Option<(String, usize)> {
    let trimmed = text.trim_start();
    if !trimmed.starts_with(['"', '`']) {
        return None;
    }
    let name = first_string(trimmed)?;
    // The string, its quotes and any escapes: find the end by reading again.
    let quote = trimmed.chars().next()?;
    let mut end = 1;
    let mut chars = trimmed[1..].chars();
    while let Some(c) = chars.next() {
        end += c.len_utf8();
        if c == '\\' && quote == '"' {
            end += chars.next().map_or(0, char::len_utf8);
        } else if c == quote {
            break;
        }
    }
    Some((name, text.len() - trimmed.len() + end))
}
