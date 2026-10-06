use std::fmt::Write as _;
use std::path::Path;
use std::time::Instant;

use rustc_hash::{FxHashMap, FxHashSet};
use schemars::JsonSchema;
use serde::Serialize;

use crate::bindings::{self, Definition, Evidence, Finding, Relation, Resolver, Site};
use crate::container::{innermost, named_container_of, within};
use crate::graph::EdgeCategory;
use crate::pipeline::ScoredState;
use crate::types::{Fragment, FragmentId, FragmentKind};

pub const IMPACT_SCHEMA: &str = "diffctx.impact.v1";

/// The whole serialized answer, not its fragment tokens: a hook injects this
/// into a context the reader did not ask to spend, and locate taught that a
/// budget on the parts leaves the envelope unbounded (#300).
pub const IMPACT_TOKEN_CAP: u32 = 2_000;

/// The line log is one git call per changed symbol; past this many the range
/// is a rewrite, and commit overlap on it says nothing a reader would act on.
const MAX_BLAMED_SYMBOLS: usize = 50;

/// Call sites named per file in the text form before the rest fold into a
/// count.
const MAX_CALLERS_SHOWN_PER_FILE: usize = 3;

/// Contracts are a list of facts the reader can grep for; past this many the
/// list is the diff's own `pub` lines read back.
const MAX_CONTRACTS: usize = 12;
/// A declaration longer than this many lines before its body opens is
/// compared on its first lines only.
const MAX_SIGNATURE_LINES: usize = 12;
const CALLER_LOSING_LIMITS: &[crate::resource::LimitReason] = &[
    crate::resource::LimitReason::Deadline,
    crate::resource::LimitReason::DiscoveryTruncated,
    crate::resource::LimitReason::CandidateLimit,
];
/// Paths listed before the rest fold into a count: the reader has the diff.
const MAX_CHANGED_FILES_LISTED: usize = 24;

/// A symbol name with more definitions than this cannot be traced by name.
const MAX_DEFINITIONS: usize = 2;

const SCHEMA_DIRECTORIES: &[&str] = &[
    "migrations",
    "migration",
    "versions",
    "schema",
    "schemas",
    "openapi",
    "swagger",
];
/// Names a data or API description file takes: `schema.json`,
/// `openapi.yaml`, `user.schema.json`. A script named for a schema is code.
const SCHEMA_FILE_STEMS: &[&str] = &["schema", "openapi", "swagger"];
const SCHEMA_DATA_EXTENSIONS: &[&str] = &["json", "yaml", "yml", "graphql", "graphqls", "sql"];
const SCHEMA_FILE_EXTENSIONS: &[&str] = &[
    ".sql",
    ".proto",
    ".graphql",
    ".graphqls",
    ".avsc",
    ".prisma",
];

/// What a change reaches that its own diff does not show (#310): the
/// callers outside the diff of each changed symbol, whether a test guards
/// each caller, which symbols more than one commit of the range touched,
/// and the public contracts the range edits.
#[derive(Serialize, JsonSchema, Clone)]
pub struct ImpactOutput {
    pub schema: &'static str,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<String>,
    /// The index captured as a tree, when the range is the staged changes:
    /// the snapshot this answer is about. A later `git add` makes it stale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_tree: Option<String>,
    /// Set on a `--symbol` query (#336): `changed` then lists the symbol's
    /// definitions, and `changed_files` the files that hold them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    pub changed_files: Vec<String>,
    /// Deleted by the change: no fragment, but what still names them is
    /// what breaks (#347).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deleted_files: Vec<String>,
    /// Lines outside the change that still name a deleted or renamed path:
    /// a Dockerfile `COPY`, a workflow step, an import (#313).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stale_references: Vec<StaleReference>,
    #[serde(default, skip_serializing_if = "crate::render::is_zero")]
    pub stale_references_omitted: usize,
    #[serde(default, skip_serializing_if = "crate::render::is_zero")]
    pub commit_count: usize,
    pub changed: Vec<ChangedSymbol>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contracts: Vec<Contract>,
    /// Nothing outside the diff depends on it, no contract moved, no symbol
    /// was edited twice: the diff already says everything. A hook stays
    /// silent on this, which is why it is a field and not an absence.
    pub empty: bool,
    /// Callers dropped to fit `IMPACT_TOKEN_CAP`, lowest weight first.
    #[serde(default, skip_serializing_if = "crate::render::is_zero")]
    pub truncated: usize,
    /// Changed files left out of `changed_files` to fit the cap; the count
    /// is still whole. The reader holds the file list already.
    #[serde(default, skip_serializing_if = "crate::render::is_zero")]
    pub changed_files_omitted: usize,
    /// Contracts beyond `MAX_CONTRACTS`, schema changes never among them.
    #[serde(default, skip_serializing_if = "crate::render::is_zero")]
    pub contracts_omitted: usize,
    /// The run stopped at a limit that can lose a caller — the deadline, a
    /// truncated reverse discovery, a capped candidate universe — and `empty`
    /// is never claimed while this is set. Caps every real repository trips
    /// (a large lockfile, the fragment or edge cap) are not listed: they
    /// fired on every run and made the hook speak on every commit (#306).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limits: Vec<crate::resource::LimitReason>,
    /// Whether any test file was in view. Without one, an absent `tested_by`
    /// says nothing about the caller, and the text form does not call it
    /// untested.
    pub tests_known: bool,
    /// Changed definitions no test reaches by a static link, listed or not:
    /// a change nobody calls can still be a change nothing tests.
    #[serde(default, skip_serializing_if = "crate::render::is_zero")]
    pub untested_definitions: usize,
    /// Files the run withheld (`.diffctx/ignore`, gitignore, secret names):
    /// changed files of the range, or files naming the queried symbol. Their
    /// calls and tests were not read, so a zero says nothing about them
    /// (#392).
    #[serde(default, skip_serializing_if = "crate::render::is_zero")]
    pub withheld_files: usize,
    /// The token cap the answer was fitted to.
    #[serde(skip)]
    #[schemars(skip)]
    pub cap: u32,
}

#[derive(Serialize, JsonSchema, Clone)]
pub struct ChangedSymbol {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    pub lines: String,
    pub kind: String,
    /// Commits of the range that touched these lines (blame at the head);
    /// zero when the range is not `base..head`.
    #[serde(default, skip_serializing_if = "crate::render::is_zero")]
    pub commits: usize,
    pub callers: Vec<Caller>,
    /// Why callers could not be resolved at all: a name defined in many
    /// files in a language without a binding model, a trait method reached
    /// through dynamic dispatch. Its empty `callers` is then no finding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unresolved: Option<String>,
    /// No test reaches the definition itself by a static link.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub untested: bool,
    /// Call sites left working by the change: it only added fields or
    /// parameters with defaults, so they are counted, not listed (#388).
    #[serde(default, skip_serializing_if = "crate::render::is_zero")]
    pub compatible_callers: usize,
}

#[derive(Serialize, JsonSchema, Clone)]
pub struct Caller {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    pub lines: String,
    pub weight: f64,
    /// A test file that calls this caller, directly or through `tested_via`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tested_by: Option<String>,
    /// The function between the caller and the test that reaches it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tested_via: Option<String>,
    /// A test reaches the caller's file but none of the paths to this caller:
    /// the one case where an absent `tested_by` is a finding, not a gap in
    /// what static analysis can see (a route hit over HTTP, a fixture).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub file_tested: bool,
    /// The decorator that registers the caller (`@register_node`,
    /// `@app.route`): what reaches it is whatever reads that registry, which
    /// no static test link follows, so its absence is no finding (#388).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registered_by: Option<String>,
    /// `resolved`: an import binding links the reference to the changed
    /// definition (Python, JavaScript, TypeScript). `name`: the lexical model
    /// of a language without a binding model — a mention in code of a file
    /// that names the module. `candidate`: the binding or the receiver is not
    /// known; `reason` says which.
    pub evidence: &'static str,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
    /// `reference` when the definition is named without being called: a
    /// callback, a value passed on.
    #[serde(default, skip_serializing_if = "is_call")]
    pub relation: &'static str,
    /// The lines that reference the definition, when known.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sites: Vec<u32>,
}

fn is_call(relation: &&'static str) -> bool {
    *relation == "call"
}

#[derive(Serialize, JsonSchema, Clone)]
pub struct StaleReference {
    pub path: String,
    pub line: u32,
    /// The deleted or renamed path the line still names.
    pub target: String,
    /// `deleted`, or `renamed to <new path>`.
    pub kind: String,
}

#[derive(Serialize, JsonSchema, Clone)]
pub struct Contract {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    pub kind: &'static str,
}

fn is_code_file(path: &str) -> bool {
    let ext = Path::new(path)
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
        .unwrap_or_default();
    crate::config::extensions::CODE_EXTENSIONS.contains(ext.as_str())
}

/// The caller must name the symbol itself, in code: the language builders
/// link a call by name to every definition of that name in the repository
/// (case-folded), so an edge alone says "same name somewhere", a comment
/// mention says nothing, and `Cli` in a doc line is not `Cli::parse()`.
/// Languages whose line comment is `#`; elsewhere `#[derive]`, `#include`
/// and `#define` are code.
const HASH_COMMENT_EXTENSIONS: &[&str] = &[
    "py", "pyi", "rb", "sh", "bash", "zsh", "pl", "pm", "r", "jl", "ex", "exs", "nim", "ps1", "cr",
    "tcl", "yml", "yaml", "toml", "mk", "cmake", "pp",
];

/// Who may stand before a `.` in front of the name and still mean it.
enum Receiver<'a> {
    /// A method: any object can carry it.
    Any,
    /// A module-level definition: only its own module, by stem or by an
    /// alias the caller's file binds to it. `re.search` is not a call to a
    /// changed `download.search` (#345).
    Module {
        stem: &'a str,
        aliases: &'a [String],
    },
}

/// The line with string literals and comments blanked, positions kept: a
/// name inside `"must be refused"` or a comment is not a reference (#345).
/// `triple` carries an open Python triple-quoted string across lines.
fn code_of(
    line: &str,
    hash_comments: bool,
    single_quotes: bool,
    triple: &mut Option<&'static str>,
) -> String {
    let bytes = line.as_bytes();
    let mut out = bytes.to_vec();
    let mut i = 0;
    let mut quote: Option<u8> = None;
    while i < bytes.len() {
        let rest = &bytes[i..];
        let blanked = if let Some(t) = *triple {
            close_triple(rest, t, triple)
        } else if let Some(q) = quote {
            close_quote(rest, q, &mut quote)
        } else {
            match lexeme(rest, hash_comments, single_quotes) {
                Lexeme::Comment => {
                    out[i..].fill(b' ');
                    break;
                }
                Lexeme::Triple(t) => {
                    *triple = Some(t);
                    t.len()
                }
                Lexeme::Quote(q) => {
                    quote = Some(q);
                    1
                }
                Lexeme::Code => {
                    i += 1;
                    continue;
                }
            }
        };
        out[i..i + blanked].fill(b' ');
        i += blanked;
    }
    String::from_utf8(out).unwrap_or_default()
}

