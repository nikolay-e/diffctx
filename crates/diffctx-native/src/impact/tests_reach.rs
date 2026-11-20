//! Which tests reach a definition.
//!
//! A test reaches code in two ways. It uses it: the test, or a function it calls
//! (two hops up the uses, [`MAX_TEST_HOPS`]), holds a resolved use of the
//! definition, which a cell of the definition's language answers
//! ([`Cell::users`](crate::decide::Cell::users)); and it runs it: the built binary
//! of a crate, the console script a manifest declares, a script spelled by its
//! path on a command line, the key of a framework's entry. Nothing calls those, so
//! each is a [`Run`] the test makes of one entry (`runs`); so is a string a changed
//! definition holds and a test asserts (the literal channel). A pytest fixture is run by the tests and fixtures that ask for
//! it by name, and a scheduled job by the scheduler: the first is read off the
//! fixture's scope ([`TestsReach::fixture_users`]), the second off a test that names the
//! job to the scheduler.
//!
//! For the files no cell reads, the files of the group that spell a name are the
//! users, held to the same bar as a caller, and a test file's name links it to the
//! source it asks for (`testfiles::naming_links`). Nothing here reads the ranking
//! graph. A claim that a definition has no test link is printed only where `any`
//! says tests are in view and the search above it is closed
//! ([`TestsReach::open_above`]): every definition above it is accounted for.

use std::cell::{OnceCell, RefCell};
use std::path::Path;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::facts::EntryKind;

use super::runs::{self, Run, spelled_as_command};
use super::rust;
use super::{
    Cells, Fragment, FragmentId, FragmentKind, Link, MAX_TEST_HOPS, ScoredState, TestLink,
    base_symbol, cell_caller, lifted_caller, read_or_note, rel, spelling_files,
};
use crate::tokens::Names;
use crate::vocabulary::family::Group;
use diffctx_base::testfiles::{DeclaredTests, naming_links};

/// What a use of a definition leads to one hop up: the fragments that hold it,
/// and a test file whose own code does with no fragment to name it.
#[derive(Default)]
struct Up<'a> {
    fragments: Vec<&'a Fragment>,
    test: Option<(String, Option<String>)>,
}

pub(super) struct TestsReach<'a> {
    state: &'a ScoredState,
    cells: &'a Cells,
    /// The fragments of each file, by the path the answer prints.
    by_rel: FxHashMap<String, Vec<&'a Fragment>>,
    /// The path the answer prints for each path a fragment spells.
    rel_of: FxHashMap<&'a str, String>,
    /// The source files no cell reads whose name a test file's asks for.
    named: FxHashSet<String>,
    test_fragments: Vec<&'a Fragment>,
    /// Fragments outside test files that sit in a test scope the framework
    /// itself makes: a Rust `#[test]` in an inline `mod tests` (#528), Vitest's
    /// in-source block, a `unittest.TestCase`, a `TEST_CASE` (the scope that holds
    /// an occurrence decides, never the file's path).
    inline_tests: FxHashSet<&'a FragmentId>,
    /// The test scopes themselves, by the path the answer prints: a use inside one
    /// belongs to a test although the fragment that holds it is no test's own (a
    /// `proptest! { .. }` block sits in the fragment of its module).
    test_spans: FxHashMap<String, Vec<(u32, u32)>>,
    /// Test files the size cap kept out.
    unread: Vec<String>,
    /// Entries a test runs: binaries (`CARGO_BIN_EXE_<name>`, `cargo_bin("<name>")`:
    /// #391, #678), console scripts, scripts spelled by their path.
    runs: Vec<Run>,
    /// The `>>>` and fenced examples that call a function of their own file: a test
    /// of it that does not end the search for the tests that call it.
    doctests: Vec<Run>,
    /// The strings of a changed definition that a test asserts (the literal channel).
    asserted: Vec<Run>,
    /// The fragments that hold such a string or use one that does: what a test
    /// of the text can reach.
    text_reach: OnceCell<FxHashSet<FragmentId>>,
    /// The repository holds a test file the walk never reached: tests the
    /// change does not touch and nothing links to are still tests in view
    /// (#123), so an absent link is a finding and not a blank.
    repo_tests: bool,
    /// `tested_by` per fragment: callers repeat across changed symbols, and
    /// each answer walks every test fragment.
    memo: RefCell<FxHashMap<FragmentId, (Vec<TestLink>, bool)>>,
    /// Whether the fragment ends the search above it, per fragment.
    stops: RefCell<FxHashMap<FragmentId, bool>>,
    /// Whether the search above the fragment is open, per fragment.
    above: RefCell<FxHashMap<FragmentId, bool>>,
    /// The files the runners' configurations declare tests, whatever their names.
    declared: DeclaredTests,
    /// The changed files, by the path the answer prints: a test the change itself
    /// touches is named before the others.
    changed: FxHashSet<String>,
}

