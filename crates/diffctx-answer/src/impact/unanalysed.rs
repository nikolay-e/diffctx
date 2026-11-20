//! The files a change touched that no reader traced, and the names nothing accounted for.

use std::path::Path;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::decide::cells::Cells;
use crate::decide::losses::Effect;
use crate::decide::{self};

use crate::run_provenance::state::ScoredState;

use diffctx_base::types::Fragment;

use diffctx_occurrence::tokens::occurrence::{Hedge, Reason};

use super::mentions::{class_of, has_data_extension};

use super::callers::{extension, is_code_file, is_document};
use super::changes::{
    changed_rels, is_schema_file, layout_files, rel, same_runtime, spelled_lines,
};
use super::dispatch::base_symbol;
use super::output::{ChangedSymbol, PathReference, UnanalysedFile, Unknown};
use super::removed::MAX_NAMED_BY_PER_TARGET;

/// The changed files this run did not read whole (#454), and the changed
/// files outside code whose content something reads — config keys, a
/// template, a query, a stylesheet — which nothing here traces (#559),
/// ordered by language then path so the listed ones and the counted rest are
/// stable.
/// The reason of a changed file's lines that no tracked definition holds.
pub(crate) const OUTSIDE_DEFINITIONS: &str = "outside_definitions";

/// Runs of changed lines listed per file before the rest are dropped.
pub(super) const MAX_OUTSIDE_RUNS: usize = 8;

/// The changed code the cells' definitions do not hold, one entry per file with
/// its runs of lines.
pub(super) fn outside_definitions(cells: &Cells) -> Vec<UnanalysedFile> {
    cells
        .iter()
        .flat_map(decide::cell::Cell::outside_definitions)
        .map(|(path, spans)| UnanalysedFile {
            language: diffctx_base::languages::get_language_for_file(&path),
            path,
            reason: OUTSIDE_DEFINITIONS,
            lines: spans
                .into_iter()
                .take(MAX_OUTSIDE_RUNS)
                .flat_map(|(first, last)| [first, last])
                .collect(),
        })
        .collect()
}

/// Changed code that no changed definition holds, in a file the run read no
/// definition of it from: an unnamed definition (`def`, `defn`, a dotted Lua
/// function), a body its signature does not span, a class member the facts do not
/// model, a migration's `ALTER`. What it does to its readers is not traced, so the
/// answer says so, and the names its lines and its enclosing header spell are
/// searched as text.
pub(super) struct Nameless {
    pub(super) path: String,
    pub(super) runs: Vec<(u32, u32)>,
    pub(super) terms: Vec<String>,
}

/// The line above `line` that opens the block it sits in: the nearest one with a
/// smaller indentation, where a definition states its name.
fn header_above(text: &str, line: u32) -> Option<&str> {
    header_row(&text.lines().collect::<Vec<_>>(), line).map(|(_, header)| header)
}

/// The line above `line` that opens its block, and its 1-based number.
fn header_row<'a>(rows: &[&'a str], line: u32) -> Option<(u32, &'a str)> {
    let at = line.checked_sub(1)? as usize;
    let indent = |l: &str| l.len() - l.trim_start().len();
    let own = indent(rows.get(at)?);
    rows[..at]
        .iter()
        .enumerate()
        .rev()
        .find(|(_, l)| !l.trim().is_empty() && indent(l) < own)
        .map(|(n, l)| (n as u32 + 1, *l))
}

/// The names a changed config line sits under: the value of each enclosing `key: value`
/// header (`- alert: DemoDown`), outermost last. A reader spells the definition by
/// that name, whichever line of it the change touched.
pub(super) fn enclosing_names(text: &str, line: u32) -> Vec<String> {
    let rows: Vec<&str> = text.lines().collect();
    let mut names = Vec::new();
    let mut at = line;
    while let Some((n, header)) = header_row(&rows, at) {
        if let Some((_, value)) = header.split_once(": ") {
            names.extend(super::keys::terms(value));
        }
        at = n;
    }
    names
}

/// The words a line spells that a reader may spell back. A bare keyword in capitals
/// (`RENAME`, `ALTER`) names nothing.
fn name_terms(line: &str) -> impl Iterator<Item = String> + '_ {
    super::keys::key_words(line).filter(|w| {
        w.chars()
            .any(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit())
    })
}