enum Lexeme {
    Code,
    Comment,
    Triple(&'static str),
    Quote(u8),
}

fn lexeme(rest: &[u8], hash_comments: bool, single_quotes: bool) -> Lexeme {
    let b = rest[0];
    if hash_comments && (rest.starts_with(b"\"\"\"") || rest.starts_with(b"'''")) {
        return Lexeme::Triple(if b == b'"' { "\"\"\"" } else { "'''" });
    }
    // In the `#` languages `//` is floor division, defined-or or a
    // rational, and the call after it is code (#414).
    let comment = if hash_comments {
        b == b'#'
    } else {
        rest.starts_with(b"//")
    };
    if comment {
        Lexeme::Comment
    } else if b == b'"' || b == b'`' || (single_quotes && b == b'\'') {
        Lexeme::Quote(b)
    } else {
        Lexeme::Code
    }
}

/// Bytes of a triple-quoted string at `rest`, closing it at its delimiter.
fn close_triple(rest: &[u8], t: &'static str, triple: &mut Option<&'static str>) -> usize {
    if rest.starts_with(t.as_bytes()) {
        *triple = None;
        t.len()
    } else {
        1
    }
}

/// Bytes of a quoted string at `rest`: an escape takes two, the quote ends it.
fn close_quote(rest: &[u8], q: u8, quote: &mut Option<u8>) -> usize {
    match rest {
        [b'\\', _, ..] => 2,
        [b, ..] if *b == q => {
            *quote = None;
            1
        }
        _ => 1,
    }
}

/// `code_of` for the binding model's languages (Python and JS/TS).
pub(crate) fn mask_code(line: &str, python: bool, triple: &mut Option<&'static str>) -> String {
    code_of(line, python, true, triple)
}

fn receiver_before(code: &str, at: usize) -> Option<&str> {
    let head = code[..at].strip_suffix('.')?;
    let head = head.strip_suffix('?').unwrap_or(head);
    let start = head
        .rfind(|c: char| !(c.is_alphanumeric() || c == '_'))
        .map_or(0, |i| i + 1);
    Some(&head[start..])
}

fn mentions(caller: &Fragment, symbol: Option<&str>, receiver: &Receiver) -> bool {
    let Some(sym) = symbol else { return true };
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let ext = Path::new(caller.path())
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let hash_comments = HASH_COMMENT_EXTENSIONS.contains(&ext.as_str());
    // A Rust `'a` is a lifetime, not the start of a string.
    let single_quotes = ext != "rs";
    let mut triple = None;
    caller.content.lines().any(|line| {
        let code = code_of(line, hash_comments, single_quotes, &mut triple);
        let head = code.trim_start();
        // A `*` alone opens a block-comment continuation; `*out = f()` is a
        // deref assignment, and `#[derive]` is Rust, not a comment.
        let star_comment = head.starts_with('*')
            && head[1..]
                .chars()
                .next()
                .is_none_or(|c| c.is_whitespace() || c == '/');
        if star_comment || head.starts_with("/*") || head.starts_with("--") {
            return false;
        }
        let bytes = code.as_bytes();
        code.match_indices(sym).any(|(at, _)| {
            let before = at.checked_sub(1).map(|i| bytes[i]);
            let after = bytes.get(at + sym.len()).copied();
            if before.is_some_and(is_word) || after.is_some_and(is_word) {
                return false;
            }
            match (receiver, receiver_before(&code, at)) {
                (Receiver::Module { stem, aliases }, Some(r)) => {
                    r.eq_ignore_ascii_case(stem) || aliases.iter().any(|a| a == r)
                }
                _ => true,
            }
        })
    })
}

/// Names the caller's file binds the module to: `import a.stem as R`,
/// `from a import stem as R`, `import * as R from "./stem"`.
fn module_aliases(file_fragments: &[&Fragment], stem: &str) -> Vec<String> {
    let mut aliases = Vec::new();
    for f in file_fragments {
        for line in f.content.lines() {
            let words: Vec<&str> = line
                .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '*'))
                .filter(|w| !w.is_empty())
                .collect();
            if !words.iter().any(|w| w.eq_ignore_ascii_case(stem)) {
                continue;
            }
            for pair in words.windows(2) {
                if pair[0] == "as" && pair[1] != "*" {
                    aliases.push(pair[1].to_string());
                }
            }
        }
    }
    aliases.sort();
    aliases.dedup();
    aliases
}

/// A method lives inside a type, or names its receiver in the declaration
/// (`func (l *Ledger) Reconcile`, Kotlin `fun String.pad()`); anything else
/// is reached through its module.
/// `file`: the fragments of the core's own file, in fragment order.
fn is_member(core: &Fragment, file: &[&Fragment]) -> bool {
    if enclosing_class(core, file).is_some() {
        return true;
    }
    let head = without_decorators(&core.content).trim_start();
    let receiver = head.starts_with("func (")
        || head
            .strip_prefix("fun ")
            .is_some_and(|rest| rest.split('(').next().is_some_and(|n| n.contains('.')));
    receiver
        || file.iter().any(|f| {
            f.path() == core.path()
                && f.id != core.id
                && (f.kind.is_container() || f.kind == FragmentKind::Impl)
                && f.start_line() <= core.start_line()
                && f.end_line() >= core.end_line()
        })
}

/// Languages where a directory is a namespace: a sibling file reaches the
/// changed symbol with no import to name it by. Rust, Python and JavaScript
/// siblings still name the module (`use`, `import`), so for them it must
/// appear among the caller file's identifiers.
const PACKAGE_SCOPED_EXTENSIONS: &[&str] = &[
    "go", "java", "kt", "kts", "scala", "cs", "php", "c", "cc", "cpp", "cxx", "h", "hpp", "m",
    "mm", "swift", "dart",
];

/// What impact knows of each file in view: the identifiers of all its
/// fragments (an import sits outside the calling fragment) and the
/// fragments themselves, for the import lines.
struct Files<'a> {
    identifiers: FxHashMap<&'a str, FxHashSet<&'a str>>,
    fragments: FxHashMap<&'a str, Vec<&'a Fragment>>,
}

/// A caller that never names the changed file's module and lives in another
/// directory is a name match, not a call: `Command` in a binary crate is not
/// `std::process::Command`, however many files construct one. Same-directory
/// callers in a package-scoped language (Go, Java, Kotlin) pass, because a
/// package needs no import to reach its neighbours.
fn references_module(
    caller: &Fragment,
    files: &Files,
    symbol: Option<&str>,
    changed_path: &str,
) -> bool {
    let changed = Path::new(changed_path);
    let caller_path = Path::new(caller.path());
    if caller_path == changed {
        return true;
    }
    let empty = FxHashSet::default();
    // The import sits at the top of the file, outside the calling fragment.
    let file_words = files.identifiers.get(caller.path()).unwrap_or(&empty);
    let ext = caller_path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if caller_path.parent() == changed.parent() && PACKAGE_SCOPED_EXTENSIONS.contains(&ext.as_str())
    {
        return true;
    }
    let Some(stem) = changed.file_stem().and_then(|s| s.to_str()) else {
        return true;
    };
    let stem = stem.to_lowercase();
    if stem.len() < 2 {
        // Identifiers are indexed from two characters; a one-letter module
        // cannot be told apart from anything, so nothing is dropped on it.
        return true;
    }
    if matches!(
        stem.as_str(),
        "mod" | "lib" | "main" | "index" | "init" | "__init__"
    ) {
        // A module named for its role (`mod.rs`, `index.ts`, `__init__.py`)
        // is imported by its directory, and a binary root is not imported.
        let dir = changed
            .parent()
            .and_then(|d| d.file_name())
            .and_then(|d| d.to_str())
            .map(str::to_lowercase)
            .filter(|d| {
                !matches!(
                    d.as_str(),
                    "src" | "lib" | "app" | "bin" | "tests" | "test" | "pkg" | "internal"
                )
            });
        return dir.is_some_and(|d| file_words.contains(d.as_str()));
    }
    file_words.contains(stem.as_str())
        || symbol.is_some_and(|s| imported_from_package(changed, files, caller.path(), s))
}

/// `from models import Steps` reaches `models/wearables.py::Steps` through
/// the package that re-exports it: the caller names the package, never the
/// module (#351). Only an import line naming both the package and the symbol
/// counts — `models` alone is a common word. The resolver of #329 replaces
/// this.
fn imported_from_package(changed: &Path, files: &Files, caller_path: &str, symbol: &str) -> bool {
    let Some(dir) = changed
        .parent()
        .and_then(|d| d.file_name())
        .and_then(|d| d.to_str())
    else {
        return false;
    };
    let has_word = |line: &str, word: &str| {
        line.split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .any(|w| w == word)
    };
    files.fragments.get(caller_path).is_some_and(|frags| {
        frags.iter().flat_map(|f| f.content.lines()).any(|line| {
            let head = line.trim_start();
            ["from ", "import ", "use ", "export "]
                .iter()
                .any(|k| head.starts_with(k))
                && has_word(head, dir)
                && has_word(head, symbol)
        })
    })
}

/// A fragment that only imports is where a name comes in, not where it is
/// called: the module docstring and the import block of a file link to every
/// changed symbol the file uses and would be listed as callers with nothing
/// to check.
fn is_import_block(content: &str) -> bool {
    let mut saw_code = false;
    let mut in_docstring = false;
    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if in_docstring {
            if line.contains("\"\"\"") || line.contains("'''") {
                in_docstring = false;
            }
            continue;
        }
        if line.starts_with("\"\"\"") || line.starts_with("'''") {
            let rest = &line[3..];
            if !rest.contains("\"\"\"") && !rest.contains("'''") {
                in_docstring = true;
            }
            continue;
        }
        if line.starts_with("//")
            || line.starts_with('#')
            || line.starts_with("/*")
            || line.starts_with('*')
        {
            continue;
        }
        let is_import = line.starts_with("import ")
            || line.starts_with("from ")
            || line.starts_with("use ")
            || line.starts_with("pub use ")
            || line.starts_with("require ")
            || line.starts_with("require(")
            || line.starts_with("const ") && line.contains("require(")
            || line.starts_with("export ") && line.contains(" from ")
            || line.starts_with("package ")
            || line.starts_with("using ")
            || line.starts_with("extern crate ")
            || line.starts_with("mod ")
            || line == ")"
            || line == "};"
            || line == "}"
            || line.ends_with(",") && !line.contains('(');
        if !is_import {
            saw_code = true;
            break;
        }
    }
    !saw_code
}

fn rel(state: &ScoredState, path: &str) -> String {
    crate::paths::display_rel_or_abs(&state.root_dir, Path::new(path))
}

fn lines_of(id: &FragmentId) -> String {
    format!("{}-{}", id.start_line, id.end_line)
}

/// Visibility the source states: `pub`, `export`, `public`, a Go
/// capitalised name. Python and Ruby state none, so no contract is claimed
/// for them rather than a guessed one.
fn is_public(path: &str, symbol: Option<&str>, content: &str) -> bool {
    if path.ends_with(".go") {
        return symbol.is_some_and(|s| s.chars().next().is_some_and(|c| c.is_ascii_uppercase()));
    }
    let head = content.trim_start();
    // `pub(crate)`, `pub(super)` and `pub(in …)` reach no other crate.
    head.starts_with("pub ")
        || head.starts_with("export ")
        || head.starts_with("public ")
        || head.starts_with("module.exports")
        || head.starts_with("exports.")
}

/// Whether every module on a Rust file's path is public from its crate
/// root: `pub fn` in a `pub(crate) mod` is crate-internal (#359). A binary
/// crate exports nothing. Files outside Rust are left to `is_public`.
fn rust_module_is_public(state: &ScoredState, path: &str) -> bool {
    if !path.ends_with(".rs") {
        return true;
    }
    let source = state.run.source();
    let read = |p: &Path| {
        source
            .read_to_string(p)
            .or_else(|| std::fs::read_to_string(p).ok())
    };
    let file = Path::new(path);
    let Some(src_dir) = file.ancestors().find(|d| {
        d.file_name().is_some_and(|n| n == "src")
            && d.parent().is_some_and(|c| {
                c.join("Cargo.toml").exists() || read(&c.join("Cargo.toml")).is_some()
            })
    }) else {
        return true;
    };
    let Ok(rel_path) = file.strip_prefix(src_dir) else {
        return true;
    };
    let mut segments: Vec<String> = rel_path
        .with_extension("")
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect();
    if segments.last().is_some_and(|s| s == "mod") {
        segments.pop();
    }
    if segments.first().is_some_and(|s| s == "main" || s == "bin") {
        return false;
    }
    if segments == ["lib"] {
        return true;
    }
    let mut parent = src_dir.join("lib.rs");
    for (i, seg) in segments.iter().enumerate() {
        let Some(text) = read(&parent) else {
            return i > 0;
        };
        let declared_pub = text.lines().map(without_attributes).any(|l| {
            l.strip_prefix("pub mod ")
                .and_then(|rest| {
                    rest.trim_start()
                        .trim_start_matches("r#")
                        .strip_prefix(seg.as_str())
                })
                .is_some_and(|after| after.trim_start().starts_with([';', '{']))
                || (l.starts_with("pub use ") && l.contains(&format!("{seg}::")))
        });
        if !declared_pub {
            return false;
        }
        let dir = parent.parent().unwrap_or(src_dir).to_path_buf();
        let base = if parent
            .file_name()
            .is_some_and(|n| n == "lib.rs" || n == "mod.rs")
        {
            dir
        } else {
            dir.join(parent.file_stem().unwrap_or_default())
        };
        let flat = base.join(format!("{seg}.rs"));
        parent = if flat.exists() || read(&flat).is_some() {
            flat
        } else {
            base.join(seg).join("mod.rs")
        };
    }
    true
}

/// A line without the `#[cfg(..)]`-style attributes written before its item.
fn without_attributes(line: &str) -> &str {
    let mut rest = line.trim_start();
    while let Some(attr) = rest.strip_prefix("#[") {
        let mut depth = 1usize;
        let Some(end) = attr.char_indices().find_map(|(i, c)| {
            match c {
                '[' => depth += 1,
                ']' => depth -= 1,
                _ => {}
            }
            (depth == 0).then_some(i)
        }) else {
            return rest;
        };
        rest = attr[end + 1..].trim_start();
    }
    rest
}