impl<'a> TestsReach<'a> {
    /// Ends at the run's deadline with what it has read. The run is then tripped
    /// for good, so no caller is searched and no claim about a test rests on this
    /// partial reach.
    pub(super) fn build(
        state: &'a ScoredState,
        cells: &'a Cells,
        diff_range: Option<&str>,
    ) -> Self {
        let mut by_rel: FxHashMap<String, Vec<&Fragment>> = FxHashMap::default();
        let mut rel_of: FxHashMap<&str, String> = FxHashMap::default();
        for f in state.run.scan(&state.all_fragments) {
            let shown = rel_of
                .entry(f.path())
                .or_insert_with(|| rel(state, f.path()))
                .clone();
            by_rel.entry(shown).or_default().push(f);
        }
        let read_by_a_cell = |path: &str| cells.of(&rel(state, path)).is_some();
        // The files no cell reads that a test file's name asks for: the naming
        // convention is a rule over paths, whatever the ranking linked.
        let listed: Vec<&str> = {
            let mut seen: FxHashSet<&str> = FxHashSet::default();
            state
                .all_fragments
                .iter()
                .map(Fragment::path)
                .filter(|path| seen.insert(*path))
                .collect()
        };
        let named: FxHashSet<String> = naming_links(&listed)
            .into_iter()
            .filter(|(test, source)| {
                diffctx_base::testfiles::is_test_path(Path::new(test))
                    && !read_by_a_cell(source)
                    && state.run.check()
            })
            .map(|(_, source)| source.to_string())
            .collect();
        let mut tests_at: FxHashMap<&str, Vec<(u32, u32)>> = FxHashMap::default();
        let rust_functions = state
            .all_fragments
            .iter()
            .filter(|f| f.kind == FragmentKind::Function && rust::is_rust(f.path()));
        for f in state.run.bounded(rust_functions) {
            tests_at.entry(f.path()).or_insert_with(|| {
                let text = state.run.source().read_to_string(Path::new(f.path()));
                // A read that failed is noted on the run by the source itself.
                text.map_or_else(Vec::new, |t| {
                    rust::test_functions(&t).unwrap_or_else(|e| {
                        tracing::warn!("impact: {} is not read as UTF-8 ({e})", f.path());
                        state
                            .run
                            .note(diffctx_snapshot::resource::LimitReason::ReadFailed);
                        Vec::new()
                    })
                })
            });
        }
        for (&spelled, shown) in state.run.bounded(&rel_of) {
            if !tests_at.contains_key(spelled) {
                let scopes = in_source_scopes(state, cells, spelled, shown);
                if !scopes.is_empty() {
                    tests_at.insert(spelled, scopes);
                }
            }
        }
        // Files the runner's configuration declares tests, whatever their names.
        let repo_paths = read_or_note(state, state.run.source().list_paths(&state.root_dir));
        let declared = DeclaredTests::read(
            &repo_paths.iter().map(String::as_str).collect::<Vec<_>>(),
            |path| {
                state
                    .run
                    .source()
                    .read_to_string(&state.root_dir.join(path))
            },
        );
        for (&spelled, shown) in &rel_of {
            if declared.is_test(shown)
                || (declared.is_loaded(shown)
                    && state
                        .run
                        .source()
                        .read_to_string(Path::new(spelled))
                        .and_then(|text| crate::facts::local_facts(shown, &text))
                        .is_some_and(|blob| holds_test_calls(&blob)))
            {
                tests_at.entry(spelled).or_default().push((1, u32::MAX));
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
        let test_spans: FxHashMap<String, Vec<(u32, u32)>> = tests_at
            .iter()
            .filter_map(|(spelled, spans)| Some((rel_of.get(spelled)?.clone(), spans.clone())))
            .collect();
        let test_fragments: Vec<&Fragment> = state
            .all_fragments
            .iter()
            .filter(|f| {
                diffctx_base::testfiles::is_test_path(Path::new(f.path()))
                    || inline_tests.contains(&f.id)
            })
            .collect();
        let unread: Vec<String> = state
            .run
            .limited_files()
            .into_iter()
            .filter(|f| {
                f.reason == diffctx_snapshot::resource::LimitReason::FileTooLarge
                    && declared.runs(&f.path)
            })
            .map(|f| f.path)
            .collect();
        let spelled: FxHashMap<&str, &str> = rel_of
            .iter()
            .map(|(path, shown)| (shown.as_str(), *path))
            .collect();
        let runs = runs::all(state, cells, diff_range, &spelled, &declared);
        let doctests = runs::doctest_runs(state);
        let asserted = runs::literal_runs(state, diff_range, &declared);
        let repo_tests = test_fragments.is_empty()
            && named.is_empty()
            && runs.is_empty()
            && doctests.is_empty()
            && asserted.is_empty()
            && repo_holds_test_file(state, &declared);
        Self {
            state,
            cells,
            by_rel,
            rel_of,
            named,
            test_fragments,
            inline_tests,
            test_spans,
            unread,
            runs,
            doctests,
            asserted,
            text_reach: OnceCell::new(),
            repo_tests,
            memo: RefCell::default(),
            stops: RefCell::default(),
            above: RefCell::default(),
            declared,
            changed: super::changed_rels(state).collect(),
        }
    }

    pub(super) fn any(&self) -> bool {
        !self.test_fragments.is_empty()
            || !self.named.is_empty()
            || !self.runs.is_empty()
            || !self.doctests.is_empty()
            || !self.asserted.is_empty()
            || self.repo_tests
    }

    /// A test file the size cap kept out that spells the caller's name, among the
    /// files of the caller's own group.
    pub(super) fn unread_naming(&self, caller: &Fragment, own: &str) -> Option<String> {
        let name = caller.symbol_name.as_deref().map(base_symbol)?;
        let group = Group::of_name(own)?;
        let tokens = self.cells.group_tokens(self.state, group)?;
        let names = Names::new([name]);
        self.unread
            .iter()
            .find(|path| {
                group.has_path(path)
                    && tokens.spells(path, &names) == Some(true)
                    && !self.cells.of(path).is_some_and(|cell| cell.is_bound(path))
            })
            .cloned()
    }

    /// A test file by its name or by a runner's configuration, as the answer prints it.
    pub(super) fn is_test_file(&self, shown: &str) -> bool {
        self.declared.runs(shown)
    }

    fn in_test_span(&self, shown: &str, line: u32) -> bool {
        self.test_spans
            .get(shown)
            .is_some_and(|spans| spans.iter().any(|&(a, b)| a <= line && line <= b))
    }

    pub(super) fn is_test(&self, f: &Fragment) -> bool {
        diffctx_base::testfiles::is_test_path(Path::new(f.path()))
            || self.inline_tests.contains(&f.id)
    }

    /// The cell that reads the file of `f`, with the path the answer prints.
    fn cell_of(&self, f: &Fragment) -> Option<(&'a crate::decide::Cell, &str)> {
        let shown = self.rel_of.get(f.path())?;
        Some((self.cells.of(shown)?, shown.as_str()))
    }

    /// The tests that guard a caller: each names the caller's symbol in code and
    /// reaches the caller's module — the same bar a caller has to clear. A
    /// bare name match (`render` in a Rust test, `render()` in a Python
    /// script) attributed guards to tests that never import the file (#312),
    /// and a test edge to the file proves the file is tested, not the caller.
    /// A test function names itself; a run of an entry names the file only.
    fn direct(&self, caller: &Fragment, link: &Link) -> Vec<(String, Option<String>)> {
        if self.is_test(caller) {
            return vec![(
                caller.path().to_string(),
                caller
                    .symbol_name
                    .as_deref()
                    .map(|n| base_symbol(n).to_string()),
            )];
        }
        // A chunk inside a long function is named `main[88]`; the test
        // names `main`.
        let name = caller.symbol_name.as_deref().map(base_symbol);
        let mut found: Vec<(String, Option<String>)> = self
            .entered_by(&self.runs, caller)
            .chain(self.entered_by(&self.asserted, caller))
            .collect();
        // A test that does not spell the name cannot link to it.
        if let Some(name) = name.filter(|s| s.len() >= 3) {
            found.extend(
                self.test_fragments
                    .iter()
                    .filter(|t| t.content.contains(name))
                    .filter(|t| link(t, caller))
                    .map(|t| {
                        (
                            t.path().to_string(),
                            t.symbol_name.as_deref().map(|n| base_symbol(n).to_string()),
                        )
                    }),
            );
        }
        found
    }

    /// The tests of the runs that enter `caller`, as (file, function).
    fn entered_by<'r>(
        &'r self,
        runs: &'r [Run],
        caller: &'r Fragment,
    ) -> impl Iterator<Item = (String, Option<String>)> + 'r {
        runs.iter()
            .filter(|run| run.enters(caller))
            .flat_map(|run| &run.tests)
            .map(|(path, line)| {
                let name = self
                    .cells
                    .of(path)
                    .and_then(|cell| cell.function_at(path, *line))
                    .map(str::to_string);
                (path.clone(), name)
            })
    }

