//! What a change did to a definition: its public surface, whether it moved verbatim, a signature that
//! changed, the schema files, and the commits that touched it.

use std::path::Path;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::decide::cells::Cells;

use crate::run_provenance::state::ScoredState;

use diffctx_base::languages::runtime_of;

use diffctx_base::types::{Fragment, FragmentId};

use diffctx_occurrence::tokens::Names;

use super::MAX_REMOVED_NAMES_TRACED;
use super::MAX_SIGNATURE_LINES;
use super::dispatch::base_symbol;
use super::removed::read_or_note;

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

/// The files of the definition's group that spell one of `names`, by path as the
/// answer prints it: what the token index holds, whatever a builder linked. A file
/// of no group, or a group the run could not read, has none.
pub(super) fn spelling_files(
    state: &ScoredState,
    cells: &Cells,
    file: &str,
    names: &[String],
) -> Vec<String> {
    let Some(tokens) = diffctx_resolve::vocabulary::family::Group::of_name(file)
        .and_then(|group| cells.group_tokens(state, group))
    else {
        return Vec::new();
    };
    let wanted = Names::new(names.iter().map(String::as_str));
    tokens
        .spelling(&wanted)
        .into_iter()
        .map(|at| tokens.files()[at as usize].as_str().to_string())
        .collect()
}

/// The names a use of a definition the fragments hold could carry: its own, and the
/// last part of a qualified one (a method is called as `m`, not `A.m`).
pub(super) fn names_of(symbol: &str) -> Vec<String> {
    let name = base_symbol(symbol);
    let last = name.rsplit(['.', ':']).next().unwrap_or(name);
    let mut names = vec![name.to_string()];
    if last != name {
        names.push(last.to_string());
    }
    names
}

/// No runtime crosses from one to the other: a Kotlin test cannot run a
/// Swift function, and a Python name in a Kotlin file is not a call of it
/// (#458, #487).
pub(super) fn same_runtime(a: &str, b: &str) -> bool {
    let (ra, rb) = (runtime_of(a), runtime_of(b));
    ra.is_empty() || rb.is_empty() || ra == rb
}

pub(super) fn rel(state: &ScoredState, path: &str) -> String {
    diffctx_base::paths::display_rel_or_abs(&state.root_dir, Path::new(path))
}

/// The changed files, by the path the answer prints.
pub(super) fn changed_rels(state: &ScoredState) -> impl Iterator<Item = String> + '_ {
    state
        .changed_files
        .iter()
        .map(|p| rel(state, &p.to_string_lossy()))
}

/// The revision a range ends at, when it names one.
pub(super) fn head_of(range: Option<&str>) -> Option<String> {
    range
        .map(diffctx_snapshot::snapshot::split_diff_range)
        .and_then(|(_, head)| head)
}

/// The lines of the tree at the range's head that spell one of `needles`.
pub(super) fn spelled_lines(
    state: &ScoredState,
    diff_range: Option<&str>,
    needles: &[String],
) -> diffctx_snapshot::grep::Grepped<diffctx_snapshot::snapshot::Row> {
    read_or_note(
        state,
        diffctx_snapshot::grep::grep_lines(
            &state.root_dir,
            &state.run.source(),
            needles,
            head_of(diff_range).as_deref(),
        ),
    )
}

/// The files the search for the changed definitions' uses did not read that spell
/// one of their names: those the ignore rules withhold are counted on the run, and
/// the number outside the run's pathspecs is returned. What they say of a caller is
/// unknown, so a definition nothing read calls is not one nothing calls (gh#713,
/// fj#221).
pub(super) fn unread_namers(
    state: &ScoredState,
    diff_range: Option<&str>,
    mut names: Vec<String>,
) -> usize {
    names.sort_unstable();
    names.dedup();
    names.truncate(MAX_REMOVED_NAMES_TRACED);
    let grepped = read_or_note(
        state,
        diffctx_snapshot::grep::grep_code_words(
            &state.root_dir,
            &state.run.source(),
            &names,
            head_of(diff_range).as_deref(),
            1,
        ),
    );
    // A changed file the policy withholds is counted by the change set already.
    state.run.note_withheld(
        grepped
            .withheld
            .into_iter()
            .filter(|path| {
                let at = diffctx_base::paths::repo_join(&state.root_dir, path);
                !state.diffed_files.contains(&at)
            })
            .collect(),
    );
    let outside: FxHashSet<&str> = grepped
        .rows
        .iter()
        .map(|(path, _, _)| path.as_str())
        .filter(|path| {
            !diffctx_snapshot::source::in_scope(
                &state.root_dir,
                &diffctx_base::paths::repo_join(&state.root_dir, path),
                &state.scope,
            )
        })
        .collect();
    outside.len()
}

