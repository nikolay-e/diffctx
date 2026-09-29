use std::fmt::Write as _;
use std::path::Path;
use std::time::Instant;

use rustc_hash::{FxHashMap, FxHashSet};
use schemars::JsonSchema;
use serde::Serialize;

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
/// Paths listed before the rest fold into a count: the reader has the diff.
const MAX_CHANGED_FILES_LISTED: usize = 24;

/// A symbol name with more definitions than this cannot be traced by name.
const MAX_DEFINITIONS: usize = 2;

const SCHEMA_FILE_MARKERS: &[&str] = &[
    "/migrations/",
    "/migration/",
    "/alembic/versions/",
    "schema",
    "openapi",
    "swagger",
];
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
#[derive(Serialize, JsonSchema)]
pub struct ImpactOutput {
    pub schema: &'static str,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<String>,
    pub changed_files: Vec<String>,
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
    /// The run stopped at a limit (a deadline, a cap): callers may be
    /// missing, and `empty` is never claimed while this is set.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limits: Vec<crate::resource::LimitReason>,
    /// Whether any test file was in view. Without one, an absent `tested_by`
    /// says nothing about the caller, and the text form does not call it
    /// untested.
    pub tests_known: bool,
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
}

#[derive(Serialize, JsonSchema, Clone)]
pub struct Caller {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    pub lines: String,
    pub weight: f64,
    /// A test file that names this caller or is linked to its file; absent
    /// means the suite as diffctx sees it does not exercise it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tested_by: Option<String>,
}

#[derive(Serialize, JsonSchema, Clone)]
pub struct Contract {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    pub kind: &'static str,
}