fn is_schema_file(path: &str) -> bool {
    // A test named for the schema it checks is not a schema.
    if crate::testfiles::is_test_path(Path::new(path)) {
        return false;
    }
    let lower = path.to_lowercase();
    if SCHEMA_FILE_EXTENSIONS
        .iter()
        .any(|ext| lower.ends_with(ext))
    {
        return true;
    }
    let p = Path::new(&lower);
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
    let in_schema_dir = p.parent().is_some_and(|d| {
        d.components()
            .any(|c| SCHEMA_DIRECTORIES.contains(&c.as_os_str().to_str().unwrap_or("")))
    });
    // A migration is code by extension and a schema by where it lives.
    let migration = p
        .components()
        .any(|c| matches!(c.as_os_str().to_str(), Some("migrations" | "migration")))
        || lower.contains("/alembic/versions/");
    if migration {
        return true;
    }
    if !SCHEMA_DATA_EXTENSIONS.contains(&ext) {
        return false;
    }
    let file = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
    in_schema_dir
        || file
            .split('.')
            .any(|part| SCHEMA_FILE_STEMS.contains(&part))
}

/// The revision a change is measured against: the left side of a range, a
/// lone revision itself, HEAD for the working tree.
fn base_revision(diff_range: Option<&str>) -> String {
    match diff_range.map(crate::git::split_diff_range) {
        Some((Some(base), _)) => base,
        _ => diff_range.unwrap_or("HEAD").to_string(),
    }
}

/// A fragment without its decorators. The decorator belongs to the fragment
/// (Python `@app.route(…, methods={"GET"})`, a TS `@Component({…})`) but not
/// to the declaration: `export` comes after it, and a `{` in it would end the
/// declaration's head before the signature began.
fn without_decorators(content: &str) -> &str {
    let mut rest = content;
    while let Some(line) = rest.lines().next() {
        let t = line.trim_start();
        if !(t.is_empty() || t.starts_with('@')) {
            break;
        }
        rest = rest.get(line.len() + 1..).unwrap_or("");
    }
    rest
}

/// The declaration a caller depends on: from the definition's first line
/// through the one that opens its body (`{`, `=>`, a trailing `:`), cut
/// there, so a body on the same line is not part of it.
fn declaration_head(content: &str) -> String {
    let mut head = Vec::new();
    for line in without_decorators(content)
        .lines()
        .take(MAX_SIGNATURE_LINES)
    {
        if let Some(at) = line.find("=>") {
            head.push(&line[..at + 2]);
            break;
        }
        if let Some(at) = line.find('{') {
            head.push(&line[..=at]);
            break;
        }
        head.push(line);
        if line.trim_end().ends_with(':') || line.trim_end().ends_with(';') {
            break;
        }
    }
    head.join("\n")
}

/// Whether an exported symbol's declaration moved: its head is looked for in
/// the file at the base revision, whitespace-insensitively. A body-only edit
/// is what the callers list already says, and a contracts section repeated
/// on every refactor teaches the reader to skip it (#325). A file the base
/// does not have is new, and every export in it is a new contract.
fn signature_changed(state: &ScoredState, base: &str, core: &Fragment) -> bool {
    let rel_path = rel(state, core.path());
    let Ok(old) = crate::git::show_file_at_revision(&state.root_dir, base, Path::new(&rel_path))
    else {
        return true;
    };
    let squash = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let head = squash(&declaration_head(&core.content));
    head.is_empty() || !squash(&old).contains(&head)
}

/// `(base, head)` for blame: a two-sided range as given, a single revision
/// against HEAD (its commits are `rev..HEAD`), nothing for the working tree.
fn commit_bounds(diff_range: Option<&str>) -> Option<(String, String)> {
    let range = diff_range?;
    if range == "HEAD" {
        return None;
    }
    match crate::git::split_diff_range(range) {
        (Some(base), Some(head)) => Some((base, head)),
        (None, None) => Some((range.to_string(), "HEAD".to_string())),
        _ => None,
    }
}

/// Distinct commits of the range that touched the fragment's lines. The
/// line log follows the span back through the range, so a commit whose
/// lines a later one overwrote still counts; blame at the head would not
/// see it.
fn commits_touching(state: &ScoredState, base: &str, head: &str, frag: &Fragment) -> usize {
    let rel_path = rel(state, frag.path());
    let span = format!("{},{}:{}", frag.id.start_line, frag.id.end_line, rel_path);
    let range = format!("{base}..{head}");
    let Ok(out) = crate::git::run_git(
        &state.root_dir,
        &["log", "--format=%H", "--no-patch", "-L", &span, &range],
    ) else {
        return 0;
    };
    out.lines()
        .map(str::trim)
        .filter(|l| (l.len() == 40 || l.len() == 64) && l.bytes().all(|b| b.is_ascii_hexdigit()))
        .collect::<FxHashSet<_>>()
        .len()
}

struct TestIndex<'a> {
    /// Source path -> one test path linked to it by a test edge.
    by_path: FxHashMap<String, String>,
    test_fragments: Vec<&'a Fragment>,
    /// `tested_by` per fragment: callers repeat across changed symbols, and
    /// each answer walks every test fragment.
    memo: std::cell::RefCell<FxHashMap<FragmentId, Option<(String, Option<String>)>>>,
}

impl<'a> TestIndex<'a> {
    fn build(state: &'a ScoredState) -> Self {
        let graph = &state.scoring_result.graph;
        let mut by_path: FxHashMap<String, String> = FxHashMap::default();
        graph.for_each_categorized_edge(|src, dst, cat| {
            if cat != EdgeCategory::TestEdge {
                return;
            }
            let src_is_test = crate::testfiles::is_test_path(Path::new(src.path.as_ref()));
            let dst_is_test = crate::testfiles::is_test_path(Path::new(dst.path.as_ref()));
            let (source, test) = match (src_is_test, dst_is_test) {
                (true, false) => (dst, src),
                (false, true) => (src, dst),
                _ => return,
            };
            by_path
                .entry(source.path.to_string())
                .or_insert_with(|| test.path.to_string());
        });
        let test_fragments = state
            .all_fragments
            .iter()
            .filter(|f| crate::testfiles::is_test_path(Path::new(f.path())))
            .collect();
        Self {
            by_path,
            test_fragments,
            memo: std::cell::RefCell::default(),
        }
    }

    fn any(&self) -> bool {
        !self.test_fragments.is_empty() || !self.by_path.is_empty()
    }

    /// A test guards a caller when it names the caller's symbol in code and
    /// reaches the caller's module — the same bar a caller has to clear. A
    /// bare name match (`render` in a Rust test, `render()` in a Python
    /// script) attributed guards to tests that never import the file (#312),
    /// and a test edge to the file proves the file is tested, not the caller.
    fn direct(&self, caller: &Fragment, link: &Link) -> Option<String> {
        if crate::testfiles::is_test_path(Path::new(caller.path())) {
            return Some(caller.path().to_string());
        }
        // A chunk inside a long function is named `main[88]`; the test
        // names `main`.
        let name = caller
            .symbol_name
            .as_deref()
            .map(base_symbol)
            .filter(|s| s.len() >= 3)?;
        // A test that does not spell the name cannot link to it.
        self.test_fragments
            .iter()
            .filter(|t| t.content.contains(name))
            .find(|t| link(t, caller))
            .map(|t| t.path().to_string())
    }

    /// The test that reaches the caller, and the function in between when
    /// the test calls something that calls it — a public tool over a helper
    /// is how most code is tested (#348). Two hops up the call graph.
    fn tested_by(
        &self,
        caller: &'a Fragment,
        state: &ScoredState,
        by_id: &FxHashMap<&FragmentId, &'a Fragment>,
        link: &Link,
    ) -> Option<(String, Option<String>)> {
        if let Some(known) = self.memo.borrow().get(&caller.id) {
            return known.clone();
        }
        let answer = self.walk_up(caller, state, by_id, link);
        self.memo
            .borrow_mut()
            .insert(caller.id.clone(), answer.clone());
        answer
    }

    fn walk_up(
        &self,
        caller: &'a Fragment,
        state: &ScoredState,
        by_id: &FxHashMap<&FragmentId, &'a Fragment>,
        link: &Link,
    ) -> Option<(String, Option<String>)> {
        if let Some(test) = self.direct(caller, link) {
            return Some((test, None));
        }
        let graph = &state.scoring_result.graph;
        let mut frontier: Vec<&Fragment> = vec![caller];
        let mut seen: FxHashSet<&FragmentId> = FxHashSet::from_iter([&caller.id]);
        for _ in 0..MAX_TEST_HOPS {
            let mut next = Vec::new();
            for f in &frontier {
                graph.for_each_reverse_neighbor(&f.id, |src, _| {
                    if graph.edge_category(src, &f.id) != Some(EdgeCategory::Semantic)
                        || !graph.is_forward(src, &f.id)
                    {
                        return;
                    }
                    let Some(up) = by_id.get(src).copied() else {
                        return;
                    };
                    if link(up, f) && seen.insert(&up.id) {
                        next.push(up);
                    }
                });
            }
            for up in &next {
                if crate::testfiles::is_test_path(Path::new(up.path())) {
                    return Some((up.path().to_string(), None));
                }
                if let Some(test) = self.direct(up, link) {
                    return Some((test, up.symbol_name.clone()));
                }
            }
            frontier = next;
        }
        None
    }

    fn file_tested(
        &self,
        caller: &Fragment,
        reaches_file: &dyn Fn(&Fragment, &Fragment) -> bool,
    ) -> bool {
        self.by_path.contains_key(caller.path())
            || self.test_fragments.iter().any(|t| reaches_file(t, caller))
    }
}

/// Whether the first fragment references the second's definition: through
/// a resolved binding in the languages that have a binding model, through
/// the lexical model elsewhere.
type Link<'l> = dyn Fn(&Fragment, &Fragment) -> bool + 'l;

/// How far up the call graph a test may sit and still count as reaching a
/// caller: the test, a function it calls, and one more.
const MAX_TEST_HOPS: usize = 2;

/// The chunks of one long function (`main[88]`, `main[120]`) are one caller
/// with several call sites, not several callers, and so is a file's
/// module-level code split into unnamed chunks (#349); distinct functions
/// stay distinct, and so do evidence levels.
fn merge_by_symbol(callers: Vec<Caller>) -> Vec<Caller> {
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
        let same = |m: &&mut Caller| {
            m.path == c.path
                && m.evidence == c.evidence
                && m.symbol.as_deref().map(base_symbol) == c.symbol.as_deref().map(base_symbol)
        };
        match merged.iter_mut().find(|m| same(m)) {
            Some(m) => {
                m.symbol = m.symbol.as_deref().map(|s| base_symbol(s).to_string());
                m.weight = m.weight.max(c.weight);
                m.sites.extend(c.sites);
                m.sites.sort_unstable();
                m.sites.dedup();
                if c.relation == "call" {
                    m.relation = "call";
                }
                if m.tested_by.is_none() {
                    m.tested_by = c.tested_by;
                    m.tested_via = c.tested_via;
                }
                m.file_tested = m.tested_by.is_none() && (m.file_tested || c.file_tested);
            }
            None => merged.push(c),
        }
    }
    merged
}

fn base_symbol(symbol: &str) -> &str {
    symbol.split('[').next().unwrap_or(symbol)
}

/// The trait a Rust method implements or declares (`impl Trait for Type`,
/// `trait Trait`), whose calls may arrive through dynamic dispatch the graph
/// does not model. Read from the file: the impl block is not a fragment.
fn dispatched_trait(state: &ScoredState, core: &Fragment) -> Option<String> {
    if !core.path().ends_with(".rs") {
        return None;
    }
    let text = state
        .run
        .source()
        .read_to_string(Path::new(core.path()))
        .or_else(|| std::fs::read_to_string(core.path()).ok())?;
    let indent = |l: &str| l.len() - l.trim_start().len();
    let lines: Vec<&str> = text.lines().collect();
    let own = lines
        .get(core.start_line().saturating_sub(1) as usize)
        .map_or(0, |l| indent(l));
    if own == 0 {
        return None;
    }
    let head = lines[..core.start_line().saturating_sub(1) as usize]
        .iter()
        .rev()
        .find(|l| !l.trim().is_empty() && indent(l) < own)?
        .trim_start();
    let head = head.strip_prefix("pub ").unwrap_or(head);
    let head = head.strip_prefix("unsafe ").unwrap_or(head);
    let name = if let Some(rest) = head.strip_prefix("impl") {
        let (lhs, _) = rest.split_once(" for ")?;
        skip_generics(lhs)
    } else {
        head.strip_prefix("trait ")?
    };
    // `std::fmt::Display<T>` names `Display`; `trait Foo: Bar` names `Foo`.
    let name = name.trim().split(['<', ' ', '{']).next().unwrap_or("");
    let name = name.rsplit("::").next().unwrap_or(name);
    let name = name.split(':').next().unwrap_or(name);
    (!name.is_empty()).then(|| name.to_string())
}