pub(super) fn nameless_changes(
    state: &ScoredState,
    cells: &Cells,
    held: &FxHashMap<String, Vec<(u32, u32)>>,
) -> Vec<Nameless> {
    let outside: FxHashMap<String, Vec<(u32, u32)>> = cells
        .iter()
        .flat_map(decide::cell::Cell::outside_definitions)
        .collect();
    let source = state.run.source();
    let changed: FxHashSet<String> = changed_rels(state).collect();
    let mut out: Vec<Nameless> = Vec::new();
    for h in state.hunk_texts.iter().filter(|h| !h.layout) {
        let path = h.path.as_str();
        // A file the range adds has no reader to lose, but a migration alters
        // what other files already name.
        let sql = extension(path) == "sql";
        let added_file = h.old_start == 0 && h.old_len == 0 && !sql;
        let code: Vec<&str> = h
            .added
            .iter()
            .chain(&h.removed)
            .map(String::as_str)
            .filter(|l| l.chars().any(char::is_alphanumeric))
            .collect();
        if added_file
            || code.is_empty()
            || !changed.contains(path)
            || !(is_code_file(path) || sql)
            || diffctx_base::testfiles::is_test_path(Path::new(path))
        {
            continue;
        }
        let (first, last) = h.new_range();
        let touches = |&(lo, hi): &(u32, u32)| lo <= last && hi >= first;
        let accounted = match cells.of(path) {
            // What a cell reads is its own: a removal is its removal, and
            // lines outside its definitions are its `outside_definitions`.
            Some(cell) => {
                h.added.is_empty()
                    || cell
                        .changed()
                        .iter()
                        .any(|d| d.file == path && touches(&d.lines))
                    || outside
                        .get(path)
                        .is_some_and(|spans| spans.iter().any(touches))
            }
            None => held
                .get(path)
                .is_some_and(|spans| spans.iter().any(touches)),
        };
        if accounted {
            continue;
        }
        let text = source.read_to_string(&state.root_dir.join(path));
        let header = text.as_deref().and_then(|t| header_above(t, first));
        let terms: Vec<String> = code
            .into_iter()
            .chain(header)
            .flat_map(name_terms)
            .collect();
        match out.iter_mut().find(|n| n.path == path) {
            Some(n) => {
                n.runs.push((first, last));
                n.terms.extend(terms);
            }
            None => out.push(Nameless {
                path: path.to_string(),
                runs: vec![(first, last)],
                terms,
            }),
        }
    }
    for n in &mut out {
        n.terms.sort();
        n.terms.dedup();
    }
    out
}

pub(super) fn unanalysed_files(state: &ScoredState, changed: &[String]) -> Vec<UnanalysedFile> {
    let mut fragmented: FxHashMap<String, bool> = FxHashMap::default();
    for f in state.run.bounded(&state.all_fragments) {
        *fragmented.entry(rel(state, f.path())).or_default() |= f.kind.is_definition_kind();
    }
    let source = state.run.source();
    let too_large = state
        .run
        .limited_by(diffctx_snapshot::resource::LimitReason::FileTooLarge);
    let layout = layout_files(state);
    let mut out: Vec<UnanalysedFile> = changed
        .iter()
        .filter(|path| too_large.contains(*path) || state.run.check())
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
                        .is_some_and(|t| {
                            diffctx_base::languages::sniff_language(path, &t) == Some("bash")
                        })
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
                return Some(UnanalysedFile::new(
                    path.clone(),
                    "content_untraced",
                    Vec::new(),
                ));
            }
            // No fragment at all. A deleted or withheld file never reaches
            // `changed`; a code file over the size cap or one that could not
            // be read was never analysed, and saying nothing made its callers
            // a clean zero. An empty file has nothing to analyse.
            // Over the cap on either side (a head read in full had its removals skipped
            // when the base was too large to parse); named past the deadline too (gh#619).
            if too_large.contains(path) {
                return Some(UnanalysedFile::new(path.clone(), "too_large", Vec::new()));
            }
            let Some(&has_definition) = fragmented.get(path) else {
                let reason = if source.read_bytes(&state.root_dir.join(path)).is_none() {
                    "unreadable"
                } else {
                    return None;
                };
                return Some(UnanalysedFile::new(path.clone(), reason, Vec::new()));
            };
            let (reason, lines) =
                diffctx_facts::parsers::unanalysed_reason(path, has_definition, || {
                    source.read_bytes(&state.root_dir.join(path))
                })?;
            Some(UnanalysedFile::new(path.clone(), reason, lines))
        })
        .collect();
    // A deleted file over the size cap had its removals skipped: as unanalysed as a
    // changed one, and never in `changed`.
    out.extend(
        state
            .deleted_files
            .iter()
            .filter(|p| too_large.contains(*p) && !changed.contains(p))
            .map(|p| UnanalysedFile::new(p.clone(), "too_large", Vec::new())),
    );
    out.sort_by(|a, b| {
        a.language
            .cmp(&b.language)
            .then_with(|| a.path.cmp(&b.path))
    });
    out
}