    /// Every test that reaches the caller at the fewest hops, the ones the change
    /// touches first, then in path then name order, and the function in between when the test calls something that
    /// calls it — a public tool over a helper is how most code is tested
    /// (#348). Up to [`MAX_TEST_HOPS`] hops up the uses; none: no test.
    pub(super) fn tested_by(
        &self,
        caller: &'a Fragment,
        state: &ScoredState,
        by_id: &FxHashMap<&FragmentId, &'a Fragment>,
        link: &Link,
    ) -> Vec<TestLink> {
        self.reach(caller, state, by_id, link).0
    }

    /// Whether the tests of `caller` are the nearest only: a user above the
    /// level the walk stopped at was left unwalked, or a use on the walked path is
    /// hedged (a dispatch, an operator or a protocol call no use names).
    pub(super) fn nearest_only(
        &self,
        caller: &'a Fragment,
        state: &ScoredState,
        by_id: &FxHashMap<&FragmentId, &'a Fragment>,
        link: &Link,
    ) -> bool {
        self.reach(caller, state, by_id, link).1
    }

    fn reach(
        &self,
        caller: &'a Fragment,
        state: &ScoredState,
        by_id: &FxHashMap<&FragmentId, &'a Fragment>,
        link: &Link,
    ) -> (Vec<TestLink>, bool) {
        if let Some(known) = self.memo.borrow().get(&caller.id) {
            return known.clone();
        }
        let (mut answer, nearest) = self.walk_up(caller, state, by_id, link);
        for test in &mut answer {
            test.path = rel(state, &test.path);
        }
        answer.sort_by(|a, b| {
            let untouched = |t: &TestLink| !self.changed.contains(&t.path);
            untouched(a).cmp(&untouched(b)).then_with(|| a.cmp(b))
        });
        answer.dedup();
        let known = (answer, nearest || state.run.tripped());
        self.memo
            .borrow_mut()
            .insert(caller.id.clone(), known.clone());
        known
    }