/// Files whose change no reader sees: a layout edit, and a file git renames byte
/// for byte (#593), whose stale namers are the stale list's.
pub(super) fn layout_files(state: &ScoredState) -> FxHashSet<&str> {
    let edited: FxHashSet<&str> = state.hunk_texts.iter().map(|h| h.path.as_str()).collect();
    state
        .change_classes
        .iter()
        .filter(|(_, class, _)| *class == diffctx_facts::change_class::ChangeClass::Layout)
        .map(|(p, _, _)| p.as_str())
        .chain(
            state
                .renamed_files
                .iter()
                .map(|(_, new)| new.as_str())
                .filter(|new| !edited.contains(new)),
        )
        .collect()
}

pub(super) fn lines_of(id: &FragmentId) -> String {
    format!("{}-{}", id.start_line, id.end_line)
}

/// Visibility the source states: `pub`, `export`, `public`, a Go
/// capitalised name. Python and Ruby state none, so no contract is claimed
/// for them rather than a guessed one.
pub(super) fn is_public(path: &str, symbol: Option<&str>, content: &str) -> bool {
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
pub(super) fn moved_verbatim(
    run: &diffctx_base::changed_lines::HunkText,
    hunks: &[diffctx_base::changed_lines::HunkText],
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

pub(super) fn is_schema_file(path: &str) -> bool {
    // A test named for the schema it checks is not a schema.
    if diffctx_base::testfiles::is_test_path(Path::new(path)) {
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

/// A fragment without its decorators. The decorator belongs to the fragment
/// (Python `@app.route(…, methods={"GET"})`, a TS `@Component({…})`) but not
/// to the declaration: `export` comes after it, and a `{` in it would end the
/// declaration's head before the signature began.
pub(super) fn without_decorators(content: &str) -> &str {
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
pub(super) fn declaration_head(content: &str) -> String {
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

/// The base side's text of the files whose public signatures impact compares,
/// read through the analysis' base reader and kept: the check of each public
/// symbol read the same few files again and again. Only a read that succeeded
/// is kept, so a failure is met again as the first one was.
pub(super) struct BaseTexts<'a> {
    root: &'a Path,
    source: diffctx_snapshot::source::Source,
    texts: std::cell::RefCell<FxHashMap<String, std::rc::Rc<str>>>,
}

impl<'a> BaseTexts<'a> {
    pub(super) fn new(root: &'a Path, base: diffctx_snapshot::source::Source) -> Self {
        Self {
            root,
            source: base,
            texts: std::cell::RefCell::default(),
        }
    }

    /// `Ok(None)`: the base has no such file.
    fn read(&self, rel_path: &str) -> diffctx_snapshot::snapshot::Result<Option<std::rc::Rc<str>>> {
        if let Some(text) = self.texts.borrow().get(rel_path) {
            return Ok(Some(text.clone()));
        }
        let Some(text) = self.source.read_lossy(&self.root.join(rel_path))? else {
            return Ok(None);
        };
        let text: std::rc::Rc<str> = text.into();
        self.texts
            .borrow_mut()
            .insert(rel_path.to_string(), text.clone());
        Ok(Some(text))
    }
}

/// Whether an exported symbol's declaration moved: its head is looked for in
/// the file at the base revision, whitespace-insensitively. A body-only edit
/// is what the callers list already says, and a contracts section repeated
/// on every refactor teaches the reader to skip it (#325). A file the base
/// does not have is new, and every export in it is a new contract.
pub(super) fn signature_changed(
    state: &ScoredState,
    base_texts: &BaseTexts,
    core: &Fragment,
) -> bool {
    let rel_path = rel(state, core.path());
    let old = match base_texts.read(&rel_path) {
        Ok(Some(old)) => old,
        Ok(None) => return true,
        Err(e) => {
            state.run.note(diffctx_snapshot::resource::reason_for(&e));
            return true;
        }
    };
    let squash = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let head = squash(&declaration_head(&core.content));
    head.is_empty() || !squash(&old).contains(&head)
}

/// `(base, head)` for blame: a two-sided range as given, a single revision
/// against HEAD (its commits are `rev..HEAD`), nothing for the working tree.
pub(super) fn commit_bounds(diff_range: Option<&str>) -> Option<(String, String)> {
    let range = diff_range?;
    if range == "HEAD" {
        return None;
    }
    match diffctx_snapshot::snapshot::split_diff_range(range) {
        (Some(base), Some(head)) => Some((base, head)),
        (None, None) => Some((range.to_string(), "HEAD".to_string())),
        _ => None,
    }
}

/// Distinct commits of the range that touched the fragment's lines. The
/// line log follows the span back through the range, so a commit whose
/// lines a later one overwrote still counts; blame at the head would not
/// see it.
pub(super) fn commits_touching(
    state: &ScoredState,
    base: &str,
    head: &str,
    frag: &Fragment,
) -> usize {
    let rel_path = rel(state, frag.path());
    let span = format!("{},{}:{}", frag.id.start_line, frag.id.end_line, rel_path);
    let range = format!("{base}..{head}");
    let Ok(out) = diffctx_snapshot::snapshot::run_git(
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