/// Where a name is spelled outside the code impact traces: config, data,
/// templates, markup, and, for a module's name, another runtime's scripts
/// (`python -m controls.foo_check` in a CronWorkflow). A changed definition
/// no traced caller reaches, but such a file names, is no "nothing calls
/// it": a zero needs a witness that the name is read nowhere it went
/// unsearched (#505).
#[derive(Default)]
pub(super) struct UntracedNamers {
    /// Each needle, by the lines naming it as a whole word.
    rows: FxHashMap<String, Vec<(String, u32)>>,
    /// The names spelled as a literal, a value or a query string.
    literal: super::keys::LiteralNames,
    /// The files in scope that could spell a name no cell traces.
    pub(super) unread: UnreadFiles,
}

/// The files of the searched tree that could spell a definition's name without a
/// cell having read them: a script, a template, a config, code of another language.
#[derive(Default)]
pub(super) struct UnreadFiles {
    /// The groups of the scripts, templates and configs that can run a name: no document,
    /// data dump or manifest.
    text: FxHashSet<Option<diffctx_resolve::vocabulary::family::Group>>,
    /// The groups of the code no cell reads.
    code: FxHashSet<Option<diffctx_resolve::vocabulary::family::Group>>,
    /// The files of the groups whose cell leaves a region of a file unread (a C macro
    /// body, a shell command word), by group.
    hiding: FxHashMap<diffctx_resolve::vocabulary::family::Group, Vec<String>>,
}

impl UnreadFiles {
    fn of(state: &ScoredState) -> Self {
        let Ok(paths) = state.run.source().list_paths(&state.root_dir) else {
            return Self {
                text: FxHashSet::from_iter([None]),
                ..Self::default()
            };
        };
        let mut unread = Self::default();
        for path in paths.iter().filter(|p| {
            diffctx_snapshot::source::in_scope(
                &state.root_dir,
                &diffctx_base::paths::repo_join(&state.root_dir, p),
                &state.scope,
            )
        }) {
            if !is_code_file(path) {
                if reads_a_name(path) && !holds_no_command(path) {
                    unread
                        .text
                        .insert(diffctx_resolve::vocabulary::family::Group::of_name(path));
                }
            } else if unread_code(path) {
                unread
                    .code
                    .insert(diffctx_resolve::vocabulary::family::Group::of_name(path));
            }
            if let Some(group) =
                diffctx_resolve::vocabulary::family::Group::of_name(path).filter(hides_a_region)
            {
                unread.hiding.entry(group).or_default().push(path.clone());
            }
        }
        unread
    }

    /// Whether a file this definition's cell did not read could spell its name: one of
    /// its own group is read by its cell, so only a file of another group counts.
    pub(super) fn could_spell(
        &self,
        own: Option<diffctx_resolve::vocabulary::family::Group>,
    ) -> bool {
        self.text.iter().any(|group| own.is_none() || *group != own)
            || self.code.iter().any(|group| *group != own)
    }