/// `<T: Clone> Iterator<Item = T>` without its leading parameter list.
fn skip_generics(text: &str) -> &str {
    let text = text.trim_start();
    if !text.starts_with('<') {
        return text;
    }
    let mut depth = 0usize;
    let mut prev = ' ';
    for (i, c) in text.char_indices() {
        match c {
            '<' => depth += 1,
            // `Fn() -> u32` closes nothing.
            '>' if prev != '-' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return &text[i + 1..];
                }
            }
            _ => {}
        }
        prev = c;
    }
    ""
}

/// The class a fragment sits in, among `file`, the fragments of its own
/// file in fragment order.
/// The indentation of a text's first non-blank, non-decorator line.
fn indent_of(text: &str) -> usize {
    text.lines()
        .find(|l| !l.trim().is_empty() && !l.trim_start().starts_with('@'))
        .map_or(0, |l| l.len() - l.trim_start().len())
}

fn enclosing_class<'a>(f: &Fragment, file: &[&'a Fragment]) -> Option<&'a Fragment> {
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

/// Decorators that shape a definition without handing it to anyone.
const SHAPING_DECORATORS: &[&str] = &[
    "property",
    "staticmethod",
    "classmethod",
    "dataclass",
    "abstractmethod",
    "override",
    "cached_property",
    "wraps",
    "lru_cache",
    "cache",
    "total_ordering",
    "final",
    "overload",
    "contextmanager",
    "asynccontextmanager",
    "Override",
    "Deprecated",
    "SuppressWarnings",
    "FunctionalInterface",
];

/// The decorator that hands a definition to a registry, a router or a
/// container, on the definition or on the class around it.
fn registering_decorator(f: &Fragment, file: &[&Fragment]) -> Option<String> {
    let of = |content: &str| {
        content
            .lines()
            .map(str::trim_start)
            .take_while(|l| l.is_empty() || l.starts_with('@'))
            .filter_map(|l| {
                let name = l
                    .strip_prefix('@')?
                    .split(|c: char| c == '(' || c.is_whitespace())
                    .next()?;
                let last = name.rsplit('.').next().unwrap_or(name);
                (!name.is_empty()
                    && !SHAPING_DECORATORS.contains(&last)
                    && !name.starts_with("pytest."))
                .then(|| name.to_string())
            })
            .next()
    };
    of(&f.content).or_else(|| enclosing_class(f, file).and_then(|c| of(&c.content)))
}

/// A field or parameter line that brings its own default (`x: int = 0`,
/// `x = 0`, TypeScript `x?: T`): adding one breaks no call site.
static DEFAULTED_LINE: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
    regex::Regex::new(
        r"^\s*(readonly\s+)?[A-Za-z_$][\w$]*\s*(\?\s*:[^=]*|:[^=]*=\s*\S.*|=\s*[^=\s].*)$",
    )
    .expect("defaulted line regex")
});

