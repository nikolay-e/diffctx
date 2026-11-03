#![allow(dead_code, reason = "each test file uses its own part of the fixture")]

//! A stand-in for the Python extractor, to hold the facts layer to its contract
//! without depending on a language file: it reads a small indentation language
//! the way a syntax tree would, so the hashes mean what the contract says.
//!
//! - `def name(args):` opens a definition; its body is the more-indented lines
//!   after it. Inside a `class Name:` the definition's container is the class.
//! - `body_hash` is the body's tokens plus each line's block depth, comments and
//!   blank lines left out: a uniform re-indent keeps it, a statement moved into
//!   another block changes it. `text_hash` is the body's bytes.
//! - A first body line `"""text"""` is the doc, not the body.
//! - `@@container A.B`, `@@ns type`, `@@overload N`, `@@impl T`, `@@cfg X` before a
//!   `def` set the parts of its key that only a language's rules decide.
//! - `#parse-error`, `#deadline` and `#panic` anywhere in the text make the
//!   extractor report a parse error, run out of budget, or panic.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use _diffctx::facts::{
    BlobFacts, Def, DefKey, DefKind, Disambig, Extractor, Fact, FactsCache, Language, LocalKey,
    ModulePath, Name, Namespace, ProjectId, Registry, Span, TypeName, Visibility, hash_text,
    hash_tokens,
};
use _diffctx::snapshot::{AnalysisContext, Oid, RangeSpec, RunBudget};

use crate::snapshot_fixture::Repo;

#[derive(Clone)]
pub struct Toy {
    pub language: Language,
    pub calls: Arc<AtomicUsize>,
    pub version: u32,
    pub grammar: u64,
}

impl Toy {
    pub fn new() -> Self {
        Self {
            language: Language::Python,
            calls: Arc::new(AtomicUsize::new(0)),
            version: 1,
            grammar: 7,
        }
    }

    /// The same stand-in reading TypeScript files.
    pub fn typescript() -> Self {
        Self {
            language: Language::TypeScript,
            ..Self::new()
        }
    }

    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    pub fn registry(&self) -> Registry {
        Registry::new().with(self.clone())
    }

    /// This extractor and `other`, each for its own language.
    pub fn registry_with(&self, other: &Toy) -> Registry {
        Registry::new().with(self.clone()).with(other.clone())
    }
}

#[derive(Default)]
struct Pending {
    container: Option<Vec<Name>>,
    ns: Option<Namespace>,
    disambig: Option<Disambig>,
    decorators: Vec<Name>,
}

struct Line<'t> {
    start: usize,
    end: usize,
    number: u32,
    indent: usize,
    text: &'t str,
}

fn lines_of(text: &str) -> Vec<Line<'_>> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (index, raw) in text.split('\n').enumerate() {
        let end = start + raw.len();
        lines.push(Line {
            start,
            end,
            number: index as u32 + 1,
            indent: raw.len() - raw.trim_start().len(),
            text: raw.trim(),
        });
        start = end + 1;
    }
    lines
}

fn span(from: usize, to: usize, lines: (u32, u32)) -> Span {
    Span::new(from as u32..to as u32, lines)
}

impl Extractor for Toy {
    fn language(&self) -> Language {
        self.language
    }

    fn version(&self) -> u32 {
        self.version
    }

    fn grammar(&self) -> u64 {
        self.grammar
    }

    fn extract(&self, bytes: &[u8], _budget: &RunBudget) -> BlobFacts {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let text = String::from_utf8_lossy(bytes);
        assert!(
            !text.contains("#panic"),
            "the toy extractor panics on purpose"
        );
        let mut blob = BlobFacts::new(self.language);
        let lines = lines_of(&text);
        let mut classes: Vec<(usize, Name)> = Vec::new();
        let mut pending = Pending::default();
        let mut at = 0;
        while at < lines.len() {
            let line = &lines[at];
            at += 1;
            if line.text.is_empty() || line.text.starts_with('#') {
                continue;
            }
            while classes
                .last()
                .is_some_and(|(indent, _)| line.indent <= *indent)
            {
                classes.pop();
            }
            if let Some(directive) = line.text.strip_prefix("@@") {
                apply(&mut pending, directive);
            } else if let Some(decorator) = line.text.strip_prefix('@') {
                pending.decorators.push(Name::new(decorator.trim()));
            } else if let Some(rest) = line.text.strip_prefix("class ") {
                let name = rest.split([':', '(']).next().unwrap_or("").trim();
                classes.push((line.indent, Name::new(name)));
            } else if let Some(rest) = line.text.strip_prefix("def ") {
                let container = pending
                    .container
                    .take()
                    .unwrap_or_else(|| classes.iter().map(|(_, name)| name.clone()).collect());
                let end = body_end(&lines, at - 1);
                blob.facts.push(Fact::Definition(definition(
                    &text,
                    &lines[at - 1..end],
                    rest,
                    container,
                    std::mem::take(&mut pending),
                )));
                at = end;
            }
        }
        if text.contains("#parse-error") {
            blob.parse_error(vec![(1, 1)]);
        }
        if text.contains("#deadline") {
            blob.deadline();
        }
        blob.in_source_order();
        blob
    }
}