    /// Whether a sibling of `path` in a group whose cell leaves a region of a file unread
    /// could spell a removed name there: the account of a removed name looks only where
    /// the base resolved it, while a live definition's account reads its whole group.
    fn hides_beside(
        &self,
        own: Option<diffctx_resolve::vocabulary::family::Group>,
        path: &str,
    ) -> bool {
        own.and_then(|group| self.hiding.get(&group))
            .is_some_and(|files| files.iter().any(|f| f != path))
    }
}

/// A group whose cell reads a file's definitions and calls but not all of its text.
fn hides_a_region(group: &diffctx_resolve::vocabulary::family::Group) -> bool {
    use diffctx_resolve::vocabulary::family::{Family, Group};
    matches!(
        group,
        Group::Native | Group::Family(Family::C | Family::Shell)
    )
}

/// A manifest or a file of variables: a config reader parses its keys and pointers,
/// and no command or template of it calls a function by its name. A Maven POM
/// points at a class to run, never at a method.
fn holds_no_command(path: &str) -> bool {
    matches!(extension(path).as_str(), "json" | "toml" | "env")
        || matches!(
            Path::new(path).file_name().and_then(|n| n.to_str()),
            Some("go.mod" | "go.work" | "go.sum" | "pom.xml")
        )
}

/// Whether a file outside code can name a definition: a document mentions a name
/// without reading it, and a dump of data or a lockfile holds no call.
fn reads_a_name(path: &str) -> bool {
    !is_document(path)
        && !matches!(
            extension(path).as_str(),
            "diff" | "patch" | "lock" | "tex" | "bib" | "txt" | "csv" | "tsv" | "log" | "svg"
        )
}

/// The files that name a definition and are listed; the rest are counted. By a
/// module's stem or a Terraform variable assigned in a `.hcl`, a name spelled in
/// more files than this is a common word, not a reference: it witnesses nothing.
pub(super) const MAX_UNTRACED_NAMERS: usize = 12;

/// Module names that say a file's role, not which file it is.
const ROLE_STEMS: &str = "index main mod lib init __init__ __main__ app util utils helpers common core base types config settings test tests conftest setup";

/// A name only code would spell: `foo_check`, `formatDate`, `LIMIT`, `v2`.
/// A plain word (`naming`, `total`) is prose in a dataset or a paper as
/// often as a reference, and witnesses nothing.
pub fn distinctive(word: &str) -> bool {
    let has = |f: fn(&char) -> bool| word.chars().any(|c| f(&c));
    let shaped = word.contains('_')
        || has(char::is_ascii_digit)
        || (has(char::is_ascii_uppercase) && has(char::is_ascii_lowercase))
        || !has(char::is_ascii_lowercase);
    shaped
        && word.chars().count() >= super::keys::MIN_LITERAL_NAME
        && !word.chars().all(|c| c.is_ascii_digit() || c == '_')
        && uncommon(word)
}

fn uncommon(word: &str) -> bool {
    !diffctx_config::stopwords::CODE_STOPWORDS.contains(&word.to_lowercase())
        && !ROLE_STEMS.split(' ').any(|r| r == word)
}

/// A name whose spelling in text is searched: one only code would spell, or a plain
/// lowercase word of at least the literal floor. A script, a Makefile or a template
/// calls a definition by its bare name, plain or not; a name shorter than the floor or
/// as common as `main` is not searched, and an answer that rests on its absence says so.
pub(super) fn searched_as_text(word: &str) -> bool {
    distinctive(word)
        || (word.len() >= super::keys::MIN_LITERAL_NAME
            && word.bytes().all(|b| b.is_ascii_lowercase())
            && uncommon(word))
}

/// Whether text outside the language can call `core` by its name: a stage of a
/// Dockerfile is no function a script runs.
pub(super) fn callable_by_name(path: &str) -> bool {
    diffctx_base::languages::get_language_for_file(path) != Some("dockerfile")
}

