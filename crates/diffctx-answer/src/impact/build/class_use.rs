//! The uses of a CSS class. A class is never called: its readers are the class tokens of markup, and
//! a script or a template may build the name at run time. The answer lists the literal readers and
//! calls the reads complete only when no other file spells the name and no file could compose it.

use diffctx_base::types::Fragment;

use crate::decide::{self};

use super::super::callers::{extension, is_document};
use super::super::changes::head_of;
use super::super::mentions::commented_in;
use super::super::output::Caller;
use super::super::removed::read_or_note;

use super::context::Impact;

pub(super) struct ClassUse {
    pub(super) readers: Vec<Caller>,
    /// A file that spells the name where no class attribute reads it, or one that could build it.
    open: bool,
}

impl ClassUse {
    /// A class is never called; its reads are complete unless something may read it unseen.
    pub(super) fn coverage(
        &self,
        stopped: Option<decide::gap::Gap>,
    ) -> decide::coverage::Coverages {
        decide::coverage::Coverages::local(stopped.into_iter().collect())
            .with_reads(self.open.then_some(decide::gap::Gap::NotTraced))
    }
}

const MARKUP: [&str; 7] = ["astro", "html", "htm", "vue", "svelte", "jsx", "tsx"];

const STYLESHEETS: [&str; 4] = ["css", "scss", "less", "sass"];

/// The files that can hold markup or a script that builds a class name.
const WEB: [&str; 26] = [
    "astro", "html", "htm", "vue", "svelte", "jsx", "tsx", "js", "mjs", "cjs", "ts", "mts", "cts",
    "php", "erb", "twig", "j2", "jinja", "hbs", "ejs", "liquid", "njk", "pug", "haml", "cshtml",
    "mdx",
];

/// The lines of those files that touch a class name at run time or interpolate a class attribute.
const COMPOSING: &str = r#"classList|className|class:list|:class|\[class|ngClass|classnames|clsx|innerHTML|outerHTML|insertAdjacentHTML|setAttribute[[:space:]]*\([[:space:]]*["']class|class[[:space:]]*=[[:space:]]*([<{@$]|["']([^"']*[{<$@]|[^"']*$))"#;

pub(super) fn of(cx: &Impact<'_>, core: &Fragment) -> Option<ClassUse> {
    let class = core.symbol_name.as_deref()?;
    if diffctx_base::languages::get_language_for_file(core.path()) != Some("css") {
        return None;
    }
    if !class
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Some(ClassUse {
            readers: Vec::new(),
            open: true,
        });
    }
    let state = cx.state;
    let head = head_of(cx.diff_range);
    let search = |pattern: &str, pathspec: &[&str]| {
        let found = read_or_note(
            state,
            diffctx_snapshot::grep::grep_lines_matching(
                &state.root_dir,
                &state.run.source(),
                pattern,
                head.as_deref(),
                pathspec,
            ),
        );
        state.run.note_withheld(found.withheld);
        found.rows
    };
    let mut readers = Vec::new();
    let mut open = false;
    let token = r"(^|[^[:alnum:]_-])";
    for (path, line, text) in search(&format!("{token}{class}([^[:alnum:]_-]|$)"), &[]) {
        let ext = extension(&path);
        if STYLESHEETS.contains(&ext.as_str()) || is_document(&path) || commented_in(&path, &text) {
            continue;
        }
        if MARKUP.contains(&ext.as_str()) && in_class_value(&text, class) {
            readers.push(Caller::at_line(
                path,
                None,
                format!("{line}-{line}"),
                "resolved",
                "reference",
                line,
            ));
        } else {
            open = true;
        }
    }
    open = open
        || *cx.composed.get_or_init(|| {
            let specs = WEB.map(|ext| format!(":(glob,icase)**/*.{ext}"));
            search(COMPOSING, &specs.each_ref().map(String::as_str))
                .iter()
                .any(|(_, _, text)| composes(text))
        });
    Some(ClassUse { readers, open })
}

fn token_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-'
}

