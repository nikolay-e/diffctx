use std::fmt::Write as _;
use std::path::Path;
use std::time::Instant;

use rustc_hash::{FxHashMap, FxHashSet};
use schemars::JsonSchema;
use serde::Serialize;

use crate::bindings::{self, Evidence, Finding, Relation};
use crate::container::{innermost, named_container_of, within};
use crate::graph::EdgeCategory;
use crate::lang::{self, At, LanguageFacts, Registry, UseSite};
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
/// Paths listed before the rest fold into a count: the reader has the diff.
const MAX_CHANGED_FILES_LISTED: usize = 24;
/// Lines a removed name is read from per file: one caller per function is
/// the answer, and a flood of one common word is not.
const MAX_REMOVED_HITS_PER_FILE: usize = 20;
/// Past this many lines a removed name is a common word: unresolved, not
/// traced.
const MAX_REMOVED_NAME_HITS: usize = 500;
/// Unanalysed files kept when the answer must shrink; the rest are counted.
const MAX_UNANALYSED_LISTED: usize = 6;
/// Unanalysed paths the text names per language; the reader holds the diff.
const UNANALYSED_EXAMPLES: usize = 2;

/// A symbol name with more definitions than this cannot be traced by name.
const MAX_DEFINITIONS: usize = 2;

const SCHEMA_DIRECTORIES: &[&str] = &[
    "migrations",
    "migration",
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
    /// What the snapshot holds, when the caller knows it better than a tree
    /// id says — the hook's session edits, the index a commit records: the
    /// header names it (#549).
    #[serde(skip)]
    pub range_name: Option<String>,
    /// Set on a `--symbol` query (#336): `changed` then lists the symbol's
    /// definitions, and `changed_files` the files that hold them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    /// A `--symbol` name no file defines but files spell as a word: where,
    /// as text. `changed` is empty then (Forgejo #114).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol_named: Option<SymbolNamed>,
    /// What git tracks and the change modifies; untracked work is listed
    /// apart, in `untracked_files`.
    pub changed_files: Vec<String>,
    /// New files git does not track yet, analysed with the change (#418).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub untracked_files: Vec<String>,
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
    /// Lines outside the change that name a changed file by its path from
    /// another runtime or a config: a command line, a Dockerfile, a
    /// settings entry that runs it (#393, #523). Imports are callers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub named_by: Vec<PathReference>,
    #[serde(default, skip_serializing_if = "crate::render::is_zero")]
    pub named_by_omitted: usize,
    /// The changed files outside code whose changed keys were searched as
    /// text; the lines naming them are in `named_by` with their `key`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub key_searches: Vec<KeySearch>,
    /// Changed configs whose keys and values the capped search left out;
    /// they stay in `unanalysed_files`.
    #[serde(skip)]
    #[schemars(skip)]
    pub key_files_unsearched: usize,
    /// Lines outside the change that read a changed entry's Astro content
    /// collection by name: `getCollection('blog')` reads every entry under
    /// `src/content/blog/`, and no import or path leads there (#432).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub collection_reads: Vec<CollectionRead>,
    #[serde(default, skip_serializing_if = "crate::render::is_zero")]
    pub collection_reads_omitted: usize,
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
    /// Of `truncated`, the possible callers: the header counts every caller
    /// found, shown or shed, and keeps the possible ones apart (#413).
    #[serde(skip)]
    pub truncated_possible: usize,
    /// Symbols shed whole; the header's definition count still holds them.
    #[serde(skip)]
    pub symbols_omitted: usize,
    /// Of `truncated`, the callers no test reaches: a header that counts
    /// every caller found counts their test links the same way.
    #[serde(skip)]
    pub truncated_untested: usize,
    /// Of `truncated`, the word matches: they are listed as text, so the
    /// header keeps them out of the caller count.
    #[serde(skip)]
    pub truncated_textual: usize,
    /// Changed files left out of `changed_files` to fit the cap; the count
    /// is still whole. The reader holds the file list already.
    #[serde(default, skip_serializing_if = "crate::render::is_zero")]
    pub changed_files_omitted: usize,
    /// Untracked files left out of `untracked_files` to fit the same cap.
    #[serde(default, skip_serializing_if = "crate::render::is_zero")]
    pub untracked_files_omitted: usize,
    /// Contracts beyond `MAX_CONTRACTS`, schema changes never among them.
    #[serde(default, skip_serializing_if = "crate::render::is_zero")]
    pub contracts_omitted: usize,
    /// A limit that can have lost a caller, by the rule pack's coverage uses:
    /// a run-wide stop (the deadline, a truncated discovery, a failed git
    /// read) or a per-file limit on a changed file. `empty` is never claimed
    /// while this is set. The graph's per-node cap, which every real
    /// repository trips (#306), is not listed: a caller it cut an edge from
    /// is recovered by the same tests an edge faces.
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
    /// The changed definitions of that count which nothing calls, so they
    /// are not among `changed`: `path::symbol (lines)` each.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub untested_unlisted: Vec<String>,
    /// The changed definitions of that count whose only callers are inside
    /// the diff: live code, not code nothing calls (#441).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub untested_called_in_diff: Vec<String>,
    /// The changed definitions of that kind a runner or a framework enters
    /// without a caller (a script, a program's `main`, a registered or
    /// dispatched handler), named by role: `path::symbol (lines) — role`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entry_unlisted: Vec<String>,
    /// Changed definitions whose hunks are comments, formatting or a
    /// verbatim move only, which no caller sees, so their callers were not
    /// traced (#370): `path::symbol (lines)` each. Unsaid, a comment edit
    /// read "no resolved static callers" while the function had three (#498).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layout_only: Vec<String>,
    /// Lock files the range changes: dependency versions moved, and what that
    /// reaches is not traced. Dropped from every count, a dependency bump
    /// answered "No changes to analyse" (#495).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lockfile_changes: Vec<String>,
    /// `layout_only` entries left out to fit the cap; the count is whole.
    #[serde(default, skip_serializing_if = "crate::render::is_zero")]
    pub layout_only_omitted: usize,
    /// Changed definitions whose callers were traced; zero with `layout_only`
    /// set means the answer has nothing but layout to report.
    #[serde(skip)]
    pub traced_definitions: usize,
    /// Files the run withheld (`.diffctx/ignore`, gitignore, secret names):
    /// changed files of the range, or files naming the queried symbol. Their
    /// calls and tests were not read, so a zero says nothing about them
    /// (#392).
    #[serde(default, skip_serializing_if = "crate::render::is_zero")]
    pub withheld_files: usize,
    /// Changed code files whose definitions were not read — no grammar for
    /// the language in this build, or a parse that failed — so their callers
    /// and contracts are not counted, and `empty` is never claimed (#454).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unanalysed_files: Vec<UnanalysedFile>,
    #[serde(default, skip_serializing_if = "crate::render::is_zero")]
    pub unanalysed_files_omitted: usize,
    /// Of `unanalysed_files_omitted`, the `content_untraced` ones.
    #[serde(skip)]
    #[schemars(skip)]
    pub untraced_omitted: usize,
    /// The token cap the answer was fitted to.
    #[serde(skip)]
    #[schemars(skip)]
    pub cap: u32,
}

/// The files outside documents and data that spell a name nothing defines:
/// a textual match, not a resolved binding.
#[derive(Debug, Serialize, JsonSchema, Clone)]
pub struct SymbolNamed {
    /// Every file that spells the name, listed or not.
    pub files: usize,
    /// Code and config first, `MAX_TEXTUAL_SHOWN` of them.
    pub rows: Vec<NamedFile>,
    /// What this build read as text and so could not tell a definition in
    /// (#454); empty when every file naming the symbol was analysed.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub caveat: String,
}

#[derive(Debug, Serialize, JsonSchema, Clone)]
pub struct NamedFile {
    pub path: String,
    /// The first line that spells the name.
    pub line: u32,
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
    /// The role that gives this definition an implicit caller: a test its
    /// runner runs, a script's own guard, a program's `main`, a handler a
    /// framework registers or dispatches (R3). Such a definition is no test
    /// gap and its callers are not hedged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
    /// Why callers could not be resolved at all: a name defined in many
    /// files in a language without a binding model, a trait method reached
    /// through dynamic dispatch. Its empty `callers` is then no finding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub unresolved: Option<Unknown>,
    /// No test reaches the definition itself by a static link.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub untested: bool,
    /// Call sites left working by the change: it only added fields or
    /// parameters with defaults, so they are counted, not listed (#388).
    #[serde(default, skip_serializing_if = "crate::render::is_zero")]
    pub compatible_callers: usize,
    /// Files in view that spell the name but were not fully analysed: a
    /// caller there is not seen, so an empty `callers` says nothing about
    /// them (#454).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missed_in: Vec<UnanalysedFile>,
}

/// What the analysis cannot say about a definition's callers, and why
/// (#505): an unknown is named, never read as no caller. The answer carries
/// it as its sentence.
#[derive(Clone, Debug, PartialEq)]
pub enum Unknown {
    /// A trait method, reached through `dyn Trait` or a generic bound.
    Dispatched { trait_name: String },
    /// A name defined in many files of a language without a binding model.
    Ambiguous {
        name: String,
        definitions: usize,
        language: &'static str,
    },
    /// A removed name on too many lines of code to trace by name.
    CommonWord { name: String, lines: usize },
    /// Reads of a member through a receiver nothing types.
    UnresolvedReads { name: String, reads: usize },
    /// A member with no direct caller that a supertype, an interface or the
    /// runtime can call: `via` names that route, which nothing traces.
    Overridable { via: String },
    /// Code in a language no binding model reads: its callers are only the
    /// files the ranking graph links, so finding none proves nothing (#505).
    NotModelled { language: &'static str },
}

impl std::fmt::Display for Unknown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Dispatched { trait_name: t } => write!(
                f,
                "implements `{t}`; calls through `dyn {t}` or a generic bound are not resolved"
            ),
            Self::Ambiguous {
                name,
                definitions,
                language,
            } => write!(
                f,
                "`{name}` is ambiguous: {definitions} {language} definitions and no binding model to tell them apart, so callers outside its own file are not resolved"
            ),
            Self::CommonWord { name, lines } => write!(
                f,
                "`{name}` is a whole word on {lines} or more lines of code; too common to trace by name"
            ),
            Self::UnresolvedReads { name, reads } => write!(
                f,
                "{reads} read(s) of `.{name}` through a receiver the binding model cannot type"
            ),
            Self::Overridable { via } => {
                write!(
                    f,
                    "calls through {via} are not traced, and no direct caller was found"
                )
            }
            Self::NotModelled { language } => write!(
                f,
                "no binding model reads {language}: calls in its own file and through its types are not traced, so none found is no zero"
            ),
        }
    }
}

