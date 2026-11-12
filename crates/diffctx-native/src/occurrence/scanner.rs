//! The reader of occurrences: a byte scan for the spellings of a name in one
//! file, never a resolver. A spelling is a run of bytes equal to the name whose
//! neighbours are not identifier bytes of the file's language; the comment and
//! string ranges the extractor recorded say what it is.

use std::cmp::max;
use std::ops::Range;

use super::{Class, Occurrence};
use crate::facts::{FileFacts, Language, Lexical};

/// Whether the language binds a name regardless of its case. Where unsure, it
/// does: a wider scan only adds candidates.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Case {
    Sensitive,
    Insensitive,
}

impl Case {
    /// The case rule of the language's names: PHP, SQL and Dockerfile are
    /// insensitive.
    pub const fn of(language: Language) -> Self {
        match language {
            Language::Php | Language::Sql | Language::Dockerfile => Self::Insensitive,
            Language::Python
            | Language::JavaScript
            | Language::TypeScript
            | Language::Rust
            | Language::Kotlin
            | Language::Go
            | Language::Java
            | Language::C
            | Language::Cpp
            | Language::Config
            | Language::HelmTemplate
            | Language::HelmValues
            | Language::Terraform
            | Language::Shell
            | Language::ShellUnread
            | Language::Html
            | Language::Astro
            | Language::Swift => Self::Sensitive,
        }
    }
}

/// The spellings of the names in one file.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Scan {
    /// Occurrences in code, by start byte.
    pub code: Vec<Occurrence>,
    /// Occurrences in string literals, by start byte.
    pub mentions: Vec<Occurrence>,
    /// The blob has a loss: its comment and string
    /// ranges are not trusted, every spelling is in `code`, and the caller makes
    /// each a candidate.
    pub lost: bool,
}

/// Where a spelling stands, widest last: a spelling reaching code is code, one
/// reaching a string and no code is a mention.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Standing {
    Comment,
    Mention,
    Code,
}

fn standing(lexical: Option<Lexical>) -> Standing {
    match lexical {
        None => Standing::Code,
        Some(Lexical::String) => Standing::Mention,
        Some(Lexical::Comment) => Standing::Comment,
    }
}

/// Whether the language reads `$` as part of an identifier.
const fn dollar_joins(language: Language) -> bool {
    match language {
        Language::JavaScript
        | Language::TypeScript
        | Language::Html
        | Language::Astro
        | Language::Java => true,
        Language::Python
        | Language::Rust
        | Language::Kotlin
        | Language::Go
        | Language::Php
        | Language::C
        | Language::Cpp
        | Language::Config
        | Language::HelmTemplate
        | Language::HelmValues
        | Language::Terraform
        | Language::Dockerfile
        | Language::Shell
        | Language::ShellUnread
        | Language::Swift
        | Language::Sql => false,
    }
}

fn is_identifier_byte(language: Language, byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || byte == b'_'
        || byte >= 0x80
        || (byte == b'$' && dollar_joins(language))
}

/// Whether `text[start..end]` stands alone: the bytes around it that touch it
/// are not identifier bytes. A name that begins or ends on a byte no identifier
/// has needs no check on that side.
fn stands_alone(text: &[u8], start: usize, end: usize, language: Language) -> bool {
    let joins = |byte: u8| is_identifier_byte(language, byte);
    let left = !joins(text[start]) || start == 0 || !joins(text[start - 1]);
    let right = !joins(text[end - 1]) || text.get(end).is_none_or(|byte| !joins(*byte));
    left && right
}

/// The byte ranges in `text` that spell `needle` and stand alone, in order.
fn spellings(text: &[u8], needle: &[u8], language: Language) -> Vec<Range<u32>> {
    let mut found = Vec::new();
    let Some(last) = text.len().checked_sub(needle.len()) else {
        return found;
    };
    let Some(first) = needle.first() else {
        return found;
    };
    let mut from = 0;
    while from <= last {
        let Some(at) = text[from..=last].iter().position(|byte| byte == first) else {
            break;
        };
        let start = from + at;
        let end = start + needle.len();
        if text[start..end] == *needle && stands_alone(text, start, end, language) {
            found.push(start as u32..end as u32);
        }
        from = start + 1;
    }
    found
}

/// The 1-based line of each of `starts`, which ascend.
fn lines_of(text: &[u8], starts: &[u32]) -> Vec<u32> {
    let breaks: Vec<u32> = text
        .iter()
        .enumerate()
        .filter(|(_, byte)| **byte == b'\n')
        .map(|(at, _)| at as u32)
        .collect();
    starts
        .iter()
        .map(|start| 1 + breaks.partition_point(|at| at < start) as u32)
        .collect()
}

/// Every spelling of `names` in `text`, the bytes of the file `file` reads, with
/// its class. Comments are not occurrences and are left out; a file with a loss
/// is scanned as all code.
pub fn scan(file: &FileFacts, text: &[u8], names: &[&str], case: Case) -> Scan {
    let language = file.language();
    if u32::try_from(text.len()).is_err() {
        return Scan {
            lost: true,
            ..Scan::default()
        };
    }
    let lost = !file.blob.losses.is_empty();
    let folded;
    let haystack: &[u8] = match case {
        Case::Sensitive => text,
        Case::Insensitive => {
            folded = text.to_ascii_lowercase();
            &folded
        }
    };
    let mut found: Vec<Range<u32>> = Vec::new();
    for name in names {
        let needle = match case {
            Case::Sensitive => name.as_bytes().to_vec(),
            Case::Insensitive => name.as_bytes().to_ascii_lowercase(),
        };
        found.extend(spellings(haystack, &needle, language));
    }
    found.sort_by_key(|range| (range.start, range.end));
    found.dedup();
    let starts: Vec<u32> = found.iter().map(|range| range.start).collect();
    let lines = lines_of(text, &starts);
    let mut scan = Scan {
        lost,
        ..Scan::default()
    };
    for (bytes, line) in found.into_iter().zip(lines) {
        let class = if lost {
            Class::Code
        } else {
            let first = standing(file.blob.lexical_at(bytes.start));
            let last = standing(file.blob.lexical_at(bytes.end - 1));
            match max(first, last) {
                Standing::Comment => continue,
                Standing::Mention => Class::Mention,
                Standing::Code => Class::Code,
            }
        };
        let occurrence = Occurrence {
            file: file.path.clone(),
            bytes,
            line,
            class,
        };
        match class {
            Class::Code => scan.code.push(occurrence),
            Class::Mention => scan.mentions.push(occurrence),
        }
    }
    scan
}