/// Code no cell reads (Objective-C, Scala and Groovy too, in the groups of the
/// languages they call), or Swift, which no cell binds to C: a foreign-function
/// binding there (`extern fn`, `ccall`, `ffi.cdef`, a bridging call, a
/// JavaScriptCore string) names a definition of another language by its bare name.
/// A shell script runs a program of any language by its bare name, and its cell
/// binds none.
fn unread_code(path: &str) -> bool {
    use diffctx_resolve::vocabulary::family::{Family, Group};
    is_code_file(path)
        && match Group::of_name(path) {
            None | Some(Group::Language(_) | Group::Family(Family::Swift | Family::Shell)) => true,
            Some(Group::Native | Group::Jvm) => {
                !Family::ALL.into_iter().any(|family| family.has_path(path))
            }
            Some(Group::Family(_) | Group::Web) => false,
        }
}

/// Code of a language no cell of `own`'s language binds to it: a shell script
/// runs a program of any language by its bare name.
fn foreign_code(path: &str, own: Option<diffctx_resolve::vocabulary::family::Group>) -> bool {
    unread_code(path) && diffctx_resolve::vocabulary::family::Group::of_name(path) != own
}

/// A file that may spell a definition of `own`'s group where no cell accounts for
/// the spelling: text the definition's own cell does not read, or foreign code.
fn unaccounted_reader(path: &str, own: Option<diffctx_resolve::vocabulary::family::Group>) -> bool {
    let read_by_own =
        own.is_some() && diffctx_resolve::vocabulary::family::Group::of_name(path) == own;
    (!is_code_file(path) && !read_by_own) || foreign_code(path, own)
}

/// The name a reader spells: `var.region` is written `region`, a C++ member
/// `Engine::open` is written `open`.
pub(super) fn literal_base(symbol: &str) -> &str {
    let name = base_symbol(symbol);
    name.rsplit(['.', ':']).next().unwrap_or(name)
}

/// The words a reader may still spell for the names `def` was renamed from.
pub(super) fn left_words(def: &decide::changed::Changed) -> impl Iterator<Item = &str> {
    def.left_names()
        .filter(|_| !def.named_by_its_path())
        .map(literal_base)
}

/// The word a reader spells for `def`: none for a script named by its path, whose
/// last dotted part is its extension (`sh`), the word that runs every script.
pub(super) fn spelled_word(def: &decide::changed::Changed) -> Option<&str> {
    (!def.named_by_its_path()).then(|| literal_base(&def.name))
}

/// The word a reader spells for `core`.
pub(super) fn spelled_name(core: &Fragment) -> Option<&str> {
    core.symbol_name
        .as_deref()
        .map(literal_base)
        .filter(|name| !name.is_empty())
}

/// Whether `text` spells `word` other than as a member (`obj.word`): a shell function runs
/// as a command, never through an object.
fn spelled_bare(text: &str, word: &str) -> bool {
    text.match_indices(word).any(|(at, _)| {
        !text[..at]
            .chars()
            .next_back()
            .is_some_and(|c| super::keys::word_char(c) || c == '.')
            && !text[at + word.len()..]
                .chars()
                .next()
                .is_some_and(super::keys::word_char)
    })
}

/// Whether `text` names `word` the way Go assembly names a Go function: `·word(SB)`. A plain
/// word (`Helper`) is a name there and prose anywhere else.
fn assembly_symbol(text: &str, word: &str) -> bool {
    text.match_indices(word)
        .any(|(at, _)| text[..at].ends_with('·') && text[at + word.len()..].starts_with(['(', '<']))
}

fn module_stem(path: &str) -> Option<&str> {
    Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| distinctive(s))
}