/// The parameters between a declaration's first `(` and its match, split at
/// top-level commas, whitespace squashed.
fn parameter_list(text: &str) -> Option<(String, Vec<String>, String)> {
    let open = text.find('(')?;
    let mut depth = 0i32;
    let mut close = None;
    for (i, c) in text[open..].char_indices() {
        match c {
            '(' | '[' | '{' | '<' => depth += 1,
            ')' | ']' | '}' | '>' => {
                depth -= 1;
                if depth == 0 && c == ')' {
                    close = Some(open + i);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close?;
    let squash = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut params = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    for c in text[open + 1..close].chars() {
        match c {
            '(' | '[' | '{' | '<' => depth += 1,
            ')' | ']' | '}' | '>' => depth -= 1,
            ',' if depth == 0 => {
                params.push(squash(&cur));
                cur.clear();
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    params.push(squash(&cur));
    params.retain(|p| !p.is_empty());
    Some((squash(&text[..open]), params, squash(&text[close..])))
}

fn defaulted(param: &str) -> bool {
    (param.contains('=') && !param.contains("=>") && !requires_value(param))
        || param.contains("?:")
        || param.starts_with('*')
        || param.starts_with("...")
}

/// `x: int = Field(...)` and `x = ...` declare a default that is a
/// required marker, not a value.
fn requires_value(line: &str) -> bool {
    let Some((_, value)) = line.split_once('=') else {
        return false;
    };
    let value = value.trim().trim_end_matches(',').trim();
    value == "..." || value.starts_with("Field(...") || value.starts_with("Field( ...")
}

/// A line that adds a field or a parameter with its own default.
fn adds_defaulted(line: &str) -> bool {
    // `__slots__ = ("y",)` or `__hash__ = None` changes how every instance
    // is built or compared; a default does not make it compatible.
    let dunder = line.trim_start().starts_with("__");
    DEFAULTED_LINE.is_match(line) && !dunder && !line.contains("=>") && !requires_value(line)
}

/// A field or parameter declaration of any kind: what may not follow an
/// inserted one, since everything after it would shift position.
fn declares_member(line: &str) -> bool {
    static MEMBER: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
        regex::Regex::new(r"^\s*(readonly\s+)?\*{0,2}[A-Za-z_$][\w$]*\s*(\??\s*:|=|,\s*$)")
            .expect("member regex")
    });
    MEMBER.is_match(line)
}

/// Whether every hunk in the definition only added fields or parameters
/// that carry a default (#388): the call sites keep working, so they are
/// counted instead of listed.
fn compatible_extension(core: &Fragment, hunks: &[&crate::change_class::HunkText]) -> bool {
    let head_end = core.start_line() + declaration_head(&core.content).lines().count() as u32;
    !hunks.is_empty()
        && hunks.iter().all(|h| {
            let (_, end) = h.new_range();
            if h.removed.is_empty() {
                // Appended only: the next declaration line after the run
                // must close the list, or the inserted member shifts every
                // positional argument behind it.
                let after = core
                    .content
                    .lines()
                    .skip((end + 1).saturating_sub(core.start_line()) as usize)
                    .find(|l| !l.trim().is_empty());
                let appended = after.is_none_or(|l| !declares_member(l));
                (core.kind.is_container() || end < head_end)
                    && appended
                    && h.added
                        .iter()
                        .filter(|l| !l.trim().is_empty())
                        .all(|l| adds_defaulted(l))
            } else {
                // A parameter list lives in the declaration head; the same
                // shape in a body line is a call (`compute(a)` →
                // `compute(a, strict=True)`), and it breaks nothing it adds.
                let (start, _) = h.new_range();
                if start < core.start_line() || end >= head_end {
                    return false;
                }
                let (Some((old_pre, old, old_post)), Some((new_pre, new, new_post))) = (
                    parameter_list(&h.removed.join("\n")),
                    parameter_list(&h.added.join("\n")),
                ) else {
                    return false;
                };
                old_pre == new_pre
                    && old_post == new_post
                    && new.len() > old.len()
                    && new[..old.len()] == old[..]
                    && new[old.len()..].iter().all(|p| defaulted(p))
            }
        })
}

/// `deadline`: the line log (one git call per changed symbol) runs only
/// while there is time left; commit overlap is the least actionable of the
/// three signals and the first to yield.
pub fn build_impact(
    state: &ScoredState,
    diff_range: Option<&str>,
    query: Option<&str>,
    deadline: Option<Instant>,
) -> ImpactOutput {
    let graph = &state.scoring_result.graph;
    let by_id: FxHashMap<&FragmentId, &Fragment> =
        state.all_fragments.iter().map(|f| (&f.id, f)).collect();
    let changed_paths: FxHashSet<&str> = state
        .changed_files
        .iter()
        .filter_map(|p| p.to_str())
        .collect();
    let tests = TestIndex::build(state);
    // A caller in a changed file is outside the diff unless its lines are
    // in a hunk: the reader holds the hunks, not the rest of the file (#362).
    let mut hunk_lines: FxHashMap<&str, Vec<(u32, u32)>> = FxHashMap::default();
    for h in &state.hunk_texts {
        // A pure removal sits between new lines N and N+1: as `(N+1, N)` the
        // overlap test below holds only for a fragment spanning both, not
        // for the caller that ends on N right above the deleted lines.
        let range = if h.new_len == 0 {
            (h.new_start + 1, h.new_start)
        } else {
            h.new_range()
        };
        hunk_lines.entry(h.path.as_str()).or_default().push(range);
    }
    let in_a_hunk = |id: &FragmentId| {
        if state.hunk_texts.is_empty() {
            return changed_paths.contains(id.path.as_ref());
        }
        hunk_lines
            .get(rel(state, id.path.as_ref()).as_str())
            .is_some_and(|ranges| {
                ranges
                    .iter()
                    .any(|&(a, b)| id.start_line <= b && id.end_line >= a)
            })
    };
    let hunks_in = |core: &Fragment| -> Vec<&crate::change_class::HunkText> {
        let path = rel(state, core.path());
        state
            .hunk_texts
            .iter()
            .filter(|h| {
                let (a, b) = h.new_range();
                h.path == path && core.start_line() <= b && core.end_line() >= a
            })
            .collect()
    };
    let mut untested_definitions = 0usize;

    // A hunk inside a function body seeds a nested fragment — a `let`, an
    // inner block — that nobody calls; the symbol a reader knows, and the one
    // the callers link to, is the named container around it.
    let mut cores: Vec<&Fragment> = state
        .core_ids
        .iter()
        .filter_map(|id| by_id.get(id).copied())
        .filter_map(|f| named_container_of(f, &state.all_fragments))
        .collect();
    cores.sort_by(|a, b| a.id.cmp(&b.id));
    cores.dedup_by(|a, b| a.id == b.id);

    let analysed = state.analysed_range.as_deref().or(diff_range);
    let bounds = commit_bounds(analysed).filter(|_| state.commit_count >= 2);
    let base = base_revision(analysed);

    let mut changed: Vec<ChangedSymbol> = Vec::new();
    // Schema changes first, then the public symbols something calls, then
    // the rest: what the cap keeps is what a reader would look at first.
    let mut contracts: Vec<Contract> = Vec::new();
    let mut public_api: Vec<(bool, Contract)> = Vec::new();
    // Definitions are counted per language: the builders link a call to the
    // definitions of its own language, so a Python wrapper named like a Rust
    // function is not what makes the Rust name ambiguous. An `impl` block
    // extends the type it names rather than competing with it: a struct with
    // a `Display` and an `Error` impl is one definition, not three.
    let mut definitions: FxHashMap<(&str, String), usize> = FxHashMap::default();
    let mut defined_in: FxHashSet<(String, &str)> = FxHashSet::default();
    let mut file_identifiers: FxHashMap<&str, FxHashSet<&str>> = FxHashMap::default();
    let mut file_fragments: FxHashMap<&str, Vec<&Fragment>> = FxHashMap::default();
    for f in &state.all_fragments {
        file_fragments.entry(f.path()).or_default().push(f);
        file_identifiers
            .entry(f.path())
            .or_default()
            .extend(f.identifiers.iter().map(String::as_str));
        if let Some(name) = f
            .symbol_name
            .as_deref()
            .filter(|_| f.kind.is_definition_kind() && f.kind != FragmentKind::Impl)
        {
            let lang = crate::languages::get_language_for_file(f.path()).unwrap_or("");
            *definitions.entry((lang, name.to_lowercase())).or_default() += 1;
            defined_in.insert((name.to_string(), f.path()));
        }
    }
    let files = Files {
        identifiers: file_identifiers,
        fragments: file_fragments,
    };
    let source = state.run.source();
    let universe: FxHashSet<String> = source
        .list_paths(&state.root_dir)
        .into_iter()
        .chain(state.all_fragments.iter().map(|f| rel(state, f.path())))
        .collect();
    // The directories that hold a project manifest: in a monorepo each
    // child is one, and a name two siblings both spell links nothing.
    let projects: FxHashSet<String> = universe
        .iter()
        .filter(|p| PROJECT_MANIFESTS.contains(&p.rsplit('/').next().unwrap_or(p.as_str())))
        .map(|p| p.rsplit_once('/').map_or("", |(dir, _)| dir).to_string())
        .collect();
    let project_of = |path: &str| -> String {
        let rel_path = rel(state, path);
        let mut dir = rel_path.as_str();
        while let Some((parent, _)) = dir.rsplit_once('/') {
            if projects.contains(parent) {
                return parent.to_string();
            }
            dir = parent;
        }
        String::new()
    };
    // A lexical link stops at a sibling project's manifest: a Python test
    // naming `config` is not a test of another project's `config.ts`. It
    // crosses when one language and a qualified name say the dependency is
    // real — a Maven `it/` module importing `com.acme.core.Pricing`, a
    // workspace crate writing `core_crate::pricing`.
    let one_project = |user: &Fragment, used: &Fragment| {
        let (pu, pd) = (project_of(user.path()), project_of(used.path()));
        let nests = |outer: &str, inner: &str| {
            outer.is_empty() || inner == outer || inner.starts_with(&format!("{outer}/"))
        };
        if nests(&pu, &pd) || nests(&pd, &pu) {
            return true;
        }
        let lang = crate::languages::get_language_for_file;
        let empty = FxHashSet::default();
        let words = files.identifiers.get(user.path()).unwrap_or(&empty);
        lang(user.path()).is_some()
            && lang(user.path()) == lang(used.path())
            && Path::new(&rel(state, used.path()))
                .parent()
                .into_iter()
                .flat_map(Path::components)
                .filter_map(|c| c.as_os_str().to_str())
                .map(str::to_lowercase)
                .filter(|d| d.len() >= 2 && !GENERIC_DIRECTORIES.contains(&d.as_str()))
                .any(|d| words.contains(d.as_str()))
    };
    let resolver = Resolver::new(&state.root_dir, &source, universe);
    let file_of = |f: &Fragment| files.fragments.get(f.path()).map_or(&[][..], Vec::as_slice);
    let class_of =
        |f: &Fragment| enclosing_class(f, file_of(f)).and_then(|c| c.symbol_name.clone());
    let finding = |user: &Fragment, used: &Fragment, name: &str| -> Option<Finding> {
        // A function defined inside another one is a local name: only that
        // function can call it, never through a receiver. An enum, a record
        // or a module around a method is no such scope (`Op.PLUS.apply`).
        let scope = file_of(used).iter().find(|f| {
            f.id != used.id
                && f.kind == FragmentKind::Function
                && f.start_line() < used.start_line()
                && f.end_line() >= used.end_line()
        });
        if let Some(scope) = scope {
            if user.path() != used.path()
                || user.start_line() < scope.start_line()
                || user.end_line() > scope.end_line()
            {
                return None;
            }
        }
        let path = rel(state, used.path());
        let class = if scope.is_none() && is_member(used, file_of(used)) {
            class_of(used)
        } else {
            None
        };
        let def = Definition {
            path: &path,
            name,
            class: class.as_deref(),
        };
        let user_path = rel(state, user.path());
        let header = enclosing_class(user, file_of(user)).and_then(|c| {
            c.symbol_name
                .as_deref()
                .map(|n| (n, c.content.lines().next().unwrap_or("")))
        });
        resolver.classify(
            &Site {
                path: &user_path,
                content: &user.content,
                start_line: user.start_line(),
                class: header,
            },
            &def,
        )
    };
    let modelled = |a: &Fragment, b: &Fragment| {
        bindings::lang_of(a.path()).is_some()
            && bindings::lang_of(a.path()) == bindings::lang_of(b.path())
    };
    let link = |user: &Fragment, used: &Fragment| -> bool {
        let Some(name) = used.symbol_name.as_deref().map(base_symbol) else {
            return false;
        };
        if modelled(user, used) {
            finding(user, used, name).is_some_and(|f| f.evidence == Evidence::Resolved)
        } else {
            one_project(user, used)
                && mentions(user, Some(name), &Receiver::Any)
                && references_module(user, &files, Some(name), used.path())
        }
    };
    let reaches_file = |test: &Fragment, caller: &Fragment| -> bool {
        if modelled(test, caller) {
            resolver.imports_file(&rel(state, test.path()), &rel(state, caller.path()))
        } else {
            one_project(test, caller) && references_module(test, &files, None, caller.path())
        }
    };
    let cores = innermost(cores);
    // An `impl` block extends the type it names: with that type among the
    // cores, a second entry for the block repeats the type (#403).
    let types: FxHashSet<(&str, &str)> = cores
        .iter()
        .filter(|c| c.kind != FragmentKind::Impl)
        .filter_map(|c| Some((c.path(), c.symbol_name.as_deref()?)))
        .collect();
    let cores: Vec<&Fragment> = cores
        .into_iter()
        .filter(|c| {
            c.kind != FragmentKind::Impl
                || !c
                    .symbol_name
                    .as_deref()
                    .is_some_and(|s| types.contains(&(c.path(), s)))
        })
        .collect();
    // A deletion that only borders a definition removed whatever followed
    // it, at its own indentation or shallower: the definitions it took are
    // reported as removed, this one did not change — and it is no part of
    // the diff when it calls another changed symbol. Lines removed between
    // two lines the definition spans are its own body, at any indentation
    // (a dedented SQL string). A core no hunk overlaps was attached to a
    // deletion beside it: the hunks name its file, never its lines.
    let borders_only = |core: &Fragment| -> bool {
        let core_hunks = hunks_in(core);
        if core_hunks.is_empty() {
            let core_path = rel(state, core.path());
            return state.hunk_texts.iter().any(|h| h.path == core_path);
        }
        let header_indent = indent_of(without_decorators(&core.content));
        core_hunks.iter().all(|h| {
            let interior = core.start_line() <= h.new_start && core.end_line() > h.new_start;
            h.added.is_empty()
                && !interior
                && h.removed
                    .iter()
                    .find(|l| !l.trim().is_empty())
                    .is_some_and(|l| indent_of(l) <= header_indent)
        })
    };
    let bordering: FxHashSet<&FragmentId> = cores
        .iter()
        .filter(|c| borders_only(c))
        .map(|c| &c.id)
        .collect();
    for (i, core) in cores.iter().enumerate() {
        // Past the deadline the symbols already answered are the answer:
        // `limits` then says it is partial, and a mass reformat returns what
        // it found instead of nothing (#382).
        if !state.run.check() {
            break;
        }
        let core_hunks = hunks_in(core);
        // A formatter run or a comment changes nothing a caller sees (#370).
        if query.is_none() && !core_hunks.is_empty() && core_hunks.iter().all(|h| h.is_layout()) {
            continue;
        }
        if query.is_none() && bordering.contains(&core.id) {
            continue;
        }
        let compatible = query.is_none() && compatible_extension(core, &core_hunks);
        let symbol = core.symbol_name.as_deref();
        let member = is_member(core, file_of(core));
        let stem = Path::new(core.path())
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        // `build`, `new`, `run`: a name defined all over the repository links
        // by name to every definition, and its "callers" would be everyone's.
        let core_lang = crate::languages::get_language_for_file(core.path()).unwrap_or("");
        // With a binding model, imports decide which definition a reference
        // reaches; a name defined in many files is ambiguous only without one.
        let binding_model = symbol.is_some() && bindings::lang_of(core.path()).is_some();
        // A qualified query (`path:name`) names its definition; only a bare
        // name or a diff leaves the lexical model guessing among many.
        let qualified = query.is_some_and(|q| q.contains(':'));
        let ambiguous = !binding_model
            && !qualified
            && symbol
                .map(|s| {
                    definitions
                        .get(&(core_lang, s.to_lowercase()))
                        .copied()
                        .unwrap_or(0)
                        > MAX_DEFINITIONS
                })
                .unwrap_or(false);
        let mut sources: Vec<(&Fragment, f64, Option<Finding>)> = Vec::new();
        // Why a linked fragment is not listed, per core, under RUST_LOG=debug:
        // the precision filters are the part of this view that field reports
        // question first.
        let mut dropped: FxHashMap<&'static str, usize> = FxHashMap::default();
        graph.for_each_reverse_neighbor(&core.id, |src, weight| {
            let reason = if ambiguous {
                "ambiguous_name"
            } else if (state.core_ids.contains(src) && !bordering.contains(src))
                || within(src, core)
                || (query.is_none() && in_a_hunk(src))
            {
                // A queried name has no diff: a call from elsewhere in its
                // own file is a caller like any other.
                "inside_the_diff"
            } else if let Some((frag, found)) = by_id
                .get(src)
                .filter(|f| {
                    binding_model
                        && modelled(f, core)
                        && f.kind != FragmentKind::Excerpt
                        && !is_import_block(&f.content)
                })
                .and_then(|f| {
                    finding(f, core, symbol.map(base_symbol).unwrap_or(""))
                        .filter(|x| x.evidence == Evidence::Resolved)
                        .map(|x| (*f, x))
                })
            {
                // A resolved binding is stronger evidence than which way the
                // edge between the two fragments happens to weigh more.
                sources.push((frag, weight, Some(found)));
                return;
            } else if graph.edge_category(src, &core.id) != Some(EdgeCategory::Semantic) {
                tracing::trace!(
                    "impact {}: {} -> {:?} {:?} w={:.2} back={:.2}",
                    symbol.unwrap_or("?"),
                    rel(state, src.path.as_ref()),
                    graph.edge_category(src, &core.id),
                    (src.start_line, src.end_line),
                    weight,
                    graph.forward_edge_weight(&core.id, src).unwrap_or(0.0)
                );
                "not_semantic"
            } else if !graph.is_forward(src, &core.id) {
                tracing::trace!(
                    "impact {}: {} {:?} not forward w={:.2} back={:.2}",
                    symbol.unwrap_or("?"),
                    rel(state, src.path.as_ref()),
                    (src.start_line, src.end_line),
                    weight,
                    graph.forward_edge_weight(&core.id, src).unwrap_or(0.0)
                );
                "not_forward"
            } else {
                let Some(frag) = by_id.get(src) else {
                    *dropped.entry("unknown_fragment").or_default() += 1;
                    return;
                };
                // A file that defines the same name calls its own.
                let shadowed = frag.path() != core.path()
                    && symbol.is_some_and(|s| defined_in.contains(&(s.to_string(), frag.path())));
                if frag.kind == FragmentKind::Excerpt {
                    "excerpt"
                } else if shadowed && !binding_model {
                    "defines_same_name"
                } else if is_import_block(&frag.content) {
                    "import_block"
                } else if !is_code_file(frag.path()) {
                    "not_code"
                } else if binding_model && modelled(frag, core) {
                    match finding(frag, core, symbol.map(base_symbol).unwrap_or("")) {
                        Some(f) => {
                            sources.push((frag, weight, Some(f)));
                            return;
                        }
                        None => "binding_rejected",
                    }
                } else if !{
                    let aliases = if member {
                        Vec::new()
                    } else {
                        files
                            .fragments
                            .get(frag.path())
                            .map(|f| module_aliases(f, stem))
                            .unwrap_or_default()
                    };
                    let receiver = if member {
                        Receiver::Any
                    } else {
                        Receiver::Module {
                            stem,
                            aliases: &aliases,
                        }
                    };
                    mentions(frag, symbol, &receiver)
                } {
                    "no_mention_in_code"
                } else if !references_module(frag, &files, symbol, core.path()) {
                    "no_module_reference"
                } else {
                    sources.push((frag, weight, None));
                    return;
                }
            };
            *dropped.entry(reason).or_default() += 1;
        });
        if !dropped.is_empty() {
            tracing::debug!(
                "impact {}::{}: kept {}, dropped {:?}",
                rel(state, core.path()),
                symbol.unwrap_or("?"),
                sources.len(),
                dropped
            );
        }
        let weights: FxHashMap<&FragmentId, f64> =
            sources.iter().map(|(f, w, _)| (&f.id, *w)).collect();
        let findings: FxHashMap<&FragmentId, Finding> = sources
            .iter()
            .filter_map(|(f, _, finding)| finding.clone().map(|x| (&f.id, x)))
            .collect();
        // An import line links too, at the import weight; the function that
        // makes the call is the caller, the import is how it got the name.
        let named_files: FxHashSet<&str> = sources
            .iter()
            .filter(|(f, _, _)| f.symbol_name.is_some())
            .map(|(f, _, _)| f.path())
            .collect();
        sources.retain(|(f, _, _)| f.symbol_name.is_some() || !named_files.contains(f.path()));
        let callers: Vec<Caller> = innermost(sources.iter().map(|(f, _, _)| *f).collect())
            .into_iter()
            .map(|frag| {
                let tested = tests.tested_by(frag, state, &by_id, &link);
                let found = findings.get(&frag.id);
                let registered_by = tested
                    .is_none()
                    .then(|| registering_decorator(frag, file_of(frag)))
                    .flatten();
                Caller {
                    path: rel(state, frag.path()),
                    symbol: frag.symbol_name.clone(),
                    lines: lines_of(&frag.id),
                    weight: (weights[&frag.id] * 1e3).round() / 1e3,
                    file_tested: tested.is_none()
                        && registered_by.is_none()
                        && tests.file_tested(frag, &reaches_file),
                    registered_by,
                    tested_by: tested.as_ref().map(|(p, _)| rel(state, p)),
                    tested_via: tested.and_then(|(_, via)| via),
                    evidence: match found.map(|f| f.evidence) {
                        Some(Evidence::Resolved) => "resolved",
                        Some(_) => "candidate",
                        None => "name",
                    },
                    reason: found.and_then(|f| match f.evidence {
                        Evidence::Candidate(r) => Some(r),
                        _ => None,
                    }),
                    relation: match found.map(|f| f.relation) {
                        Some(Relation::Reference) => "reference",
                        _ => "call",
                    },
                    sites: found.map(|f| f.lines.clone()).unwrap_or_default(),
                }
            })
            .collect();
        let mut callers = merge_by_symbol(callers);
        callers.sort_by(|a, b| {
            b.weight
                .total_cmp(&a.weight)
                .then_with(|| a.path.cmp(&b.path))
                .then_with(|| a.lines.cmp(&b.lines))
        });
        let in_time = deadline.is_none_or(|d| Instant::now() < d);
        let commits = match (&bounds, i < MAX_BLAMED_SYMBOLS && in_time) {
            (Some((base, head)), true) => commits_touching(state, base, head, core),
            _ => 0,
        };
        if query.is_none()
            && is_public(core.path(), symbol, without_decorators(&core.content))
            && rust_module_is_public(state, core.path())
            && signature_changed(state, &base, core)
        {
            public_api.push((
                callers.is_empty(),
                Contract {
                    path: rel(state, core.path()),
                    symbol: core.symbol_name.clone(),
                    kind: "public_api",
                },
            ));
        }
        // A changed symbol nothing depends on is the diff itself; the reader
        // holds that already, and listing it spends the cap on nothing.
        // A queried symbol's definition is half the answer: where it is.
        // Unless nothing could be resolved: an empty list is then no finding,
        // and silence would read as one. A trait method is reached through
        // its trait, whatever else shares its name (`fmt` in `impl Display`).
        let unresolved = dispatched_trait(state, core)
            .map(|t| {
                format!(
                    "implements `{t}`; calls through `dyn {t}` or a generic bound are not resolved"
                )
            })
            .or_else(|| {
                symbol.filter(|_| ambiguous).map(|s| {
                    let n = definitions
                        .get(&(core_lang, s.to_lowercase()))
                        .copied()
                        .unwrap_or(0);
                    format!("`{s}` has {n} {core_lang} definitions and this language has no binding model here")
                })
            });
        let untested = query.is_none()
            && tests.any()
            && !crate::testfiles::is_test_path(Path::new(core.path()))
            && registering_decorator(core, file_of(core)).is_none()
            && tests.tested_by(core, state, &by_id, &link).is_none();
        untested_definitions += usize::from(untested);
        let compatible_callers = if compatible {
            let n = callers.iter().filter(|c| c.evidence != "candidate").count();
            callers.clear();
            n
        } else {
            0
        };
        if callers.is_empty()
            && compatible_callers == 0
            && commits < 2
            && query.is_none()
            && unresolved.is_none()
        {
            continue;
        }
        changed.push(ChangedSymbol {
            path: rel(state, core.path()),
            symbol: core.symbol_name.clone(),
            lines: lines_of(&core.id),
            kind: format!("{:?}", core.kind).to_lowercase(),
            commits,
            callers,
            unresolved,
            untested,
            compatible_callers,
        });
    }
    if query.is_none() && state.run.check() {
        let mut defined: FxHashMap<&str, FxHashSet<String>> = FxHashMap::default();
        for (n, p) in &defined_in {
            defined.entry(n.as_str()).or_default().insert(rel(state, p));
        }
        let head_rev = analysed
            .map(crate::git::split_diff_range)
            .and_then(|(_, head)| head);
        let binds_to = |file: &str, text: &str, name: &str, old: &str| {
            let is_old = |spec: &str| matches!(resolver.resolve(file, spec), bindings::Resolution::File(f) if f == old);
            let b = resolver.bindings(file);
            b.star_from.iter().any(|m| is_old(m))
                || b.names.iter().any(|(local, t)| match t {
                    bindings::Target::Member { module, name: n } => n == name && is_old(module),
                    bindings::Target::Module(m) => {
                        text.contains(&format!("{local}.{name}")) && is_old(m)
                    }
                })
        };
        for (sym, contract) in
            removed_definitions(state, &defined, head_rev.as_deref(), &in_a_hunk, &binds_to)
        {
            if let Some(c) = contract {
                contracts.push(c);
            }
            if !sym.callers.is_empty() {
                changed.push(sym);
            }
        }
    }
    // Symbols with something to say first: callers, then commit overlap.
    changed.sort_by(|a, b| {
        b.callers
            .len()
            .cmp(&a.callers.len())
            .then_with(|| b.commits.cmp(&a.commits))
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.lines.cmp(&b.lines))
    });

    let changed_files: Vec<String> = state
        .changed_files
        .iter()
        .map(|p| rel(state, &p.to_string_lossy()))
        .collect();
    let layout_files: FxHashSet<&str> = state
        .change_classes
        .iter()
        .filter(|(_, class, _)| *class == crate::change_class::ChangeClass::Layout)
        .map(|(p, _, _)| p.as_str())
        .collect();
    for path in &changed_files {
        if is_schema_file(path) && !layout_files.contains(path.as_str()) {
            contracts.push(Contract {
                path: path.clone(),
                symbol: None,
                kind: "schema",
            });
        }
    }

    public_api.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.path.cmp(&b.1.path)));
    contracts.extend(public_api.into_iter().map(|(_, c)| c));
    let contracts_omitted = contracts.len().saturating_sub(MAX_CONTRACTS);
    contracts.truncate(MAX_CONTRACTS);
    let limits: Vec<crate::resource::LimitReason> = state
        .run
        .reasons()
        .into_iter()
        .filter(|r| CALLER_LOSING_LIMITS.contains(r))
        .collect();
    let (stale, stale_omitted) = if query.is_none() {
        stale_references(state, analysed)
    } else {
        (Vec::new(), 0)
    };
    let empty = changed.is_empty() && contracts.is_empty() && limits.is_empty() && stale.is_empty();
    let mut output = ImpactOutput {
        schema: IMPACT_SCHEMA,
        name: state
            .root_dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| state.root_dir.to_string_lossy().to_string()),
        range: diff_range.map(str::to_string),
        index_tree: state.index_tree.clone(),
        symbol: query.map(str::to_string),
        changed_files,
        deleted_files: state.deleted_files.clone(),
        stale_references: stale,
        stale_references_omitted: stale_omitted,
        commit_count: state.commit_count,
        changed,
        contracts,
        empty,
        truncated: 0,
        changed_files_omitted: 0,
        contracts_omitted,
        limits,
        tests_known: tests.any(),
        untested_definitions,
        withheld_files: state.policy_excluded_count,
        cap: IMPACT_TOKEN_CAP,
    };
    fit_to_cap(&mut output);
    output
}

/// A declaration a removed line spells: `function f(`, `def f(`, `fn f(`,
/// `class C`, `interface I`, `func (r T) f(`, `def self.f`, `const f = () =>`.
static REMOVED_DECLARATION: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(
    || {
        regex::Regex::new(
        r"^\s*((?:(?:export|default|pub(?:\([^)]*\))?|public|private|protected|static|async|abstract|readonly|override|declare)\s+)*)(?:(?:function\*?|def|fn|func|class|interface|type|enum|struct|trait)\s+(?:\([^)]*\)\s*)?(?:self\.)?([A-Za-z_$][\w$]*)|(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*(?::[^=]+)?=\s*(?:async\s+)?(?:function\b|\([^)]*\)\s*(?::[^=]*)?=>|[A-Za-z_$][\w$]*\s*=>))",
    )
    .expect("removed declaration regex")
    },
);

/// A member a removed line declares inside a class or an interface:
/// `m?(): void;`, `m(x: T) {`, `async m() {`.
static REMOVED_MEMBER: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
    regex::Regex::new(
        r"^\s*(?:(?:public|private|protected|static|async|abstract|readonly|override|get|set)\s+)*([A-Za-z_$][\w$]*)\??\s*(?:<[^>]*>)?\s*\([^)]*\)\s*(?::[^;{=]*)?\s*[;{]?\s*$",
    )
    .expect("removed member regex")
});

