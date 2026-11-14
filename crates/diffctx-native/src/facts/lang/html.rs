//! What a web page's scripts state, read where they stand in the page. A page
//! (`.html`, `.htm`) is read as JavaScript and an Astro component (`.astro`) as
//! TypeScript, by the TypeScript extractor, over a copy of the page in which
//! everything but the code is blanked: the bytes and lines of every fact are
//! the page's own, so a definition inside a `<script>` is reported at its line
//! of the page and a body-only edit of it is the definition's change.
//!
//! What is code: the content of each `<script>` that runs as JavaScript (no
//! `type`, `module`, a JavaScript or TypeScript MIME type) and has no `src`, and
//! an Astro file's frontmatter. A classic `<script src="…">` is a
//! [`ImportKind::SideEffect`] import of the file it names, as `importScripts`
//! is for a worker: the page sees the globals of the scripts it loads and no
//! others. A module script's `src` loads nothing into the page's scope.
//!
//! Not read: attribute values (`onclick="…"`), template expressions of an Astro
//! file (`{…}`), and every other part of the markup. A name spelled only there
//! is not found by the cell.

use std::ops::Range;

use super::LineStarts;
use crate::facts::lang::typescript::{Dialect, extract_as, relative_of};
use crate::facts::{
    BlobFacts, Extractor, Fact, Import, ImportKind, Language, ModulePath, Scope, Star,
};
use crate::snapshot::RunBudget;

pub struct HtmlExtractor;
pub struct AstroExtractor;

impl Extractor for HtmlExtractor {
    fn language(&self) -> Language {
        Language::Html
    }

    fn extract(&self, text: &[u8], budget: &RunBudget) -> BlobFacts {
        page(Language::Html, Dialect::JavaScript, text, budget)
    }
}

impl Extractor for AstroExtractor {
    fn language(&self) -> Language {
        Language::Astro
    }

    fn extract(&self, text: &[u8], budget: &RunBudget) -> BlobFacts {
        page(Language::Astro, Dialect::TypeScript, text, budget)
    }
}

fn page(language: Language, dialect: Dialect, text: &[u8], budget: &RunBudget) -> BlobFacts {
    let read = scan(text, language == Language::Astro);
    let mut blob = extract_as(language, dialect, &read.masked, budget);
    let lines = LineStarts::new(text);
    for load in read.loads {
        blob.facts.push(Fact::Import(Import {
            site: lines.span(load.at),
            enclosing: Scope::Module,
            relative: relative_of(&load.src),
            module: ModulePath::from(load.src),
            kind: ImportKind::SideEffect,
            names: Vec::new(),
            star: Star::None,
            default: None,
        }));
    }
    blob.in_source_order();
    blob
}

/// A classic `<script src>`: the tag, and the path as written.
struct Load {
    at: Range<usize>,
    src: String,
}

struct Page {
    /// The page with everything but code blanked, newlines kept.
    masked: Vec<u8>,
    loads: Vec<Load>,
}

fn scan(text: &[u8], astro: bool) -> Page {
    let mut masked: Vec<u8> = text
        .iter()
        .map(|byte| if *byte == b'\n' { b'\n' } else { b' ' })
        .collect();
    let mut loads = Vec::new();
    let mut at = 0;
    if astro {
        if let Some(code) = frontmatter(text) {
            masked[code.clone()].copy_from_slice(&text[code.clone()]);
            at = code.end;
        }
    }
    while at < text.len() {
        if text[at] != b'<' {
            at += 1;
            continue;
        }
        if text[at..].starts_with(b"<!--") {
            at = find(text, at + 4, b"-->").map_or(text.len(), |end| end + 3);
            continue;
        }
        let name: &[u8] = if opens(text, at, b"script") {
            b"script"
        } else if opens(text, at, b"style") {
            b"style"
        } else {
            at += 1;
            continue;
        };
        let Some(open_end) = tag_end(text, at) else {
            break;
        };
        let written = &text[at + 1 + name.len()..open_end - 1];
        let empty = written.last() == Some(&b'/');
        let close = if empty {
            open_end
        } else {
            find_close(text, open_end, name)
        };
        if name == b"script" {
            let attributes = attributes(written);
            let attribute = |wanted: &str| {
                attributes
                    .iter()
                    .find(|(name, _)| name == wanted)
                    .map(|(_, value)| value.as_str())
            };
            if let Some(module) = runs_as_code(attribute("type"), astro) {
                match attribute("src") {
                    Some(src) if !module => {
                        if !src.is_empty() && !src.contains("://") && !src.starts_with("//") {
                            loads.push(Load {
                                at: at..open_end,
                                src: src.to_string(),
                            });
                        }
                    }
                    Some(_) => {}
                    None => masked[open_end..close].copy_from_slice(&text[open_end..close]),
                }
            }
        }
        at = if empty {
            open_end
        } else {
            find(text, close, b">").map_or(text.len(), |end| end + 1)
        };
    }
    Page { masked, loads }
}