fn is_container(kind: FragmentKind) -> bool {
    use FragmentKind as K;
    matches!(
        kind,
        K::Function
            | K::Class
            | K::Struct
            | K::Impl
            | K::Interface
            | K::Enum
            | K::Module
            | K::Type
            | K::Record
            | K::Property
            | K::Declaration
            | K::Definition
    )
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

fn mentions(caller: &Fragment, symbol: Option<&str>) -> bool {
    let Some(sym) = symbol else { return true };
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let ext = Path::new(caller.path())
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let hash_comments = HASH_COMMENT_EXTENSIONS.contains(&ext.as_str());
    caller.content.lines().any(|line| {
        let head = line.trim_start();
        // A `*` alone opens a block-comment continuation; `*out = f()` is a
        // deref assignment, and `#[derive]` is Rust, not a comment.
        let star_comment = head.starts_with('*')
            && head[1..]
                .chars()
                .next()
                .is_none_or(|c| c.is_whitespace() || c == '/');
        if head.starts_with("//")
            || (hash_comments && head.starts_with('#'))
            || star_comment
            || head.starts_with("/*")
            || head.starts_with("--")
        {
            return false;
        }
        let bytes = line.as_bytes();
        line.match_indices(sym).any(|(at, _)| {
            let before = at.checked_sub(1).map(|i| bytes[i]);
            let after = bytes.get(at + sym.len()).copied();
            !before.is_some_and(is_word) && !after.is_some_and(is_word)
        })
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

/// A caller that never names the changed file's module and lives in another
/// directory is a name match, not a call: `Command` in a binary crate is not
/// `std::process::Command`, however many files construct one. Same-directory
/// callers in a package-scoped language (Go, Java, Kotlin) pass, because a
/// package needs no import to reach its neighbours.
fn references_module(
    caller: &Fragment,
    file_identifiers: &FxHashMap<&str, FxHashSet<&str>>,
    changed_path: &str,
) -> bool {
    let changed = Path::new(changed_path);
    let caller_path = Path::new(caller.path());
    let empty = FxHashSet::default();
    // The import sits at the top of the file, outside the calling fragment.
    let file_words = file_identifiers.get(caller.path()).unwrap_or(&empty);
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

/// The fragment itself when it is a named container, else the smallest
/// named container in the same file that encloses it.
fn named_container_of<'a>(core: &'a Fragment, all: &'a [Fragment]) -> Option<&'a Fragment> {
    let is_named = |f: &Fragment| is_container(f.kind) && f.symbol_name.is_some();
    if is_named(core) {
        return Some(core);
    }
    all.iter()
        .filter(|f| {
            f.path() == core.path()
                && is_named(f)
                && f.id.start_line <= core.id.start_line
                && f.id.end_line >= core.id.end_line
                && f.id != core.id
        })
        .min_by_key(|f| f.line_count())
}

/// Of fragments nested in one another (a method and its impl, a function
/// and its enclosing class), the innermost is the one that says where.
fn innermost<'a>(mut frags: Vec<&'a Fragment>) -> Vec<&'a Fragment> {
    frags.sort_by(|a, b| a.id.cmp(&b.id));
    let keep: Vec<bool> = frags
        .iter()
        .map(|outer| {
            !frags.iter().any(|inner| {
                inner.id != outer.id
                    && inner.path() == outer.path()
                    && inner.id.start_line >= outer.id.start_line
                    && inner.id.end_line <= outer.id.end_line
            })
        })
        .collect();
    frags
        .into_iter()
        .zip(keep)
        .filter_map(|(f, k)| k.then_some(f))
        .collect()
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
    head.starts_with("pub ")
        || head.starts_with("pub(")
        || head.starts_with("export ")
        || head.starts_with("public ")
        || head.starts_with("module.exports")
        || head.starts_with("exports.")
}

fn is_schema_file(path: &str) -> bool {
    let lower = path.to_lowercase();
    SCHEMA_FILE_EXTENSIONS
        .iter()
        .any(|ext| lower.ends_with(ext))
        || SCHEMA_FILE_MARKERS.iter().any(|m| lower.contains(m))
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
        }
    }

    fn any(&self) -> bool {
        !self.test_fragments.is_empty() || !self.by_path.is_empty()
    }

    /// A test guards a caller when it names the caller's symbol in code and
    /// reaches the caller's module — the same bar a caller has to clear. A
    /// bare name match (`render` in a Rust test, `render()` in a Python
    /// script) attributed guards to tests that never import the file (#312).
    fn tested_by(
        &self,
        caller: &Fragment,
        file_identifiers: &FxHashMap<&str, FxHashSet<&str>>,
    ) -> Option<String> {
        if crate::testfiles::is_test_path(Path::new(caller.path())) {
            return Some(caller.path().to_string());
        }
        if let Some(path) = self.by_path.get(caller.path()) {
            return Some(path.clone());
        }
        // A chunk inside a long function is named `main[88]`; the test
        // names `main`.
        let symbol = caller
            .symbol_name
            .as_deref()
            .map(|s| s.split('[').next().unwrap_or(s))
            .filter(|s| s.len() >= 3)?;
        self.test_fragments
            .iter()
            .find(|t| {
                mentions(t, Some(symbol)) && references_module(t, file_identifiers, caller.path())
            })
            .map(|t| t.path().to_string())
    }
}

/// `deadline`: the line log (one git call per changed symbol) runs only
/// while there is time left; commit overlap is the least actionable of the
/// three signals and the first to yield.
pub fn build_impact(
    state: &ScoredState,
    diff_range: Option<&str>,
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

    let bounds = commit_bounds(diff_range).filter(|_| state.commit_count >= 2);

    let mut changed: Vec<ChangedSymbol> = Vec::new();
    // Schema changes first, then the public symbols something calls, then
    // the rest: what the cap keeps is what a reader would look at first.
    let mut contracts: Vec<Contract> = Vec::new();
    let mut public_api: Vec<(bool, Contract)> = Vec::new();
    // Definitions are counted per language: the builders link a call to the
    // definitions of its own language, so a Python wrapper named like a Rust
    // function is not what makes the Rust name ambiguous.
    let mut definitions: FxHashMap<(&str, String), usize> = FxHashMap::default();
    let mut defined_in: FxHashSet<(String, &str)> = FxHashSet::default();
    let mut file_identifiers: FxHashMap<&str, FxHashSet<&str>> = FxHashMap::default();
    for f in &state.all_fragments {
        file_identifiers
            .entry(f.path())
            .or_default()
            .extend(f.identifiers.iter().map(String::as_str));
        if let Some(name) = f.symbol_name.as_deref().filter(|_| is_container(f.kind)) {
            let lang = crate::languages::get_language_for_file(f.path()).unwrap_or("");
            *definitions.entry((lang, name.to_lowercase())).or_default() += 1;
            defined_in.insert((name.to_string(), f.path()));
        }
    }
    let cores = innermost(cores);
    for (i, core) in cores.iter().enumerate() {
        let symbol = core.symbol_name.as_deref();
        // `build`, `new`, `run`: a name defined all over the repository links
        // by name to every definition, and its "callers" would be everyone's.
        let core_lang = crate::languages::get_language_for_file(core.path()).unwrap_or("");
        let ambiguous = symbol
            .map(|s| {
                definitions
                    .get(&(core_lang, s.to_lowercase()))
                    .copied()
                    .unwrap_or(0)
                    > MAX_DEFINITIONS
            })
            .unwrap_or(false);
        let mut sources: Vec<(&Fragment, f64)> = Vec::new();
        // Why a linked fragment is not listed, per core, under RUST_LOG=debug:
        // the precision filters are the part of this view that field reports
        // question first.
        let mut dropped: FxHashMap<&'static str, usize> = FxHashMap::default();
        graph.for_each_reverse_neighbor(&core.id, |src, weight| {
            let reason = if ambiguous {
                "ambiguous_name"
            } else if changed_paths.contains(src.path.as_ref()) || state.core_ids.contains(src) {
                "inside_the_diff"
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
                let shadowed =
                    symbol.is_some_and(|s| defined_in.contains(&(s.to_string(), frag.path())));
                if frag.kind == FragmentKind::Excerpt {
                    "excerpt"
                } else if shadowed {
                    "defines_same_name"
                } else if is_import_block(&frag.content) {
                    "import_block"
                } else if !is_code_file(frag.path()) {
                    "not_code"
                } else if !mentions(frag, symbol) {
                    "no_mention_in_code"
                } else if !references_module(frag, &file_identifiers, core.path()) {
                    "no_module_reference"
                } else {
                    sources.push((frag, weight));
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
            sources.iter().map(|(f, w)| (&f.id, *w)).collect();
        // An import line links too, at the import weight; the function that
        // makes the call is the caller, the import is how it got the name.
        let named_files: FxHashSet<&str> = sources
            .iter()
            .filter(|(f, _)| f.symbol_name.is_some())
            .map(|(f, _)| f.path())
            .collect();
        sources.retain(|(f, _)| f.symbol_name.is_some() || !named_files.contains(f.path()));
        let mut callers: Vec<Caller> = innermost(sources.iter().map(|(f, _)| *f).collect())
            .into_iter()
            .map(|frag| Caller {
                path: rel(state, frag.path()),
                symbol: frag.symbol_name.clone(),
                lines: lines_of(&frag.id),
                weight: (weights[&frag.id] * 1e3).round() / 1e3,
                tested_by: tests
                    .tested_by(frag, &file_identifiers)
                    .map(|p| rel(state, &p)),
            })
            .collect();
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
        if is_public(core.path(), symbol, &core.content) {
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
        if callers.is_empty() && commits < 2 {
            continue;
        }
        changed.push(ChangedSymbol {
            path: rel(state, core.path()),
            symbol: core.symbol_name.clone(),
            lines: lines_of(&core.id),
            kind: format!("{:?}", core.kind).to_lowercase(),
            commits,
            callers,
        });
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
    for path in &changed_files {
        if is_schema_file(path) {
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
    let limits = state.run.reasons();
    let empty = changed.is_empty() && contracts.is_empty() && limits.is_empty();
    let mut output = ImpactOutput {
        schema: IMPACT_SCHEMA,
        name: state
            .root_dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| state.root_dir.to_string_lossy().to_string()),
        range: diff_range.map(str::to_string),
        changed_files,
        commit_count: state.commit_count,
        changed,
        contracts,
        empty,
        truncated: 0,
        changed_files_omitted: 0,
        contracts_omitted,
        limits,
        tests_known: tests.any(),
    };
    fit_to_cap(&mut output);
    output
}

fn serialized_tokens(output: &ImpactOutput) -> u32 {
    serde_json::to_string(output)
        .map(|s| crate::tokenizer::count_tokens(&s))
        .unwrap_or(0)
}

/// Drops the globally weakest caller until the JSON fits the cap, then
/// symbols that have nothing left to say. The cap is on what the reader
/// receives, so it is measured on the serialized text, not on parts.
fn fit_to_cap(output: &mut ImpactOutput) {
    while serialized_tokens(output) > IMPACT_TOKEN_CAP {
        // Callers are the answer; everything else yields to them first, and
        // the file list — which the reader holds as the diff — first of all.
        if output.changed_files.len() > MAX_CHANGED_FILES_LISTED {
            output.changed_files_omitted += output.changed_files.len() - MAX_CHANGED_FILES_LISTED;
            output.changed_files.truncate(MAX_CHANGED_FILES_LISTED);
            continue;
        }
        if output.contracts.pop().is_some() {
            output.contracts_omitted += 1;
            continue;
        }
        if let Some(i) = output.changed.iter().rposition(|c| c.callers.is_empty()) {
            output.changed.remove(i);
            continue;
        }
        let weakest = output
            .changed
            .iter()
            .enumerate()
            .filter(|(_, c)| !c.callers.is_empty())
            .min_by(|(_, a), (_, b)| {
                let wa = a.callers.last().map(|c| c.weight).unwrap_or(0.0);
                let wb = b.callers.last().map(|c| c.weight).unwrap_or(0.0);
                wa.total_cmp(&wb)
            })
            .map(|(i, _)| i);
        match weakest {
            // The strongest caller of a symbol is the fact; the symbol goes
            // whole once that is all it has left, never one caller at a time.
            Some(i) if output.changed[i].callers.len() > 1 => {
                output.changed[i].callers.pop();
                output.truncated += 1;
            }
            Some(i) => {
                output.truncated += output.changed[i].callers.len();
                output.changed.remove(i);
            }
            None => return,
        }
    }
    output.empty = output.changed.is_empty()
        && output.contracts.is_empty()
        && output.truncated == 0
        && output.contracts_omitted == 0
        && output.limits.is_empty();
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
    let mut out = String::new();
    let range = output.range.as_deref().unwrap_or("working tree");
    let callers: usize = output.changed.iter().map(|c| c.callers.len()).sum();
    let untested = output
        .changed
        .iter()
        .flat_map(|c| &c.callers)
        .filter(|c| c.tested_by.is_none())
        .count();
    let guard_summary = if output.tests_known {
        format!(", {untested} untested")
    } else {
        String::new()
    };
    let _ = writeln!(
        out,
        "diffctx impact for {range}: {} changed file(s), {} caller(s) outside the diff{guard_summary}",
        output.changed_files.len() + output.changed_files_omitted,
        callers
    );
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
    if output.empty {
        let _ = writeln!(out, "Nothing outside the diff depends on this change.");
        return out;
    }
    for sym in &output.changed {
        let commits = if sym.commits >= 2 {
            format!(" [touched by {} commits]", sym.commits)
        } else {
            String::new()
        };
        let _ = writeln!(
            out,
            "- {}{commits}",
            label(&sym.path, sym.symbol.as_deref(), &sym.lines)
        );
        // Thirty call sites in one test module are one fact, not thirty
        // lines: the first few name where, the count says how many.
        let mut shown_per_file: FxHashMap<&str, usize> = FxHashMap::default();
        let mut folded_per_file: FxHashMap<&str, (usize, usize)> = FxHashMap::default();
        for c in &sym.callers {
            let shown = shown_per_file.entry(c.path.as_str()).or_default();
            if *shown >= MAX_CALLERS_SHOWN_PER_FILE {
                let entry = folded_per_file.entry(c.path.as_str()).or_default();
                entry.0 += 1;
                if c.tested_by.is_none() {
                    entry.1 += 1;
                }
                continue;
            }
            *shown += 1;
            let guard = match (&c.tested_by, output.tests_known) {
                (Some(t), _) => format!("tested by {t}"),
                (None, true) => "UNTESTED".to_string(),
                (None, false) => "no test link".to_string(),
            };
            let _ = writeln!(
                out,
                "  - called from {} — {guard}",
                label(&c.path, c.symbol.as_deref(), &c.lines)
            );
        }
        let mut folded: Vec<(&str, (usize, usize))> = folded_per_file.into_iter().collect();
        folded.sort();
        for (path, (more, untested)) in folded {
            let _ = writeln!(
                out,
                "  - and {more} more call site(s) in {path}{}",
                if untested > 0 && output.tests_known {
                    format!(", {untested} UNTESTED")
                } else {
                    String::new()
                }
            );
        }
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
            output.truncated, IMPACT_TOKEN_CAP
        );
    }
    out
}

/// JSON Schema 2020-12 for `diffctx.impact.v1`, pinned by `tests/context_schema.rs`.
pub fn impact_schema() -> serde_json::Value {
    let mut generator = schemars::generate::SchemaSettings::draft2020_12().into_generator();
    let schema = generator.root_schema_for::<ImpactOutput>();
    serde_json::to_value(schema).expect("schema serializes")
}