/// The member a class-body line declares. `notify(x);` with no modifier
/// and no return type is a call statement inside a method body, not a
/// declaration: read as one, deleting the call reported a removed API.
fn member_declared(line: &str) -> Option<&str> {
    let name = REMOVED_MEMBER.captures(line)?.get(1)?.as_str();
    let code = line.trim();
    let modified = code
        .split_whitespace()
        .next()
        .is_some_and(|w| MEMBER_MODIFIERS.contains(&w));
    let call = code
        .strip_suffix(';')
        .is_some_and(|c| c.trim_end().ends_with(')'));
    (modified || !call).then_some(name)
}

const MEMBER_MODIFIERS: &[&str] = &[
    "public",
    "private",
    "protected",
    "static",
    "async",
    "abstract",
    "readonly",
    "override",
    "declare",
    "get",
    "set",
];

/// Directory names every project has: naming one says nothing about which.
const GENERIC_DIRECTORIES: &[&str] = &[
    "src", "main", "java", "kotlin", "scala", "lib", "test", "tests", "app", "pkg", "internal",
    "cmd", "include", "source", "sources",
];

const NOT_MEMBER_NAMES: &[&str] = &[
    "if",
    "for",
    "while",
    "switch",
    "catch",
    "return",
    "function",
    "super",
    "constructor",
    "await",
    "new",
    "typeof",
    "self",
    "cls",
    "this",
];

fn declared_name(line: &str) -> Option<(&str, bool)> {
    let c = REMOVED_DECLARATION.captures(line)?;
    let prefix = c.get(1).map_or("", |m| m.as_str());
    let name = c.get(2).or_else(|| c.get(3))?.as_str();
    Some((
        name,
        prefix.contains("export") || prefix.contains("pub ") || prefix.trim() == "pub",
    ))
}