/// Whether the tag at `at` opens an element called `name`.
fn opens(text: &[u8], at: usize, name: &[u8]) -> bool {
    let rest = &text[at + 1..];
    rest.len() > name.len()
        && rest[..name.len()].eq_ignore_ascii_case(name)
        && matches!(rest[name.len()], b' ' | b'\t' | b'\n' | b'\r' | b'>' | b'/')
}

fn find(text: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    text.get(from..)?
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|at| from + at)
}

/// Where `</name` starts at or after `from`, or the end of the text.
fn find_close(text: &[u8], from: usize, name: &[u8]) -> usize {
    let mut at = from;
    while let Some(found) = find(text, at, b"</") {
        let rest = &text[found + 2..];
        if rest.len() >= name.len() && rest[..name.len()].eq_ignore_ascii_case(name) {
            return found;
        }
        at = found + 2;
    }
    text.len()
}

/// The index past the `>` that ends the tag opened at `at`, quotes respected.
fn tag_end(text: &[u8], at: usize) -> Option<usize> {
    let mut quote: Option<u8> = None;
    for (offset, byte) in text[at + 1..].iter().enumerate() {
        match (quote, *byte) {
            (Some(open), byte) if byte == open => quote = None,
            (Some(_), _) => {}
            (None, b'"' | b'\'') => quote = Some(*byte),
            (None, b'>') => return Some(at + 2 + offset),
            (None, _) => {}
        }
    }
    None
}

/// The attributes of a tag as `(lower-cased name, value)`.
fn attributes(written: &[u8]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < written.len() {
        if written[at].is_ascii_whitespace() || written[at] == b'/' {
            at += 1;
            continue;
        }
        let start = at;
        while at < written.len()
            && !written[at].is_ascii_whitespace()
            && !matches!(written[at], b'=' | b'/')
        {
            at += 1;
        }
        let name = String::from_utf8_lossy(&written[start..at]).to_ascii_lowercase();
        let mut value = String::new();
        if written.get(at) == Some(&b'=') {
            at += 1;
            let begin = at;
            match written.get(at) {
                Some(quote @ (b'"' | b'\'')) => {
                    let quote = *quote;
                    at += 1;
                    let inside = at;
                    while at < written.len() && written[at] != quote {
                        at += 1;
                    }
                    value = String::from_utf8_lossy(&written[inside..at]).into_owned();
                    at += 1;
                }
                _ => {
                    while at < written.len() && !written[at].is_ascii_whitespace() {
                        at += 1;
                    }
                    value = String::from_utf8_lossy(&written[begin..at]).into_owned();
                }
            }
        }
        out.push((name, value));
    }
    out
}

/// Whether a `<script>` of this `type` is code the extractor reads, and if so
/// whether it is a module. A script with no `type` is classic in a page and a
/// module in an Astro file.
fn runs_as_code(written: Option<&str>, astro: bool) -> Option<bool> {
    let written = written.map(|t| t.trim().to_ascii_lowercase());
    match written.as_deref() {
        None | Some("") => Some(astro),
        Some("module") => Some(true),
        Some(
            "text/javascript"
            | "application/javascript"
            | "text/ecmascript"
            | "application/ecmascript"
            | "text/jsx"
            | "text/typescript"
            | "application/typescript",
        ) => Some(false),
        Some(_) => None,
    }
}

/// An Astro file's frontmatter: the lines between its first two `---` lines,
/// when the first line of the file is one.
fn frontmatter(text: &[u8]) -> Option<Range<usize>> {
    let first_end = text.iter().position(|byte| *byte == b'\n')?;
    if text[..first_end].trim_ascii() != b"---" {
        return None;
    }
    let start = first_end + 1;
    let mut line = start;
    while line <= text.len() {
        let end = text[line..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(text.len(), |n| line + n);
        if text[line..end].trim_ascii() == b"---" {
            return Some(start..line);
        }
        if end == text.len() {
            return None;
        }
        line = end + 1;
    }
    None
}