impl Serialize for Unknown {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
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
    /// A test file the size cap kept out that names this caller: an absent
    /// `tested_by` says nothing about it (#542).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unread_test: Option<String>,
    /// `resolved`: a binding links the reference to the changed definition.
    /// `name`: a word match — the lexical model of a language without a
    /// binding model, or a removed definition's name found by search; the
    /// text form lists it as text and never counts it as a caller.
    /// `candidate`: the binding or the receiver is not known; `reason` says
    /// which.
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
pub struct CollectionRead {
    pub path: String,
    pub line: u32,
    /// The collection the call names; absent when the name is computed at
    /// run time, so the call may read any collection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,
}

#[derive(Serialize, JsonSchema, Clone)]
pub struct PathReference {
    pub path: String,
    pub line: u32,
    /// The changed file the line names.
    pub target: String,
    /// A key the change touched in `target`, which the line spells: a
    /// textual match, not a resolved binding (#686).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

/// A changed file outside code whose changed keys were searched as text.
#[derive(Serialize, JsonSchema, Clone)]
pub struct KeySearch {
    pub path: String,
    pub keys: Vec<String>,
    /// The lines outside the diff naming any of them.
    pub named: usize,
}

#[derive(Serialize, JsonSchema, Clone)]
pub struct UnanalysedFile {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<&'static str>,
    /// `no_grammar`, `parse_timeout`, `parse_failed` (nothing read),
    /// `parse_partial` (read but for the error lines), `too_large` (over the
    /// size cap), `unreadable`, or `content_untraced`: a file outside code
    /// whose keys, values or rules something may read, of which only the
    /// lines naming the file by path were searched.
    pub reason: &'static str,
    /// Where the parse broke, the first few lines.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lines: Vec<u32>,
}

#[derive(Serialize, JsonSchema, Clone)]
pub struct Contract {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    pub kind: &'static str,
}

/// Whether `code` binds `name` as a local — `let name`, `let mut name` — so
/// that its calls of `name` reach the local, not a definition elsewhere.
fn binds_locally(code: &str, name: &str) -> bool {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    code.match_indices(name).any(|(at, _)| {
        let before = code[..at].trim_end();
        let head = before.strip_suffix("mut").map_or(before, str::trim_end);
        let Some(rest) = head.strip_suffix("let") else {
            return false;
        };
        !rest.chars().next_back().is_some_and(word)
            && !code[at + name.len()..].chars().next().is_some_and(word)
    })
}

/// The line of `f` that first spells `name` as a whole word.
fn first_naming_line(f: &Fragment, name: &str) -> Option<u32> {
    f.content
        .lines()
        .position(|l| names_word(l, name))
        .map(|i| f.start_line() + i as u32)
}

/// Whether `line` holds a call of `name`: the whole word, then `(`.
fn calls_word(line: &str, name: &str) -> bool {
    let word = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    line.match_indices(name).any(|(at, _)| {
        !line[..at].chars().next_back().is_some_and(word)
            && line[at + name.len()..].starts_with('(')
    })
}

/// Whether `text` holds `name` as a whole word; `$` is a word character,
/// as in a JavaScript identifier.
fn names_word(text: &str, name: &str) -> bool {
    let word = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    text.match_indices(name).any(|(at, _)| {
        !text[..at].chars().next_back().is_some_and(word)
            && !text[at + name.len()..].chars().next().is_some_and(word)
    })
}

fn is_code_file(path: &str) -> bool {
    let ext = Path::new(path)
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
        .unwrap_or_default();
    crate::config::extensions::CODE_EXTENSIONS.contains(ext.as_str())
}

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

/// `content` as code: comments and string literals blanked, interpolations
/// kept, read off the file's grammar (#460, #475). A name inside
/// `"must be refused"` or a comment is not a reference (#345). A file
/// without a grammar is read as written.
pub(crate) fn code_text(path: &str, content: &str) -> std::sync::Arc<str> {
    crate::parsers::code_only(path, content).unwrap_or_else(|| content.into())
}

fn receiver_before(code: &str, at: usize) -> Option<&str> {
    let head = code[..at].strip_suffix('.')?;
    let head = head.strip_suffix('?').unwrap_or(head);
    let start = head
        .rfind(|c: char| !(c.is_alphanumeric() || c == '_'))
        .map_or(0, |i| i + 1);
    Some(&head[start..])
}

/// The caller must name the symbol itself, in code: the language builders
/// link a call by name to every definition of that name in the repository
/// (case-folded), so an edge alone says "same name somewhere", a comment
/// mention says nothing, and `Cli` in a doc line is not `Cli::parse()`.
fn mentions(caller: &Fragment, symbol: Option<&str>, receiver: &Receiver) -> bool {
    let Some(sym) = symbol else { return true };
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    code_text(caller.path(), &caller.content)
        .lines()
        .any(|code| {
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
/// (Kotlin `fun String.pad()`); anything else is reached through its module.
/// `file`: the fragments of the core's own file, in fragment order.
pub(crate) fn is_member(core: &Fragment, file: &[&Fragment]) -> bool {
    if enclosing_class(core, file).is_some() {
        return true;
    }
    let head = without_decorators(&core.content).trim_start();
    let receiver = head
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
    "java", "kt", "kts", "scala", "cs", "php", "c", "cc", "cpp", "cxx", "h", "hpp", "m", "mm",
    "swift", "dart",
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
/// callers in a package-scoped language (Java, C#, Swift) pass, because a
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
    // A shell `lib.sh` is sourced by its own name, not its directory (#497).
    let sourced_by_name = changed.extension().is_some_and(|e| {
        matches!(
            e.to_string_lossy().to_lowercase().as_str(),
            "sh" | "bash" | "zsh" | "ksh"
        )
    });
    if !sourced_by_name
        && matches!(
            stem.as_str(),
            "mod" | "lib" | "main" | "index" | "init" | "__init__"
        )
    {
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

fn extension(path: &str) -> String {
    path.rsplit_once('.')
        .map_or("", |(_, e)| e)
        .to_ascii_lowercase()
}

/// A file read for its prose, whose consumers find it by its path. Not
/// `.txt`: `requirements.txt` and `CMakeLists.txt` are read by tools.
fn is_document(path: &str) -> bool {
    matches!(extension(path).as_str(), "md" | "markdown" | "rst" | "adoc")
}

/// The changed files this run did not read whole (#454), and the changed
/// files outside code whose content something reads — config keys, a
/// template, a query, a stylesheet — which nothing here traces (#559),
/// ordered by language then path so the listed ones and the counted rest are
/// stable.
fn unanalysed_files(state: &ScoredState, changed: &[String]) -> Vec<UnanalysedFile> {
    let mut fragmented: FxHashMap<String, bool> = FxHashMap::default();
    for f in state.run.bounded(&state.all_fragments) {
        *fragmented.entry(rel(state, f.path())).or_default() |= f.kind.is_definition_kind();
    }
    let source = state.run.source();
    let too_large = state
        .run
        .limited_by(crate::resource::LimitReason::FileTooLarge);
    let layout: FxHashSet<&str> = state
        .change_classes
        .iter()
        .filter(|(_, class, _)| *class == crate::change_class::ChangeClass::Layout)
        .map(|(p, _, _)| p.as_str())
        .collect();
    let mut out: Vec<UnanalysedFile> = state
        .run
        .bounded(changed)
        .filter_map(|path| {
            // Only the lines naming the file by path are searched for it. A
            // document or a binary has no other consumer, a schema is listed
            // as a contract and a layout edit reaches no reader; anything
            // else read as a clean zero while its readers went unsearched.
            // An extensionless shell script is code by its shebang, and the
            // shell model reads it (#541).
            let script = || {
                Path::new(path).extension().is_none()
                    && source
                        .read_to_string(&state.root_dir.join(path))
                        .is_some_and(|t| crate::languages::sniff_language(path, &t) == Some("bash"))
            };
            if !is_code_file(path) && !script() {
                let binary = || {
                    source
                        .read_bytes(&state.root_dir.join(path))
                        .is_some_and(|b| b.iter().take(8000).any(|&c| c == 0))
                };
                if is_document(path)
                    || is_schema_file(path)
                    || has_data_extension(path)
                    || layout.contains(path.as_str())
                    || binary()
                {
                    return None;
                }
                return Some(UnanalysedFile {
                    path: path.clone(),
                    language: crate::languages::get_language_for_file(path),
                    reason: "content_untraced",
                    lines: Vec::new(),
                });
            }
            // No fragment at all. A deleted or withheld file never reaches
            // `changed`; a code file over the size cap or one that could not
            // be read was never analysed, and saying nothing made its callers
            // a clean zero. An empty file has nothing to analyse.
            // Over the cap on either side: a head read in full still had its
            // removals skipped when the base was too large to parse.
            if too_large.contains(path) {
                return Some(UnanalysedFile {
                    path: path.clone(),
                    language: crate::languages::get_language_for_file(path),
                    reason: "too_large",
                    lines: Vec::new(),
                });
            }
            let Some(&has_definition) = fragmented.get(path) else {
                let reason = if source.read_bytes(&state.root_dir.join(path)).is_none() {
                    "unreadable"
                } else {
                    return None;
                };
                return Some(UnanalysedFile {
                    path: path.clone(),
                    language: crate::languages::get_language_for_file(path),
                    reason,
                    lines: Vec::new(),
                });
            };
            let (reason, lines) = crate::parsers::unanalysed_reason(path, has_definition, || {
                source.read_bytes(&state.root_dir.join(path))
            })?;
            Some(UnanalysedFile {
                path: path.clone(),
                language: crate::languages::get_language_for_file(path),
                reason,
                lines,
            })
        })
        .collect();
    // A deleted file over the size cap had its removals skipped
    // (`removed_by_models`): as unanalysed as a changed one, and never in
    // `changed`.
    out.extend(
        state
            .deleted_files
            .iter()
            .filter(|p| too_large.contains(*p) && !changed.contains(p))
            .map(|p| UnanalysedFile {
                path: p.clone(),
                language: crate::languages::get_language_for_file(p),
                reason: "too_large",
                lines: Vec::new(),
            }),
    );
    out.sort_by(|a, b| {
        a.language
            .cmp(&b.language)
            .then_with(|| a.path.cmp(&b.path))
    });
    out
}

/// Files in view, other than the definition's own, that spell `name` but
/// whose definitions were not all read: a caller there is not seen.
/// Where a name is spelled outside the code impact traces: config, data,
/// templates, markup, and, for a module's name, another runtime's scripts
/// (`python -m controls.foo_check` in a CronWorkflow). A changed definition
/// no traced caller reaches, but such a file names, is no "nothing calls
/// it": a zero needs a witness that the name is read nowhere it went
/// unsearched (#505).
#[derive(Default)]
struct UntracedNamers {
    /// Each needle, by the lines naming it as a whole word.
    rows: FxHashMap<String, Vec<(String, u32)>>,
}

/// A name spelled in more untraced files than this is a common word, not a
/// reference: it witnesses nothing.
const MAX_UNTRACED_NAMERS: usize = 12;

/// Module names that say a file's role, not which file it is.
const ROLE_STEMS: &str = "index main mod lib init __init__ __main__ app util utils helpers common core base types config settings test tests conftest setup";

/// A name only code would spell: `foo_check`, `formatDate`, `LIMIT`, `v2`.
/// A plain word (`naming`, `total`) is prose in a dataset or a paper as
/// often as a reference, and witnesses nothing.
pub(crate) fn distinctive(word: &str) -> bool {
    let has = |f: fn(&char) -> bool| word.chars().any(|c| f(&c));
    let shaped = word.contains('_')
        || has(char::is_ascii_digit)
        || (has(char::is_ascii_uppercase) && has(char::is_ascii_lowercase))
        || !has(char::is_ascii_lowercase);
    shaped
        && word.chars().count() >= 4
        && !word.chars().all(|c| c.is_ascii_digit() || c == '_')
        && !crate::stopwords::CODE_STOPWORDS.contains(&word.to_lowercase())
        && !ROLE_STEMS.split(' ').any(|r| r == word)
}

fn module_stem(path: &str) -> Option<&str> {
    Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| distinctive(s))
}

impl UntracedNamers {
    fn build(state: &ScoredState, cores: &[&Fragment], diff_range: Option<&str>) -> Self {
        let mut needles: Vec<String> = cores
            .iter()
            .filter_map(|c| c.symbol_name.as_deref().map(base_symbol))
            .chain(cores.iter().filter_map(|c| module_stem(c.path())))
            .filter(|w| distinctive(w))
            .map(str::to_string)
            .collect();
        needles.sort();
        needles.dedup();
        if needles.is_empty() {
            return Self::default();
        }
        let head = diff_range
            .map(crate::git::split_diff_range)
            .and_then(|(_, head)| head);
        let wanted: FxHashSet<&str> = needles.iter().map(String::as_str).collect();
        let changed: FxHashSet<String> = state
            .changed_files
            .iter()
            .map(|p| rel(state, &p.to_string_lossy()))
            .collect();
        let grepped = read_or_note(
            state,
            crate::git::grep_lines(&state.root_dir, &needles, head.as_deref()),
        );
        let mut rows: FxHashMap<String, Vec<(String, u32)>> = FxHashMap::default();
        for (path, line, text) in &grepped.rows {
            // A comment or a document mentions a name without reading it.
            if changed.contains(path)
                || is_document(path)
                || matches!(
                    extension(path).as_str(),
                    "diff"
                        | "patch"
                        | "lock"
                        | "tex"
                        | "bib"
                        | "txt"
                        | "csv"
                        | "tsv"
                        | "log"
                        | "svg"
                )
                || commented(text)
            {
                continue;
            }
            let mut seen: FxHashSet<&str> = FxHashSet::default();
            for word in text.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
                if wanted.contains(word) && seen.insert(word) {
                    rows.entry(word.to_string())
                        .or_default()
                        .push((path.clone(), *line));
                }
            }
        }
        Self { rows }
    }

    /// The lines naming `word` in the files `admit` keeps, one per file;
    /// none when the word is too common to witness anything.
    fn naming(&self, word: &str, admit: impl Fn(&str) -> bool) -> Vec<(String, u32)> {
        let mut by_path: Vec<(String, u32)> = Vec::new();
        for (path, line) in self.rows.get(word).into_iter().flatten() {
            if admit(path) && !by_path.iter().any(|(p, _)| p == path) {
                by_path.push((path.clone(), *line));
            }
        }
        if by_path.len() > MAX_UNTRACED_NAMERS {
            return Vec::new();
        }
        by_path.sort();
        by_path
    }

    /// The files outside code that spell `core`'s name.
    fn of(&self, core: &Fragment) -> Vec<UnanalysedFile> {
        let Some(name) = core.symbol_name.as_deref().map(base_symbol) else {
            return Vec::new();
        };
        self.naming(name, |p| !is_code_file(p))
            .into_iter()
            .map(|(path, line)| UnanalysedFile {
                language: crate::languages::get_language_for_file(&path),
                path,
                reason: "not_traced",
                lines: vec![line],
            })
            .collect()
    }

    /// The lines naming a changed module by its name outside the code that
    /// imports it: a config value, a `python -m pkg.mod` in another
    /// runtime's script (#613). Named by path, a path's own spelling, is
    /// already in `known`.
    fn module_refs(
        &self,
        state: &ScoredState,
        cores: &[&Fragment],
        known: &[PathReference],
    ) -> Vec<PathReference> {
        let mut out: Vec<PathReference> = Vec::new();
        let mut files: Vec<&str> = cores.iter().map(|c| c.path()).collect();
        files.sort_unstable();
        files.dedup();
        for own in files {
            let Some(stem) = module_stem(own) else {
                continue;
            };
            let target = rel(state, own);
            for (path, line) in self
                .naming(stem, |p| !is_code_file(p) || !same_runtime(p, own))
                .into_iter()
                .take(MAX_NAMED_BY_PER_TARGET)
            {
                if !known.iter().any(|k| k.path == path && k.target == target) {
                    out.push(PathReference {
                        path,
                        line,
                        target: target.clone(),
                        key: None,
                    });
                }
            }
        }
        out
    }
}

/// Whether a file could import `own`'s definitions: it sits in its
/// directory or below (a package's own files, a Rust child module), or
/// spells its module's stem or directory (`billing/years`, the Go package
/// `billing`). One that only spells the name calls another of that name: an
/// over-cap file of another package was a hedge on an unexported function
/// it could never reach (#591, #599).
fn names_module(path: &str, own: &str, spells: impl Fn(&str) -> bool) -> bool {
    let (path, own) = (Path::new(path), Path::new(own));
    own.parent().is_none_or(|dir| path.starts_with(dir))
        || [own.file_stem(), own.parent().and_then(Path::file_name)]
            .into_iter()
            .flatten()
            .any(|w| spells(&w.to_string_lossy()))
}

fn missed_namers(
    state: &ScoredState,
    files: &Files,
    registry: &Registry,
    name: &str,
    own: &str,
) -> Vec<UnanalysedFile> {
    let source = state.run.source();
    // Identifiers are stored lowercased: compared as written, a camelCase
    // name never matched and its unparsed namers went unsaid.
    let word = name.to_lowercase();
    let mut out: Vec<UnanalysedFile> = state
        .run
        .bounded(&files.identifiers)
        // A file of another runtime cannot call the name, read or not: a
        // shell script that broke the parse is no hedge on a JS function
        // (#521).
        .filter(|(path, words)| {
            **path != own
                && words.contains(word.as_str())
                && same_runtime(path, own)
                && names_module(path, own, |w| words.contains(w.to_lowercase().as_str()))
        })
        .filter_map(|(path, _)| {
            let rel_path = rel(state, path);
            let has_definition = files
                .fragments
                .get(path)
                .is_some_and(|frags| frags.iter().any(|f| f.kind.is_definition_kind()));
            let (reason, lines) =
                crate::parsers::unanalysed_reason(&rel_path, has_definition, || {
                    source.read_bytes(Path::new(path))
                })?;
            // A tree broken on some lines read the rest: a use of the name
            // there was seen, and only one where it broke can be lost (#537).
            if reason == "parse_partial"
                && source.read_bytes(Path::new(path)).is_some_and(|bytes| {
                    let text = String::from_utf8_lossy(&bytes);
                    !crate::parsers::spelled_where_broken(&rel_path, &text, name)
                })
            {
                return None;
            }
            // A model that reads past a broken line says whether the name is
            // where it could not read: an `echo` argument is no hedge (#521).
            if registry
                .for_path(&rel_path)
                .and_then(|m| m.names_unread(&rel_path, name))
                == Some(false)
            {
                return None;
            }
            Some(UnanalysedFile {
                language: crate::languages::get_language_for_file(&rel_path),
                path: rel_path,
                reason,
                lines,
            })
        })
        .collect();
    // Code of the name's own runtime embedded in a shell script (`python -
    // <<EOF`, `node -e '…'`) runs it, and nothing reads that code: a script
    // whose embedded code names it is said, not left out (#427).
    let runtime = runtime_of(own);
    for path in state.run.bounded(files.identifiers.keys()) {
        if *path == own
            || crate::languages::get_language_for_file(path) != Some("bash")
            || !files.identifiers[path].contains(word.as_str())
        {
            continue;
        }
        let Some(line) = source
            .read_to_string(Path::new(path))
            .and_then(|text| embedded_naming(&text, &runtime, name))
        else {
            continue;
        };
        out.push(UnanalysedFile {
            language: Some("bash"),
            path: rel(state, path),
            reason: "embedded_code",
            lines: vec![line],
        });
    }
    // A file over the size cap was never read into view, yet it may call the
    // name like any other: it is said, not left out (#484). The run keeps
    // the files the cap kept out.
    let in_view: FxHashSet<String> = out.iter().map(|f| f.path.clone()).collect();
    let too_large: Vec<String> = state
        .run
        .limited_files()
        .into_iter()
        .filter(|f| f.reason == crate::resource::LimitReason::FileTooLarge)
        .map(|f| f.path)
        .filter(|p| {
            !in_view.contains(p) && rel(state, own) != *p && is_code_file(p) && same_runtime(p, own)
        })
        .collect();
    for path in state.run.bounded(too_large) {
        let Some(bytes) = source.read_bytes(&state.root_dir.join(&path)) else {
            continue;
        };
        let text = String::from_utf8_lossy(&bytes);
        if names_word(&text, name)
            && names_module(&path, &rel(state, own), |w| names_word(&text, w))
        {
            out.push(UnanalysedFile {
                language: crate::languages::get_language_for_file(&path),
                path,
                reason: "too_large",
                lines: Vec::new(),
            });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// The runtime a language runs in, for the languages that call across
/// their own boundary: JVM languages call each other, JavaScript runs the
/// HTML and single-file components that embed it, Swift, Objective-C, C and
/// C++ bridge and link. Anything else is its own runtime.
fn runtime_of(path: &str) -> String {
    let language = crate::languages::get_language_for_file(path).unwrap_or("");
    match language {
        "java" | "kotlin" | "scala" | "groovy" | "clojure" => "jvm",
        "javascript" | "typescript" | "tsx" | "jsx" | "html" | "svelte" | "vue" => "js",
        "python" | "cython" => "python",
        "csharp" | "c_sharp" | "fsharp" => "dotnet",
        "swift" | "objectivec" | "c" | "cpp" => "native",
        other => other,
    }
    .to_string()
}

/// The line of the first command in `script` that runs `runtime`'s
/// interpreter on code naming `name`: a heredoc fed to it, or its `-c`/`-e`
/// argument.
fn embedded_naming(script: &str, runtime: &str, name: &str) -> Option<u32> {
    let (interpreters, as_file): (&[&str], &str) = match runtime {
        "python" => (&["python", "pypy"], "embedded.py"),
        "js" => (&["node", "deno", "bun"], "embedded.js"),
        _ => return None,
    };
    let tree = crate::parsers::parse_tree("bash", script)?;
    let text = |n: tree_sitter::Node| n.utf8_text(script.as_bytes()).unwrap_or("");
    // Only code names it: a comment or a string in the embedded program
    // does not (`# column-aligned`).
    let names = |embedded: &str| names_word(&code_text(as_file, embedded), name);
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
        if node.kind() != "command" {
            continue;
        }
        let mut cursor = node.walk();
        let words: Vec<&str> = node.named_children(&mut cursor).map(text).collect();
        let runs = words.iter().any(|w| {
            let base = w.rsplit('/').next().unwrap_or(w);
            interpreters.iter().any(|i| base.starts_with(i))
        });
        if !runs {
            continue;
        }
        let inline = words
            .iter()
            .position(|w| matches!(*w, "-c" | "-e"))
            .and_then(|i| words.get(i + 1))
            .is_some_and(|code| names(code.trim_matches(['\'', '"'])));
        let fed = node.parent().is_some_and(|p| {
            p.kind() == "redirected_statement" && {
                let mut cursor = p.walk();
                let redirects: Vec<tree_sitter::Node> = p.named_children(&mut cursor).collect();
                redirects
                    .into_iter()
                    .filter(|r| r.kind() == "heredoc_redirect")
                    .any(|r| {
                        let mut cursor = r.walk();
                        let bodies: Vec<tree_sitter::Node> =
                            r.named_children(&mut cursor).collect();
                        bodies
                            .into_iter()
                            .filter(|b| b.kind() == "heredoc_body")
                            .any(|b| names(text(b)))
                    })
            }
        });
        if inline || fed {
            return u32::try_from(node.start_position().row + 1).ok();
        }
    }
    None
}

/// No runtime crosses from one to the other: a Kotlin test cannot run a
/// Swift function, and a Python name in a Kotlin file is not a call of it
/// (#458, #487).
fn same_runtime(a: &str, b: &str) -> bool {
    let (ra, rb) = (runtime_of(a), runtime_of(b));
    ra.is_empty() || rb.is_empty() || ra == rb
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

/// An added run whose lines, blank ones aside, are a run removed elsewhere in
/// the same file: the definition moved, its text and indentation did not.
fn moved_verbatim(
    run: &crate::change_class::HunkText,
    hunks: &[crate::change_class::HunkText],
) -> bool {
    let text = |lines: &[String]| -> Vec<String> {
        lines
            .iter()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.trim_end().to_string())
            .collect()
    };
    let added = text(&run.added);
    run.removed.is_empty()
        && !added.is_empty()
        && hunks
            .iter()
            .any(|h| h.path == run.path && h.added.is_empty() && text(&h.removed) == added)
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
    // A migration is code by extension and a schema by where it lives; so is
    // the Rails schema dump. A bare `versions/` is not one (`.yarn/versions`),
    // alembic's is named whole.
    let in_db = |name: &str| {
        p.parent()
            .and_then(|d| d.file_name())
            .is_some_and(|d| d == name)
    };
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    // Prose and tables beside the migrations (a plan, an inventory) are no
    // schema (#463).
    let prose = matches!(
        ext,
        "md" | "markdown" | "rst" | "txt" | "adoc" | "tsv" | "csv"
    );
    let migration = !prose
        && p.components()
            .any(|c| matches!(c.as_os_str().to_str(), Some("migrations" | "migration")))
        || lower.contains("/alembic/versions/")
        || lower.contains("db/migrate/")
        || (in_db("db") && matches!(stem, "schema" | "structure"));
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

/// The base revision's text of the files whose public signatures impact
/// compares, read through one `cat-file --batch` and kept: the check of each
/// public symbol ran `git show` of its whole file, the same few files again
/// and again. Only a read that succeeded is kept, so a failure is met again
/// as the first one was.
struct BaseTexts<'a> {
    root: &'a Path,
    batch: std::cell::RefCell<Option<crate::git::CatFileBatch>>,
    texts: std::cell::RefCell<FxHashMap<(String, String), std::rc::Rc<str>>>,
}

impl<'a> BaseTexts<'a> {
    fn new(root: &'a Path) -> Self {
        Self {
            root,
            batch: std::cell::RefCell::new(None),
            texts: std::cell::RefCell::default(),
        }
    }

    fn read(&self, rev: &str, rel_path: &str) -> crate::git::Result<std::rc::Rc<str>> {
        let key = (rev.to_string(), rel_path.to_string());
        if let Some(text) = self.texts.borrow().get(&key) {
            return Ok(text.clone());
        }
        let mut batch = self.batch.borrow_mut();
        if batch.is_none() {
            *batch = Some(crate::git::CatFileBatch::new(self.root)?);
        }
        let text: std::rc::Rc<str> = batch
            .as_mut()
            .expect("opened above")
            .get(rev, Path::new(rel_path))?
            .into();
        self.texts.borrow_mut().insert(key, text.clone());
        Ok(text)
    }
}

/// Whether an exported symbol's declaration moved: its head is looked for in
/// the file at the base revision, whitespace-insensitively. A body-only edit
/// is what the callers list already says, and a contracts section repeated
/// on every refactor teaches the reader to skip it (#325). A file the base
/// does not have is new, and every export in it is a new contract.
fn signature_changed(
    state: &ScoredState,
    base_texts: &BaseTexts,
    base: &str,
    core: &Fragment,
) -> bool {
    let rel_path = rel(state, core.path());
    let Ok(old) = base_texts.read(base, &rel_path) else {
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
    /// Functions outside test files an attribute marks as tests: a Rust
    /// `#[test]` in an inline `mod tests` (#528).
    inline_tests: FxHashSet<&'a FragmentId>,
    /// Test files the size cap kept out, with their code.
    unread: Vec<(String, std::sync::Arc<str>)>,
    /// A binary's entry file -> a test that runs the built binary
    /// (`CARGO_BIN_EXE_<name>`, `cargo_bin("<name>")`), which reaches its
    /// `main` through no call (#391, #678).
    binary_tests: FxHashMap<String, String>,
    /// The repository holds a test file the walk never reached: tests the
    /// change does not touch and nothing links to are still tests in view
    /// (#123), so an absent link is a finding and not a blank.
    repo_tests: bool,
    /// `tested_by` per fragment: callers repeat across changed symbols, and
    /// each answer walks every test fragment.
    memo: std::cell::RefCell<FxHashMap<FragmentId, Option<(String, Option<String>)>>>,
}

impl<'a> TestIndex<'a> {
    fn build(state: &'a ScoredState, diff_range: Option<&str>) -> Self {
        let graph = &state.scoring_result.graph;
        let mut by_path: FxHashMap<String, String> = FxHashMap::default();
        graph.for_each_categorized_edge(|src, dst, cat| {
            if cat != EdgeCategory::TestEdge || !state.run.check() {
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
        let mut tests_at: FxHashMap<&str, Vec<(u32, u32)>> = FxHashMap::default();
        for f in &state.all_fragments {
            if f.kind == FragmentKind::Function && lang::rust::is_rust(f.path()) {
                tests_at.entry(f.path()).or_insert_with(|| {
                    let text = state.run.source().read_to_string(Path::new(f.path()));
                    text.map(|t| lang::rust::test_functions(&t))
                        .unwrap_or_default()
                });
            }
        }
        let inline_tests: FxHashSet<&FragmentId> = state
            .all_fragments
            .iter()
            .filter(|f| {
                tests_at.get(f.path()).is_some_and(|spans| {
                    spans
                        .iter()
                        .any(|&(a, b)| a <= f.start_line() && f.end_line() <= b)
                })
            })
            .map(|f| &f.id)
            .collect();
        let test_fragments: Vec<&Fragment> = state
            .all_fragments
            .iter()
            .filter(|f| {
                crate::testfiles::is_test_path(Path::new(f.path())) || inline_tests.contains(&f.id)
            })
            .collect();
        let source = state.run.source();
        let too_large: Vec<String> = state
            .run
            .limited_files()
            .into_iter()
            .filter(|f| {
                f.reason == crate::resource::LimitReason::FileTooLarge
                    && crate::testfiles::is_test_path(Path::new(&f.path))
            })
            .map(|f| f.path)
            .collect();
        let unread = state
            .run
            .bounded(too_large)
            .filter_map(|path| {
                let bytes = source.read_bytes(&state.root_dir.join(&path))?;
                let code = code_text(&path, &String::from_utf8_lossy(&bytes));
                Some((path, code))
            })
            .collect();
        let binary_tests = binary_tests(state, diff_range);
        let repo_tests = test_fragments.is_empty()
            && by_path.is_empty()
            && binary_tests.is_empty()
            && repo_holds_test_file(state);
        Self {
            by_path,
            test_fragments,
            inline_tests,
            unread,
            binary_tests,
            repo_tests,
            memo: std::cell::RefCell::default(),
        }
    }

    fn any(&self) -> bool {
        !self.test_fragments.is_empty()
            || !self.by_path.is_empty()
            || !self.binary_tests.is_empty()
            || self.repo_tests
    }

    fn unread_naming(&self, caller: &Fragment, own: &str) -> Option<String> {
        let name = caller
            .symbol_name
            .as_deref()
            .map(base_symbol)
            .filter(|s| s.len() >= 3)?;
        self.unread
            .iter()
            .find(|(path, text)| {
                same_runtime(path, own)
                    && names_word(text, name)
                    && names_module(path, own, |w| names_word(text, w))
            })
            .map(|(path, _)| path.clone())
    }

    fn is_test(&self, f: &Fragment) -> bool {
        crate::testfiles::is_test_path(Path::new(f.path())) || self.inline_tests.contains(&f.id)
    }

    /// A test guards a caller when it names the caller's symbol in code and
    /// reaches the caller's module — the same bar a caller has to clear. A
    /// bare name match (`render` in a Rust test, `render()` in a Python
    /// script) attributed guards to tests that never import the file (#312),
    /// and a test edge to the file proves the file is tested, not the caller.
    fn direct(&self, caller: &Fragment, link: &Link) -> Option<String> {
        if self.is_test(caller) {
            return Some(caller.path().to_string());
        }
        if caller.symbol_name.as_deref() == Some("main") {
            if let Some(test) = self.binary_tests.get(caller.path()) {
                return Some(test.clone());
            }
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
        // A chunk of a long definition (`Output[35]`) is reached by what
        // reaches the definition: the graph's edges end at the whole.
        let name = caller.symbol_name.as_deref().map(base_symbol);
        let whole = (caller.symbol_name.as_deref() != name)
            .then(|| {
                by_id.values().copied().filter(|f| {
                    f.symbol_name.as_deref() == name
                        && f.path() == caller.path()
                        && f.start_line() <= caller.start_line()
                        && f.end_line() >= caller.end_line()
                })
            })
            .into_iter()
            .flatten()
            .max_by_key(|f| f.line_count());
        let mut frontier: Vec<&Fragment> = std::iter::once(caller).chain(whole).collect();
        let mut seen: FxHashSet<&FragmentId> = frontier.iter().map(|f| &f.id).collect();
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
                if self.is_test(up) {
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

/// Whether the tree the run reads holds any test file, linked or not.
fn repo_holds_test_file(state: &ScoredState) -> bool {
    let is_test = |p: &Path| crate::testfiles::is_test_path(p);
    match state.run.source().list_blobs(&[]) {
        Some(Ok(blobs)) => state.run.bounded(blobs).any(|b| is_test(&b.path)),
        Some(Err(_)) => false,
        None => crate::git::run_git_z(
            &state.root_dir,
            &[
                "ls-files",
                "-z",
                "--cached",
                "--others",
                "--exclude-standard",
            ],
        )
        .is_ok_and(|files| state.run.bounded(files).any(|f| is_test(Path::new(&f)))),
    }
}

/// The Rust binaries in the analysed fragments, keyed by their entry file
/// as fragments spell it, each mapped to one test of its crate that runs
/// the built binary. `src/main.rs` is the package's binary, named in
/// `Cargo.toml`; `src/bin/<name>.rs` is `<name>`.
fn binary_tests(state: &ScoredState, diff_range: Option<&str>) -> FxHashMap<String, String> {
    let mut entries: Vec<(String, String, String)> = Vec::new();
    let mut seen: FxHashSet<&str> = FxHashSet::default();
    for f in &state.all_fragments {
        let own = f.path();
        if !seen.insert(own) || !lang::rust::is_rust(own) {
            continue;
        }
        let path = rel(state, own);
        let (krate, name) = if let Some(krate) = path.strip_suffix("src/main.rs") {
            let manifest = state
                .run
                .source()
                .read_to_string(&state.root_dir.join(format!("{krate}Cargo.toml")));
            let Some(name) = manifest.as_deref().and_then(package_name) else {
                continue;
            };
            (krate.to_string(), name)
        } else if let Some((krate, file)) = path.split_once("src/bin/") {
            let name = file.trim_end_matches("/main.rs").trim_end_matches(".rs");
            if name.contains('/') {
                continue;
            }
            (krate.to_string(), name.to_string())
        } else {
            continue;
        };
        entries.push((own.to_string(), krate, name));
    }
    if entries.is_empty() {
        return FxHashMap::default();
    }
    let head = diff_range
        .map(crate::git::split_diff_range)
        .and_then(|(_, head)| head);
    let needles = vec!["CARGO_BIN_EXE".to_string(), "cargo_bin".to_string()];
    let grepped = read_or_note(
        state,
        crate::git::grep_lines(&state.root_dir, &needles, head.as_deref()),
    );
    let mut out: FxHashMap<String, String> = FxHashMap::default();
    for (own, krate, name) in entries {
        let exe = format!("CARGO_BIN_EXE_{name}");
        let quoted = format!("\"{name}\"");
        let test = grepped
            .rows
            .iter()
            .filter(|(path, _, text)| {
                path.starts_with(&format!("{krate}tests/"))
                    && (names_word(text, &exe)
                        || (text.contains("cargo_bin") && text.contains(&quoted)))
            })
            .map(|(path, _, _)| path.clone())
            .min();
        if let Some(test) = test {
            out.insert(own, test);
        }
    }
    out
}

/// `[package] name = "…"` in a Cargo manifest.
fn package_name(manifest: &str) -> Option<String> {
    let package = manifest.split("[package]").nth(1)?;
    let section = package.split("\n[").next()?;
    section.lines().find_map(|l| {
        let (key, value) = l.split_once('=')?;
        (key.trim() == "name").then(|| value.trim().trim_matches('"').to_string())
    })
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
                // The caller spans every chunk it was split into, or a call
                // site in a later chunk sits outside the range shown (#472).
                let span = |l: &str| -> Option<(u32, u32)> {
                    let (a, b) = l.split_once('-')?;
                    Some((a.parse().ok()?, b.parse().ok()?))
                };
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
                if m.tested_by.is_none() {
                    m.tested_by = c.tested_by;
                    m.tested_via = c.tested_via;
                }
                m.unread_test = m.unread_test.take().or(c.unread_test);
                m.file_tested = m.tested_by.is_none() && (m.file_tested || c.file_tested);
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
/// runtime for a Python operator, any interface a Go method set satisfies.
/// Read as "nothing calls them", an override of `Runnable.run` or an
/// `__eq__` a test exercises was reported as dead (#566). A private or
/// static member is reached only directly, so its zero stands.
fn dispatch_route(state: &ScoredState, core: &Fragment, file: &[&Fragment]) -> Option<String> {
    let name = base_symbol(core.symbol_name.as_deref()?);
    let ext = Path::new(core.path())
        .extension()?
        .to_str()?
        .to_ascii_lowercase();
    match ext.as_str() {
        // Traits are the Rust model's (`LanguageFacts::dispatched`).
        "rs" => return None,
        "go" => {
            return core
                .content
                .trim_start()
                .starts_with("func (")
                .then(|| "any interface its type's method set satisfies".to_string());
        }
        "py" | "pyi" if name.len() > 4 && name.starts_with("__") && name.ends_with("__") => {
            return Some(format!(
                "the operator or protocol the runtime maps to `{name}`"
            ));
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
    let supers = enclosing_class(core, file)
        .map(|c| supertypes(&c.content, &ext))
        .unwrap_or_default();
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
            .map(|(bases, _)| bases.to_string())
            .unwrap_or_default()
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
        after.map(|i| flat[i..].to_string()).unwrap_or_default()
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
        .map(|t| t.rsplit('.').next().unwrap_or(t).to_string())
        .collect()
}

/// The class a fragment sits in, among `file`, the fragments of its own
/// file in fragment order.
/// The indentation of a text's first non-blank, non-decorator line.
fn indent_of(text: &str) -> usize {
    text.lines()
        .find(|l| !l.trim().is_empty() && !l.trim_start().starts_with('@'))
        .map_or(0, |l| l.len() - l.trim_start().len())
}

/// The object literal a method belongs to — `cart` of
/// `const cart = { total() {} }` — reached as `cart.total()` the way a
/// class's method is reached through its instance (#473).
pub(crate) fn object_owner(f: &Fragment, file: &[&Fragment]) -> Option<String> {
    file.iter()
        .filter(|o| {
            o.kind == FragmentKind::Variable
                && o.id != f.id
                && o.start_line() <= f.start_line()
                && o.end_line() >= f.end_line()
                && o.content
                    .split_once('=')
                    .is_some_and(|(_, v)| v.trim_start().starts_with('{'))
        })
        .min_by_key(|o| o.line_count())
        .and_then(|o| o.symbol_name.clone())
}

/// `name: (…) => …` or `name(…) {` as a property of an object no variable
/// holds — a store built inside `create(...)`: a method of an owner the
/// syntax does not name (#473).
pub(crate) fn is_object_property(f: &Fragment, name: &str) -> bool {
    let head = f.content.trim_start();
    [
        format!("{name}:"),
        format!("\"{name}\":"),
        format!("'{name}':"),
    ]
    .iter()
    .any(|k| head.starts_with(k.as_str()))
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
    // `@value.setter`, `@attr.s`, `@define`, `@enum.unique`, Lombok: they
    // shape what they decorate and register it nowhere (#414).
    "setter",
    "getter",
    "deleter",
    "s",
    "attrs",
    "define",
    "frozen",
    "mutable",
    "unique",
    "Data",
    "Value",
    "Getter",
    "Setter",
    "Builder",
    "Slf4j",
    "ToString",
    "EqualsAndHashCode",
    "NoArgsConstructor",
    "AllArgsConstructor",
    "RequiredArgsConstructor",
];

/// The decorator that hands a definition to a registry, a router or a
/// container, on the definition or on the class around it.
/// JVM annotations that hand a method or a bean to a framework that calls
/// it: request mappings, schedulers, listeners, bean factories. A
/// class-level `@Component` registers the class, not every method on it.
const JVM_ENTRY_POINTS: &[&str] = &[
    "RequestMapping",
    "GetMapping",
    "PostMapping",
    "PutMapping",
    "DeleteMapping",
    "PatchMapping",
    "Scheduled",
    "EventListener",
    "TransactionalEventListener",
    "KafkaListener",
    "RabbitListener",
    "JmsListener",
    "SqsListener",
    "ExceptionHandler",
    "MessageMapping",
    "SubscribeMapping",
    "QueryMapping",
    "MutationMapping",
    "SubscriptionMapping",
    "SchemaMapping",
    "BatchMapping",
    "Bean",
    "PostConstruct",
    "PreDestroy",
    "Tool",
    "GET",
    "POST",
    "PUT",
    "DELETE",
    "PATCH",
    "HEAD",
    "OPTIONS",
    "Get",
    "Post",
    "Put",
    "Delete",
    "Patch",
];

/// Packages whose annotations are trusted as entry points when imported.
const JVM_FRAMEWORK_PACKAGES: &[&str] = &[
    "org.springframework.",
    "jakarta.",
    "javax.",
    "io.micronaut.",
    "io.quarkus.",
    "org.eclipse.microprofile.",
];

fn is_jvm_path(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    [".kt", ".kts", ".java", ".scala"]
        .iter()
        .any(|e| p.ends_with(e))
}

/// The framework entry point annotating the JVM definition itself, its
/// import naming a framework package. Unknown annotations are neutral.
fn jvm_entry_point(
    f: &Fragment,
    file: &[&Fragment],
    annotations: Option<&[String]>,
) -> Option<String> {
    let names: Vec<String> = match annotations {
        Some(names) => names.to_vec(),
        None => f
            .content
            .lines()
            .map(str::trim_start)
            .take_while(|l| l.is_empty() || l.starts_with('@'))
            .filter_map(|l| {
                let name = l
                    .strip_prefix('@')?
                    .split(|c: char| c == '(' || c.is_whitespace())
                    .next()?;
                Some(name.rsplit('.').next().unwrap_or(name).to_string())
            })
            .collect(),
    };
    names.into_iter().find(|name| {
        JVM_ENTRY_POINTS.contains(&name.as_str())
            && file.iter().flat_map(|fr| fr.content.lines()).any(|line| {
                let line = line.trim_start();
                let Some(path) = line.strip_prefix("import ") else {
                    return false;
                };
                let path = path.trim_end_matches(';').trim();
                let path = path.split(" as ").next().unwrap_or(path);
                JVM_FRAMEWORK_PACKAGES.iter().any(|p| path.starts_with(p))
                    && (path.ends_with(&format!(".{name}")) || path.ends_with(".*"))
            })
    })
}

/// A Python function decorated `@pytest.fixture` / `@fixture`, with or
/// without arguments.
fn is_pytest_fixture(f: &Fragment) -> bool {
    f.path().ends_with(".py")
        && f.content
            .lines()
            .map(str::trim_start)
            .take_while(|l| l.is_empty() || l.starts_with('@'))
            .any(|l| {
                let d = l.trim_start_matches('@');
                d.starts_with("pytest.fixture") || d.starts_with("fixture")
            })
}

/// The line of `f`'s own `def` whose parameter list names `name`: how a
/// test or a fixture asks pytest for a fixture.
fn names_parameter(f: &Fragment, name: &str) -> Option<u32> {
    let header = f.content.lines().position(|l| {
        let t = l.trim_start();
        t.starts_with("def ") || t.starts_with("async def ")
    })?;
    let signature: String = f
        .content
        .lines()
        .skip(header)
        .take(6)
        .collect::<Vec<_>>()
        .join(" ");
    let params = signature.split_once('(')?.1.split_once(')')?.0;
    params
        .split(',')
        .map(|p| p.trim().split([':', '=']).next().unwrap_or("").trim())
        .any(|p| p == name)
        .then(|| f.start_line() + header as u32)
}

/// How a definition is entered by something that is no caller: its module's
/// own script guard, or a curried registration call (`tool("name")(fn)`).
enum Entered {
    Script,
    Registered(String),
}

/// A statement of `f` that enters the definition `name` of its own file:
/// the module's `if __name__ == "__main__":` guard calling it, or a curried
/// registration call naming it. Both are how it is run, not who calls it.
fn entered_by(f: &Fragment, name: &str) -> Option<Entered> {
    static CURRIED: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
        regex::Regex::new(r"^\s*([A-Za-z_][\w.]*\([^()]*\))\(\s*([A-Za-z_][\w.]*)\s*\)\s*$")
            .expect("curried registration regex")
    });
    if !f.path().ends_with(".py") && !f.path().ends_with(".pyi") {
        return None;
    }
    let first = f.content.lines().find(|l| !l.trim().is_empty())?.trim();
    let guard = first.starts_with("if __name__ ==")
        && (first.contains("\"__main__\"") || first.contains("'__main__'"));
    if guard {
        return f
            .content
            .lines()
            .any(|l| calls_word(l, name))
            .then_some(Entered::Script);
    }
    f.content.lines().find_map(|l| {
        let caps = CURRIED.captures(l)?;
        (&caps[2] == name).then(|| Entered::Registered(caps[1].to_string()))
    })
}

/// The class of a `do_<METHOD>` handler a standard-library HTTP server
/// dispatches by method name: the base it names.
fn http_handler_base(core: &Fragment, file: &[&Fragment]) -> Option<&'static str> {
    let name = base_symbol(core.symbol_name.as_deref()?);
    let method = name.strip_prefix("do_")?;
    if method.is_empty() || !method.chars().all(|c| c.is_ascii_uppercase()) {
        return None;
    }
    let class = enclosing_class(core, file)?;
    let supers = supertypes(&class.content, "py");
    [
        "BaseHTTPRequestHandler",
        "SimpleHTTPRequestHandler",
        "CGIHTTPRequestHandler",
    ]
    .into_iter()
    .find(|base| supers.iter().any(|s| s == base))
}

/// `main` of a program in a language whose runtime calls it.
fn is_program_main(core: &Fragment) -> bool {
    let Some(name) = core.symbol_name.as_deref().map(base_symbol) else {
        return false;
    };
    let ext = Path::new(core.path())
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "go" | "java" | "kt" | "kts" | "scala" | "rs" | "c" | "cc" | "cpp" | "swift" => {
            name == "main"
        }
        "cs" => name == "Main",
        _ => false,
    }
}

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
    let setup = crate::profile::phase("impact.setup");
    let graph = &state.scoring_result.graph;
    // A signature variant as long as its fragment shares its id (a Kotlin
    // class header ending `) {`); the id names the fragment, not its stub.
    let mut by_id: FxHashMap<&FragmentId, &Fragment> = FxHashMap::default();
    for f in &state.all_fragments {
        by_id
            .entry(&f.id)
            .and_modify(|seen| {
                if seen.kind.is_signature() && !f.kind.is_signature() {
                    *seen = f;
                }
            })
            .or_insert(f);
    }
    let changed_paths: FxHashSet<&str> = state
        .changed_files
        .iter()
        .filter_map(|p| p.to_str())
        .collect();
    let tests = TestIndex::build(state, state.analysed_range.as_deref().or(diff_range));
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
    let mut untested_unlisted: Vec<String> = Vec::new();
    let mut untested_called_in_diff: Vec<String> = Vec::new();
    let mut entry_unlisted: Vec<String> = Vec::new();
    let mut layout_only: Vec<String> = Vec::new();
    let mut traced_definitions = 0usize;

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
    let base_texts = BaseTexts::new(&state.root_dir);

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
    let universe: FxHashSet<String> = read_or_note(state, source.list_paths(&state.root_dir))
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
    let one_tree = |a: &str, b: &str| {
        let (pa, pb) = (project_of(a), project_of(b));
        nests(&pa, &pb) || nests(&pb, &pa)
    };
    let one_project = |user: &Fragment, used: &Fragment| {
        if one_tree(user.path(), used.path()) {
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
    // Files the range deleted or renamed away are files an import may still
    // name: resolved, a remaining import of one is a broken caller, not an
    // external package.
    let registry = Registry::for_analysis(
        &state.root_dir,
        &source,
        universe
            .into_iter()
            .chain(state.deleted_files.iter().cloned())
            .chain(state.renamed_files.iter().map(|(old, _)| old.clone()))
            .collect(),
    );
    let file_of = |f: &Fragment| files.fragments.get(f.path()).map_or(&[][..], Vec::as_slice);
    // The definition a fragment stands for, as the model of its language
    // resolved it.
    let definition = |model: &dyn LanguageFacts, f: &Fragment| {
        let path = rel(state, f.path());
        model.definition(&At {
            path: &path,
            frag: f,
            file: file_of(f),
        })
    };
    let finding = |user: &Fragment, used: &Fragment| -> Option<Finding> {
        let model = registry.for_pair(user.path(), used.path())?;
        let def = definition(model, used)?;
        let path = rel(state, user.path());
        model
            .classify(
                &UseSite {
                    path: &path,
                    lines: (user.start_line(), user.end_line()),
                    frag: Some((user, file_of(user))),
                },
                def,
            )
            .filter(|f| !untyped_guess(f) || one_tree(user.path(), used.path()))
    };
    let registration = |f: &Fragment| -> Option<String> {
        if is_jvm_path(f.path()) {
            let annotations = registry
                .for_path(f.path())
                .and_then(|m| m.annotations(definition(m, f)?));
            jvm_entry_point(f, file_of(f), annotations.as_deref())
        } else {
            registering_decorator(f, file_of(f))
        }
    };
    let modelled = |a: &Fragment, b: &Fragment| registry.for_pair(a.path(), b.path()).is_some();
    let link = |user: &Fragment, used: &Fragment| -> bool {
        let Some(name) = used.symbol_name.as_deref().map(base_symbol) else {
            return false;
        };
        if !same_runtime(user.path(), used.path()) {
            return false;
        }
        if modelled(user, used) {
            finding(user, used).is_some_and(|f| f.evidence == Evidence::Resolved)
        } else {
            one_project(user, used)
                && mentions(user, Some(name), &Receiver::Any)
                && references_module(user, &files, Some(name), used.path())
        }
    };
    let reaches_file = |test: &Fragment, caller: &Fragment| -> bool {
        if !same_runtime(test.path(), caller.path()) {
            return false;
        }
        match registry.for_pair(test.path(), caller.path()) {
            Some(model) => model.reaches_file(&rel(state, test.path()), &rel(state, caller.path())),
            None => {
                one_project(test, caller) && references_module(test, &files, None, caller.path())
            }
        }
    };
    // A field changes its class too: how the class is built (a dataclass's
    // constructor, a column's default) is its callers' business, and what
    // reads the field is the field's (#388, #488); so does an attribute the
    // method around it sets (#426). A query names one definition.
    let holding: Vec<&Fragment> = cores
        .iter()
        .filter(|f| query.is_none() && f.kind == FragmentKind::Property)
        .filter_map(|f| {
            file_of(f)
                .iter()
                .copied()
                .filter(|c| {
                    c.id != f.id
                        && (c.kind.is_container() || c.kind == FragmentKind::Function)
                        && c.symbol_name.is_some()
                        && within(&f.id, c)
                })
                .min_by_key(|c| c.line_count())
        })
        .collect();
    let mut cores = innermost(cores);
    cores.extend(holding);
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
    // A constructor is called by its class's name (`cp.ComponentModel(b, 1)`,
    // `new Store()`): its callers are the class's (#318). Mapped after the
    // bordering test, which a class header with no hunk of its own would fail.
    let cores: Vec<&Fragment> = {
        let mut seen: FxHashSet<&FragmentId> = FxHashSet::default();
        cores
            .into_iter()
            .map(|c| {
                let constructor = c
                    .symbol_name
                    .as_deref()
                    .map(base_symbol)
                    .is_some_and(|s| matches!(s, "__init__" | "constructor"));
                constructor
                    .then(|| enclosing_class(c, file_of(c)))
                    .flatten()
                    .unwrap_or(c)
            })
            .filter(|c| seen.insert(&c.id))
            .collect()
    };
    // A queried name has no diff: a call from elsewhere in its own file is a
    // caller like any other.
    let inside_the_diff = |id: &FragmentId| {
        (state.core_ids.contains(id) && !bordering.contains(id))
            || (query.is_none() && in_a_hunk(id))
    };
    let untraced = if query.is_none() {
        UntracedNamers::build(state, &cores, diff_range)
    } else {
        UntracedNamers::default()
    };
    drop(setup);
    let callers = crate::profile::phase("impact.callers");
    for (i, core) in cores.iter().enumerate() {
        // Past the deadline the symbols already answered are the answer:
        // `limits` then says it is partial, and a mass reformat returns what
        // it found instead of nothing (#382).
        if !state.run.check() {
            break;
        }
        let core_hunks = hunks_in(core);
        // A formatter run or a comment changes nothing a caller sees (#370),
        // and neither does a definition moved verbatim within its file.
        if query.is_none()
            && !core_hunks.is_empty()
            && core_hunks
                .iter()
                .all(|h| h.layout || moved_verbatim(h, &state.hunk_texts))
        {
            layout_only.push(label(
                &rel(state, core.path()),
                core.symbol_name.as_deref(),
                &lines_of(&core.id),
            ));
            continue;
        }
        if query.is_none() && bordering.contains(&core.id) {
            continue;
        }
        traced_definitions += 1;
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
        let binding_model = symbol.is_some() && registry.for_path(core.path()).is_some();
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
        // Sources the diff holds go through the same tests as any caller: one
        // that passes is no caller outside the diff, but it makes the
        // definition live rather than one nothing calls (#441).
        let mut in_diff: FxHashSet<FragmentId> = FxHashSet::default();
        // Every fragment with an edge into the core, kept or not: a capped
        // fragment that names it without one is where the edge cap can have
        // hidden a caller.
        let mut linked: FxHashSet<FragmentId> = FxHashSet::default();
        // Fragments outside the diff that read the member through a receiver
        // nothing types: counted, since such a read may or may not be one.
        let mut unresolved_readers: FxHashSet<FragmentId> = FxHashSet::default();
        // Whether a fragment that reaches the core is a caller, and with
        // what evidence; the reason it is not, otherwise.
        let judge = |frag: &Fragment| -> Result<Option<Finding>, &'static str> {
            // A file that defines the same name calls its own.
            let shadowed = frag.path() != core.path()
                && symbol.is_some_and(|s| defined_in.contains(&(s.to_string(), frag.path())));
            if frag.kind == FragmentKind::Excerpt {
                Err("excerpt")
            } else if !same_runtime(frag.path(), core.path()) {
                Err("other_runtime")
            } else if shadowed && !binding_model {
                Err("defines_same_name")
            } else if is_import_block(&frag.content) {
                Err("import_block")
            } else if !binding_model
                && symbol.is_some_and(|s| {
                    binds_locally(&code_text(frag.path(), &frag.content), base_symbol(s))
                })
            {
                // `let read = |p| p + 1; read(1)` calls its own closure (#391).
                Err("local_binding")
            } else if !is_code_file(frag.path()) {
                Err("not_code")
            } else if binding_model && modelled(frag, core) {
                match finding(frag, core) {
                    Some(f) if f.evidence == Evidence::Candidate(bindings::UNRESOLVED_READ) => {
                        Err(bindings::UNRESOLVED_READ)
                    }
                    found => found.map(Some).ok_or("binding_rejected"),
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
                Err("no_mention_in_code")
            } else if !references_module(frag, &files, symbol, core.path()) {
                Err("no_module_reference")
            } else {
                Ok(None)
            }
        };
        graph.for_each_reverse_neighbor(&core.id, |src, weight| {
            linked.insert(src.clone());
            // A signature variant stands for its fragment: the caller is the
            // whole definition, and its call sites sit in the body.
            let src = by_id
                .get(src)
                .filter(|f| f.kind.is_signature())
                .and_then(|sig| {
                    file_of(sig)
                        .iter()
                        .filter(|f| {
                            !f.kind.is_signature()
                                && f.start_line() == sig.start_line()
                                && f.end_line() >= sig.end_line()
                        })
                        .min_by_key(|f| f.line_count())
                })
                .map_or(src, |f| &f.id);
            if inside_the_diff(src) {
                in_diff.insert(src.clone());
            }
            // Ambiguity is across files: inside the definition's own file a
            // name is that file's own, as a file defining the same name
            // calls its own (#538).
            let reason = if ambiguous && src.path != core.id.path {
                "ambiguous_name"
            } else if within(src, core) {
                "own_body"
            } else if let Some((frag, found)) = by_id
                .get(src)
                .filter(|f| {
                    binding_model
                        && modelled(f, core)
                        && f.kind != FragmentKind::Excerpt
                        && !is_import_block(&f.content)
                })
                .and_then(|f| {
                    finding(f, core)
                        .filter(|x| x.evidence == Evidence::Resolved)
                        .map(|x| (*f, x))
                })
            {
                // A resolved binding is stronger evidence than which way the
                // edge between the two fragments happens to weigh more.
                sources.push((frag, weight, Some(found)));
                return;
            } else if let Some(frag) = by_id
                .get(src)
                .filter(|f| in_diff.contains(&f.id) && judge(f).is_ok())
            {
                // A caller inside the diff is never listed: it decides only
                // whether the definition reads as called, and how the
                // ranking graph weighs the edge is no evidence there (#552).
                sources.push((frag, weight, None));
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
                match judge(frag) {
                    Ok(found) => {
                        sources.push((frag, weight, found));
                        return;
                    }
                    Err(reason) => {
                        if reason == bindings::UNRESOLVED_READ && !inside_the_diff(&frag.id) {
                            unresolved_readers.insert(frag.id.clone());
                        }
                        reason
                    }
                }
            };
            *dropped.entry(reason).or_default() += 1;
        });
        // The per-node edge cap keeps a fragment's heaviest out-edges, so a
        // caller that references many names can lose its edge to the changed
        // definition, and the answer read "0 callers" (#446, Forgejo #68). A
        // capped fragment naming the definition faces the same tests as an
        // edge would have; models that read uses off the syntax do so below.
        // The definition's own file is no exception: a long function there
        // calling it is capped like any other (#547).
        if !ambiguous && !registry.answers(core.path(), lang::Relation::Uses) {
            let word = symbol.map(str::to_lowercase).unwrap_or_default();
            let capped = &graph.cap_stats.capped_sources;
            for frag in files
                .fragments
                .values()
                .flat_map(|frags| frags.iter().copied())
                .filter(|f| {
                    capped.contains(&f.id)
                        && !linked.contains(&f.id)
                        && !within(&f.id, core)
                        && f.identifiers.contains(&word)
                })
            {
                match judge(frag) {
                    Ok(found) => {
                        if inside_the_diff(&frag.id) {
                            in_diff.insert(frag.id.clone());
                        }
                        sources.push((frag, 0.0, found));
                    }
                    Err(bindings::UNRESOLVED_READ) if !inside_the_diff(&frag.id) => {
                        unresolved_readers.insert(frag.id.clone());
                    }
                    Err(_) => {}
                }
            }
        }
        let kept = sources.len();
        sources.retain(|(f, _, _)| !in_diff.contains(&f.id));
        let mut called_in_diff = sources.len() < kept;
        if called_in_diff {
            dropped.insert("inside_the_diff", kept - sources.len());
        }
        // A pytest fixture is called by the runner, for every test or fixture
        // that names it as a parameter — in its own file or under the
        // directory of the conftest that defines it (#401).
        if let Some(name) = symbol.filter(|_| is_pytest_fixture(core)) {
            let scope = Path::new(core.path()).parent().map(Path::to_path_buf);
            let listed: FxHashSet<FragmentId> =
                sources.iter().map(|(f, _, _)| f.id.clone()).collect();
            let users: Vec<(&Fragment, u32)> = files
                .fragments
                .iter()
                .filter(|(path, _)| {
                    **path == core.path()
                        || (Path::new(core.path()).file_name() == Some("conftest.py".as_ref())
                            && scope
                                .as_ref()
                                .is_some_and(|d| Path::new(path).starts_with(d)))
                })
                .flat_map(|(_, frags)| frags.iter().copied())
                .filter(|f| f.id != core.id && !listed.contains(&f.id))
                .filter_map(|f| names_parameter(f, base_symbol(name)).map(|line| (f, line)))
                .collect();
            for (frag, line) in users {
                if inside_the_diff(&frag.id) {
                    called_in_diff = true;
                    continue;
                }
                let weight = graph.forward_edge_weight(&frag.id, &core.id).unwrap_or(0.0);
                sources.push((
                    frag,
                    weight,
                    Some(Finding {
                        evidence: Evidence::Resolved,
                        relation: Relation::Reference,
                        lines: vec![line],
                    }),
                ));
            }
        }
        // A model that reads uses off the syntax finds a caller the graph
        // never linked — a call through an inherited receiver, through the
        // implicit `this` of the class, a function the edge cap dropped — and
        // implementations of an abstract member are their own relation.
        let core_def = registry
            .for_path(core.path())
            .filter(|m| m.modelled().contains(&lang::Relation::Uses))
            .and_then(|m| Some((m, definition(m, core)?)));
        let use_lines = |path: &str| -> Vec<u32> {
            match core_def {
                Some((model, def)) if model.covers(path) => {
                    model.use_lines(&rel(state, path), def).unwrap_or_default()
                }
                _ => Vec::new(),
            }
        };
        if let Some((model, def)) = core_def {
            let listed: FxHashSet<FragmentId> =
                sources.iter().map(|(f, _, _)| f.id.clone()).collect();
            let mut seen = listed;
            let mut found: Vec<(&Fragment, Finding)> = Vec::new();
            for (path, frags) in &files.fragments {
                for line in use_lines(path) {
                    let Some(frag) = frags
                        .iter()
                        .filter(|f| {
                            f.start_line() <= line
                                && f.end_line() >= line
                                && f.kind != FragmentKind::Excerpt
                                && !f.kind.is_signature()
                        })
                        .min_by_key(|f| f.line_count())
                    else {
                        continue;
                    };
                    if within(&frag.id, core) {
                        continue;
                    }
                    if inside_the_diff(&frag.id) {
                        called_in_diff = called_in_diff || finding(frag, core).is_some();
                        continue;
                    }
                    if !seen.insert(frag.id.clone()) {
                        continue;
                    }
                    match finding(frag, core) {
                        Some(f) if f.evidence == Evidence::Candidate(bindings::UNRESOLVED_READ) => {
                            unresolved_readers.insert(frag.id.clone());
                        }
                        Some(f) => found.push((frag, f)),
                        None => {}
                    }
                }
            }
            let implementers = if model.modelled().contains(&lang::Relation::Implementations) {
                let paths: Vec<String> = files
                    .fragments
                    .keys()
                    .filter(|p| model.covers(p))
                    .map(|p| rel(state, p))
                    .collect();
                model.implementers(def, &paths).unwrap_or_default()
            } else {
                Vec::new()
            };
            for imp in implementers {
                let abs = state.root_dir.join(&imp.path);
                let Some(frag) = files
                    .fragments
                    .iter()
                    .find(|(p, _)| Path::new(p) == abs.as_path())
                    .and_then(|(_, frags)| {
                        frags.iter().copied().find(|f| {
                            f.symbol_name.as_deref() == Some(imp.name.as_str())
                                && f.start_line() <= imp.start_line
                                && f.end_line() >= imp.start_line
                        })
                    })
                else {
                    continue;
                };
                if (query.is_none() && in_a_hunk(&frag.id)) || !seen.insert(frag.id.clone()) {
                    continue;
                }
                found.push((
                    frag,
                    Finding {
                        evidence: Evidence::Resolved,
                        relation: Relation::Implementation,
                        lines: vec![imp.start_line],
                    },
                ));
            }
            for (frag, f) in found {
                let weight = graph.forward_edge_weight(&frag.id, &core.id).unwrap_or(0.0);
                sources.push((frag, weight, Some(f)));
            }
        }
        // A local `const items = rows(xs)` is a line of its function, not a
        // caller of its own: the function is, with its test link (#452).
        // An anonymous arrow (`.map((p) => …)`) is a line of the definition
        // around it as well (#520), and a field of the class holding it. So
        // is a chunk of a long function (`main[88]`), which no test edge
        // reaches: its calls arrive at the function (#579).
        for source in &mut sources {
            let anonymous =
                source.0.kind == FragmentKind::Function && source.0.symbol_name.is_none();
            let field = source.0.kind == FragmentKind::Property;
            let chunk = source.0.kind == FragmentKind::Chunk;
            if source.0.kind != FragmentKind::Variable && !anonymous && !field && !chunk {
                continue;
            }
            let parent = source.0.symbol_name.as_deref().map(base_symbol);
            let enclosing = file_of(source.0)
                .iter()
                .filter(|f| {
                    (f.kind == FragmentKind::Function
                        || (anonymous && f.kind == FragmentKind::Variable)
                        || (field && f.kind.is_container())
                        || (chunk
                            && f.kind != FragmentKind::Chunk
                            && parent.is_none_or(|p| f.symbol_name.as_deref() == Some(p))))
                        && f.symbol_name.is_some()
                        && f.id != source.0.id
                        && f.start_line() <= source.0.start_line()
                        && f.end_line() >= source.0.end_line()
                })
                .min_by_key(|f| f.line_count());
            if let Some(f) = enclosing {
                source.0 = f;
            }
        }
        // Two lines lifted into one function are one caller with both sites.
        let mut lifted: Vec<(&Fragment, f64, Option<Finding>)> = Vec::with_capacity(sources.len());
        for (frag, weight, found) in sources {
            match lifted.iter_mut().find(|(f, _, _)| f.id == frag.id) {
                Some((_, w, known)) => {
                    *w = w.max(weight);
                    match (known.as_mut(), found) {
                        (Some(k), Some(f)) => {
                            if f.evidence == Evidence::Resolved {
                                k.evidence = Evidence::Resolved;
                            }
                            k.lines.extend(f.lines);
                            k.lines.sort_unstable();
                            k.lines.dedup();
                        }
                        (None, f) => *known = f,
                        _ => {}
                    }
                }
                None => lifted.push((frag, weight, found)),
            }
        }
        sources = lifted;
        sources.retain(|(f, _, _)| !within(&f.id, core));
        // The module's own guard and a registration call run the definition;
        // they are no callers of it (R3).
        let entered = symbol.and_then(|name| {
            file_of(core)
                .iter()
                .find_map(|f| entered_by(f, base_symbol(name)))
        });
        if let Some(name) = symbol.filter(|_| entered.is_some()) {
            sources.retain(|(f, _, _)| {
                f.path() != core.path() || entered_by(f, base_symbol(name)).is_none()
            });
        }
        let kept = sources.len();
        sources.retain(|(f, _, _)| !inside_the_diff(&f.id));
        called_in_diff = called_in_diff || sources.len() < kept;
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
        // A name match links an import line too, at the import weight; the
        // function that makes the call is the caller, the import is how it got
        // the name. A binding finding skips import lines, so the module-level
        // code it names calls the definition beside that function (#449).
        let named_files: FxHashSet<&str> = sources
            .iter()
            .filter(|(f, _, _)| f.symbol_name.is_some())
            .map(|(f, _, _)| f.path())
            .collect();
        sources.retain(|(f, _, found)| {
            f.symbol_name.is_some() || found.is_some() || !named_files.contains(f.path())
        });
        let callers: Vec<Caller> = innermost(sources.iter().map(|(f, _, _)| *f).collect())
            .into_iter()
            .map(|frag| {
                let tested = tests.tested_by(frag, state, &by_id, &link);
                let found = findings.get(&frag.id);
                let registered_by = tested.is_none().then(|| registration(frag)).flatten();
                Caller {
                    path: rel(state, frag.path()),
                    symbol: frag.symbol_name.clone(),
                    lines: lines_of(&frag.id),
                    weight: (weights[&frag.id] * 1e3).round() / 1e3,
                    file_tested: tested.is_none()
                        && registered_by.is_none()
                        && tests.file_tested(frag, &reaches_file),
                    unread_test: tested
                        .is_none()
                        .then(|| tests.unread_naming(frag, &rel(state, frag.path())))
                        .flatten(),
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
                        Some(Relation::Implementation) => "implementation",
                        _ => "call",
                    },
                    // A word match cites the line that spells the name, not
                    // the start of the function around it.
                    sites: match found {
                        Some(f) => f.lines.clone(),
                        None => symbol
                            .and_then(|name| first_naming_line(frag, base_symbol(name)))
                            .into_iter()
                            .collect(),
                    },
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
        let public_change = match registry
            .for_path(core.path())
            .filter(|m| m.modelled().contains(&lang::Relation::PublicApi))
        {
            Some(model) => definition(model, core)
                .is_some_and(|def| model.public_change(def, &base).unwrap_or(false)),
            None => {
                is_public(core.path(), symbol, without_decorators(&core.content))
                    && lang::rust::module_is_public(&state.run.source(), core.path())
                    && signature_changed(state, &base_texts, &base, core)
            }
        };
        // A test's declarations are no API: nothing but its runner calls
        // them (#517).
        if query.is_none() && public_change && !tests.is_test(core) {
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
        let route = callers
            .is_empty()
            .then(|| dispatch_route(state, core, file_of(core)))
            .flatten();
        // The role that gives the definition an implicit caller (R3): where a
        // static test link exists it is shown, otherwise the role is named.
        let test_link = |frag| {
            tests
                .tested_by(frag, state, &by_id, &link)
                .map(|(test, via)| {
                    let test = rel(state, &test);
                    match via {
                        Some(via) => format!("reachable from tests: {test} via {via}"),
                        None => format!("reachable from tests: {test}"),
                    }
                })
        };
        let mut linkless_role = false;
        let entry: Option<String> = if tests.is_test(core) {
            Some("test (run by the test runner)".to_string())
        } else if matches!(entered, Some(Entered::Script)) {
            Some("run as a script (if __name__ == \"__main__\")".to_string())
        } else if let Some(by) = registration(core).or_else(|| match &entered {
            Some(Entered::Registered(call)) => Some(call.clone()),
            _ => None,
        }) {
            Some(match test_link(core) {
                Some(link) => format!("registered by `{by}`; {link}"),
                None => format!(
                    "registered by `{by}` \u{2014} reached through the framework, not traced"
                ),
            })
        } else if let Some(base) = http_handler_base(core, file_of(core)) {
            // The class link is modelled: no test reaching it is a finding,
            // not an unmodelled gap, so the role does not excuse it.
            let link = enclosing_class(core, file_of(core)).and_then(test_link);
            Some(match link {
                Some(link) => format!("dispatched by `{base}`; {link}"),
                None => {
                    linkless_role = true;
                    format!("dispatched by `{base}`")
                }
            })
        } else if is_program_main(core) {
            Some(match test_link(core) {
                Some(link) => format!("program entry point; {link}"),
                // A test that runs the binary is modelled for Rust: none
                // reaching `main` is a finding there, not an unmodelled gap.
                None if core.path().ends_with(".rs") => {
                    linkless_role = true;
                    "program entry point".to_string()
                }
                None => "program entry point".to_string(),
            })
        } else {
            None
        };
        let unresolved = core_def
            .and_then(|(model, def)| model.dispatched(def))
            .map(|trait_name| Unknown::Dispatched { trait_name })
            .or_else(|| {
                symbol.filter(|_| ambiguous).map(|s| Unknown::Ambiguous {
                    name: s.to_string(),
                    definitions: definitions
                        .get(&(core_lang, s.to_lowercase()))
                        .copied()
                        .unwrap_or(0),
                    language: core_lang,
                })
            })
            .or_else(|| {
                let reads = unresolved_readers.len();
                (reads > 0).then(|| Unknown::UnresolvedReads {
                    name: symbol.map(base_symbol).unwrap_or("").to_string(),
                    reads,
                })
            })
            .or_else(|| route.clone().map(|via| Unknown::Overridable { via }))
            .or_else(|| {
                (callers.is_empty()
                    && is_code_file(core.path())
                    && !registry.answers(core.path(), lang::Relation::Callers))
                .then(|| Unknown::NotModelled {
                    language: if core_lang.is_empty() {
                        "this language"
                    } else {
                        core_lang
                    },
                })
            });
        // An entry point is called by its runner or framework: the hedge for
        // callers nothing traces would be a false unknown.
        let unresolved = unresolved.filter(|u| {
            entry.is_none()
                || !matches!(
                    u,
                    Unknown::Overridable { .. }
                        | Unknown::NotModelled { .. }
                        | Unknown::Dispatched { .. }
                )
        });
        // A method only dispatch reaches is reached through its class: a test
        // handing the class to the runtime (a server, a test runner, a
        // visitor) reaches the method (#578). A caller a test calls is the
        // second hop of a test link: a caller read off the syntax has no
        // graph edge for the walk to take (#503).
        let untested = query.is_none()
            && tests.any()
            && !tests.is_test(core)
            && (entry.is_none() || linkless_role)
            && !callers.iter().any(|c| {
                c.evidence != "candidate" && c.tested_by.is_some() && c.tested_via.is_none()
            })
            && tests.tested_by(core, state, &by_id, &link).is_none()
            && !route
                .and_then(|_| enclosing_class(core, file_of(core)))
                .is_some_and(|class| tests.tested_by(class, state, &by_id, &link).is_some());
        untested_definitions += usize::from(untested);
        let compatible_callers = if compatible {
            let n = callers.iter().filter(|c| c.evidence != "candidate").count();
            callers.clear();
            n
        } else {
            0
        };
        // A file that cannot name the definition cannot call it, read or
        // not: a function local to another (#546), a private Rust item
        // outside its module (#599).
        let model_def = registry
            .for_path(core.path())
            .and_then(|m| Some((m, definition(m, core)?)));
        let mut missed_in: Vec<UnanalysedFile> = symbol
            .map(|s| missed_namers(state, &files, &registry, s, core.path()))
            .unwrap_or_default()
            .into_iter()
            .filter(|f| model_def.is_none_or(|(m, def)| m.names_from(def, &f.path)))
            .collect();
        if callers.is_empty()
            && compatible_callers == 0
            && unresolved.is_none()
            && missed_in.is_empty()
        {
            missed_in = untraced
                .of(core)
                .into_iter()
                .filter(|f| model_def.is_none_or(|(m, def)| m.names_from(def, &f.path)))
                .collect();
        }
        if callers.is_empty()
            && compatible_callers == 0
            && commits < 2
            && query.is_none()
            && unresolved.is_none()
            && missed_in.is_empty()
        {
            // Counted in the header, so named: a count of nothing listed
            // tells the reader neither where nor what (#398).
            let named = label(
                &rel(state, core.path()),
                core.symbol_name.as_deref(),
                &lines_of(&core.id),
            );
            if untested {
                let named = match entry.as_ref().filter(|_| linkless_role) {
                    Some(role) => format!("{named} \u{2014} {role}"),
                    None => named,
                };
                if called_in_diff {
                    untested_called_in_diff.push(named);
                } else {
                    untested_unlisted.push(named);
                }
            } else if let Some(role) = entry.as_ref().filter(|_| !tests.is_test(core)) {
                entry_unlisted.push(format!("{named} \u{2014} {role}"));
            }
            continue;
        }
        changed.push(ChangedSymbol {
            path: rel(state, core.path()),
            symbol: core.symbol_name.clone(),
            lines: lines_of(&core.id),
            kind: format!("{:?}", core.kind).to_lowercase(),
            commits,
            callers,
            entry,
            unresolved,
            untested,
            compatible_callers,
            missed_in,
        });
    }
    drop(callers);
    let removed = crate::profile::phase("impact.removed");
    if query.is_none() && state.run.check() {
        let mut defined: FxHashMap<&str, FxHashSet<String>> = FxHashMap::default();
        for (n, p) in &defined_in {
            defined.entry(n.as_str()).or_default().insert(rel(state, p));
        }
        let head_rev = analysed
            .map(crate::git::split_diff_range)
            .and_then(|(_, head)| head);
        let binds_to = |file: &str, text: &str, name: &str, old: &str| {
            registry
                .for_path(file)
                .and_then(|m| m.binds(file, text, name, old))
                .unwrap_or(false)
        };
        let read_off_facts = |path: &str| registry.answers(path, lang::Relation::Removals);
        let from_facts = removed_by_models(state, &registry, &base, head_rev.as_deref(), &one_tree);
        for (sym, contract) in removed_definitions(
            state,
            &defined,
            &base,
            head_rev.as_deref(),
            &in_a_hunk,
            &binds_to,
            &read_off_facts,
            &one_tree,
        )
        .into_iter()
        .chain(from_facts)
        {
            let mut sym = sym;
            // A caller of a removed definition is tested the way any caller
            // is: a test that reaches it, or the test file it sits in (#413).
            for c in &mut sym.callers {
                let Some(site) = c.sites.first().copied() else {
                    continue;
                };
                let Some(frag) = state
                    .all_fragments
                    .iter()
                    .filter(|f| {
                        f.symbol_name == c.symbol
                            && f.start_line() <= site
                            && f.end_line() >= site
                            && rel(state, f.path()) == c.path
                    })
                    .min_by_key(|f| f.line_count())
                else {
                    continue;
                };
                let tested = tests.tested_by(frag, state, &by_id, &link);
                c.file_tested = tested.is_none() && tests.file_tested(frag, &reaches_file);
                c.tested_by = tested.as_ref().map(|(p, _)| rel(state, p));
                c.tested_via = tested.and_then(|(_, via)| via);
            }
            if let Some(c) =
                contract.filter(|c| !crate::testfiles::is_test_path(Path::new(&c.path)))
            {
                contracts.push(c);
            }
            // Unresolved is an answer too: dropped, it read as no caller.
            if !sym.callers.is_empty() || sym.unresolved.is_some() {
                changed.push(sym);
            }
        }
    }
    drop(removed);
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
    // A queried symbol changes nothing: the file holding its definition is
    // no contract that moved.
    for path in changed_files.iter().filter(|_| query.is_none()) {
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
    // Pack's rule, not a list of our own: a run-wide stop, or a per-file
    // limit on a changed file. The graph's per-node cap is routine and stays
    // out; the callers it cut an edge from are recovered per definition.
    let changed_set: FxHashSet<&str> = changed_files.iter().map(String::as_str).collect();
    let untracked: FxHashSet<String> = state
        .untracked_files
        .iter()
        .map(|p| rel(state, &p.to_string_lossy()))
        .collect();
    let (stale, stale_omitted) = if query.is_none() {
        let _stale = crate::profile::phase("impact.stale_paths");
        stale_references(state, analysed)
    } else {
        (Vec::new(), 0)
    };
    let (named_by, named_by_omitted) = if query.is_none() {
        let _named = crate::profile::phase("impact.named_by");
        let (mut named, omitted) = named_by_path(state, analysed);
        let by_module = untraced.module_refs(state, &cores, &named);
        named.extend(by_module);
        (named, omitted)
    } else {
        (Vec::new(), 0)
    };
    let (collection_reads, collection_reads_omitted) = if query.is_none() {
        read_by_collection(state, analysed)
    } else {
        (Vec::new(), 0)
    };
    let mut unanalysed = if query.is_none() {
        unanalysed_files(state, &changed_files)
    } else {
        Vec::new()
    };
    let mut named_by = named_by;
    let mut named_by_omitted = named_by_omitted;
    let (key_searches, key_files_unsearched) = {
        let untraced: Vec<String> = unanalysed
            .iter()
            .filter(|f| f.reason == "content_untraced")
            .map(|f| f.path.clone())
            .collect();
        let (refs, omitted, searches, unsearched) = key_references(state, &untraced, analysed);
        unanalysed.retain(|f| !searches.iter().any(|k| k.path == f.path));
        named_by.extend(refs);
        named_by_omitted += omitted;
        (searches, unsearched)
    };
    // Read after the last phase that can stop: a deadline the stale-reference
    // search hit was missing from an answer that had dropped its lines. A
    // changed file outside code holds no definition and calls nothing: one
    // over the cap is named in `unanalysed`, and no caller is missing for it
    // (#626).
    let (mut limits, _, _) = crate::resource::classify_limits(&state.run, &[], &|p| {
        changed_set.contains(p) && (is_code_file(p) || Path::new(p).extension().is_none())
    });
    // A changed file cut to the fragment cap keeps every fragment the diff
    // touched: pack loses context there, impact loses no changed definition,
    // and the cap fires on most large files (#306).
    limits.retain(|r| *r != crate::resource::LimitReason::FragmentLimit);
    let mut output = ImpactOutput {
        schema: IMPACT_SCHEMA,
        name: state
            .root_dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| state.root_dir.to_string_lossy().to_string()),
        range: diff_range.map(str::to_string),
        index_tree: state.index_tree.clone(),
        range_name: None,
        symbol: query.map(str::to_string),
        symbol_named: None,
        untracked_files: changed_files
            .iter()
            .filter(|f| untracked.contains(f.as_str()))
            .cloned()
            .collect(),
        changed_files: changed_files
            .into_iter()
            .filter(|f| !untracked.contains(f.as_str()))
            .collect(),
        deleted_files: state.deleted_files.clone(),
        stale_references: stale,
        stale_references_omitted: stale_omitted,
        named_by,
        named_by_omitted,
        key_searches,
        key_files_unsearched,
        collection_reads,
        collection_reads_omitted,
        commit_count: state.commit_count,
        changed,
        contracts,
        empty: false,
        truncated: 0,
        truncated_possible: 0,
        symbols_omitted: 0,
        truncated_untested: 0,
        truncated_textual: 0,
        changed_files_omitted: 0,
        untracked_files_omitted: 0,
        contracts_omitted,
        limits,
        tests_known: tests.any(),
        untested_definitions,
        untested_unlisted,
        untested_called_in_diff,
        entry_unlisted,
        layout_only,
        lockfile_changes: state.lockfile_changes.clone(),
        layout_only_omitted: 0,
        traced_definitions,
        withheld_files: state.policy_excluded_count + state.run.withheld_count(),
        unanalysed_files: unanalysed,
        unanalysed_files_omitted: 0,
        untraced_omitted: 0,
        cap: IMPACT_TOKEN_CAP,
    };
    output.empty = proves_empty(&output);
    output
}

/// Directory names every project has: naming one says nothing about which.
const GENERIC_DIRECTORIES: &[&str] = &[
    "src", "main", "java", "kotlin", "scala", "lib", "test", "tests", "app", "pkg", "internal",
    "cmd", "include", "source", "sources",
];

/// Definitions the change removed and nothing at the head defines again,
/// with the lines outside the diff that still name them (#367). They have
/// no fragment at the head, so the callers graph never reaches them: an
/// optional call (`engine.setGain?.()`) silently becomes a no-op.
/// Kotlin declarations the range removed: in the base revision of a changed
/// file and gone from it at the head, matched by owner, name, kind and
/// receiver. What still uses one is found by the binding model, at the head.
/// A read the answer depends on, or nothing with the run marked: a failed
/// `git grep` read as no match turned callers and stale references into a
/// clean zero with exit 0.
fn read_or_note<T: Default>(state: &ScoredState, read: crate::git::Result<T>) -> T {
    read.unwrap_or_else(|e| {
        tracing::warn!("impact: a git read failed ({e}); its answer is unknown");
        state.run.note(crate::resource::LimitReason::ReadFailed);
        T::default()
    })
}

/// The named definitions of each file in view, by repo-relative path: what
/// holds a line that names a removed definition. Looked up per line, the
/// path of every fragment was rebuilt for every grep hit (#424).
fn definition_holders(state: &ScoredState) -> FxHashMap<String, Vec<&Fragment>> {
    let mut holders: FxHashMap<String, Vec<&Fragment>> = FxHashMap::default();
    for f in state
        .all_fragments
        .iter()
        .filter(|f| f.kind.is_definition_kind() && f.symbol_name.is_some())
    {
        holders.entry(rel(state, f.path())).or_default().push(f);
    }
    holders
}

/// Definitions the models that read removals off their facts say the range
/// removed, with the lines at the head still using them.
fn removed_by_models(
    state: &ScoredState,
    registry: &Registry,
    base: &str,
    head: Option<&str>,
    one_tree: &dyn Fn(&str, &str) -> bool,
) -> Vec<(ChangedSymbol, Option<Contract>)> {
    // (path at the base, path at the head): a rename compares across both.
    let renamed_to: FxHashMap<&str, &str> = state
        .renamed_files
        .iter()
        .map(|(old, new)| (new.as_str(), old.as_str()))
        .collect();
    // A side over the size cap is not analysed, as fragmentation decided for
    // the head; parsing it here anyway cost seconds per MB. Its removals go
    // unread and the file is named not analysed (`unanalysed_files`).
    let cap = crate::config::limits::LIMITS.max_changed_file_size as u64;
    let head_too_large = state
        .run
        .limited_by(crate::resource::LimitReason::FileTooLarge);
    let pairs: Vec<(String, Option<String>)> = state
        .changed_files
        .iter()
        .map(|p| rel(state, &p.to_string_lossy()))
        .filter(|p| !head_too_large.contains(p))
        .map(|p| {
            let old = renamed_to
                .get(p.as_str())
                .map_or_else(|| p.clone(), |o| (*o).to_string());
            (old, Some(p))
        })
        .chain(state.deleted_files.iter().map(|p| (p.clone(), None)))
        .collect();
    let old_paths: Vec<String> = pairs.iter().map(|(old, _)| old.clone()).collect();
    let base_sizes = crate::git::blob_sizes(&state.root_dir, base, &old_paths).unwrap_or_default();
    let pairs: Vec<(String, Option<String>)> = pairs
        .into_iter()
        .filter(|(old, new)| {
            let over = base_sizes.get(old).is_some_and(|size| *size > cap);
            if over {
                // Named where its removals are reported: the head's path.
                let named = new.clone().unwrap_or_else(|| old.clone());
                state
                    .run
                    .note_file(crate::resource::LimitReason::FileTooLarge, named);
            }
            !over
        })
        .collect();
    let removed: Vec<(&dyn LanguageFacts, lang::RemovedDef)> = registry
        .models()
        .filter(|m| m.modelled().contains(&lang::Relation::Removals))
        .flat_map(|m| {
            m.removed(base, &pairs)
                .unwrap_or_default()
                .into_iter()
                .map(move |r| (m, r))
        })
        .collect();
    if removed.is_empty() {
        return Vec::new();
    }
    let names: Vec<String> = {
        let mut n: Vec<String> = removed.iter().map(|(_, d)| d.name.clone()).collect();
        n.sort();
        n.dedup();
        n
    };
    // Whole words of code files: read over every file, a common name
    // counted each withheld data file that spells it.
    let grepped = read_or_note(
        state,
        crate::git::grep_code_words(&state.root_dir, &names, head, MAX_REMOVED_NAME_HITS + 1),
    );
    state.run.note_withheld(grepped.withheld);
    let hits = grepped.rows;
    let holders = definition_holders(state);
    let changed: FxHashSet<&str> = pairs.iter().filter_map(|(_, new)| new.as_deref()).collect();
    let mut out = Vec::new();
    for (model, def) in removed {
        if !state.run.check() {
            break;
        }
        let mut lines_by_file: FxHashMap<&str, Vec<u32>> = FxHashMap::default();
        for (path, line, text) in &hits {
            if model.covers(path) && names_word(text, &def.name) {
                lines_by_file.entry(path.as_str()).or_default().push(*line);
            }
        }
        // As for a name the grammar reads off removed lines: on this many
        // lines it is a common word, and each would be classified in turn.
        let spelled: usize = lines_by_file.values().map(Vec::len).sum();
        let unresolved = (spelled > MAX_REMOVED_NAME_HITS).then(|| Unknown::CommonWord {
            name: def.name.clone(),
            lines: spelled,
        });
        if unresolved.is_some() {
            lines_by_file.clear();
        }
        let mut callers: Vec<Caller> = Vec::new();
        for (path, lines) in &lines_by_file {
            for &line in lines {
                let site = UseSite {
                    path,
                    lines: (line, line),
                    frag: None,
                };
                let Some(found) = model.classify(&site, def.def) else {
                    continue;
                };
                if untyped_guess(&found) && !one_tree(path, &def.path) {
                    continue;
                }
                let holder = holders
                    .get(*path)
                    .into_iter()
                    .flatten()
                    .filter(|f| f.start_line() <= line && f.end_line() >= line)
                    .min_by_key(|f| f.line_count());
                callers.push(Caller {
                    path: path.to_string(),
                    symbol: holder.and_then(|f| f.symbol_name.clone()),
                    lines: holder.map_or_else(|| format!("{line}-{line}"), |f| lines_of(&f.id)),
                    weight: 1.0,
                    tested_by: None,
                    tested_via: None,
                    file_tested: false,
                    registered_by: None,
                    unread_test: None,
                    evidence: match found.evidence {
                        Evidence::Resolved => "resolved",
                        _ => "candidate",
                    },
                    reason: match found.evidence {
                        Evidence::Candidate(r) => Some(r),
                        _ => None,
                    },
                    relation: match found.relation {
                        Relation::Reference => "reference",
                        Relation::Implementation => "implementation",
                        Relation::Call => "call",
                    },
                    sites: vec![line],
                });
            }
        }
        // Moved, not removed, when nothing still reaches it where it was
        // and a use reaches the name the range now declares in another
        // file: every importer names the new path (#540). A move whose
        // callers still name the old module stays a removal, with them.
        let reaches_moved = |frag: &Fragment, frags: &[&Fragment], path: &str| {
            model
                .definition(&lang::At {
                    path,
                    frag,
                    file: frags,
                })
                .is_some_and(|moved| {
                    lines_by_file.iter().any(|(at, lines)| {
                        lines.iter().any(|&line| {
                            let site = UseSite {
                                path: at,
                                lines: (line, line),
                                frag: None,
                            };
                            model
                                .classify(&site, moved)
                                .is_some_and(|f| f.evidence == Evidence::Resolved)
                        })
                    })
                })
        };
        let moved = callers.is_empty()
            && holders
                .iter()
                .filter(|(path, _)| {
                    **path != def.path && changed.contains(path.as_str()) && model.covers(path)
                })
                .any(|(path, frags)| {
                    frags.iter().any(|f| {
                        f.symbol_name.as_deref().map(base_symbol) == Some(def.name.as_str())
                            && reaches_moved(f, frags, path)
                    })
                });
        if moved {
            continue;
        }
        let contract = def.public.then(|| Contract {
            path: def.path.clone(),
            symbol: Some(def.name.clone()),
            kind: "removed_api",
        });
        out.push((
            ChangedSymbol {
                path: def.path,
                symbol: Some(def.name),
                lines: format!("{}-{}", def.lines.0, def.lines.1),
                kind: "removed".to_string(),
                commits: 0,
                callers: merge_by_symbol(callers),
                entry: None,
                unresolved,
                untested: false,
                compatible_callers: 0,
                missed_in: Vec::new(),
            },
            contract,
        ));
    }
    out
}

fn removed_definitions(
    state: &ScoredState,
    defined: &FxHashMap<&str, FxHashSet<String>>,
    base: &str,
    head: Option<&str>,
    in_a_hunk: &dyn Fn(&FragmentId) -> bool,
    binds_to: &dyn Fn(&str, &str, &str, &str) -> bool,
    read_off_facts: &dyn Fn(&str) -> bool,
    one_tree: &dyn Fn(&str, &str) -> bool,
) -> Vec<(ChangedSymbol, Option<Contract>)> {
    // Read off the grammar, the file at the base against the file at the
    // head: a removed definition is one the base declared and the head does
    // not. Read off removed lines by regex, a match arm became a removed
    // member named `Err`, a call statement a removed API, and an indented
    // line of a free function a member of the nearest class above it (#515).
    let old_path_of: FxHashMap<&str, &str> = state
        .renamed_files
        .iter()
        .map(|(old, new)| (new.as_str(), old.as_str()))
        .collect();
    let mut seen: FxHashSet<&str> = FxHashSet::default();
    // (name, path, its path and lines at the base, exported, its class or
    // interface)
    let mut removed: Vec<(String, String, (String, String), bool, Option<String>)> = Vec::new();
    for h in &state.hunk_texts {
        // A model that reads removals off its facts answers for its files
        // (`removed_by_models`).
        if h.removed.is_empty()
            || !is_code_file(&h.path)
            || read_off_facts(&h.path)
            || !seen.insert(h.path.as_str())
        {
            continue;
        }
        let old_path = old_path_of.get(h.path.as_str()).copied().unwrap_or(&h.path);
        let before = read_or_note(
            state,
            crate::git::show_file_at_revision(&state.root_dir, base, Path::new(old_path)),
        );
        let after = state
            .run
            .source()
            .read_to_string(&state.root_dir.join(&h.path))
            .unwrap_or_default();
        let (Some(at_base), Some(at_head)) = (
            crate::parsers::declarations(old_path, &before),
            crate::parsers::declarations(&h.path, &after),
        ) else {
            // Declarations the grammar could not read in time (a parse past
            // its budget on a large file) are definitions nobody can name as
            // removed: the answer says so, never "nothing removed".
            if crate::parsers::has_grammar(old_path) {
                state.run.note_file(
                    crate::resource::LimitReason::ReadFailed,
                    old_path.to_string(),
                );
            }
            continue;
        };
        let still: FxHashSet<&str> = at_head.iter().map(|d| d.name.as_str()).collect();
        let public = |d: &crate::parsers::Declaration| {
            let head = d.head.trim_start();
            is_public(old_path, Some(&d.name), head)
                || d.container.is_some_and(|c| {
                    !head.starts_with("private")
                        && !head.starts_with("protected")
                        && !head.starts_with('#')
                        && is_public(
                            old_path,
                            Some(&at_base[c].name),
                            at_base[c].head.trim_start(),
                        )
                })
        };
        for d in &at_base {
            if d.name.len() < 3
                || still.contains(d.name.as_str())
                || removed.iter().any(|(n, _, _, _, _)| *n == d.name)
            {
                continue;
            }
            removed.push((
                d.name.clone(),
                h.path.clone(),
                (
                    old_path.to_string(),
                    format!("{}-{}", d.start_line, d.end_line),
                ),
                public(d),
                d.container.map(|c| at_base[c].name.clone()),
            ));
        }
    }
    // One grep for every removed name, its lines handed to the names they
    // spell as a word: a deleted 1000-function module ran 1000 greps and
    // outlived the deadline.
    let names: Vec<String> = removed.iter().map(|(n, _, _, _, _)| n.clone()).collect();
    let wanted: FxHashSet<&str> = names.iter().map(String::as_str).collect();
    let mut hits_of: FxHashMap<&str, Vec<(String, u32, String)>> = FxHashMap::default();
    let grepped = read_or_note(
        state,
        crate::git::grep_code_words(&state.root_dir, &names, head, MAX_REMOVED_HITS_PER_FILE),
    );
    state.run.note_withheld(grepped.withheld);
    for (path, line, text) in grepped.rows {
        let spelled: FxHashSet<&str> = text
            .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
            .filter_map(|w| wanted.get(w).copied())
            .collect();
        for name in spelled {
            hits_of
                .entry(name)
                .or_default()
                .push((path.clone(), line, text.clone()));
        }
    }
    let holders = definition_holders(state);
    let mut out = Vec::new();
    for (name, path_removed, (path_at_base, lines), exported, container) in removed {
        if !state.run.check() {
            break;
        }
        let hits = hits_of.remove(name.as_str()).unwrap_or_default();
        let mut callers: Vec<Caller> = Vec::new();
        // A name on this many lines is a common word, not a reference to the
        // removed definition: tracing it by name lists noise and, on a
        // monorepo, never finishes.
        let unresolved = (hits.len() > MAX_REMOVED_NAME_HITS).then(|| Unknown::CommonWord {
            name: name.to_string(),
            lines: hits.len(),
        });
        let hits = if unresolved.is_some() {
            Vec::new()
        } else {
            hits
        };
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
            // A caller is code that can still reach the removed definition:
            // through an import the binding model resolves to the file it
            // left, else at least from the same runtime. By name alone,
            // `subprocess.run`, another file's own `run`, a shell `die` and a
            // TypeScript `test.describe` were callers (#470, #471, #458).
            let modelled =
                bindings::lang_of(&path).is_some() && bindings::lang_of(&path_removed).is_some();
            // A row the binding model let through is model-informed; one no
            // model checked is a word match (R1).
            let mut checked: Option<Option<&'static str>> = None;
            if modelled {
                // A member is reached through its class, imported by name.
                let by_name = binds_to(&path, &text, &name, &path_removed);
                let bound = by_name
                    || container
                        .as_deref()
                        .is_some_and(|c| binds_to(&path, &text, c, &path_removed));
                if path != path_removed && !bound {
                    continue;
                }
                if path != path_removed {
                    if !by_name && !one_tree(&path, &path_removed) {
                        continue;
                    }
                    checked = Some((!by_name).then_some("receiver_unresolved"));
                }
            } else if runtime_of(&path) != runtime_of(&path_removed)
                // A file that declares the name itself calls its own.
                || holders.get(&path).is_some_and(|fs| {
                    fs.iter().any(|f| f.symbol_name.as_deref() == Some(name.as_str()))
                })
            {
                continue;
            }
            if let Some(at) = moved_to {
                let reaches_new = if bindings::lang_of(&path).is_some() {
                    !binds_to(&path, &text, &name, &path_removed)
                } else {
                    at.iter().any(|f| qualified_by(&text, f)) && !qualified_by(&text, &path_removed)
                };
                if reaches_new {
                    continue;
                }
            }
            let code = code_text(&path, &text);
            // The import is where the name was taken, not a use (#450).
            if bindings::is_import_line(&code) {
                continue;
            }
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
            let holder = holders
                .get(&path)
                .into_iter()
                .flatten()
                .filter(|f| f.start_line() <= line && f.end_line() >= line)
                .min_by_key(|f| f.line_count())
                .copied();
            // The line declaring the name is where it went (a move), never a
            // caller of the removed definition; a method of the same name
            // that delegates to it still is one.
            if holder.is_some_and(|f| {
                (in_a_hunk(&f.id) && f.line_count() <= 1)
                    || (f.symbol_name.as_deref() == Some(name.as_str()) && f.start_line() == line)
            }) {
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
                unread_test: None,
                evidence: if checked.is_some() {
                    "candidate"
                } else {
                    "name"
                },
                reason: checked.flatten(),
                relation: if next.starts_with('(') || next.starts_with("?.(") {
                    "call"
                } else {
                    "reference"
                },
                sites: vec![line],
            });
        }
        let callers = merge_by_symbol(callers);
        let contract = exported.then(|| Contract {
            path: path_at_base.clone(),
            symbol: Some(name.clone()),
            kind: "removed_api",
        });
        out.push((
            ChangedSymbol {
                path: path_at_base,
                symbol: Some(name),
                lines,
                kind: "removed".to_string(),
                commits: 0,
                callers,
                entry: None,
                unresolved,
                untested: false,
                compatible_callers: 0,
                missed_in: Vec::new(),
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

/// Whether the project at `outer` holds the one at `inner`; a path under no
/// manifest belongs to every project.
fn nests(outer: &str, inner: &str) -> bool {
    outer.is_empty() || inner == outer || inner.starts_with(&format!("{outer}/"))
}

/// A use through a receiver nothing types: any object's attribute or method
/// of that name. Another project that never imports the module cannot reach
/// the definition, so such a guess does not cross a project root (#649,
/// Forgejo #86).
fn untyped_guess(found: &Finding) -> bool {
    matches!(found.evidence, Evidence::Candidate(r)
        if r == "receiver_unresolved" || r == bindings::UNRESOLVED_READ)
}

const MAX_STALE_REFERENCES: usize = 24;
const MAX_STALE_PER_TARGET: usize = 8;
/// What names a changed file is context, not breakage: fewer lines than
/// what still names a deleted one.
const MAX_NAMED_BY: usize = 12;
const MAX_NAMED_BY_PER_TARGET: usize = 3;

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
fn naming(
    token: &str,
    referrer: &str,
    target: &str,
    unique_basename: bool,
    root: &str,
) -> Option<Naming> {
    let trimmed = token.trim_start_matches("./");
    // A file at the root is spelled by its bare name, which names any file
    // of that name: `svc/tests` reading `"AGENTS.md"` reads `svc/AGENTS.md`
    // (#644), and `"/../AGENTS.md"` is a probe string, not the root's.
    // Behind a prefix it is the root's only from the repository's own path
    // or one mount point or variable: `$HOME/other/README.md` and
    // `/home/x/elsewhere/README.md` are other trees' files (#673).
    let anchored = || {
        token
            .strip_suffix(target)
            .and_then(|p| p.strip_suffix('/'))
            .is_some_and(|prefix| {
                target.contains('/')
                    || prefix == root
                    || (!prefix.trim_start_matches('/').contains('/')
                        && !matches!(prefix, "~" | "$HOME" | "${HOME}"))
            })
    };
    if (target.contains('/') || unique_basename) && (trimmed == target || anchored()) {
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
    let root = state.root_dir.to_string_lossy().replace('\\', "/");
    let head = diff_range
        .map(crate::git::split_diff_range)
        .and_then(|(_, head)| head);
    let surviving: FxHashSet<String> =
        read_or_note(state, state.run.source().list_paths(&state.root_dir))
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
    let grepped = read_or_note(
        state,
        crate::git::grep_lines(&state.root_dir, &needles, head.as_deref()),
    );
    state.run.note_withheld(grepped.withheld);
    let lines = grepped.rows;
    let renamed_to: FxHashSet<&str> = state
        .renamed_files
        .iter()
        .map(|(_, new)| new.as_str())
        .collect();
    let token_re = once_cell::sync::Lazy::force(&PATH_TOKEN);
    let mut found = Vec::new();
    let mut per_target: FxHashMap<&str, usize> = FxHashMap::default();
    let mut omitted = 0;
    // A line this change wrote about the deletion is its author's record of
    // it — a changelog entry, a note, a comment — not a reference it missed;
    // a code line it wrote still is one (#456).
    let written: FxHashSet<(&str, &str)> = state
        .hunk_texts
        .iter()
        .flat_map(|h| h.added.iter().map(move |l| (h.path.as_str(), l.trim())))
        .collect();
    for (path, line, text) in state.run.bounded(&lines) {
        if renamed_to.contains(path.as_str()) {
            continue;
        }
        if written.contains(&(path.as_str(), text.trim()))
            && (!is_code_file(path) || commented(text))
        {
            continue;
        }
        let surviving = &surviving;
        let targets = &targets;
        let root = root.as_str();
        let best = token_re
            .find_iter(text)
            .flat_map(|m| {
                targets
                    .iter()
                    .filter(|(t, _)| t != path)
                    .filter_map(move |(t, kind)| {
                        let basename = t.rsplit('/').next().unwrap_or(t);
                        // A path that ends a sentence carries its full stop.
                        let token = m.as_str().trim_end_matches('.');
                        naming(token, path, t, !surviving.contains(basename), root)
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

/// `--flag path` in a shell line and `*p` in C are code, not comments.
fn commented(text: &str) -> bool {
    let t = text.trim_start();
    ["#", "//", "/*", "* "].iter().any(|m| t.starts_with(m)) || t == "*"
}

/// An ignore or attribute file lists paths to include or exclude them; it
/// reads none of their content (#682).
fn selects_paths(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    (name.starts_with('.') && name.ends_with("ignore"))
        || name == ".gitattributes"
        || path.ends_with(".diffctx/ignore")
}

/// pip reads `requirements.txt`, `requirements-dev.txt`, `dev-requirements.txt`,
/// `requirements/base.txt` and the `constraints` kin by their names.
fn is_pip_manifest(path: &str) -> bool {
    let lower = path.to_lowercase();
    ["requirements", "constraints"]
        .iter()
        .any(|word| lower.contains(word))
}

/// Rows of data, logs and notes: an extension that says nothing reads them.
/// A `.txt` is one unless a tool reads it by its name: the language table
/// knows `CMakeLists.txt`, and pip's manifests are named by `is_pip_manifest`.
fn has_data_extension(path: &str) -> bool {
    match extension(path).as_str() {
        "csv" | "tsv" | "log" | "jsonl" | "ndjson" => true,
        "txt" => {
            let name = path.rsplit('/').next().unwrap_or(path).to_lowercase();
            !crate::languages::FILENAME_TO_LANGUAGE.contains_key(name.as_str())
                && !is_pip_manifest(path)
        }
        _ => false,
    }
}

/// Rows of data and transcripts mention paths; nothing runs them (#673).
/// An extensionless file is data unless its name or shebang says what runs
/// it — as a referrer; a changed one is a config or a manifest with readers
/// until proven otherwise (`Procfile`, `CODEOWNERS`) and goes by extension
/// alone (#123).
fn is_data(root_dir: &Path, source: &crate::source::Source, path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    if !name.contains('.') {
        return crate::languages::get_language_for_file(path).is_none()
            && !source
                .read_to_string(&root_dir.join(path))
                .is_some_and(|t| t.starts_with("#!"));
    }
    has_data_extension(path)
}

/// A structured config: its keys and values are read by a program.
fn is_config_file(path: &str) -> bool {
    matches!(
        extension(path).as_str(),
        "yaml" | "yml" | "json" | "toml" | "ini" | "env" | "properties" | "conf" | "cfg"
    )
}

/// Code, a config, or a file with no extension (a Dockerfile, a Makefile, a
/// script): where a path is spelled to be used. Anything else that survives
/// the filters (a page, a template) only links to it.
fn reads_paths(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    is_code_file(path) || is_config_file(path) || !name.contains('.')
}

/// Words that join a sentence and never a command or a value.
const FUNCTION_WORDS: &[&str] = &[
    "the", "an", "is", "are", "was", "were", "be", "in", "on", "of", "to", "for", "with", "and",
    "or", "see", "this", "that", "it", "as", "at", "by", "from", "not", "you", "we", "if", "then",
    "but", "also", "please",
];

/// A sentence that spells a path talks about the file; it does not read it:
/// `"setup steps are in README.md"` in a JSON dump or a `description:` value
/// (#673). A command line or a bare value has too few plain words around the
/// path, and one with shell syntax is never prose.
fn is_prose_about(text: &str, token: &str) -> bool {
    let rest = text.replacen(token, " ", 1);
    if rest.contains([';', '|', '&', '$', '<', '>', '`']) || rest.contains(" -") {
        return false;
    }
    let plain: Vec<String> = rest
        .split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|w| w.len() >= 2 && w.chars().all(|c| c.is_ascii_alphabetic()))
        .collect();
    plain.len() >= 3 && plain.iter().any(|w| FUNCTION_WORDS.contains(&w.as_str()))
}

/// The paths a file says it was generated from in its first lines
/// (`# GENERATED from src/dash.jsonnet - do not edit`), spelled from the
/// repository root and from the file's own directory.
fn generated_from(source: &crate::source::Source, root_dir: &Path, path: &str) -> Vec<String> {
    static MARKER: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
        regex::Regex::new(r"(?i)\bgenerated\s+from\s+([A-Za-z0-9_./@+-]+)").expect("marker regex")
    });
    let Some(text) = source.read_to_string(&root_dir.join(path)) else {
        return Vec::new();
    };
    let head: String = text.lines().take(5).collect::<Vec<_>>().join("\n");
    let Some(from) = MARKER.captures(&head).and_then(|c| c.get(1)) else {
        return Vec::new();
    };
    let from = from.as_str().trim_end_matches('.');
    let dir = path.rsplit_once('/').map_or("", |(d, _)| d);
    vec![normalize_rel(from), normalize_rel(&format!("{dir}/{from}"))]
}

/// What a changed config's lines spell that can witness a reader: words
/// (`maxPoolSize`, `DATABASE_URL`, `max-connections`, `synchronization`,
/// never `replicas`) and values (`up{job="demo"}`, `ghcr.io/acme/web:1.2`).
/// Only a structured config has keys and values; a template's or a
/// stylesheet's words are prose and selectors.
fn changed_keys(state: &ScoredState, path: &str) -> Vec<String> {
    if !is_config_file(path) {
        return Vec::new();
    }
    let mut keys: Vec<String> = state
        .hunk_texts
        .iter()
        .filter(|h| h.path == path)
        .flat_map(|h| h.added.iter().chain(&h.removed))
        .filter(|l| !commented(l))
        .flat_map(|l| key_words(l).chain(value_phrases(l)))
        .collect();
    keys.sort();
    keys.dedup();
    keys
}

/// A value that spells more than a word: a selector, a path, an address, an
/// image. It is searched whole, so it has to be long enough, carry letters
/// and a structure of its own, and start as a value does — `{"retries": 5}`
/// is the file's own syntax, `Hello world` is prose.
fn phrase_like(token: &str) -> bool {
    token.chars().count() >= 8
        && token
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || matches!(c, '/' | '.' | '_'))
        && token.chars().filter(|c| c.is_alphabetic()).count() >= 4
        && token.chars().any(|c| {
            matches!(
                c,
                '{' | '}' | '[' | ']' | '(' | ')' | '=' | '/' | ':' | '@' | '"' | '\''
            )
        })
}

/// Whether a search term is a value phrase and not a key word.
fn is_phrase(term: &str) -> bool {
    term.chars()
        .any(|c| !(c.is_alphanumeric() || c == '_' || c == '-'))
}

/// The whitespace-separated pieces of a line, kept whole where whitespace
/// belongs to the piece: inside a quoted string, and inside the brackets of
/// a selector or a call (`up{job="a", pod="b"}`) — a bracket after a word
/// character. A bracket after whitespace or another bracket is the file's own
/// flow syntax and splits like any other text.
fn split_tokens(text: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut start: Option<usize> = None;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut brackets: Vec<bool> = Vec::new();
    let mut attached = 0usize;
    let mut prev: Option<char> = None;
    for (i, c) in text.char_indices() {
        if quote.is_none() && c.is_whitespace() && attached == 0 {
            if let Some(s) = start.take() {
                tokens.push(&text[s..i]);
            }
            prev = None;
            continue;
        }
        if start.is_none() {
            start = Some(i);
            if c == '\'' || c == '"' {
                quote = Some(c);
                prev = Some(c);
                continue;
            }
        }
        match quote {
            Some(q) => {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == q {
                    quote = None;
                }
            }
            None => match c {
                '{' | '[' | '(' => {
                    let on_word = prev.is_some_and(|p| p.is_alphanumeric() || p == '_');
                    brackets.push(on_word);
                    if on_word {
                        attached += 1;
                    }
                }
                '}' | ']' | ')' => {
                    if brackets.pop() == Some(true) {
                        attached = attached.saturating_sub(1);
                    }
                }
                _ => {}
            },
        }
        prev = Some(c);
    }
    if let Some(s) = start {
        tokens.push(&text[s..]);
    }
    tokens
}

/// `token` without the flow syntax around it: the brackets of an enclosing
/// list or mapping, the comma after it, the colon of a key.
fn trim_flow(token: &str) -> &str {
    let mut t = token.trim_start_matches(['[', '{']);
    loop {
        let cut = t.trim_end_matches([',', ';']);
        let opens = cut.chars().filter(|c| matches!(c, '{' | '[' | '(')).count();
        let closes = cut.chars().filter(|c| matches!(c, '}' | ']' | ')')).count();
        if closes > opens && cut.ends_with(['}', ']', ')']) {
            t = &cut[..cut.len() - 1];
        } else {
            return cut.strip_suffix(':').unwrap_or(cut);
        }
    }
}

/// The values a config line spells that are worth searching whole, found the
/// same way on the changed side and on the reader's: `expr: 'up{job="a"} == 0'`
/// gives `up{job="a"}`, `url=http://db:5432/app` gives `http://db:5432/app`.
fn value_phrases(line: &str) -> Vec<String> {
    fn collect(text: &str, depth: usize, out: &mut Vec<String>) {
        for raw in split_tokens(text) {
            let token = trim_flow(raw);
            let inner = match token.chars().next() {
                Some(q @ ('\'' | '"')) if token.len() >= 2 && token.ends_with(q) => {
                    Some(&token[1..token.len() - 1])
                }
                _ => token.split_once('=').and_then(|(head, value)| {
                    let key = !head.is_empty()
                        && head
                            .chars()
                            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '-'));
                    key.then_some(value)
                }),
            };
            match inner {
                Some(inner) if depth < 2 => collect(inner, depth + 1, out),
                Some(_) => {}
                None if phrase_like(token) => out.push(token.to_string()),
                None => {}
            }
        }
    }
    let mut out = Vec::new();
    collect(line.trim().trim_start_matches("- "), 0, &mut out);
    out
}

fn key_words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-'))
        .map(|w| w.trim_matches('-'))
        .filter(|w| key_like(w))
        .map(str::to_string)
}

/// Shaped like a key a reader spells back: joined words, a constant's
/// capitals or one long word. `Hello` and `retries` are not.
fn key_like(word: &str) -> bool {
    let chars: Vec<char> = word.chars().collect();
    if chars.len() < 4
        || !chars.iter().any(|c| c.is_ascii_alphabetic())
        || crate::stopwords::CODE_STOPWORDS.contains(&word.to_lowercase())
    {
        return false;
    }
    let joined = (word.contains('_') || word.contains('-')) && chars.len() >= 6;
    let camel = chars
        .windows(2)
        .any(|w| w[0].is_ascii_lowercase() && w[1].is_ascii_uppercase());
    let constant = chars
        .iter()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == '_');
    let long = chars.len() >= 8 && chars.iter().all(char::is_ascii_lowercase);
    joined || camel || constant || long
}

/// The readers the typed key table finds for what the change rewrote in the
/// changed configs `untraced`: (changed file, `route /v1/orders/{}`, the
/// untouched file naming the same key, its line). Where a key has a type (an
/// environment variable, an image, a route, a table), the join knows what a
/// textual search cannot: spellings differ (`{id}` and `<int:id>`), and only
/// compatible roles meet.
fn typed_readers(state: &ScoredState, untraced: &[String]) -> Vec<(String, String, String, u32)> {
    let mut rows: Vec<(String, String, String, u32)> =
        crate::edges::config_edges::counterparts_of_change(
            &state.all_fragments,
            Some(state.root_dir.as_path()),
            state.run.changed_lines(),
        )
        .into_iter()
        .map(|c| {
            (
                rel(state, &c.changed_path),
                format!("{} {}", c.kind, c.key),
                rel(state, &c.path),
                c.line.max(1) as u32,
            )
        })
        .filter(|(target, ..)| untraced.contains(target))
        .collect();
    rows.sort();
    rows.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1 && a.2 == b.2);
    rows
}

/// Terms searched for one changed config, and for all of them: `git grep`
/// matches every needle against every line, so a diff of hundreds of configs
/// (an epoch's corpus cases and workflows) turned a 10 s answer into 330 s
/// (Forgejo #109). The smallest changes are searched first, a file's longest
/// terms first; a config left out stays "not followed" and is counted.
const MAX_KEY_TERMS_PER_FILE: usize = 16;
const MAX_KEY_NEEDLES: usize = 96;

/// The lines outside the change that spell a key or a value the change
/// touched in a file outside code (#686, #609, Forgejo #109), and the files
/// the typed key table ties to it. The textual match is a search: a term too
/// common to witness a reader is not searched, a search past its caps leaves
/// the rest of the configs unsearched (the last count returned), and a file
/// none of whose terms could be searched stays `content_untraced`.
fn key_references(
    state: &ScoredState,
    untraced: &[String],
    diff_range: Option<&str>,
) -> (Vec<PathReference>, usize, Vec<KeySearch>, usize) {
    if !state.run.check() {
        return (Vec::new(), 0, Vec::new(), 0);
    }
    let typed = typed_readers(state, untraced);
    let mut candidates: Vec<(usize, &String, Vec<String>, Vec<String>)> = untraced
        .iter()
        .enumerate()
        .map(|(index, path)| {
            let readers: Vec<String> = typed
                .iter()
                .filter(|(target, ..)| target == path)
                .map(|(_, key, ..)| key.clone())
                .collect();
            (index, path, changed_keys(state, path), readers)
        })
        .filter(|(_, _, words, readers)| !words.is_empty() || !readers.is_empty())
        .collect();
    candidates.sort_by(|a, b| (a.2.len(), a.1).cmp(&(b.2.len(), b.1)));
    let mut searched: FxHashSet<String> = FxHashSet::default();
    let mut unsearched = 0;
    let mut chosen: Vec<(usize, String, Vec<String>)> = Vec::new();
    for (index, path, mut words, mut readers) in candidates {
        words.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
        words.truncate(MAX_KEY_TERMS_PER_FILE);
        let fresh = words.iter().filter(|w| !searched.contains(*w)).count();
        if searched.len() + fresh > MAX_KEY_NEEDLES {
            if readers.is_empty() {
                unsearched += 1;
                continue;
            }
            words.clear();
        }
        searched.extend(words.iter().cloned());
        words.append(&mut readers);
        words.sort();
        words.dedup();
        chosen.push((index, path.clone(), words));
    }
    chosen.sort_by_key(|(index, ..)| *index);
    let keys: Vec<(String, Vec<String>)> = chosen
        .into_iter()
        .map(|(_, path, terms)| (path, terms))
        .collect();
    let mut needles: Vec<String> = searched.into_iter().collect();
    needles.sort();
    if keys.is_empty() {
        return (Vec::new(), 0, Vec::new(), unsearched);
    }
    let head = diff_range
        .map(crate::git::split_diff_range)
        .and_then(|(_, head)| head);
    let grepped = read_or_note(
        state,
        crate::git::grep_lines(&state.root_dir, &needles, head.as_deref()),
    );
    let changed: FxHashSet<String> = state
        .changed_files
        .iter()
        .map(|p| rel(state, &p.to_string_lossy()))
        .collect();
    let wanted: FxHashSet<&str> = needles.iter().map(String::as_str).collect();
    let phrases: Vec<&String> = needles.iter().filter(|n| is_phrase(n)).collect();
    let mut by_key: FxHashMap<String, Vec<(String, u32)>> = FxHashMap::default();
    for (path, line, text) in state.run.bounded(&grepped.rows) {
        if changed.contains(path)
            || is_document(path)
            || selects_paths(path)
            || is_data(&state.root_dir, &state.run.source(), path)
            || commented(text)
        {
            continue;
        }
        let spelled = key_words(text)
            .filter(|w| wanted.contains(w.as_str()))
            .chain(
                phrases
                    .iter()
                    .filter(|p| names_word(text, p.as_str()))
                    .map(|p| p.to_string()),
            );
        let mut seen: FxHashSet<String> = FxHashSet::default();
        for word in spelled {
            if seen.insert(word.clone()) {
                let rows = by_key.entry(word).or_default();
                if !rows.iter().any(|(p, _)| p == path) {
                    rows.push((path.clone(), *line));
                }
            }
        }
    }
    for (_, key, path, line) in &typed {
        let rows = by_key.entry(key.clone()).or_default();
        if !rows.iter().any(|(p, _)| p == path) {
            rows.push((path.clone(), *line));
        }
    }
    let mut refs: Vec<PathReference> = Vec::new();
    let mut searches: Vec<KeySearch> = Vec::new();
    let mut omitted = 0;
    let mut claimed: FxHashSet<&str> = FxHashSet::default();
    for (target, target_keys) in &keys {
        let searchable: Vec<&String> = target_keys
            .iter()
            .filter(|k| by_key.get(*k).map_or(0, Vec::len) <= MAX_UNTRACED_NAMERS)
            .collect();
        if searchable.is_empty() {
            continue;
        }
        let mut named = 0;
        let mut listed: FxHashSet<(String, u32)> = FxHashSet::default();
        for key in &searchable {
            if !claimed.insert(key.as_str()) {
                continue;
            }
            let mut rows = by_key.get(*key).cloned().unwrap_or_default();
            rows.sort();
            for (path, line) in rows {
                if !listed.insert((path.clone(), line)) {
                    continue;
                }
                named += 1;
                if named > MAX_NAMED_BY_PER_TARGET || refs.len() >= MAX_NAMED_BY {
                    omitted += 1;
                    continue;
                }
                refs.push(PathReference {
                    path,
                    line,
                    target: target.clone(),
                    key: Some((*key).clone()),
                });
            }
        }
        searches.push(KeySearch {
            path: target.clone(),
            keys: searchable.into_iter().cloned().collect(),
            named,
        });
    }
    (refs, omitted, searches, unsearched)
}

/// Lines outside the change that name a changed file by its path, from a
/// file of another runtime or a config: the command line that runs it, the
/// Dockerfile that copies it, the settings entry that runs a script (#393,
/// #523). An import is a caller, which the binding model answers; a
/// document or a comment only mentions the file.
fn named_by_path(state: &ScoredState, diff_range: Option<&str>) -> (Vec<PathReference>, usize) {
    // Every build step names a project manifest; that it changed is said
    // where dependencies are.
    let targets: Vec<String> = state
        .changed_files
        .iter()
        .map(|p| rel(state, &p.to_string_lossy()))
        .filter(|t| !PROJECT_MANIFESTS.contains(&t.rsplit('/').next().unwrap_or(t)))
        .collect();
    if targets.is_empty() {
        return (Vec::new(), 0);
    }
    let root = state.root_dir.to_string_lossy().replace('\\', "/");
    let head = diff_range
        .map(crate::git::split_diff_range)
        .and_then(|(_, head)| head);
    let mut basenames: FxHashMap<String, usize> = FxHashMap::default();
    for p in read_or_note(state, state.run.source().list_paths(&state.root_dir)) {
        if let Some(b) = p.rsplit('/').next() {
            *basenames.entry(b.to_string()).or_default() += 1;
        }
    }
    let basename_of = |t: &str| t.rsplit('/').next().unwrap_or(t).to_string();
    let mut needles: Vec<String> = targets
        .iter()
        .flat_map(|t| [t.clone(), basename_of(t)])
        .collect();
    needles.sort();
    needles.dedup();
    // A file the ignore rules withhold is not searched here; the header's
    // withheld count stays the one the answer's own reads give.
    let grepped = read_or_note(
        state,
        crate::git::grep_lines(&state.root_dir, &needles, head.as_deref()),
    );
    // What this change wrote is the diff itself.
    let written: FxHashSet<(&str, &str)> = state
        .hunk_texts
        .iter()
        .flat_map(|h| h.added.iter().map(move |l| (h.path.as_str(), l.trim())))
        .collect();
    let token_re = once_cell::sync::Lazy::force(&PATH_TOKEN);
    let mut candidates: Vec<PathReference> = Vec::new();
    for (path, line, text) in state.run.bounded(&grepped.rows) {
        // A document or a patch mentions paths without consuming them. A
        // document importing another (`@AGENTS.md`) does load it, but says
        // so on every edit of it, a constant line that teaches the reader to
        // skip the block (#563, #564).
        if is_document(path)
            || matches!(extension(path).as_str(), "diff" | "patch")
            || selects_paths(path)
            || is_data(&state.root_dir, &state.run.source(), path)
            || commented(text)
            || crate::bindings::is_import_line(text)
            || written.contains(&(path.as_str(), text.trim()))
        {
            continue;
        }
        let hit = token_re.find_iter(text).find_map(|m| {
            let token = m.as_str().trim_end_matches('.');
            targets
                .iter()
                .find(|t| {
                    let basename = basename_of(t);
                    *t != path
                        && token.ends_with(basename.as_str())
                        && matches!(
                            naming(token, path, t, basenames.get(&basename) == Some(&1), &root),
                            Some(Naming::FullPath | Naming::RelativePath | Naming::UniqueBasename)
                        )
                })
                .map(|t| (t, token))
        });
        let Some((target, token)) = hit else {
            continue;
        };
        // A sentence in a config's string (a dump, a description) is not a
        // reader; code is judged by its comments above.
        if !is_code_file(path) && is_prose_about(text, token) {
            continue;
        }
        candidates.push(PathReference {
            path: path.clone(),
            line: *line,
            target: target.clone(),
            key: None,
        });
    }
    // A generated file is its source's output: when the source names the
    // file too, the source is the consumer to open (#110).
    let spelled: FxHashSet<String> = candidates.iter().map(|r| r.path.clone()).collect();
    let source = state.run.source();
    let mut derived: FxHashMap<String, bool> = FxHashMap::default();
    candidates.retain(|r| {
        let folded = *derived.entry(r.path.clone()).or_insert_with(|| {
            generated_from(&source, &state.root_dir, &r.path)
                .iter()
                .any(|from| *from != r.path && spelled.contains(from))
        });
        !folded
    });
    // Code and config consumers come before pages and templates, so the cap
    // drops the latter first (#673).
    candidates.sort_by_key(|r| !reads_paths(&r.path));
    let mut found: Vec<PathReference> = Vec::new();
    let mut per_target: FxHashMap<String, usize> = FxHashMap::default();
    let mut omitted = 0;
    for r in candidates {
        let count = per_target.entry(r.target.clone()).or_default();
        if *count >= MAX_NAMED_BY_PER_TARGET || found.len() >= MAX_NAMED_BY {
            omitted += 1;
            continue;
        }
        *count += 1;
        found.push(r);
    }
    (found, omitted)
}

/// Astro's content API: a page reads the entries of a collection by its
/// name. The first argument is the name, or an object or array of them
/// with a `collection` key; anything else computes it.
static COLLECTION_CALL: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
    regex::Regex::new(
        r#"\b(?:getCollection|getEntry|getEntries|getEntryBySlug|getDataEntryById)\s*\(\s*(?:\[\s*)?(?:\{[^}]*?\bcollection\s*:\s*)?(?:'([^'$]*)'|"([^"$]*)"|`([^`$]*)`|([^\s)]))?"#,
    )
    .expect("collection call regex")
});

/// The lines outside the change that read the content collection of a
/// changed entry by name (#432). An entry under `src/content/<name>/` of a
/// directory holding an `astro.config.*` is read by `getCollection('<name>')`
/// and its kin, never imported or named by path, so its readers were a clean
/// zero. A call whose name is computed may read it and is listed so.
/// Read off the lines: an `.astro` page has no grammar here, and a file
/// that has one must hold the call in code, not in a string or comment.
fn read_by_collection(
    state: &ScoredState,
    diff_range: Option<&str>,
) -> (Vec<CollectionRead>, usize) {
    let paths = read_or_note(state, state.run.source().list_paths(&state.root_dir));
    let projects: Vec<&str> = paths
        .iter()
        .filter_map(|p| {
            let (dir, file) = p.rsplit_once('/').unwrap_or(("", p));
            file.starts_with("astro.config.").then_some(dir)
        })
        .collect();
    let content_of = |project: &str| {
        if project.is_empty() {
            "src/content/".to_string()
        } else {
            format!("{project}/src/content/")
        }
    };
    let project_of = |path: &str| {
        projects
            .iter()
            .copied()
            .filter(|p| p.is_empty() || path.starts_with(&format!("{p}/")))
            .max_by_key(|p| p.len())
    };
    let changed: FxHashSet<(&str, String)> = state
        .changed_files
        .iter()
        .map(|f| rel(state, &f.to_string_lossy()))
        .chain(state.deleted_files.iter().cloned())
        .filter_map(|path| {
            let project = project_of(&path)?;
            let (collection, _) = path.strip_prefix(&content_of(project))?.split_once('/')?;
            Some((project, collection.to_string()))
        })
        .collect();
    if changed.is_empty() {
        return (Vec::new(), 0);
    }
    let head = diff_range
        .map(crate::git::split_diff_range)
        .and_then(|(_, head)| head);
    let grepped = read_or_note(
        state,
        crate::git::grep_lines_matching(
            &state.root_dir,
            r"(getCollection|getEntry|getEntries|getEntryBySlug|getDataEntryById)[[:space:]]*\(",
            head.as_deref(),
            &[],
        ),
    );
    let written: FxHashSet<(&str, &str)> = state
        .hunk_texts
        .iter()
        .flat_map(|h| h.added.iter().map(move |l| (h.path.as_str(), l.trim())))
        .collect();
    let source = state.run.source();
    let mut texts: FxHashMap<&str, Option<String>> = FxHashMap::default();
    let (mut found, mut omitted) = (Vec::new(), 0);
    for (path, line, text) in state.run.bounded(&grepped.rows) {
        let Some(project) = project_of(path) else {
            continue;
        };
        if is_document(path) || commented(text) || written.contains(&(path.as_str(), text.trim())) {
            continue;
        }
        let Some(file) = texts
            .entry(path)
            .or_insert_with(|| source.read_to_string(&state.root_dir.join(path)))
        else {
            continue;
        };
        let lines: Vec<&str> = file.lines().collect();
        let code = code_text(path, file);
        let code_line = code.lines().nth(*line as usize - 1).unwrap_or("");
        // A call split over lines names its collection on the next one.
        let call = match lines.get(*line as usize) {
            Some(next) => format!("{text} {next}"),
            None => text.clone(),
        };
        for m in COLLECTION_CALL.captures_iter(&call) {
            // A call in a string or a comment reads nothing, and one on the
            // next line is that line's own.
            let at = m.get(0).map_or(0, |m| m.start());
            if at >= text.len() || code_line.get(at..at + 3) != text.get(at..at + 3) {
                continue;
            }
            let named = (1..=3)
                .find_map(|i| m.get(i))
                .map(|n| n.as_str().to_string());
            let reads = match &named {
                Some(c) => changed.contains(&(project, c.clone())),
                None => m.get(4).is_some() && changed.iter().any(|(p, _)| *p == project),
            };
            if !reads {
                continue;
            }
            if found.len() >= MAX_NAMED_BY {
                omitted += 1;
                continue;
            }
            found.push(CollectionRead {
                path: path.clone(),
                line: *line,
                collection: named,
            });
        }
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
///
/// Every step drops an entry, so the measure falls as steps add up, and the
/// first step that fits is found in doubling strides, then halved: a
/// measure per step re-rendered the whole answer for each dropped caller,
/// and 6000 callers of a removed API took over a minute to fit (#510).
fn fit_with(output: &mut ImpactOutput, cap: u32, measure: &dyn Fn(&ImpactOutput) -> u32) {
    output.cap = cap;
    let fits = |o: &ImpactOutput| measure(o) <= cap;
    let mut stride = 1;
    while !fits(output) {
        let mut ahead = output.clone();
        let taken = shed(&mut ahead, stride);
        let exhausted = taken < stride;
        if !exhausted && !fits(&ahead) {
            *output = ahead;
            stride *= 2;
            continue;
        }
        if exhausted && !fits(&ahead) {
            *output = ahead;
            break;
        }
        // `taken` steps fit and none did before this stride: the first that
        // fits lies in (lo, hi].
        let (mut lo, mut hi) = (0, taken);
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            let mut probe = output.clone();
            shed(&mut probe, mid);
            if fits(&probe) {
                (hi, ahead) = (mid, probe);
            } else {
                lo = mid;
            }
        }
        *output = ahead;
        break;
    }
    output.empty = proves_empty(output);
}

/// Up to `steps` steps of shedding; how many took place.
fn shed(output: &mut ImpactOutput, steps: usize) -> usize {
    (0..steps).take_while(|_| shed_one(output)).count()
}

/// The one rule for `empty` (#505): nothing outside the diff depends on the
/// change, no contract moved, and nothing was cut, left unread or capped.
fn proves_empty(output: &ImpactOutput) -> bool {
    output.symbol_named.is_none()
        && output.changed.is_empty()
        && output.contracts.is_empty()
        && output.stale_references.is_empty()
        && output.stale_references_omitted == 0
        && output.named_by.is_empty()
        && output.named_by_omitted == 0
        && output.key_searches.is_empty()
        && output.collection_reads.is_empty()
        && output.collection_reads_omitted == 0
        && output.truncated == 0
        && output.contracts_omitted == 0
        && output.limits.is_empty()
        && output.unanalysed_files.is_empty()
        && output.lockfile_changes.is_empty()
}

/// One step of shedding, least needed first; `false` once nothing more can
/// go. Callers are the answer, so everything else yields to them first, and
/// the file list — which the reader holds as the diff — first of all.
fn shed_one(output: &mut ImpactOutput) -> bool {
    if output.untracked_files.len() > MAX_CHANGED_FILES_LISTED {
        output.untracked_files_omitted += output.untracked_files.len() - MAX_CHANGED_FILES_LISTED;
        output.untracked_files.truncate(MAX_CHANGED_FILES_LISTED);
    }
    if output.changed_files.len() > MAX_CHANGED_FILES_LISTED {
        output.changed_files_omitted += output.changed_files.len() - MAX_CHANGED_FILES_LISTED;
        output.changed_files.truncate(MAX_CHANGED_FILES_LISTED);
        return true;
    }
    // What names a changed file by path is context: it goes before any
    // caller, a possible one included, and its count line goes with it.
    if output.named_by.pop().is_some() {
        output.named_by_omitted += 1;
        return true;
    }
    if output.named_by_omitted > 0 {
        output.named_by_omitted = 0;
        return true;
    }
    if output.layout_only.len() > MAX_UNTESTED_NAMED {
        output.layout_only_omitted += output.layout_only.len() - MAX_UNTESTED_NAMED;
        output.layout_only.truncate(MAX_UNTESTED_NAMED);
        return true;
    }
    if output.unanalysed_files.len() > MAX_UNANALYSED_LISTED {
        output.unanalysed_files_omitted += output.unanalysed_files.len() - MAX_UNANALYSED_LISTED;
        output.untraced_omitted += output.unanalysed_files[MAX_UNANALYSED_LISTED..]
            .iter()
            .filter(|f| f.reason == "content_untraced")
            .count();
        output.unanalysed_files.truncate(MAX_UNANALYSED_LISTED);
        return true;
    }
    // A word match is the weakest row there is: it goes before a guess.
    if let Some(sym) = output
        .changed
        .iter_mut()
        .find(|c| c.callers.iter().any(is_textual))
    {
        if let Some(i) = sym.callers.iter().rposition(is_textual) {
            sym.callers.remove(i);
            output.truncated += 1;
            output.truncated_textual += 1;
            return true;
        }
    }
    // A possible caller is a guess; a changed contract is a fact (#413).
    if let Some(sym) = output
        .changed
        .iter_mut()
        .find(|c| c.callers.iter().any(|x| x.evidence == "candidate"))
    {
        if let Some(i) = sym.callers.iter().rposition(|x| x.evidence == "candidate") {
            sym.callers.remove(i);
            output.truncated += 1;
            output.truncated_possible += 1;
            return true;
        }
    }
    if output.contracts.pop().is_some() {
        output.contracts_omitted += 1;
        return true;
    }
    // A symbol whose callers could not be resolved says so to the last:
    // dropping it would turn "unknown" into "nothing".
    if let Some(i) = output
        .changed
        .iter()
        .rposition(|c| c.callers.is_empty() && c.unresolved.is_none() && c.compatible_callers == 0)
    {
        output.changed.remove(i);
        output.symbols_omitted += 1;
        return true;
    }
    if shed_weakest_caller(output) {
        return true;
    }
    // A file still naming a deleted or renamed path is a defect, not a
    // caller to check: it outlives every caller row, first entries kept
    // (#600).
    if output.stale_references.pop().is_some() {
        output.stale_references_omitted += 1;
        return true;
    }
    false
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
            let shed = output.changed[i].callers.pop();
            output.truncated_untested += usize::from(shed.as_ref().is_some_and(without_test_link));
            output.truncated += 1;
            true
        }
        Some(i) if output.changed[i].unresolved.is_none() => {
            output.truncated += output.changed[i].callers.len();
            output.truncated_untested += output.changed[i]
                .callers
                .iter()
                .filter(|c| without_test_link(c))
                .count();
            output.changed.remove(i);
            output.symbols_omitted += 1;
            true
        }
        _ => false,
    }
}

/// The answer as the reader receives it, fitted to `cap` tokens measured on
/// that very text. JSON was fitted while markdown was printed, so a 148-token
/// answer cited a 2000-token cap and lost its contracts (#451); `--budget`
/// and `budget_tokens` set the cap, which stayed 2000 whatever was asked
/// (#466). `None` keeps the default.
pub fn deliver(output: &mut ImpactOutput, markdown: bool, cap: Option<u32>) -> String {
    let _render = crate::profile::phase("render");
    let cap = cap.unwrap_or(IMPACT_TOKEN_CAP);
    if markdown {
        let measure = |o: &ImpactOutput| crate::tokenizer::count_tokens(&render(o, false));
        fit_with(output, cap, &measure);
        render(output, false)
    } else {
        fit_with(output, cap, &serialized_tokens);
        serde_json::to_string(output).unwrap_or_default()
    }
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
    match (range, index_tree) {
        // `A..T` names a tree its caller built — the hook's set of edited
        // files — which is no more the index than any other tree (#463).
        (Some(r), Some(tree)) if !crate::git::is_staged_keyword(r) => {
            format!("the snapshot tree {}", &tree[..tree.len().min(10)])
        }
        (_, Some(tree)) => format!(
            "staged changes (index tree {})",
            &tree[..tree.len().min(10)]
        ),
        (None | Some("HEAD"), None) => "uncommitted changes".to_string(),
        (Some(r), None) => r.to_string(),
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
    if let (Some(symbol), Some(named)) = (&output.symbol, &output.symbol_named) {
        write_symbol_named(&mut out, symbol, named);
        return out;
    }
    write_header(&mut out, output);
    if output.empty
        && output.changed_files.is_empty()
        && output.untracked_files.is_empty()
        && output.deleted_files.is_empty()
    {
        // Every change withheld is not a clean tree: the header already says
        // how many files were withheld and that none was read.
        if output.withheld_files > 0 {
            return out;
        }
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
        if output.layout_only.is_empty() || output.traced_definitions > 0 {
            let _ = writeln!(
                out,
                "No resolved static callers outside the diff in the analysed scope."
            );
        }
        write_layout_only(&mut out, output);
        write_untested_unlisted(&mut out, output);
        return out;
    }
    // A name defined all over a language without a binding model says the
    // same thing on every row: one line holds them, and the cap goes to the
    // callers that could be resolved (#430).
    fn quiet(s: &ChangedSymbol) -> bool {
        s.callers.is_empty() && s.compatible_callers == 0 && s.missed_in.is_empty() && s.commits < 2
    }
    let too_common =
        |s: &ChangedSymbol| quiet(s) && matches!(s.unresolved, Some(Unknown::Ambiguous { .. }));
    let fold = output.changed.iter().filter(|s| too_common(s)).count() >= 2;
    // So do the methods of one trait, each reached through `dyn` (#545).
    fn dispatched(s: &ChangedSymbol) -> Option<&str> {
        match &s.unresolved {
            Some(Unknown::Dispatched { trait_name }) if quiet(s) => Some(trait_name.as_str()),
            _ => None,
        }
    }
    let mut traits: Vec<&str> = output.changed.iter().filter_map(dispatched).collect();
    traits.sort_unstable();
    let shared: Vec<&str> = traits
        .chunk_by(|a, b| a == b)
        .filter(|members| members.len() >= 2)
        .map(|members| members[0])
        .collect();
    let folded = |s: &ChangedSymbol| {
        (fold && too_common(s)) || dispatched(s).is_some_and(|t| shared.contains(&t))
    };
    for sym in output.changed.iter().filter(|s| !folded(s)) {
        write_symbol(&mut out, sym, output.tests_known, compact);
    }
    if fold {
        write_too_common(&mut out, output.changed.iter().filter(|s| too_common(s)));
    }
    for t in shared {
        let members: Vec<&ChangedSymbol> = output
            .changed
            .iter()
            .filter(|s| dispatched(s) == Some(t))
            .collect();
        let _ = writeln!(
            out,
            "- {} changed method(s) implementing `{t}` (calls through `dyn {t}` or a generic bound are not resolved): {}",
            members.len(),
            named_list(&members)
        );
    }
    write_tail(&mut out, output);
    out
}

/// Names shown on the folded line before the rest is counted.
const TOO_COMMON_NAMED: usize = 8;

fn write_too_common<'a>(out: &mut String, symbols: impl Iterator<Item = &'a ChangedSymbol>) {
    let symbols: Vec<&ChangedSymbol> = symbols.collect();
    let mut languages: Vec<&str> = symbols
        .iter()
        .filter_map(|s| match &s.unresolved {
            Some(Unknown::Ambiguous { language, .. }) => Some(*language),
            _ => None,
        })
        .collect();
    languages.sort_unstable();
    languages.dedup();
    let _ = writeln!(
        out,
        "- {} changed definition(s) with names too common to trace by name ({} has no binding model here; callers not resolved): {}",
        symbols.len(),
        languages.join(", "),
        named_list(&symbols)
    );
}

fn named_list(symbols: &[&ChangedSymbol]) -> String {
    let names: Vec<String> = symbols
        .iter()
        .take(TOO_COMMON_NAMED)
        .map(|s| format!("{}::{}", s.path, s.symbol.as_deref().unwrap_or("?")))
        .collect();
    let more = symbols.len().saturating_sub(TOO_COMMON_NAMED);
    if more > 0 {
        format!("{}, and {more} more", names.join(", "))
    } else {
        names.join(", ")
    }
}

/// A caller no static test link reaches although one could have: the one
/// predicate behind the header count, the folded count and the line (#386).
pub(crate) fn without_test_link(c: &Caller) -> bool {
    c.evidence == "resolved" && c.tested_by.is_none() && c.registered_by.is_none()
}

/// A row that rests on a word match alone: listed as text, never counted or
/// rendered as a caller (R1).
pub(crate) fn is_textual(c: &Caller) -> bool {
    c.evidence == "name"
}

/// What every row built from a word match is labelled with.
pub(crate) const TEXTUAL_MATCH: &str = "a textual match, not a resolved binding";

/// Textual rows shown before they are counted.
const MAX_TEXTUAL_SHOWN: usize = 12;

/// The files that spell a name nothing defines, from one line each: not
/// documents, data, ignore files or lockfiles, which mention without
/// reading. Code and config come first (Forgejo #114).
pub(crate) fn symbol_named(
    root_dir: &Path,
    source: &crate::source::Source,
    lines: Vec<(String, u32, String)>,
    caveat: &str,
) -> Option<SymbolNamed> {
    let mut rows: Vec<NamedFile> = lines
        .into_iter()
        .filter(|(path, _, _)| {
            !is_document(path) && !selects_paths(path) && !is_data(root_dir, source, path)
        })
        .map(|(path, line, _)| NamedFile { path, line })
        .collect();
    if rows.is_empty() {
        return None;
    }
    rows.sort_by_key(|r| {
        (
            !(is_code_file(&r.path) || is_config_file(&r.path)),
            r.path.clone(),
        )
    });
    let files = rows.len();
    rows.truncate(MAX_TEXTUAL_SHOWN);
    Some(SymbolNamed {
        files,
        rows,
        caveat: caveat.to_string(),
    })
}

/// A `--symbol` query that only text answers: the output carries the names
/// and nothing else.
pub(crate) fn symbol_named_output(
    root_dir: &Path,
    query: &str,
    named: SymbolNamed,
) -> ImpactOutput {
    ImpactOutput {
        schema: IMPACT_SCHEMA,
        name: root_dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| root_dir.to_string_lossy().to_string()),
        range: None,
        index_tree: None,
        range_name: None,
        symbol: Some(query.to_string()),
        symbol_named: Some(named),
        changed_files: Vec::new(),
        untracked_files: Vec::new(),
        deleted_files: Vec::new(),
        stale_references: Vec::new(),
        stale_references_omitted: 0,
        named_by: Vec::new(),
        named_by_omitted: 0,
        key_searches: Vec::new(),
        key_files_unsearched: 0,
        collection_reads: Vec::new(),
        collection_reads_omitted: 0,
        commit_count: 0,
        changed: Vec::new(),
        contracts: Vec::new(),
        empty: false,
        truncated: 0,
        truncated_possible: 0,
        symbols_omitted: 0,
        truncated_untested: 0,
        truncated_textual: 0,
        changed_files_omitted: 0,
        untracked_files_omitted: 0,
        contracts_omitted: 0,
        limits: Vec::new(),
        tests_known: false,
        untested_definitions: 0,
        untested_unlisted: Vec::new(),
        untested_called_in_diff: Vec::new(),
        entry_unlisted: Vec::new(),
        layout_only: Vec::new(),
        lockfile_changes: Vec::new(),
        layout_only_omitted: 0,
        traced_definitions: 0,
        withheld_files: 0,
        unanalysed_files: Vec::new(),
        unanalysed_files_omitted: 0,
        untraced_omitted: 0,
        cap: IMPACT_TOKEN_CAP,
    }
}

fn write_symbol_named(out: &mut String, symbol: &str, named: &SymbolNamed) {
    let _ = writeln!(
        out,
        "diffctx impact for symbol {symbol}: no definition found{}; named in {} file(s) ({TEXTUAL_MATCH}):",
        named.caveat, named.files
    );
    for row in &named.rows {
        let _ = writeln!(out, "  - {}:{}", row.path, row.line);
    }
    if named.files > named.rows.len() {
        let _ = writeln!(out, "  - and {} more", named.files - named.rows.len());
    }
}

/// The word matches outside the diff, one row each: `path:line names <symbol>`.
/// A file the run did not fully read says why beside it.
fn textual_rows(output: &ImpactOutput) -> Vec<String> {
    let mut rows = Vec::new();
    for sym in &output.changed {
        let name = sym.symbol.as_deref().map_or(sym.path.as_str(), base_symbol);
        for c in sym.callers.iter().filter(|c| is_textual(c)) {
            let at = c.sites.first().map(|l| format!(":{l}")).unwrap_or_default();
            rows.push(format!("{}{at} names {name}", c.path));
        }
        for f in &sym.missed_in {
            let at = f.lines.first().map(|l| format!(":{l}")).unwrap_or_default();
            rows.push(format!(
                "{}{at} names {name} ({})",
                f.path,
                crate::parsers::unanalysed_text(f.reason)
            ));
        }
    }
    rows
}

/// The caller count as far as it is known: symbols whose callers could not
/// be resolved, or a run that stopped early, make a zero no finding (#387).
fn callers_phrase(output: &ImpactOutput, callers: usize, scope: &str) -> String {
    let unresolved: Vec<&str> = output
        .changed
        .iter()
        .filter(|c| c.unresolved.is_some())
        .map(|c| c.symbol.as_deref().unwrap_or(&c.path))
        .collect();
    let unresolved_names = {
        let shown: Vec<String> = unresolved
            .iter()
            .take(3)
            .map(|n| format!("`{n}`"))
            .collect();
        match unresolved.len().saturating_sub(3) {
            0 => shown.join(", "),
            more => format!("{} and {more} more", shown.join(", ")),
        }
    };
    let mut phrase =
        if callers == 0 && !unresolved.is_empty() && unresolved.len() == output.changed.len() {
            format!("callers unresolved for {unresolved_names}")
        } else {
            let mut p = format!("{callers} caller(s){scope}");
            if !unresolved.is_empty() {
                p.push_str(&format!(", callers unresolved for {unresolved_names}"));
            }
            p
        };
    // Word matches are no callers, and no zero either: they are counted
    // where the reader looks first (R1).
    let textual = textual_rows(output).len() + output.truncated_textual;
    if textual > 0 {
        phrase.push_str(&format!(", {textual} named only textually (not resolved)"));
    }
    if !output.limits.is_empty() {
        phrase.push_str(" found in a partial run");
    }
    phrase
}

/// Which changed files were read as text and why: their callers, the
/// definitions in them and the contracts they move are all missing from the
/// counts above, and the reader has to know that to read a zero (#454).
fn write_unanalysed(out: &mut String, output: &ImpactOutput) {
    let partial = |f: &&UnanalysedFile| f.reason == "parse_partial";
    let line = |files: Vec<&UnanalysedFile>, lead: &str, more: usize| {
        if files.is_empty() {
            return None;
        }
        let mut groups: Vec<(Option<&str>, &str, Vec<String>)> = Vec::new();
        for f in files {
            let shown = match f.lines.first() {
                Some(l) if f.reason == "parse_partial" => format!("{}:{l}", f.path),
                _ => f.path.clone(),
            };
            match groups
                .iter_mut()
                .find(|(l, r, _)| *l == f.language && *r == f.reason)
            {
                Some((_, _, paths)) => paths.push(shown),
                None => groups.push((f.language, f.reason, vec![shown])),
            }
        }
        let groups: Vec<String> = groups
            .into_iter()
            .map(|(language, reason, paths)| {
                let why = crate::parsers::unanalysed_text(reason);
                let examples = paths[..paths.len().min(UNANALYSED_EXAMPLES)].join(", ");
                let rest = if paths.len() > UNANALYSED_EXAMPLES {
                    ", …"
                } else {
                    ""
                };
                let why = if reason == "content_untraced" {
                    String::new()
                } else {
                    format!(" ({why})")
                };
                // A file outside code with no language is no code (#626).
                let kind = if reason == "content_untraced" {
                    "other"
                } else {
                    "code"
                };
                format!(
                    "{} {}{why}: {examples}{rest}",
                    paths.len(),
                    language.unwrap_or(kind),
                )
            })
            .collect();
        let more = if more > 0 {
            format!(" and {more} more")
        } else {
            String::new()
        };
        Some(format!("({lead}: {}{more})", groups.join("; ")))
    };
    let untraced = |f: &&UnanalysedFile| f.reason == "content_untraced";
    let whole: Vec<&UnanalysedFile> = output
        .unanalysed_files
        .iter()
        .filter(|f| !partial(f) && !untraced(f))
        .collect();
    let parts: Vec<&UnanalysedFile> = output.unanalysed_files.iter().filter(partial).collect();
    let content: Vec<&UnanalysedFile> = output.unanalysed_files.iter().filter(untraced).collect();
    for text in [
        line(
            whole,
            "not analysed — definitions, callers and tests in them are not counted",
            output.unanalysed_files_omitted - output.untraced_omitted,
        ),
        line(
            parts,
            "analysed only in part — what sits on the lines the parse broke on is not counted",
            0,
        ),
        line(
            content,
            "not followed — only lines naming these files by path were searched; search for the keys and values the change touched",
            output.untraced_omitted,
        ),
    ]
    .into_iter()
    .flatten()
    {
        let _ = writeln!(out, "{text}");
    }
    if output.key_files_unsearched > 0 {
        let _ = writeln!(
            out,
            "(key search capped: the keys and values of {} changed config(s) were not searched)",
            output.key_files_unsearched
        );
    }
}

fn write_header(out: &mut String, output: &ImpactOutput) {
    let range = output
        .range_name
        .clone()
        .unwrap_or_else(|| range_label(output.range.as_deref(), output.index_tree.as_deref()));
    let all = || output.changed.iter().flat_map(|c| &c.callers);
    let callers = all().filter(|c| c.evidence == "resolved").count() + output.truncated
        - output.truncated_possible
        - output.truncated_textual;
    let possible = all().filter(|c| c.evidence == "candidate").count() + output.truncated_possible;
    let untested = all().filter(|c| without_test_link(c)).count() + output.truncated_untested;
    // Labelled, and only beside a caller: "0 without a static test link"
    // next to the definitions' count read as both 0 and N untested (#493).
    let mut guard_summary = if output.tests_known && callers > 0 {
        format!(", {untested} caller(s) without a static test link")
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
    // Withheld files matter where they could hide the answer: a changed
    // file, or a symbol with no caller yet. A symbol that has callers is
    // answered, and the count on every lookup taught skipping (#656).
    let withheld_count = if output.symbol.is_some() && callers + possible > 0 {
        0
    } else {
        output.withheld_files
    };
    let withheld = |what: &str| {
        let mut clause = String::new();
        if withheld_count > 0 {
            clause = format!(", {} {what} withheld by ignore rules", withheld_count);
        }
        let count = |reason: &str| {
            output
                .unanalysed_files
                .iter()
                .filter(|f| f.reason == reason)
                .count()
        };
        let partial = count("parse_partial");
        let untraced = count("content_untraced") + output.untraced_omitted;
        let unanalysed =
            output.unanalysed_files.len() + output.unanalysed_files_omitted - partial - untraced;
        if unanalysed > 0 {
            clause.push_str(&format!(", {unanalysed} not analysed"));
        }
        if partial > 0 {
            clause.push_str(&format!(", {partial} analysed only in part"));
        }
        if untraced > 0 {
            clause.push_str(&format!(", {untraced} not followed"));
        }
        clause
    };
    match &output.symbol {
        Some(symbol) => {
            let _ = writeln!(
                out,
                "diffctx impact for symbol {symbol}: {} definition(s){}, {}{guard_summary}",
                output.changed.len() + output.symbols_omitted,
                withheld("file(s) naming it"),
                callers_phrase(output, callers, ""),
            );
        }
        None => {
            let reads = output.collection_reads.len() + output.collection_reads_omitted;
            let _ = writeln!(
                out,
                "diffctx impact for {range}: {} changed file(s){}{}{}, {}{}{guard_summary}",
                output.changed_files.len()
                    + output.changed_files_omitted
                    + output.deleted_files.len(),
                if output.deleted_files.is_empty() {
                    String::new()
                } else {
                    format!(" ({} deleted)", output.deleted_files.len())
                },
                match output.untracked_files.len() + output.untracked_files_omitted {
                    0 => String::new(),
                    n => format!(", {n} untracked"),
                },
                withheld("file(s)"),
                callers_phrase(
                    output,
                    callers,
                    if output.unanalysed_files.is_empty() {
                        " outside the diff"
                    } else {
                        " outside the diff in the analysed files"
                    }
                ),
                match reads {
                    0 => String::new(),
                    n => format!(", {n} read(s) by collection name"),
                },
            );
        }
    }
    write_unanalysed(out, output);
    if withheld_count > 0 {
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
            "(partial: {}; callers may be missing)",
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
        (None, _) if c.unread_test.is_some() => format!(
            "no static test link found; {} names {} but was not read (over the size cap)",
            c.unread_test.as_deref().unwrap_or_default(),
            c.symbol.as_deref().map(base_symbol).unwrap_or("the caller")
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
        Some("import_glob") => "loaded by an import glob",
        Some("search_path") => "schema left to the search path",
        Some("global") => "a global; which scripts load it is not traced",
        _ => "binding unresolved",
    }
}

fn write_symbol(out: &mut String, sym: &ChangedSymbol, tests_known: bool, compact: bool) {
    let commits = if sym.commits >= 2 {
        format!(" [touched by {} commits]", sym.commits)
    } else {
        String::new()
    };
    // A removed definition's range is on the base side, not in the head file.
    let lines = if sym.kind == "removed" {
        format!("removed; was {}", sym.lines)
    } else {
        sym.lines.clone()
    };
    let _ = writeln!(
        out,
        "- {}{commits}{}",
        label(&sym.path, sym.symbol.as_deref(), &lines),
        if sym.untested && tests_known {
            match &sym.entry {
                Some(role) => format!(" \u{2014} {role} \u{2014} no static test link"),
                None => " — no static test link".to_string(),
            }
        } else {
            sym.entry
                .as_ref()
                .map(|role| format!(" \u{2014} {role}"))
                .unwrap_or_default()
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
    for c in sym.callers.iter().filter(|c| c.evidence == "resolved") {
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

/// Unlisted definitions shown by name before they are counted.
const MAX_UNTESTED_NAMED: usize = 8;

fn write_untested_unlisted(out: &mut String, output: &ImpactOutput) {
    write_untested(out, output);
    write_list(
        out,
        "Changed, entered by a runner or the framework, not by a caller:",
        &output.entry_unlisted,
        MAX_UNTESTED_NAMED,
        0,
    );
}

fn write_untested(out: &mut String, output: &ImpactOutput) {
    if !output.tests_known {
        return;
    }
    // Under a limit, no caller found is all the run can say.
    let uncalled = if output.limits.is_empty() {
        "Changed, nothing calls them, no static test link:"
    } else {
        "Changed, no caller found in this partial run, no static test link:"
    };
    write_list(
        out,
        uncalled,
        &output.untested_unlisted,
        MAX_UNTESTED_NAMED,
        0,
    );
    write_list(
        out,
        "Changed, called only from inside the diff, no static test link:",
        &output.untested_called_in_diff,
        MAX_UNTESTED_NAMED,
        0,
    );
}

fn write_layout_only(out: &mut String, output: &ImpactOutput) {
    write_list(
        out,
        "Changed only in comments, formatting or position, which no caller sees (callers not traced):",
        &output.layout_only,
        MAX_UNTESTED_NAMED,
        output.layout_only_omitted,
    );
}

/// A heading and its first `shown` lines; the rest, `omitted` included, is a
/// count. The heading prints whenever something is counted, so an omitted
/// count never sits under another list's heading (#451).
fn write_list(out: &mut String, heading: &str, lines: &[String], shown: usize, omitted: usize) {
    if lines.is_empty() && omitted == 0 {
        return;
    }
    let _ = writeln!(out, "{heading}");
    for line in lines.iter().take(shown) {
        let _ = writeln!(out, "  - {line}");
    }
    let more = lines.len().saturating_sub(shown) + omitted;
    if more > 0 {
        let _ = writeln!(out, "  - and {more} more");
    }
}

/// The symbol names of `layout_only`, from its `path::symbol (lines)` labels.
pub fn layout_only_names(output: &ImpactOutput) -> impl Iterator<Item = &str> {
    output.layout_only.iter().filter_map(|l| {
        let (_, rest) = l.split_once("::")?;
        rest.rsplit_once(" (").map(|(name, _)| name)
    })
}

fn write_tail(out: &mut String, output: &ImpactOutput) {
    write_list(
        out,
        "Lock files changed (dependency versions moved; what that reaches is not traced):",
        &output.lockfile_changes,
        usize::MAX,
        0,
    );
    write_layout_only(out, output);
    write_untested_unlisted(out, output);
    let stale: Vec<String> = output
        .stale_references
        .iter()
        .map(|r| format!("{}:{} names {} ({})", r.path, r.line, r.target, r.kind))
        .collect();
    write_list(
        out,
        "Still naming a deleted or renamed path:",
        &stale,
        usize::MAX,
        output.stale_references_omitted,
    );
    let textual = textual_rows(output);
    write_list(
        out,
        &format!("Named in code outside the diff ({TEXTUAL_MATCH}):"),
        &textual,
        MAX_TEXTUAL_SHOWN,
        output.truncated_textual,
    );
    let named: Vec<String> = output
        .named_by
        .iter()
        .filter(|r| r.key.is_none())
        .map(|r| format!("{}:{} names {}", r.path, r.line, r.target))
        .collect();
    write_list(
        out,
        "Named by path outside the diff (a command line, a config, another runtime):",
        &named,
        usize::MAX,
        output.named_by_omitted,
    );
    let keyed: Vec<String> = output
        .named_by
        .iter()
        .filter_map(|r| {
            let key = r.key.as_deref()?;
            Some(format!("{}:{} names {key} ({})", r.path, r.line, r.target))
        })
        .collect();
    write_list(
        out,
        &format!("Named by a changed key or value outside the diff ({TEXTUAL_MATCH}):"),
        &keyed,
        usize::MAX,
        0,
    );
    let unnamed: Vec<String> = output
        .key_searches
        .iter()
        .filter(|k| k.named == 0)
        .map(|k| format!("{} ({})", k.path, k.keys.join(", ")))
        .collect();
    if !unnamed.is_empty() {
        let _ = writeln!(
            out,
            "(changed keys and values searched as text, named nowhere outside the diff: {})",
            unnamed.join("; ")
        );
    }
    let reads: Vec<String> = output
        .collection_reads
        .iter()
        .map(|r| match &r.collection {
            Some(c) => format!("{}:{} reads collection {c}", r.path, r.line),
            None => format!("{}:{} reads a collection named at run time", r.path, r.line),
        })
        .collect();
    write_list(
        out,
        "Read by collection name outside the diff (Astro content collections):",
        &reads,
        usize::MAX,
        output.collection_reads_omitted,
    );
    let contracts: Vec<String> = output
        .contracts
        .iter()
        .map(|c| match &c.symbol {
            Some(s) => format!("{} {}::{s}", c.kind, c.path),
            None => format!("{} {}", c.kind, c.path),
        })
        .collect();
    write_list(
        out,
        "Contracts changed:",
        &contracts,
        usize::MAX,
        output.contracts_omitted,
    );
    if output.truncated > 0 {
        let _ = writeln!(
            out,
            "({} weaker caller(s) omitted to stay under {} tokens; a larger budget (--budget, budget_tokens) lists them)",
            output.truncated, output.cap
        );
    }
    let callers = || output.changed.iter().flat_map(|c| &c.callers);
    if callers().any(|c| c.tested_by.is_some() && !is_textual(c)) {
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