fn apply(pending: &mut Pending, directive: &str) {
    let (word, value) = directive.split_once(' ').unwrap_or((directive, ""));
    let value = value.trim();
    match word {
        "container" => {
            pending.container = Some(value.split('.').map(Name::new).collect());
        }
        "ns" => {
            pending.ns = Some(match value {
                "type" => Namespace::Type,
                "macro" => Namespace::Macro,
                _ => Namespace::Value,
            });
        }
        "overload" => pending.disambig = Some(Disambig::Overload(value.parse().unwrap_or(0))),
        "impl" => pending.disambig = Some(Disambig::ImplOf(TypeName::new(value))),
        "cfg" => pending.disambig = Some(Disambig::Cfg(value.to_string())),
        other => panic!("unknown directive {other}"),
    }
}

/// The index after the last line of the definition that starts at `first`.
fn body_end(lines: &[Line<'_>], first: usize) -> usize {
    let indent = lines[first].indent;
    let mut end = first + 1;
    let mut at = first + 1;
    while at < lines.len() {
        if !lines[at].text.is_empty() {
            if lines[at].indent <= indent {
                break;
            }
            end = at + 1;
        }
        at += 1;
    }
    end
}

fn definition(
    text: &str,
    lines: &[Line<'_>],
    header: &str,
    container: Vec<Name>,
    pending: Pending,
) -> Def {
    let name = header.split('(').next().unwrap_or("").trim();
    let signature = format!("def {}", header.trim_end().trim_end_matches(':'));
    let first = &lines[0];
    let last = &lines[lines.len() - 1];
    let mut body = &lines[1..];
    let mut doc = None;
    let first_code = body.iter().position(|line| !line.text.is_empty());
    if let Some(index) = first_code {
        if body[index].text.starts_with("\"\"\"") {
            let text = body[index].text.trim_matches('"');
            doc = hash_tokens(text.split_whitespace().map(str::as_bytes));
            body = &body[index + 1..];
        }
    }
    let mut depths: Vec<usize> = Vec::new();
    let mut tokens: Vec<Vec<u8>> = Vec::new();
    for line in body {
        if line.text.is_empty() || line.text.starts_with('#') {
            continue;
        }
        while depths.last().is_some_and(|top| *top > line.indent) {
            depths.pop();
        }
        if depths.last() != Some(&line.indent) {
            depths.push(line.indent);
        }
        tokens.push(format!("\u{1}{}", depths.len()).into_bytes());
        let code = line.text.split(" #").next().unwrap_or("");
        tokens.extend(code.split_whitespace().map(|word| word.as_bytes().to_vec()));
    }
    let from = body.first().map_or(last.start, |line| line.start);
    let kind = if container.is_empty() {
        DefKind::Function
    } else {
        DefKind::Method
    };
    Def {
        local: LocalKey {
            container,
            name: Name::new(name),
            ns: pending.ns.unwrap_or(Namespace::Value),
            disambig: pending.disambig,
        },
        kind,
        span: span(first.start, last.end, (first.number, last.number)),
        name_span: span(first.start, first.start, (first.number, first.number)),
        signature: Some(signature),
        decorators: pending.decorators,
        visibility: if name.starts_with('_') {
            Visibility::Private
        } else {
            Visibility::Public
        },
        body_hash: hash_tokens(tokens.iter().map(Vec::as_slice)).unwrap_or(0),
        text_hash: hash_text(text[from..last.end].as_bytes()),
        doc_hash: doc,
    }
}

/// A key as the fixture's repositories (no manifests, no project) give it.
pub fn key(module: &str, name: &str) -> DefKey {
    DefKey {
        project: ProjectId::default(),
        language: Language::Python,
        module: ModulePath::new(module),
        local: LocalKey::value(name),
    }
}

/// A key of a file of `language`.
pub fn key_in(language: Language, module: &str, name: &str) -> DefKey {
    DefKey {
        language,
        ..key(module, name)
    }
}

pub fn key_of(module: &str, local: LocalKey) -> DefKey {
    DefKey {
        project: ProjectId::default(),
        language: Language::Python,
        module: ModulePath::new(module),
        local,
    }
}

pub fn review(repo: &Repo, base: &Oid, head: &Oid) -> AnalysisContext {
    context(
        repo,
        RangeSpec::Review {
            base: base.clone(),
            head: head.clone(),
        },
    )
}

pub fn context(repo: &Repo, range: RangeSpec) -> AnalysisContext {
    AnalysisContext::new(repo.root.clone(), range, RunBudget::default()).expect("context")
}

pub fn cache_in(dir: &Path, toy: &Toy) -> FactsCache {
    FactsCache::new(dir.to_path_buf()).with_registry(toy.registry())
}

/// The entries a cache directory holds.
pub fn entries(dir: &Path) -> Vec<std::path::PathBuf> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<_> = read
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    found.sort();
    found
}

pub fn leftovers(dir: &Path) -> Vec<std::path::PathBuf> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    read.flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(".tmp-"))
        })
        .collect()
}