/// Whether `line` holds `class` as a token of the value of a `class` or `className` attribute.
fn in_class_value(line: &str, class: &str) -> bool {
    line.match_indices(class).any(|(at, _)| {
        let before = &line[..at];
        if before.ends_with(token_char) || line[at + class.len()..].starts_with(token_char) {
            return false;
        }
        let Some(open) = before.rfind(['"', '\'']) else {
            return false;
        };
        if before.matches(&before[open..=open]).count() % 2 == 0 {
            return false;
        }
        before[..open]
            .trim_end()
            .strip_suffix('=')
            .map(str::trim_end)
            .and_then(|name| {
                name.strip_suffix("className")
                    .or_else(|| name.strip_suffix("class"))
            })
            .is_some_and(|rest| !rest.ends_with(|c: char| token_char(c) || c == ':' || c == '.'))
    })
}

/// Whether `line` could build a class name at run time: a binding, an assignment or a call whose
/// argument is not a string literal, an interpolated attribute, markup written from a script.
fn composes(line: &str) -> bool {
    let after = |marker: &'static str| {
        line.match_indices(marker)
            .map(move |(at, _)| &line[at + marker.len()..])
    };
    [
        "class:list",
        ":class",
        "[class",
        "ngClass",
        "classnames",
        "clsx",
        "innerHTML",
        "outerHTML",
        "insertAdjacentHTML",
    ]
    .iter()
    .any(|marker| line.contains(marker))
        || after("classList").any(|tail| {
            !tail
                .split_once('(')
                .is_some_and(|(_, args)| literal_args(args))
        })
        || after("className").any(|tail| !literal_assignment(tail))
        || after("setAttribute").any(sets_class_from_a_value)
        || after("class").any(interpolated)
}

/// The text after the closing quote of the string literal `text` opens.
fn literal_str(text: &str) -> Option<&str> {
    let quote = text.chars().next().filter(|c| matches!(c, '"' | '\''))?;
    text[1..].split_once(quote).map(|(_, rest)| rest)
}

/// Whether `args`, the text after a call's `(`, is string literals only.
fn literal_args(mut args: &str) -> bool {
    loop {
        let Some(rest) = literal_str(args.trim_start()) else {
            return false;
        };
        match rest.trim_start().strip_prefix(',') {
            Some(more) => args = more,
            None => return rest.trim_start().starts_with(')'),
        }
    }
}

/// Whether `tail`, the text after `className`, assigns one string literal.
fn literal_assignment(tail: &str) -> bool {
    let tail = tail.trim_start();
    tail.strip_prefix("+=")
        .or_else(|| {
            tail.strip_prefix('=')
                .filter(|value| !value.starts_with('='))
        })
        .and_then(|value| literal_str(value.trim_start()))
        .is_some_and(|rest| {
            matches!(
                rest.trim_start().chars().next(),
                None | Some(';' | ',' | ')' | '}' | '>' | '/')
            )
        })
}

/// Whether `tail`, the text after `setAttribute`, sets `class` to something but a string literal.
fn sets_class_from_a_value(tail: &str) -> bool {
    let args = tail.trim_start().strip_prefix('(').map(str::trim_start);
    args.and_then(|args| {
        args.strip_prefix("\"class\"")
            .or_else(|| args.strip_prefix("'class'"))
    })
    .and_then(|rest| rest.trim_start().strip_prefix(','))
    .is_some_and(|value| !literal_args(value))
}

/// Whether `tail`, the text after `class`, is an attribute whose value interpolates or spans lines.
fn interpolated(tail: &str) -> bool {
    let Some(value) = tail.trim_start().strip_prefix('=').map(str::trim_start) else {
        return false;
    };
    match value.chars().next() {
        Some('{' | '<' | '$' | '@') => true,
        Some(quote @ ('"' | '\'')) => !value[1..]
            .split_once(quote)
            .is_some_and(|(inside, _)| !inside.contains(['{', '<', '$', '@'])),
        _ => false,
    }
}