    /// One hop up the uses of `f`: a cell's resolved uses where a cell reads the
    /// file, the ranking graph's semantic edges, held to `link`, where none does.
    fn up(&self, f: &'a Fragment, state: &ScoredState, link: &Link) -> Up<'a> {
        let mut up = Up::default();
        if let Some((cell, shown)) = self.cell_of(f) {
            let Some(name) = f.symbol_name.as_deref().map(base_symbol) else {
                return up;
            };
            for (file, line) in cell.users(shown, name, (f.start_line(), f.end_line())) {
                // A use in a local variable or an arrow is the function's: it is
                // the function a test calls.
                let user = self.by_rel.get(&file).and_then(|fragments| {
                    cell_caller(fragments, line).map(|user| lifted_caller(fragments, user))
                });
                let scoped =
                    user.is_none_or(|user| !self.is_test(user)) && self.in_test_span(&file, line);
                match user {
                    Some(user) if !scoped => up.fragments.push(user),
                    _ if scoped || self.is_test_file(&file) => {
                        let name = self
                            .cells
                            .of(&file)
                            .and_then(|cell| cell.function_at(&file, line))
                            .map(str::to_string);
                        up.test = Some((file, name));
                    }
                    _ => {}
                }
            }
            if super::is_pytest_fixture(f) {
                self.fixture_users(cell, shown, f, name, &mut up);
            }
            return up;
        }
        let Some(name) = f.symbol_name.as_deref().map(base_symbol) else {
            return up;
        };
        let Some(shown) = self.rel_of.get(f.path()) else {
            return up;
        };
        // The files of the group that spell the name, for the files no cell reads:
        // the text level's users, held to `link`.
        for path in spelling_files(state, self.cells, shown, &[name.to_string()]) {
            for user in self.by_rel.get(&path).into_iter().flatten().copied() {
                if user.id != f.id && !user.kind.is_signature() && link(user, f) {
                    up.fragments.push(user);
                }
            }
        }
        up
    }

    /// The fixture's name as its users ask for it, and the functions of its scope
    /// that could: the fixture's file, or beneath the directory of a
    /// `conftest.py` unless a nearer fixture of the same name overrides it.
    fn fixture_scope(
        &self,
        cell: Option<&crate::decide::Cell>,
        shown: &str,
        fixture: &'a Fragment,
        name: &str,
    ) -> (String, Vec<&'a Fragment>) {
        let asked = cell
            .map_or_else(Vec::new, |cell| {
                cell.framework_entries_at(shown, name, (fixture.start_line(), fixture.end_line()))
            })
            .into_iter()
            .find(|(kind, _)| *kind == EntryKind::Fixture)
            .and_then(|(_, framework)| framework.label)
            .unwrap_or_else(|| name.to_string());
        let own = Path::new(fixture.path());
        let conftest = own.file_name().and_then(|n| n.to_str()) == Some("conftest.py");
        let scope = own.parent().filter(|_| conftest);
        let in_scope = |path: &str| match scope {
            Some(dir) => Path::new(path).starts_with(dir),
            None => path == fixture.path(),
        };
        let members: Vec<&'a Fragment> = self
            .state
            .all_fragments
            .iter()
            .filter(|f| f.id != fixture.id && in_scope(f.path()))
            .collect();
        let overriding: Vec<String> = members
            .iter()
            .filter(|f| {
                f.symbol_name.as_deref().map(base_symbol) == Some(asked.as_str())
                    && super::is_pytest_fixture(f)
            })
            .map(|f| f.path().to_string())
            .collect();
        let overridden = |path: &str| {
            overriding.iter().any(|own| {
                let own = Path::new(own);
                let beneath = own
                    .file_name()
                    .and_then(|n| n.to_str())
                    .filter(|n| *n == "conftest.py")
                    .and_then(|_| own.parent());
                beneath.map_or_else(
                    || Path::new(path) == own,
                    |dir| Path::new(path).starts_with(dir),
                )
            })
        };
        let members = members
            .into_iter()
            .filter(|f| !overridden(f.path()))
            .collect();
        (asked, members)
    }

    /// The functions that ask for a pytest fixture, which no call names: by a
    /// parameter (the line of the `def`) or by `usefixtures` (the line the
    /// function starts at).
    pub(super) fn fixture_askers(
        &self,
        cell: Option<&crate::decide::Cell>,
        shown: &str,
        fixture: &'a Fragment,
        name: &str,
    ) -> Vec<(&'a Fragment, u32)> {
        let (asked, members) = self.fixture_scope(cell, shown, fixture, name);
        members
            .into_iter()
            .filter_map(|f| asks_for(f, &asked).map(|line| (f, line)))
            .collect()
    }

    /// What a fixture leads to one hop up: the functions that ask for it, and for
    /// an autouse fixture a test of its scope.
    fn fixture_users(
        &self,
        cell: &crate::decide::Cell,
        shown: &str,
        fixture: &'a Fragment,
        name: &str,
        up: &mut Up<'a>,
    ) {
        let (asked, members) = self.fixture_scope(Some(cell), shown, fixture, name);
        if is_autouse(fixture) && up.test.is_none() {
            up.test = members
                .iter()
                .filter(|f| self.is_test(f))
                .map(|f| f.path())
                .min()
                .map(|path| (path.to_string(), None));
        }
        up.fragments.extend(
            members
                .into_iter()
                .filter(|f| asks_for(f, &asked).is_some()),
        );
    }

    fn walk_up(
        &self,
        caller: &'a Fragment,
        state: &ScoredState,
        by_id: &FxHashMap<&FragmentId, &'a Fragment>,
        link: &Link,
    ) -> (Vec<TestLink>, bool) {
        let direct = |f: &Fragment, via: Option<&String>| -> Vec<TestLink> {
            self.direct(f, link)
                .into_iter()
                .map(|(path, name)| TestLink {
                    path,
                    name,
                    via: via.cloned(),
                })
                .collect()
        };
        let doctested = |f: &Fragment, via: Option<&String>| -> Vec<TestLink> {
            self.entered_by(&self.doctests, f)
                .map(|(path, name)| TestLink {
                    path,
                    name,
                    via: via.cloned(),
                })
                .collect()
        };
        // A user the walk leaves unwalked: one that is no test, or a test the list
        // does not name.
        let leaves = |f: &'a Fragment, found: &[TestLink]| -> bool {
            let up = self.up(f, state, link);
            let named = |path: &str, name: Option<&str>| {
                found
                    .iter()
                    .any(|t| t.path == path && t.name.as_deref() == name)
            };
            up.fragments.iter().any(|u| {
                !self.is_test(u) || !named(u.path(), u.symbol_name.as_deref().map(base_symbol))
            }) || up
                .test
                .is_some_and(|(path, name)| !named(&path, name.as_deref()))
        };
        let mut doctests = doctested(caller, None);
        let mut found = direct(caller, None);
        if !found.is_empty() {
            // A caller that is itself the test it found has no tests above it to miss.
            let itself = found.iter().any(|t| {
                t.path == caller.path()
                    && t.name.as_deref() == caller.symbol_name.as_deref().map(base_symbol)
            });
            let nearest = !itself && (self.hedged(caller) || leaves(caller, &found));
            found.extend(doctests);
            return (found, nearest);
        }
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
            let mut found = Vec::new();
            for &f in &frontier {
                let up = self.up(f, state, link);
                found.extend(up.test.map(|(path, name)| TestLink {
                    path,
                    name,
                    via: None,
                }));
                next.extend(up.fragments.into_iter().filter(|u| seen.insert(&u.id)));
            }
            for up in &next {
                if self.is_test(up) {
                    found.push(TestLink {
                        path: up.path().to_string(),
                        name: up
                            .symbol_name
                            .as_deref()
                            .map(|n| base_symbol(n).to_string()),
                        via: None,
                    });
                } else {
                    found.extend(direct(up, up.symbol_name.as_ref()));
                    doctests.extend(doctested(up, up.symbol_name.as_ref()));
                }
            }
            if !found.is_empty() {
                let nearest = frontier.iter().any(|f| self.hedged(f))
                    || next
                        .iter()
                        .any(|u| !self.is_test(u) && (self.hedged(u) || leaves(u, &found)));
                found.extend(doctests);
                return (found, nearest);
            }
            frontier = next;
        }
        (doctests, !frontier.is_empty())
    }

    /// Whether the search for a test of `caller`, a caller of `core`, is open: the
    /// search above it is, or a test asserts text `core` renders (a string it holds
    /// or one a definition it uses holds) and may reach the caller by a path nothing
    /// here traces, a page that renders it.
    pub(super) fn caller_open(&self, caller: &'a Fragment, core: &Fragment, link: &Link) -> bool {
        self.open_above(caller, link, true) || self.asserts_text_of(core, link)
    }

    fn asserts_text_of(&self, f: &Fragment, link: &Link) -> bool {
        let state: &'a ScoredState = self.state;
        self.text_reach
            .get_or_init(|| {
                let mut reached = FxHashSet::default();
                let mut frontier: Vec<&'a Fragment> = state
                    .run
                    .bounded(&state.all_fragments)
                    .filter(|held| self.asserted.iter().any(|run| run.enters(held)))
                    .collect();
                for _ in 0..=MAX_TEST_HOPS {
                    frontier = state
                        .run
                        .bounded(frontier)
                        .filter(|held| reached.insert(held.id.clone()))
                        .flat_map(|held| self.up(held, state, link).fragments)
                        .collect();
                }
                diffctx_base::profile::count("impact.tests.asserted", reached.len() as u64);
                reached
            })
            .contains(&f.id)
            || state.run.tripped()
    }

    /// Ends the search above it: a test, one that names it, or a definition whose
    /// occurrence account holds a hedge (a file no cell reads has none).
    fn stops(&self, f: &Fragment, link: &Link) -> bool {
        if let Some(&known) = self.stops.borrow().get(&f.id) {
            return known;
        }
        let stops = self.is_test(f)
            || !self.direct(f, link).is_empty()
            || self.entered_by(&self.doctests, f).next().is_some()
            || self.hedged(f);
        self.stops.borrow_mut().insert(f.id.clone(), stops);
        stops
    }

    /// Whether the occurrence account of `f` holds a hedge: some use of it is
    /// unresolved, a dispatch, or an operator or protocol call nothing names.
    fn hedged(&self, f: &Fragment) -> bool {
        let name = f.symbol_name.as_deref().map(base_symbol);
        (self.cell_of(f).zip(name)).is_some_and(|((cell, shown), name)| {
            cell.define(shown, name, (f.start_line(), f.end_line()))
                .iter()
                .any(|changed| cell.hedged(changed))
        })
    }

    /// Whether a test could reach `f` unnamed: the uses above it hold a test past
    /// the hop bound or a use that stops the search (`f` too when `own`: a caller
    /// no test names), a test asserts a string of `f` too many files spell to name
    /// it, or the run stopped. An absence needs a closed search.
    pub(super) fn open_above(&self, f: &'a Fragment, link: &Link, own: bool) -> bool {
        (own && self.stops(f, link))
            || self
                .asserted
                .iter()
                .any(|run| run.enters(f) && run.tests.is_empty())
            || self.reaches_open(f, link)
    }

    /// The search above `f`, once per fragment: a closed search closes everything it
    /// visited, an open one the path to its find.
    fn reaches_open(&self, f: &'a Fragment, link: &Link) -> bool {
        if let Some(&open) = self.above.borrow().get(&f.id) {
            return open || !self.state.run.check();
        }
        let mut parent = FxHashMap::from_iter([(&f.id, &f.id)]);
        let mut todo = vec![f];
        let mut found = None;
        'walk: while let Some(at) = todo.pop() {
            let up = self.up(at, self.state, link);
            if !self.state.run.check() || up.test.is_some() {
                found = Some(at);
                break;
            }
            for user in up.fragments {
                let known = self.above.borrow().get(&user.id).copied();
                if self.stops(user, link) || known == Some(true) {
                    found = Some(at);
                    break 'walk;
                }
                if known.is_none() && !parent.contains_key(&user.id) {
                    parent.insert(&user.id, &at.id);
                    todo.push(user);
                }
            }
        }
        let mut above = self.above.borrow_mut();
        let Some(mut at) = found.map(|at| &at.id) else {
            above.extend(parent.keys().map(|&id| (id.clone(), false)));
            return false;
        };
        while above.insert(at.clone(), true).is_none() {
            at = parent[at];
        }
        true
    }

    pub(super) fn file_tested(
        &self,
        caller: &Fragment,
        reaches_file: &dyn Fn(&Fragment, &Fragment) -> bool,
    ) -> bool {
        if let Some((cell, shown)) = self.cell_of(caller) {
            return cell.reached_from(shown).any(|user| self.is_test_file(user))
                || self
                    .test_fragments
                    .iter()
                    .any(|t| t.path() == caller.path());
        }
        self.named.contains(caller.path())
            || self.test_fragments.iter().any(|t| reaches_file(t, caller))
    }
}