impl UntracedNamers {
    pub(super) fn build(
        state: &ScoredState,
        cells: &Cells,
        cores: &[&Fragment],
        removed: &[&str],
        left: &[&str],
        own: &[super::keys::OwnSpan],
        diff_range: Option<&str>,
    ) -> Self {
        let literal_names: Vec<&str> = cores
            .iter()
            .filter_map(|c| c.symbol_name.as_deref())
            .chain(removed.iter().copied())
            .map(literal_base)
            .chain(left.iter().copied())
            .collect();
        let literal =
            super::keys::LiteralNames::build(state, cells, diff_range, &literal_names, own);
        let unread = UnreadFiles::of(state);
        let mut needles: Vec<String> = cores
            .iter()
            .filter_map(|c| spelled_name(c))
            .chain(removed.iter().copied().map(literal_base))
            .chain(left.iter().copied())
            .chain(
                own.iter()
                    .filter(|o| extension(&o.0) == "tf")
                    .map(|o| o.1.as_str()),
            )
            .filter(|w| searched_as_text(w))
            .chain(cores.iter().filter_map(|c| module_stem(c.path())))
            .map(str::to_string)
            .chain(
                cores
                    .iter()
                    .filter(|c| extension(c.path()) == "go")
                    .filter_map(|c| spelled_name(c))
                    .map(str::to_string),
            )
            .collect();
        needles.sort();
        needles.dedup();
        if needles.is_empty() {
            return Self {
                literal,
                unread,
                ..Self::default()
            };
        }
        let wanted: FxHashSet<&str> = needles.iter().map(String::as_str).collect();
        // The names only shell scripts define: `obj.slugify(x)` runs none of them.
        let commands: FxHashSet<&str> = (own.iter())
            .filter(|(_, name, _)| {
                (own.iter().filter(|(_, other, _)| other == name))
                    .all(|(file, ..)| super::keys::runs_as_shell(state, file))
            })
            .map(|(_, name, _)| name.as_str())
            .collect();
        let changed: FxHashSet<String> = changed_rels(state).collect();
        let grepped = spelled_lines(state, diff_range, &needles);
        let mut rows: FxHashMap<String, Vec<(String, u32)>> = FxHashMap::default();
        for (path, line, text) in &grepped.rows {
            // A comment or a document mentions a name without reading it. A file the
            // change edits is the diff's own, unless it is code no cell reads.
            if (changed.contains(path) && !unread_code(path))
                || !reads_a_name(path)
                || super::mentions::commented_in(path, text)
            {
                continue;
            }
            let mut seen: FxHashSet<&str> = FxHashSet::default();
            for word in text.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
                // A plain word in a JSON file is a label or a value, no reader of a name;
                // a plain Go name is a name only in assembly (`·Helper(SB)`).
                if wanted.contains(word)
                    && (extension(path) != "json" || distinctive(word))
                    && (searched_as_text(word) || assembly_symbol(text, word))
                    && (!commands.contains(word) || spelled_bare(text, word))
                    && seen.insert(word)
                {
                    rows.entry(word.to_string())
                        .or_default()
                        .push((path.clone(), *line));
                }
            }
        }
        Self {
            rows,
            literal,
            unread,
        }
    }

    /// `sym` with the files that spell the name of `def`, or one it was renamed
    /// from, as a literal named in its answer.
    pub(super) fn name_literals(
        &self,
        sym: &mut ChangedSymbol,
        def: &decide::changed::Changed,
        verdict: &decide::verdict::Verdict,
    ) {
        let spelled = self.literal_spellings(sym, def, verdict);
        Self::disclose(sym, spelled);
    }

    /// `name_literals` for a removed definition, which no head fragment carries: the
    /// files outside what its language reads that spell it as a word (a command, a
    /// template call) are named as well, and a name never searched as text says so.
    pub(super) fn name_removed(
        &self,
        sym: &mut ChangedSymbol,
        def: &decide::changed::Changed,
        verdict: &decide::verdict::Verdict,
    ) {
        let mut spelled = self.literal_spellings(sym, def, verdict);
        let own = diffctx_resolve::vocabulary::family::Group::of_name(&sym.path);
        let Some(word) = spelled_word(def) else {
            Self::disclose(sym, spelled);
            return;
        };
        if searched_as_text(word) {
            let named = |p: &str| spelled.iter().chain(&sym.missed_in).any(|f| f.path == p);
            let fresh: Vec<UnanalysedFile> = self
                .named_in(word, |p| unaccounted_reader(p, own))
                .into_iter()
                .filter(|(p, _)| !named(p))
                .map(|(path, line)| UnanalysedFile::new(path, "not_traced", vec![line]))
                .collect();
            spelled.extend(fresh);
        } else if self.unread.could_spell(own) || self.unread.hides_beside(own, &sym.path) {
            sym.coverage = (sym.coverage.clone()).with(Some(decide::gap::Gap::NameUnsearched));
        }
        Self::disclose(sym, spelled);
    }

    /// The files that spell the name of `def`, or one it was renamed from, as a
    /// literal or an assigned value, and are no caller `sym` already names.
    fn literal_spellings(
        &self,
        sym: &ChangedSymbol,
        def: &decide::changed::Changed,
        verdict: &decide::verdict::Verdict,
    ) -> Vec<UnanalysedFile> {
        // Terragrunt's `inputs` and a `.tftest.hcl`'s `variables` set a Terraform
        // module's variable by its name, in files the module's extractor never reads.
        let sets_variable =
            diffctx_base::languages::get_language_for_file(&sym.path) == Some("terraform");
        let mut spelled: Vec<UnanalysedFile> = Vec::new();
        let member = matches!(
            def.kind,
            diffctx_facts::facts::DefKind::Method | diffctx_facts::facts::DefKind::Constructor
        );
        for name in spelled_word(def).into_iter().chain(left_words(def)) {
            let assigned = self
                .naming(literal_base(name), |p| {
                    sets_variable && extension(p) == "hcl"
                })
                .into_iter()
                .map(|(path, line)| UnanalysedFile::new(path, "not_traced", vec![line]));
            for file in self.literal_files(name, member).into_iter().chain(assigned) {
                let named = file
                    .lines
                    .iter()
                    .any(|line| verdict.names_another(&file.path, *line))
                    || sym.callers.iter().any(|c| c.path == file.path)
                    || matches!(&sym.unresolved, Some(Unknown::Candidates { sites, .. })
                        if sites.iter().any(|s| s.path == file.path))
                    || spelled.iter().any(|f| f.path == file.path);
                if !named {
                    spelled.push(file);
                }
            }
        }
        spelled
    }

    fn disclose(sym: &mut ChangedSymbol, spelled: Vec<UnanalysedFile>) {
        if !spelled.is_empty() {
            sym.coverage = sym
                .coverage
                .clone()
                .with(Some(decide::gap::Gap::Unaccounted {
                    reason: decide::gap::Reason::OtherLanguage,
                    files: spelled.len(),
                }));
            sym.shed.name += spelled.len().saturating_sub(MAX_UNTRACED_NAMERS);
            sym.missed_in
                .extend(spelled.into_iter().take(MAX_UNTRACED_NAMERS));
        }
    }

    /// `sym`, a definition nothing reaches, with the files outside its language that
    /// spell its name, or the gap that says the name was not searched there: a file no
    /// cell read could spell it, or a string of a parsed file does (`quoted`).
    pub(super) fn name_text(&self, sym: &mut ChangedSymbol, name: &str, quoted: bool) {
        // A file as a whole has no name to spell: it is reached by its path.
        let nothing_known = sym.symbol.is_some()
            && sym.callers.is_empty()
            && sym.also_changed.is_empty()
            && sym.possibly_also_changed.is_empty()
            && sym.unresolved.is_none()
            && sym.missed_in.is_empty()
            && callable_by_name(&sym.path);
        if !nothing_known {
            return;
        }
        let own = diffctx_resolve::vocabulary::family::Group::of_name(&sym.path);
        let named: Vec<UnanalysedFile> = self
            .naming(literal_base(name), |p| unaccounted_reader(p, own))
            .into_iter()
            .map(|(path, line)| UnanalysedFile::new(path, "not_traced", vec![line]))
            .collect();
        if !named.is_empty() {
            sym.coverage = sym
                .coverage
                .clone()
                .with(Some(decide::gap::Gap::Unaccounted {
                    reason: decide::gap::Reason::OtherLanguage,
                    files: named.len(),
                }));
            sym.missed_in.extend(named);
        } else if !searched_as_text(literal_base(name)) && (quoted || self.unread.could_spell(own))
        {
            sym.coverage = sym
                .coverage
                .clone()
                .with(Some(decide::gap::Gap::NameUnsearched));
        }
    }

    /// The files that spell `name`, the name of a `member` or not, as a literal.
    pub(super) fn literal_files(&self, name: &str, member: bool) -> Vec<UnanalysedFile> {
        self.literal
            .naming(literal_base(name), member)
            .into_iter()
            .map(|(path, line)| UnanalysedFile::new(path, "not_traced", vec![line]))
            .collect()
    }

    /// The lines naming `word` in the files `admit` keeps, one per file.
    fn named_in(&self, word: &str, admit: impl Fn(&str) -> bool) -> Vec<(String, u32)> {
        let mut by_path: Vec<(String, u32)> = Vec::new();
        for (path, line) in self.rows.get(word).into_iter().flatten() {
            if admit(path) && !by_path.iter().any(|(p, _)| p == path) {
                by_path.push((path.clone(), *line));
            }
        }
        by_path.sort();
        by_path
    }

    /// `named_in`, none when the word is too common to witness anything.
    fn naming(&self, word: &str, admit: impl Fn(&str) -> bool) -> Vec<(String, u32)> {
        let named = self.named_in(word, admit);
        if named.len() > MAX_UNTRACED_NAMERS {
            return Vec::new();
        }
        named
    }

    /// The files outside code that spell `core`'s name, or a name it was renamed
    /// from, every one: the answer lists the first `MAX_UNTRACED_NAMERS` and counts
    /// the rest.
    pub(super) fn of<'a>(
        &self,
        state: &ScoredState,
        core: &'a Fragment,
        left: impl Iterator<Item = &'a str>,
        linked: bool,
    ) -> Vec<UnanalysedFile> {
        let own = diffctx_resolve::vocabulary::family::Group::of_name(&rel(state, core.path()));
        let mut files: Vec<UnanalysedFile> = Vec::new();
        for name in spelled_name(core).into_iter().chain(left) {
            let named = self.named_in(name, |p| {
                !is_code_file(p)
                    || ((linked || unread_code(p))
                        && diffctx_resolve::vocabulary::family::Group::of_name(p) != own)
            });
            for (path, line) in named {
                if !files.iter().any(|f| f.path == path) {
                    files.push(UnanalysedFile::new(path, "not_traced", vec![line]));
                }
            }
        }
        files
    }

    /// The lines naming a changed module by its name outside the code that
    /// imports it: a config value, a `python -m pkg.mod` in another
    /// runtime's script (#613). Named by path, a path's own spelling, is
    /// already in `known`.
    pub(super) fn module_refs(
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
                    let class = class_of(&state.root_dir, &state.run.source(), &path, "", None);
                    out.push(PathReference {
                        path,
                        line,
                        target: target.clone(),
                        key: None,
                        class,
                    });
                }
            }
        }
        out
    }
}

