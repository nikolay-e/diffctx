//! What the impact answer reads of the run while it traces each changed definition: the cells, the
//! tests' reach, the fragments by file and by id, and what links a fragment to the one it uses; and
//! what the phases have found so far.

use std::borrow::Cow;
use std::cell::{OnceCell, RefCell};
use std::path::Path;
use std::rc::Rc;
use std::time::Instant;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::answer::render::place;
use crate::decide::cells::Cells;
use crate::run_provenance::state::ScoredState;

use diffctx_base::types::{Fragment, FragmentId, FragmentKind};
use diffctx_facts::facts::{EntryKind, Import, framework};
use diffctx_graph::keytable::Counterpart;

use super::super::callers::{Files, Receiver, mentions, references_module};
use super::super::changes::{BaseTexts, commit_bounds, lines_of, rel, runtime_tag};
use super::super::dispatch::base_symbol;
use super::super::entry::{is_jvm_path, jvm_entry_point, registering_decorator, scala_imports};
use super::super::output::{Caller, ChangedSymbol, Contract};
use super::super::tests_reach::TestsReach;

/// The run as the impact answer reads it: nothing here changes while the definitions are traced.
pub(super) struct Impact<'a> {
    pub(super) state: &'a ScoredState,
    pub(super) query: Option<&'a str>,
    pub(super) diff_range: Option<&'a str>,
    pub(super) analysed: Option<&'a str>,
    pub(super) deadline: Option<Instant>,
    pub(super) cells: &'a Cells,
    pub(super) tests: TestsReach<'a>,
    pub(super) by_id: FxHashMap<&'a FragmentId, &'a Fragment>,
    pub(super) files: Files<'a>,
    pub(super) definitions: FxHashMap<(&'a str, String), usize>,
    pub(super) defined_in: FxHashSet<(String, &'a str)>,
    pub(super) bounds: Option<(String, String)>,
    pub(super) base_texts: BaseTexts<'a>,
    changed_paths: FxHashSet<&'a str>,
    hunk_lines: FxHashMap<&'a str, Vec<(u32, u32)>>,
    rel_of: FxHashMap<&'a str, String>,
    runtime_of: FxHashMap<&'a str, &'static str>,
    scala_import_memo: RefCell<FxHashMap<String, Rc<Vec<Import>>>>,
    metric_readers: OnceCell<Vec<Counterpart>>,
}

/// What the phases have found of the range, in the order they report it.
#[derive(Default)]
pub(super) struct Findings {
    pub(super) changed: Vec<ChangedSymbol>,
    // Schema changes first, then the public symbols something calls, then
    // the rest: what the cap keeps is what a reader would look at first.
    pub(super) contracts: Vec<Contract>,
    pub(super) public_api: Vec<(bool, Contract)>,
    pub(super) untested_definitions: usize,
    pub(super) untested_unlisted: Vec<String>,
    pub(super) untested_called_in_diff: Vec<String>,
    pub(super) entry_unlisted: Vec<String>,
    pub(super) layout_only: Vec<String>,
    pub(super) traced_definitions: usize,
    pub(super) traced_names: Vec<String>,
}

pub(super) fn fragments_by_id(state: &ScoredState) -> FxHashMap<&FragmentId, &Fragment> {
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
    by_id
}

impl<'a> Impact<'a> {
    pub(super) fn open(
        state: &'a ScoredState,
        cells: &'a Cells,
        tests: TestsReach<'a>,
        by_id: FxHashMap<&'a FragmentId, &'a Fragment>,
        diff_range: Option<&'a str>,
        query: Option<&'a str>,
        deadline: Option<Instant>,
    ) -> Self {
        let changed_paths: FxHashSet<&str> = state
            .changed_files
            .iter()
            .filter_map(|p| p.to_str())
            .collect();
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

        let analysed = state.analysed_range.as_deref().or(diff_range);
        let bounds = commit_bounds(analysed).filter(|_| state.commit_count >= 2);
        let base_texts = BaseTexts::new(&state.root_dir, state.run.source().base());
        // Definitions are counted per language: the builders link a call to the
        // definitions of its own language, so a Python wrapper named like a Rust
        // function is not what makes the Rust name ambiguous. An `impl` block
        // extends the type it names rather than competing with it: a struct with
        // a `Display` and an `Error` impl is one definition, not three.
        let mut definitions: FxHashMap<(&str, String), usize> = FxHashMap::default();
        let mut defined_in: FxHashSet<(String, &str)> = FxHashSet::default();
        let mut file_fragments: FxHashMap<&str, Vec<&Fragment>> = FxHashMap::default();
        let mut rel_of: FxHashMap<&str, String> = FxHashMap::default();
        let mut runtime_of: FxHashMap<&str, &'static str> = FxHashMap::default();
        let indexing = diffctx_base::profile::detail("impact.setup.files", "");
        for f in &state.all_fragments {
            file_fragments.entry(f.path()).or_default().push(f);
            rel_of
                .entry(f.path())
                .or_insert_with(|| rel(state, f.path()));
            runtime_of
                .entry(f.path())
                .or_insert_with(|| runtime_tag(f.path()));
            if let Some(name) = f
                .symbol_name
                .as_deref()
                .filter(|_| f.kind.is_definition_kind() && f.kind != FragmentKind::Impl)
            {
                let lang = diffctx_base::languages::get_language_for_file(f.path()).unwrap_or("");
                *definitions.entry((lang, name.to_lowercase())).or_default() += 1;
                defined_in.insert((name.to_string(), f.path()));
            }
        }
        drop(indexing);
        let files = Files {
            fragments: file_fragments,
            cells,
            state,
        };
        Self {
            state,
            query,
            diff_range,
            analysed,
            deadline,
            cells,
            tests,
            by_id,
            files,
            definitions,
            defined_in,
            bounds,
            base_texts,
            changed_paths,
            hunk_lines,
            rel_of,
            runtime_of,
            scala_import_memo: RefCell::default(),
            metric_readers: OnceCell::new(),
        }
    }

    fn rel_cow<'p>(&'p self, path: &'p str) -> Cow<'p, str> {
        match self.rel_of.get(path) {
            Some(shown) => Cow::Borrowed(shown.as_str()),
            None => Cow::Owned(rel(self.state, path)),
        }
    }

    fn same_runtime_paths(&self, a: &str, b: &str) -> bool {
        let ra = self
            .runtime_of
            .get(a)
            .copied()
            .unwrap_or_else(|| runtime_tag(a));
        let rb = self
            .runtime_of
            .get(b)
            .copied()
            .unwrap_or_else(|| runtime_tag(b));
        ra.is_empty() || rb.is_empty() || ra == rb
    }

    pub(super) fn in_a_hunk(&self, id: &FragmentId) -> bool {
        if self.state.hunk_texts.is_empty() {
            return self.changed_paths.contains(id.path.as_ref());
        }
        self.hunk_lines
            .get(self.rel_cow(id.path.as_ref()).as_ref())
            .is_some_and(|ranges| {
                ranges
                    .iter()
                    .any(|&(a, b)| id.start_line <= b && id.end_line >= a)
            })
    }

    pub(super) fn hunks_in(
        &self,
        core: &Fragment,
    ) -> Vec<&'a diffctx_base::changed_lines::HunkText> {
        let path = self.rel_cow(core.path());
        self.state
            .hunk_texts
            .iter()
            .filter(|h| {
                let (a, b) = h.new_range();
                h.path == path.as_ref() && core.start_line() <= b && core.end_line() >= a
            })
            .collect()
    }

    pub(super) fn core_place(&self, core: &Fragment) -> String {
        place(
            &self.rel_cow(core.path()),
            core.symbol_name.as_deref(),
            &lines_of(&core.id),
        )
    }

    pub(super) fn one_tree(&self, user: &str, used: &str) -> bool {
        self.state
            .run
            .source()
            .may_reference(&self.rel_cow(user), &self.rel_cow(used))
    }

    pub(super) fn one_project(&self, user: &Fragment, used: &Fragment) -> bool {
        self.one_tree(user.path(), used.path())
    }

    pub(super) fn file_of(&self, f: &Fragment) -> &[&'a Fragment] {
        self.files
            .fragments
            .get(f.path())
            .map_or(&[][..], Vec::as_slice)
    }

    pub(super) fn metric_readers(&self) -> &[Counterpart] {
        self.metric_readers.get_or_init(|| {
            diffctx_graph::keytable::metric_readers(
                &self.state.all_fragments,
                Some(self.state.root_dir.as_path()),
                self.state.run.changed_lines(),
            )
        })
    }

    pub(super) fn registration(&self, f: &Fragment) -> Option<String> {
        let (state, cells, scala_import_memo) = (self.state, self.cells, &self.scala_import_memo);
        let file_of = |f: &Fragment| self.file_of(f);
        let path = self.rel_cow(f.path());
        let path = path.as_ref();
        let stated_entry = f.symbol_name.as_deref().map(base_symbol).and_then(|name| {
            cells
                .of(path)?
                .framework_entries_at(path, name, (f.start_line(), f.end_line()))
                .into_iter()
                .find(|(kind, _)| *kind != EntryKind::Fixture)
        });
        if let Some((kind, entry)) = stated_entry {
            return Some(framework::describe(kind, &entry));
        }
        if is_jvm_path(f.path()) {
            // Kotlin's and Java's annotations are the facts' own.
            let stated = cells.of(path).and_then(|cell| {
                cell.annotations_at(
                    path,
                    f.symbol_name.as_deref().map(base_symbol)?,
                    (f.start_line(), f.end_line()),
                )
            });
            let imports = if Path::new(path)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("scala"))
            {
                scala_import_memo
                    .borrow_mut()
                    .entry(path.to_string())
                    .or_insert_with(|| {
                        Rc::new(
                            cells
                                .head_text(state, path)
                                .map_or_else(Vec::new, |text| scala_imports(&text)),
                        )
                    })
                    .clone()
            } else {
                Rc::new(cells.imports_at(path))
            };
            jvm_entry_point(f, imports.as_slice(), stated.as_deref()).or_else(|| {
                let name = f.symbol_name.as_deref().map(base_symbol)?;
                cells
                    .of(path)?
                    .serialized_at(path, name, (f.start_line(), f.end_line()))
                    .then(|| format!("{}serialized by Jackson", framework::ENTRY_PREFIX))
            })
        } else {
            registering_decorator(f, file_of(f))
        }
    }

    pub(super) fn is_tool(&self, f: &Fragment) -> bool {
        let cells = self.cells;
        let path = self.rel_cow(f.path());
        let path = path.as_ref();
        f.symbol_name
            .as_deref()
            .map(base_symbol)
            .is_some_and(|name| {
                cells.of(path).is_some_and(|cell| {
                    cell.framework_entries_at(path, name, (f.start_line(), f.end_line()))
                        .into_iter()
                        .any(|(kind, _)| kind == EntryKind::Tool)
                })
            })
    }

    pub(super) fn link(&self, user: &Fragment, used: &Fragment) -> bool {
        let (cells, files) = (self.cells, &self.files);
        let Some(name) = used.symbol_name.as_deref().map(base_symbol) else {
            return false;
        };
        if !self.same_runtime_paths(user.path(), used.path()) {
            return false;
        }
        let (user_rel, used_rel) = (self.rel_cow(user.path()), self.rel_cow(used.path()));
        if let Some(cell) = cells
            .of(user_rel.as_ref())
            .filter(|c| c.owns(used_rel.as_ref()))
        {
            return cell.links(
                user_rel.as_ref(),
                (user.start_line(), user.end_line()),
                used_rel.as_ref(),
                name,
                (used.start_line(), used.end_line()),
            );
        }
        self.one_project(user, used)
            && mentions(user, Some(name), &Receiver::Any)
            && references_module(user, files, Some(name), used.path())
    }

    pub(super) fn reaches_file(&self, test: &Fragment, caller: &Fragment) -> bool {
        let (cells, files) = (self.cells, &self.files);
        if !self.same_runtime_paths(test.path(), caller.path()) {
            return false;
        }
        let (test_rel, caller_rel) = (self.rel_cow(test.path()), self.rel_cow(caller.path()));
        if let Some(cell) = cells
            .of(test_rel.as_ref())
            .filter(|c| c.owns(caller_rel.as_ref()))
        {
            return cell.reaches(test_rel.as_ref(), caller_rel.as_ref());
        }
        self.one_project(test, caller) && references_module(test, files, None, caller.path())
    }

    /// What the tests say of a caller: the ones that reach it, and why an absent
    /// link is or is not a finding.
    /// A caller of a changed fragment (`core`) is open too where a test asserts
    /// that fragment's text.
    pub(super) fn test_links(&self, frag: &'a Fragment, core: Option<&Fragment>) -> Caller {
        let (state, tests, by_id) = (self.state, &self.tests, &self.by_id);
        let link = |user: &Fragment, used: &Fragment| self.link(user, used);
        let reaches_file = |test: &Fragment, caller: &Fragment| self.reaches_file(test, caller);
        let registration = |f: &Fragment| self.registration(f);
        let tested = tests.tested_by(frag, state, by_id, &link);
        let registered_by = tested.is_empty().then(|| registration(frag)).flatten();
        Caller {
            file_tested: tested.is_empty()
                && registered_by.is_none()
                && tests.file_tested(frag, &reaches_file),
            unread_test: tested
                .is_empty()
                .then(|| tests.unread_naming(frag, &self.rel_cow(frag.path())))
                .flatten(),
            tests_open: tested.is_empty()
                && core.map_or_else(
                    || tests.open_above(frag, &link, true),
                    |core| tests.caller_open(frag, core, &link),
                ),
            tests_partial: !tested.is_empty() && tests.nearest_only(frag, state, by_id, &link),
            registered_by,
            tests: tested,
            ..Caller::default()
        }
    }
}