/// Definitions the change removed and nothing at the head defines again,
/// with the lines outside the diff that still name them (#367). They have
/// no fragment at the head, so the callers graph never reaches them: an
/// optional call (`engine.setGain?.()`) silently becomes a no-op.
fn removed_definitions(
    state: &ScoredState,
    defined: &FxHashMap<&str, FxHashSet<String>>,
    head: Option<&str>,
    in_a_hunk: &dyn Fn(&FragmentId) -> bool,
    binds_to: &dyn Fn(&str, &str, &str, &str) -> bool,
) -> Vec<(ChangedSymbol, Option<Contract>)> {
    let mut removed: Vec<(String, &crate::change_class::HunkText, bool)> = Vec::new();
    for h in &state.hunk_texts {
        if !is_code_file(&h.path) {
            continue;
        }
        let (anchor, _) = h.new_range();
        let classes: Vec<&Fragment> = state
            .all_fragments
            .iter()
            .filter(|f| {
                f.kind.is_container() && f.start_line() <= anchor && rel(state, f.path()) == h.path
            })
            .collect();
        // As in `enclosing_class`: a class may be fragmented as its header
        // alone, so an indented line belongs to the nearest shallower class
        // above it.
        let container_of = |line: &str| {
            let own = indent_of(line);
            classes
                .iter()
                .copied()
                .filter(|c| c.end_line() >= anchor)
                .min_by_key(|c| c.line_count())
                .or_else(|| {
                    (own > 0)
                        .then(|| {
                            classes
                                .iter()
                                .copied()
                                .filter(|c| indent_of(&c.content) < own)
                                .max_by_key(|c| c.start_line())
                        })
                        .flatten()
                })
        };
        let readded: FxHashSet<String> = h
            .added
            .iter()
            .filter_map(|l| {
                declared_name(l)
                    .map(|(n, _)| n)
                    .or_else(|| container_of(l).and_then(|_| member_declared(l)))
                    .map(str::to_string)
            })
            .collect();
        for line in &h.removed {
            let container = container_of(line);
            let (name, exported) = if let Some((n, exported)) = declared_name(line) {
                (Some(n), exported)
            } else if let Some(n) = container.and_then(|_| member_declared(line)) {
                let exported = container.is_some_and(|f| {
                    is_public(
                        f.path(),
                        f.symbol_name.as_deref(),
                        without_decorators(&f.content),
                    )
                });
                (Some(n), exported)
            } else {
                (None, false)
            };
            let Some(name) = name.filter(|n| {
                n.len() >= 3
                    && !NOT_MEMBER_NAMES.contains(n)
                    && !defined.get(n).is_some_and(|at| at.contains(&h.path))
                    && !readded.contains(*n)
            }) else {
                continue;
            };
            if !removed.iter().any(|(n, _, _)| n == name) {
                removed.push((name.to_string(), h, exported));
            }
        }
    }
    let mut out = Vec::new();
    for (name, h, exported) in removed {
        let Ok(hits) = crate::git::grep_lines(&state.root_dir, std::slice::from_ref(&name), head)
        else {
            continue;
        };
        let mut callers: Vec<Caller> = Vec::new();
        // Defined again elsewhere: only the lines still bound to the old
        // module are broken; the rest already reach the new definition.
        // Without a binding model only a line qualified by the new module
        // (`new::tax_rate`) is known to reach it; dropping every other line
        // read a still-broken `old::tax_rate()` as fixed.
        let moved_to = defined.get(name.as_str()).filter(|at| !at.is_empty());
        let qualified_by = |text: &str, file: &str| {
            let stem = Path::new(file)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            text.contains(&format!("{stem}::{name}")) || text.contains(&format!("{stem}.{name}"))
        };
        for (path, line, text) in hits {
            if !is_code_file(&path) {
                continue;
            }
            if let Some(at) = moved_to {
                let reaches_new = if bindings::lang_of(&path).is_some() {
                    !binds_to(&path, &text, &name, &h.path)
                } else {
                    at.iter().any(|f| qualified_by(&text, f)) && !qualified_by(&text, &h.path)
                };
                if reaches_new {
                    continue;
                }
            }
            let ext = Path::new(&path)
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            let code = code_of(
                &text,
                HASH_COMMENT_EXTENSIONS.contains(&ext.as_str()),
                ext != "rs",
                &mut None,
            );
            let bytes = code.as_bytes();
            let Some(at) = code
                .match_indices(name.as_str())
                .map(|(at, _)| at)
                .find(|&at| {
                    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'$';
                    !at.checked_sub(1).is_some_and(|i| word(bytes[i]))
                        && !bytes.get(at + name.len()).copied().is_some_and(word)
                })
            else {
                continue;
            };
            let holder = state
                .all_fragments
                .iter()
                .filter(|f| {
                    rel(state, f.path()) == path
                        && f.start_line() <= line
                        && f.end_line() >= line
                        && f.kind.is_definition_kind()
                        && f.symbol_name.is_some()
                })
                .min_by_key(|f| f.line_count());
            // A line declaring the name is where it went (a move), never a
            // caller of the removed definition; a method of the same name
            // that delegates to it still is one.
            if holder.is_some_and(|f| in_a_hunk(&f.id) && f.line_count() <= 1)
                || declared_name(&text).is_some_and(|(n, _)| n == name)
            {
                continue;
            }
            let next = code[at + name.len()..].trim_start();
            callers.push(Caller {
                path: path.clone(),
                symbol: holder.and_then(|f| f.symbol_name.clone()),
                lines: holder.map_or_else(|| format!("{line}-{line}"), |f| lines_of(&f.id)),
                weight: 1.0,
                tested_by: None,
                tested_via: None,
                file_tested: false,
                registered_by: None,
                evidence: "name",
                reason: None,
                relation: if next.starts_with('(') || next.starts_with("?.(") {
                    "call"
                } else {
                    "reference"
                },
                sites: vec![line],
            });
        }
        let callers = merge_by_symbol(callers);
        let lines = format!(
            "{}-{}",
            h.old_start,
            h.old_start + h.old_len.saturating_sub(1)
        );
        let contract = exported.then(|| Contract {
            path: h.path.clone(),
            symbol: Some(name.clone()),
            kind: "removed_api",
        });
        out.push((
            ChangedSymbol {
                path: h.path.clone(),
                symbol: Some(name),
                lines,
                kind: "removed".to_string(),
                commits: 0,
                callers,
                unresolved: None,
                untested: false,
                compatible_callers: 0,
            },
            contract,
        ));
    }
    out
}

const PROJECT_MANIFESTS: &[&str] = &[
    "package.json",
    "pyproject.toml",
    "setup.py",
    "Cargo.toml",
    "go.mod",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "composer.json",
    "Gemfile",
    "mix.exs",
    "deno.json",
];

const MAX_STALE_REFERENCES: usize = 24;
const MAX_STALE_PER_TARGET: usize = 8;

/// How a line can name a path: the strongest first, so a line naming two
/// targets is attributed to the one it names most exactly.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Naming {
    FullPath,
    RelativePath,
    Module,
    UniqueBasename,
}

fn normalize_rel(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    parts.join("/")
}

fn without_extension(path: &str) -> &str {
    match path.rfind('.') {
        Some(dot) if !path[dot..].contains('/') && dot > path.rfind('/').map_or(0, |s| s + 1) => {
            &path[..dot]
        }
        _ => path,
    }
}

/// How `token`, found in `referrer`, names `target` — or `None` when it is a
/// different name that merely shares a basename or a word (#369, #383).
fn naming(token: &str, referrer: &str, target: &str, unique_basename: bool) -> Option<Naming> {
    let trimmed = token.trim_start_matches("./");
    if trimmed == target || token.ends_with(&format!("/{target}")) {
        return Some(Naming::FullPath);
    }
    let dir = referrer.rsplit_once('/').map_or("", |(d, _)| d);
    if token.contains('/') {
        let joined = normalize_rel(&format!("{dir}/{token}"));
        if joined == target || joined == without_extension(target) {
            return Some(Naming::RelativePath);
        }
    }
    // A Python relative import: `.probes` is the sibling module, each
    // further dot one package up.
    let dots = token.chars().take_while(|c| *c == '.').count();
    if dots > 0 && !token.contains('/') {
        let rest = &token[dots..];
        let mut base = dir.to_string();
        for _ in 1..dots {
            base = base
                .rsplit_once('/')
                .map_or(String::new(), |(d, _)| d.to_string());
        }
        let module = normalize_rel(&format!("{base}/{}", rest.replace('.', "/")));
        let target_module = without_extension(target);
        if !rest.is_empty()
            && (module == target_module || format!("{module}/__init__") == target_module)
        {
            return Some(Naming::RelativePath);
        }
    }
    // `pkg.probes` / `pkg/probes`: a module path of at least two segments
    // that the target's path ends with.
    let module = without_extension(target);
    let segments: Vec<&str> = module.split('/').collect();
    for take in 2..=segments.len() {
        let tail = &segments[segments.len() - take..];
        let dotted = tail.join(".");
        let slashed = tail.join("/");
        if token == dotted || token.starts_with(&format!("{dotted}.")) || trimmed == slashed {
            return Some(Naming::Module);
        }
    }
    let basename = target.rsplit('/').next().unwrap_or(target);
    (unique_basename && basename.contains('.') && trimmed == basename)
        .then_some(Naming::UniqueBasename)
}

/// What still names a path the change deleted or renamed, in the tree the
/// change leaves behind: the range's head, or the working tree.
fn stale_references(state: &ScoredState, diff_range: Option<&str>) -> (Vec<StaleReference>, usize) {
    let mut targets: Vec<(String, String)> = state
        .deleted_files
        .iter()
        .map(|d| (d.clone(), "deleted".to_string()))
        .collect();
    targets.extend(
        state
            .renamed_files
            .iter()
            .map(|(old, new)| (old.clone(), format!("renamed to {new}"))),
    );
    if targets.is_empty() {
        return (Vec::new(), 0);
    }
    let head = diff_range
        .map(crate::git::split_diff_range)
        .and_then(|(_, head)| head);
    let surviving: FxHashSet<String> = state
        .run
        .source()
        .list_paths(&state.root_dir)
        .into_iter()
        .filter_map(|p| p.rsplit('/').next().map(str::to_string))
        .collect();
    let mut needles: Vec<String> = Vec::new();
    for (target, _) in &targets {
        needles.push(target.clone());
        if let Some(name) = Path::new(target).file_name().and_then(|n| n.to_str()) {
            needles.push(name.to_string());
        }
        // A module is named without its extension (`from pkg.probes import`).
        if is_code_file(target) {
            needles.extend(
                Path::new(target)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(str::to_string),
            );
        }
    }
    needles.sort();
    needles.dedup();
    let Ok(lines) = crate::git::grep_lines(&state.root_dir, &needles, head.as_deref()) else {
        return (Vec::new(), 0);
    };
    let renamed_to: FxHashSet<&str> = state
        .renamed_files
        .iter()
        .map(|(_, new)| new.as_str())
        .collect();
    let token_re = once_cell::sync::Lazy::force(&PATH_TOKEN);
    let mut found = Vec::new();
    let mut per_target: FxHashMap<&str, usize> = FxHashMap::default();
    let mut omitted = 0;
    for (path, line, text) in &lines {
        if renamed_to.contains(path.as_str()) {
            continue;
        }
        let surviving = &surviving;
        let targets = &targets;
        let best = token_re
            .find_iter(text)
            .flat_map(|m| {
                targets
                    .iter()
                    .filter(|(t, _)| t != path)
                    .filter_map(move |(t, kind)| {
                        let basename = t.rsplit('/').next().unwrap_or(t);
                        naming(m.as_str(), path, t, !surviving.contains(basename))
                            .map(|n| (n, std::cmp::Reverse(t.len()), t, kind))
                    })
            })
            .min();
        let Some((_, _, target, kind)) = best else {
            continue;
        };
        let count = per_target.entry(target.as_str()).or_default();
        if *count >= MAX_STALE_PER_TARGET || found.len() >= MAX_STALE_REFERENCES {
            omitted += 1;
            continue;
        }
        *count += 1;
        found.push(StaleReference {
            path: path.clone(),
            line: *line,
            target: target.clone(),
            kind: kind.clone(),
        });
    }
    (found, omitted)
}

/// A run of characters a path or a module name is spelled with.
static PATH_TOKEN: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
    regex::Regex::new(r"[A-Za-z0-9_./@~$+-]*[A-Za-z0-9_$][A-Za-z0-9_./@~$+-]*")
        .expect("path token regex")
});

fn serialized_tokens(output: &ImpactOutput) -> u32 {
    serde_json::to_string(output)
        .map(|s| crate::tokenizer::count_tokens(&s))
        .unwrap_or(0)
}

/// Drops possible callers first, then the globally weakest caller, until
/// `measure` fits `cap`, then symbols that have nothing left to say. The cap
/// is on what the reader receives, so it is measured on the delivered text.
fn fit_with(output: &mut ImpactOutput, cap: u32, measure: &dyn Fn(&ImpactOutput) -> u32) {
    output.cap = cap;
    while measure(output) > cap && shed_one(output) {}
    output.empty = output.changed.is_empty()
        && output.contracts.is_empty()
        && output.stale_references.is_empty()
        && output.stale_references_omitted == 0
        && output.truncated == 0
        && output.contracts_omitted == 0
        && output.limits.is_empty();
}

/// One step of shedding, least needed first; `false` once nothing more can
/// go. Callers are the answer, so everything else yields to them first, and
/// the file list — which the reader holds as the diff — first of all.
fn shed_one(output: &mut ImpactOutput) -> bool {
    if output.changed_files.len() > MAX_CHANGED_FILES_LISTED {
        output.changed_files_omitted += output.changed_files.len() - MAX_CHANGED_FILES_LISTED;
        output.changed_files.truncate(MAX_CHANGED_FILES_LISTED);
        return true;
    }
    if output.contracts.pop().is_some() {
        output.contracts_omitted += 1;
        return true;
    }
    if output.stale_references.pop().is_some() {
        output.stale_references_omitted += 1;
        return true;
    }
    if let Some(sym) = output
        .changed
        .iter_mut()
        .find(|c| c.callers.iter().any(|x| x.evidence == "candidate"))
    {
        if let Some(i) = sym.callers.iter().rposition(|x| x.evidence == "candidate") {
            sym.callers.remove(i);
            output.truncated += 1;
            return true;
        }
    }
    // A symbol whose callers could not be resolved says so to the last:
    // dropping it would turn "unknown" into "nothing".
    if let Some(i) = output
        .changed
        .iter()
        .rposition(|c| c.callers.is_empty() && c.unresolved.is_none() && c.compatible_callers == 0)
    {
        output.changed.remove(i);
        return true;
    }
    shed_weakest_caller(output)
}