/// The line spans of `spelled` (as the answer prints it, `shown`) that the
/// framework's rule makes test scopes although the file is no test file by name.
/// The token index says which files could hold one before any is read.
fn in_source_scopes(
    state: &ScoredState,
    cells: &Cells,
    spelled: &str,
    shown: &str,
) -> Vec<(u32, u32)> {
    let path = Path::new(shown);
    let markers = diffctx_base::testfiles::in_source_markers(path);
    if markers.is_empty() || diffctx_base::testfiles::is_test_path(path) {
        return Vec::new();
    }
    let names = Names::new(markers.iter().copied());
    let spells = Group::of_name(shown)
        .and_then(|group| cells.group_tokens(state, group))
        .and_then(|tokens| tokens.spells(shown, &names));
    if spells != Some(true) {
        return Vec::new();
    }
    state
        .run
        .source()
        .read_to_string(Path::new(spelled))
        .map_or_else(Vec::new, |text| {
            diffctx_base::testfiles::in_source_test_scopes(path, &text)
        })
}

/// The lines of `f` before its own `def`: its decorators, however they wrap.
fn declaration(f: &Fragment) -> impl Iterator<Item = &str> {
    f.content.lines().take_while(|line| {
        let line = line.trim_start();
        !(line.starts_with("def ") || line.starts_with("async def "))
    })
}