/// The most files of a definition's group the account could not place that are
/// listed in `missed_in`: the gap carries the count, the rows are a sample.
const MAX_UNACCOUNTED_ROWS: usize = 20;

/// The files the account could not read, as the answer's rows: by path, the first
/// few. The definition's own file is no row: the gap says its loss, and its name
/// there is the definition.
pub(super) fn unaccounted_rows(
    verdict: &decide::verdict::Verdict,
    own: &str,
) -> Vec<UnanalysedFile> {
    let mut rows: Vec<UnanalysedFile> = (verdict.hedges().iter())
        .filter_map(|(_, hedge)| {
            let (file, reason, lines) = match hedge {
                Hedge::Candidate {
                    file,
                    line,
                    reason: Reason::Embedded,
                    ..
                } => (file, "embedded_code", vec![*line]),
                Hedge::Unread { file, loss } => match decide::losses::effect_of(loss) {
                    // A file a use stands in is listed as that use, and its loss stays
                    // in the coverage. A file read in part is named still: the lines
                    // the parse lost may hold a caller beside the one it recovered.
                    Effect::File(reason) | Effect::Subtree(reason, _)
                        if file.as_str() != own
                            && (matches!(reason, decide::gap::Reason::Unparsed)
                                || !verdict.uses.iter().any(|u| u.file == file.as_str())) =>
                    {
                        (file, reason.row_reason(), Vec::new())
                    }
                    _ => return None,
                },
                _ => return None,
            };
            Some(UnanalysedFile::new(
                file.as_str().to_string(),
                reason,
                lines,
            ))
        })
        .collect();
    rows.sort_by(|a, b| a.path.cmp(&b.path));
    rows.dedup_by(|a, b| a.path == b.path);
    rows.truncate(MAX_UNACCOUNTED_ROWS);
    rows
}