fn shed_weakest_caller(output: &mut ImpactOutput) -> bool {
    // A symbol's only caller goes last: while another symbol still has a
    // tail to shed, dropping a whole symbol would make the selection look
    // arbitrary, one function listed with all its callers and another
    // with none (#386).
    let weakest_among = |min_callers: usize| {
        output
            .changed
            .iter()
            .enumerate()
            .filter(|(_, c)| c.callers.len() >= min_callers)
            .min_by(|(_, a), (_, b)| {
                let wa = a.callers.last().map(|c| c.weight).unwrap_or(0.0);
                let wb = b.callers.last().map(|c| c.weight).unwrap_or(0.0);
                wa.total_cmp(&wb)
            })
            .map(|(i, _)| i)
    };
    let weakest = weakest_among(2).or_else(|| weakest_among(1));
    match weakest {
        // The strongest caller of a symbol is the fact; the symbol goes
        // whole once that is all it has left, never one caller at a time.
        Some(i) if output.changed[i].callers.len() > 1 => {
            output.changed[i].callers.pop();
            output.truncated += 1;
            true
        }
        Some(i) if output.changed[i].unresolved.is_none() => {
            output.truncated += output.changed[i].callers.len();
            output.changed.remove(i);
            true
        }
        _ => false,
    }
}

fn fit_to_cap(output: &mut ImpactOutput) {
    fit_with(output, IMPACT_TOKEN_CAP, &serialized_tokens);
}

/// The automatic (hook) answer: candidates counted, not listed, and the
/// whole text, limits and metadata included, within `cap` tokens.
pub fn render_automatic(output: &mut ImpactOutput, cap: u32) -> String {
    let measure = |o: &ImpactOutput| crate::tokenizer::count_tokens(&render(o, true));
    fit_with(output, cap, &measure);
    render(output, true)
}

/// `HEAD` is what the hook and an omitted MCP `diff_ref` analyse: the
/// uncommitted work, not the last commit. The staged changes name the index
/// tree they were captured as. The JSON keeps the range as given (#347).
fn range_label(range: Option<&str>, index_tree: Option<&str>) -> String {
    if let Some(tree) = index_tree {
        return format!(
            "staged changes (index tree {})",
            &tree[..tree.len().min(10)]
        );
    }
    match range {
        None | Some("HEAD") => "uncommitted changes".to_string(),
        Some(r) => r.to_string(),
    }
}

fn label(path: &str, symbol: Option<&str>, lines: &str) -> String {
    match symbol {
        Some(s) => format!("{path}::{s} ({lines})"),
        None => format!("{path}:{lines}"),
    }
}

/// The hook's and the CLI's text form: one line per changed symbol, one per
/// caller beneath it, contracts last. Short enough to read before a commit.
pub fn render_markdown(output: &ImpactOutput) -> String {
    render(output, false)
}

fn render(output: &ImpactOutput, compact: bool) -> String {
    let mut out = String::new();
    write_header(&mut out, output);
    if output.empty && output.changed_files.is_empty() && output.deleted_files.is_empty() {
        // `HEAD` is the uncommitted work, so on a clean tree it reads as
        // "nothing is reached" while the last commit may reach a lot (#368).
        let working_tree = output.index_tree.is_none()
            && output.symbol.is_none()
            && matches!(output.range.as_deref(), None | Some("HEAD"));
        let _ = writeln!(
            out,
            "No changes to analyse.{}",
            if working_tree {
                " The working tree matches HEAD; HEAD~1..HEAD is the last commit."
            } else {
                ""
            }
        );
        return out;
    }
    if output.empty {
        let _ = writeln!(
            out,
            "No resolved static callers outside the diff in the analysed scope."
        );
        return out;
    }
    for sym in &output.changed {
        write_symbol(&mut out, sym, output.tests_known, compact);
    }
    write_tail(&mut out, output);
    out
}

/// A caller no static test link reaches although one could have: the one
/// predicate behind the header count, the folded count and the line (#386).
fn without_test_link(c: &Caller) -> bool {
    c.evidence != "candidate" && c.tested_by.is_none() && c.registered_by.is_none()
}

/// The caller count as far as it is known: symbols whose callers could not
/// be resolved, or a run that stopped early, make a zero no finding (#387).
fn callers_phrase(output: &ImpactOutput, callers: usize, scope: &str) -> String {
    let unresolved = output
        .changed
        .iter()
        .filter(|c| c.unresolved.is_some())
        .count();
    let mut phrase = if callers == 0 && unresolved > 0 && unresolved == output.changed.len() {
        "callers unresolved".to_string()
    } else {
        let mut p = format!("{callers} caller(s){scope}");
        if unresolved > 0 {
            p.push_str(&format!(", callers unresolved for {unresolved} symbol(s)"));
        }
        p
    };
    if !output.limits.is_empty() {
        phrase.push_str(" found before the run stopped");
    }
    phrase
}

fn write_header(out: &mut String, output: &ImpactOutput) {
    let range = range_label(output.range.as_deref(), output.index_tree.as_deref());
    let all = || output.changed.iter().flat_map(|c| &c.callers);
    let callers = all().filter(|c| c.evidence != "candidate").count();
    let possible = all().filter(|c| c.evidence == "candidate").count();
    let untested = all().filter(|c| without_test_link(c)).count();
    let mut guard_summary = if output.tests_known {
        format!(", {untested} without a static test link")
    } else {
        String::new()
    };
    if possible > 0 {
        guard_summary.push_str(&format!(", {possible} possible"));
    }
    if output.tests_known && output.untested_definitions > 0 {
        guard_summary.push_str(&format!(
            ", {} changed definition(s) without a static test link",
            output.untested_definitions
        ));
    }
    let withheld = |what: &str| {
        if output.withheld_files == 0 {
            String::new()
        } else {
            format!(
                ", {} {what} withheld by ignore rules",
                output.withheld_files
            )
        }
    };
    match &output.symbol {
        Some(symbol) => {
            let _ = writeln!(
                out,
                "diffctx impact for symbol {symbol}: {} definition(s){}, {}{guard_summary}",
                output.changed.len(),
                withheld("file(s) naming it"),
                callers_phrase(output, callers, ""),
            );
        }
        None => {
            let _ = writeln!(
                out,
                "diffctx impact for {range}: {} changed file(s){}{}, {}{guard_summary}",
                output.changed_files.len()
                    + output.changed_files_omitted
                    + output.deleted_files.len(),
                if output.deleted_files.is_empty() {
                    String::new()
                } else {
                    format!(" ({} deleted)", output.deleted_files.len())
                },
                withheld("more"),
                callers_phrase(output, callers, " outside the diff"),
            );
        }
    }
    if output.withheld_files > 0 {
        let _ = writeln!(
            out,
            "(the withheld files were not read: calls and tests in them are not counted)"
        );
    }
    if !output.limits.is_empty() {
        let reasons: Vec<String> = output
            .limits
            .iter()
            .map(|r| {
                serde_json::to_value(r)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default()
            })
            .collect();
        let _ = writeln!(
            out,
            "(partial: the run stopped at {}; callers may be missing)",
            reasons.join(", ")
        );
    }
}

/// Candidates shown per changed symbol before they are counted instead.
const MAX_CANDIDATES_SHOWN: usize = 3;

fn guard_of(c: &Caller, tests_known: bool) -> String {
    match (&c.tested_by, &c.tested_via) {
        (Some(t), Some(via)) => format!("reachable from tests: {t} via {via}"),
        (Some(t), None) => format!("reachable from tests: {t}"),
        (None, _) if c.registered_by.is_some() => format!(
            "registered by `@{}`; whatever reads that registry reaches it, which static test links do not follow",
            c.registered_by.as_deref().unwrap_or_default()
        ),
        (None, _) if c.file_tested => "no static test link (its file has tests)".to_string(),
        (None, _) if tests_known => "no static test link found".to_string(),
        (None, _) => "no test files in view".to_string(),
    }
}

fn reason_text(reason: Option<&str>) -> &'static str {
    match reason {
        Some("receiver_unresolved") => "receiver unresolved",
        Some("import_unresolved") => "import not resolved",
        Some("ambiguous_module") => "module ambiguous",
        _ => "binding unresolved",
    }
}

fn write_symbol(out: &mut String, sym: &ChangedSymbol, tests_known: bool, compact: bool) {
    let commits = if sym.commits >= 2 {
        format!(" [touched by {} commits]", sym.commits)
    } else {
        String::new()
    };
    let _ = writeln!(
        out,
        "- {}{commits}{}",
        label(&sym.path, sym.symbol.as_deref(), &sym.lines),
        if sym.untested && tests_known {
            " — no static test link"
        } else {
            ""
        }
    );
    if let Some(note) = &sym.unresolved {
        let _ = writeln!(out, "  - callers not resolved: {note}");
    }
    if sym.compatible_callers > 0 {
        let _ = writeln!(
            out,
            "  - {} call site(s), signature-compatible: only fields or parameters with defaults were added",
            sym.compatible_callers
        );
    }
    write_resolved_callers(out, sym, tests_known);
    write_candidate_callers(out, sym, compact);
}

fn write_resolved_callers(out: &mut String, sym: &ChangedSymbol, tests_known: bool) {
    // Thirty call sites in one test module are one fact, not thirty
    // lines: the first few name where, the count says how many.
    let mut shown_per_file: FxHashMap<&str, usize> = FxHashMap::default();
    let mut folded_per_file: FxHashMap<&str, (usize, usize)> = FxHashMap::default();
    for c in sym.callers.iter().filter(|c| c.evidence != "candidate") {
        let shown = shown_per_file.entry(c.path.as_str()).or_default();
        if *shown >= MAX_CALLERS_SHOWN_PER_FILE {
            let entry = folded_per_file.entry(c.path.as_str()).or_default();
            entry.0 += 1;
            if without_test_link(c) {
                entry.1 += 1;
            }
            continue;
        }
        *shown += 1;
        let verb = if c.relation == "reference" {
            "referenced from"
        } else {
            "called from"
        };
        let _ = writeln!(
            out,
            "  - {verb} {} — {}",
            label(&c.path, c.symbol.as_deref(), &c.lines),
            guard_of(c, tests_known)
        );
    }
    let mut folded: Vec<(&str, (usize, usize))> = folded_per_file.into_iter().collect();
    folded.sort();
    for (path, (more, untested)) in folded {
        let _ = writeln!(
            out,
            "  - and {more} more call site(s) in {path}{}",
            if untested > 0 && tests_known {
                format!(", {untested} without a static test link")
            } else {
                String::new()
            }
        );
    }
}

fn write_candidate_callers(out: &mut String, sym: &ChangedSymbol, compact: bool) {
    let candidates: Vec<&Caller> = sym
        .callers
        .iter()
        .filter(|c| c.evidence == "candidate")
        .collect();
    let shown = if compact { 0 } else { MAX_CANDIDATES_SHOWN };
    for c in candidates.iter().take(shown) {
        let _ = writeln!(
            out,
            "  - possibly called from {} ({})",
            label(&c.path, c.symbol.as_deref(), &c.lines),
            reason_text(c.reason)
        );
    }
    if candidates.len() > shown {
        let _ = writeln!(
            out,
            "  - and {} {}possible caller(s) whose binding is unresolved",
            candidates.len() - shown,
            if shown > 0 { "more " } else { "" }
        );
    }
}

fn write_tail(out: &mut String, output: &ImpactOutput) {
    if !output.stale_references.is_empty() {
        let _ = writeln!(out, "Still naming a deleted or renamed path:");
        for r in &output.stale_references {
            let _ = writeln!(
                out,
                "  - {}:{} names {} ({})",
                r.path, r.line, r.target, r.kind
            );
        }
    }
    if output.stale_references_omitted > 0 {
        let _ = writeln!(out, "  - and {} more", output.stale_references_omitted);
    }
    if !output.contracts.is_empty() {
        let _ = writeln!(out, "Contracts changed:");
        for c in &output.contracts {
            let _ = writeln!(
                out,
                "  - {} {}",
                c.kind,
                match &c.symbol {
                    Some(s) => format!("{}::{s}", c.path),
                    None => c.path.clone(),
                }
            );
        }
    }
    if output.contracts_omitted > 0 {
        let _ = writeln!(out, "  - and {} more", output.contracts_omitted);
    }
    if output.truncated > 0 {
        let _ = writeln!(
            out,
            "({} weaker caller(s) omitted to stay under {} tokens)",
            output.truncated, output.cap
        );
    }
    let callers = || output.changed.iter().flat_map(|c| &c.callers);
    if callers().any(|c| c.evidence == "name") {
        let _ = writeln!(
            out,
            "Callers outside Python and JS/TS are matched by name in code that names the module, not by a resolved binding."
        );
    }
    if callers().any(|c| c.tested_by.is_some()) {
        let _ = writeln!(
            out,
            "Test links are static references within {MAX_TEST_HOPS} call hops, not execution coverage."
        );
    }
}

/// JSON Schema 2020-12 for `diffctx.impact.v1`, pinned by `tests/context_schema.rs`.
pub fn impact_schema() -> serde_json::Value {
    let mut generator = schemars::generate::SchemaSettings::draft2020_12().into_generator();
    let schema = generator.root_schema_for::<ImpactOutput>();
    serde_json::to_value(schema).expect("schema serializes")
}