/// `@pytest.fixture(autouse=True)`: every test of the fixture's scope asks for it.
fn is_autouse(fixture: &Fragment) -> bool {
    declaration(fixture).any(|line| line.replace(' ', "").contains("autouse=True"))
}

/// The line at which `f` asks for the fixture `name`: its `def` when a parameter
/// is the name, the function's first line for `@pytest.mark.usefixtures("name")`.
fn asks_for(f: &Fragment, name: &str) -> Option<u32> {
    super::names_parameter(f, name).or_else(|| {
        declaration(f)
            .any(|line| line.contains("usefixtures") && spelled_as_command(line, name))
            .then(|| f.start_line())
    })
}

/// Whether the tree the run reads holds any test file, linked or not.
fn repo_holds_test_file(state: &ScoredState, declared: &DeclaredTests) -> bool {
    match state.run.source().list() {
        Ok(listing) => state.run.bounded(&listing.blobs).any(|b| {
            declared.runs(&diffctx_base::paths::to_posix_display(
                b.path.to_string_lossy(),
            ))
        }),
        Err(_) => false,
    }
}

/// Whether a script's facts hold a call of a test registrar: `describe(`, `it(`,
/// `test(` or a member call on one (`describe.each(..)`, `it.only(..)`). A file a
/// runner loads beside the code it tests is a test only when it does; `re.test(s)`
/// is a call of `test` on something else, and so is a name the file binds itself (a
/// definition, an import, a parameter or a local called `it`): the runner's global
/// is not what the call reaches.
fn holds_test_calls(blob: &crate::facts::BlobFacts) -> bool {
    use crate::facts::{Fact, Receiver, UseKind};
    const REGISTRARS: [&str; 3] = ["describe", "it", "test"];
    let binds = |name: &str| {
        blob.facts.iter().any(|fact| match fact {
            Fact::Definition(def) => def.local.name.as_str() == name,
            Fact::LocalBinding(local) => local.name.as_str() == name,
            Fact::Import(import) => {
                import.default.as_ref().is_some_and(|d| d.as_str() == name)
                    || import.star.alias().is_some_and(|a| a.as_str() == name)
                    || import
                        .names
                        .iter()
                        .any(|(n, alias)| alias.as_ref().unwrap_or(n).as_str() == name)
            }
            _ => false,
        })
    };
    blob.facts.iter().any(|fact| match fact {
        Fact::Reference(r) if r.use_kind == UseKind::Call => {
            let registrar = match r.qualifier.first() {
                None if r.receiver == Receiver::None => Some(&r.name),
                None => None,
                Some(root) => Some(root),
            };
            registrar
                .is_some_and(|name| REGISTRARS.contains(&name.as_str()) && !binds(name.as_str()))
        }
        _ => false,
    })
}
