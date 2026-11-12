//! Which tests reach a definition.
//!
//! A test reaches code in two ways. It uses it: the test, or a function it calls
//! (two hops up the uses, [`MAX_TEST_HOPS`]), holds a resolved use of the
//! definition, which a cell of the definition's language answers
//! ([`Cell::users`](crate::decide::Cell::users)); and it runs it: the built binary
//! of a crate, the console script a manifest declares, a script spelled by its
//! path on a command line. Nothing calls those, so each is a [`Run`] the test
//! makes of one entry; so is a string a changed definition holds and a test
//! asserts (the literal channel). A pytest fixture is run by the tests and fixtures that ask for
//! it by name, and a scheduled job by the scheduler: the first is read off the
//! fixture's scope ([`TestsReach::fixture_users`]), the second off a test that names the
//! job to the scheduler.
//!
//! For the files no cell reads, the files of the group that spell a name are the
//! users, held to the same bar as a caller, and a test file's name links it to the
//! source it asks for (`testfiles::naming_links`). Nothing here reads the ranking
//! graph. A claim that a definition
//! has no test link is printed only where `any` says tests are in view.

use std::cell::RefCell;
use std::path::Path;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::facts::EntryKind;

use super::mentions::{Naming, commented, naming, starts_a_program};
use super::rust;
use super::{
    Cells, Fragment, FragmentId, FragmentKind, Link, MAX_TEST_HOPS, ScoredState, TestLink,
    base_symbol, cell_caller, lifted_caller, names_word, read_or_note, rel, spelling_files,
};
use crate::decide::Group;
use crate::testfiles::naming_links;
use crate::tokens::Names;

/// How a test runs code that no use of its own reaches: `file` as the fragments
/// spell it, `symbol` the one function of the file that runs (none: the whole
/// file runs), `tests` the lines of the tests that run it, as (file, line).
struct Run {
    file: String,
    symbol: Option<String>,
    tests: Vec<(String, u32)>,
}

/// The distinct (file, line) sites, in path then line order.
fn ordered(mut sites: Vec<(String, u32)>) -> Vec<(String, u32)> {
    sites.sort();
    sites.dedup();
    sites
}

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
    /// Functions outside test files an attribute marks as tests: a Rust
    /// `#[test]` in an inline `mod tests` (#528).
    inline_tests: FxHashSet<&'a FragmentId>,
    /// Test files the size cap kept out.
    unread: Vec<String>,
    /// Entries a test runs: binaries (`CARGO_BIN_EXE_<name>`, `cargo_bin("<name>")`:
    /// #391, #678), console scripts, scripts spelled by their path.
    runs: Vec<Run>,
    /// The repository holds a test file the walk never reached: tests the
    /// change does not touch and nothing links to are still tests in view
    /// (#123), so an absent link is a finding and not a blank.
    repo_tests: bool,
    /// `tested_by` per fragment: callers repeat across changed symbols, and
    /// each answer walks every test fragment.
    memo: RefCell<FxHashMap<FragmentId, Vec<TestLink>>>,
}

impl<'a> TestsReach<'a> {
    pub(super) fn build(
        state: &'a ScoredState,
        cells: &'a Cells,
        diff_range: Option<&str>,
    ) -> Self {
        let mut by_rel: FxHashMap<String, Vec<&Fragment>> = FxHashMap::default();
        let mut rel_of: FxHashMap<&str, String> = FxHashMap::default();
        for f in &state.all_fragments {
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
                crate::testfiles::is_test_path(Path::new(test))
                    && !read_by_a_cell(source)
                    && state.run.check()
            })
            .map(|(_, source)| source.to_string())
            .collect();
        let mut tests_at: FxHashMap<&str, Vec<(u32, u32)>> = FxHashMap::default();
        for f in &state.all_fragments {
            if f.kind == FragmentKind::Function && rust::is_rust(f.path()) {
                tests_at.entry(f.path()).or_insert_with(|| {
                    let text = state.run.source().read_to_string(Path::new(f.path()));
                    // A read that failed is noted on the run by the source itself.
                    text.map_or_else(Vec::new, |t| rust::test_functions(&t))
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
        let unread: Vec<String> = state
            .run
            .limited_files()
            .into_iter()
            .filter(|f| {
                f.reason == crate::resource::LimitReason::FileTooLarge
                    && crate::testfiles::is_test_path(Path::new(&f.path))
            })
            .map(|f| f.path)
            .collect();
        let spelled: FxHashMap<&str, &str> = rel_of
            .iter()
            .map(|(path, shown)| (shown.as_str(), *path))
            .collect();
        let mut runs = binary_runs(state, diff_range);
        runs.extend(console_runs(state, diff_range, &spelled));
        runs.extend(script_runs(state, diff_range, &spelled));
        runs.extend(framework_runs(state, cells, diff_range));
        runs.extend(literal_runs(state, diff_range));
        let repo_tests = test_fragments.is_empty()
            && named.is_empty()
            && runs.is_empty()
            && repo_holds_test_file(state);
        Self {
            state,
            cells,
            by_rel,
            rel_of,
            named,
            test_fragments,
            inline_tests,
            unread,
            runs,
            repo_tests,
            memo: RefCell::default(),
        }
    }

    pub(super) fn any(&self) -> bool {
        !self.test_fragments.is_empty()
            || !self.named.is_empty()
            || !self.runs.is_empty()
            || self.repo_tests
    }

    /// A test file the size cap kept out that spells the caller's name, among the
    /// files of the caller's own group.
    pub(super) fn unread_naming(&self, caller: &Fragment, own: &str) -> Option<String> {
        let name = caller.symbol_name.as_deref().map(base_symbol)?;
        let group = Group::of_path(own)?;
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

    pub(super) fn is_test(&self, f: &Fragment) -> bool {
        crate::testfiles::is_test_path(Path::new(f.path())) || self.inline_tests.contains(&f.id)
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
            .runs
            .iter()
            .filter(|run| {
                run.file == caller.path()
                    && run
                        .symbol
                        .as_deref()
                        .is_none_or(|symbol| name == Some(symbol))
            })
            .flat_map(|run| &run.tests)
            .map(|(path, line)| {
                let name = self
                    .cells
                    .of(path)
                    .and_then(|cell| cell.function_at(path, *line))
                    .map(str::to_string);
                (path.clone(), name)
            })
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

    /// Every test that reaches the caller at the fewest hops, in path then name
    /// order, and the function in between when the test calls something that
    /// calls it — a public tool over a helper is how most code is tested
    /// (#348). Up to [`MAX_TEST_HOPS`] hops up the uses; none: no test.
    pub(super) fn tested_by(
        &self,
        caller: &'a Fragment,
        state: &ScoredState,
        by_id: &FxHashMap<&FragmentId, &'a Fragment>,
        link: &Link,
    ) -> Vec<TestLink> {
        if let Some(known) = self.memo.borrow().get(&caller.id) {
            return known.clone();
        }
        let mut answer = self.walk_up(caller, state, by_id, link);
        for test in &mut answer {
            test.path = rel(state, &test.path);
        }
        answer.sort();
        answer.dedup();
        self.memo
            .borrow_mut()
            .insert(caller.id.clone(), answer.clone());
        answer
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
                match self.by_rel.get(&file).and_then(|fragments| {
                    cell_caller(fragments, line).map(|user| lifted_caller(fragments, user))
                }) {
                    Some(user) => up.fragments.push(user),
                    None if crate::testfiles::is_test_path(Path::new(&file)) => {
                        let name = self
                            .cells
                            .of(&file)
                            .and_then(|cell| cell.function_at(&file, line))
                            .map(str::to_string);
                        up.test = Some((file, name));
                    }
                    None => {}
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
    ) -> Vec<TestLink> {
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
        let found = direct(caller, None);
        if !found.is_empty() {
            return found;
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
                }
            }
            if !found.is_empty() {
                return found;
            }
            frontier = next;
        }
        Vec::new()
    }

    pub(super) fn file_tested(
        &self,
        caller: &Fragment,
        reaches_file: &dyn Fn(&Fragment, &Fragment) -> bool,
    ) -> bool {
        if let Some((cell, shown)) = self.cell_of(caller) {
            return cell
                .reached_from(shown)
                .any(|user| crate::testfiles::is_test_path(Path::new(user)))
                || self
                    .test_fragments
                    .iter()
                    .any(|t| t.path() == caller.path());
        }
        self.named.contains(caller.path())
            || self.test_fragments.iter().any(|t| reaches_file(t, caller))
    }
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
fn repo_holds_test_file(state: &ScoredState) -> bool {
    let is_test = |p: &Path| crate::testfiles::is_test_path(p);
    match state.run.source().list() {
        Ok(listing) => state.run.bounded(&listing.blobs).any(|b| is_test(&b.path)),
        Err(_) => false,
    }
}

/// The Rust binaries in the analysed fragments, keyed by their entry file
/// as fragments spell it, each run by one test of its crate that runs
/// the built binary. `src/main.rs` is the package's binary, named in
/// `Cargo.toml`; `src/bin/<name>.rs` is `<name>`.
fn binary_runs(state: &ScoredState, diff_range: Option<&str>) -> Vec<Run> {
    let mut entries: Vec<(String, String, String)> = Vec::new();
    let mut seen: FxHashSet<&str> = FxHashSet::default();
    for f in &state.all_fragments {
        let own = f.path();
        if !seen.insert(own) || !rust::is_rust(own) {
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
        return Vec::new();
    }
    let head = diff_range
        .map(crate::snapshot::split_diff_range)
        .and_then(|(_, head)| head);
    let needles = vec!["CARGO_BIN_EXE".to_string(), "cargo_bin".to_string()];
    let grepped = read_or_note(
        state,
        crate::grep::grep_lines(
            &state.root_dir,
            &state.run.source(),
            &needles,
            head.as_deref(),
        ),
    );
    let mut out = Vec::new();
    for (own, krate, name) in entries {
        let exe = format!("CARGO_BIN_EXE_{name}");
        let quoted = format!("\"{name}\"");
        let tests = ordered(
            grepped
                .rows
                .iter()
                .filter(|(path, _, text)| {
                    path.starts_with(&format!("{krate}tests/"))
                        && (names_word(text, &exe)
                            || (text.contains("cargo_bin") && text.contains(&quoted)))
                })
                .map(|(path, line, _)| (path.clone(), *line))
                .collect(),
        );
        if !tests.is_empty() {
            out.push(Run {
                file: own,
                symbol: Some("main".to_string()),
                tests,
            });
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

/// A console script a manifest declares: the name a command line spells, the
/// file and the function it runs (none: the file).
struct Script {
    name: String,
    file: String,
    function: Option<String>,
}

/// The console scripts of the repository's manifests: `[project.scripts]` and
/// `[tool.poetry.scripts]` of a `pyproject.toml` (`name = "pkg.mod:func"`), `bin`
/// of a `package.json` (a name and the file it runs).
fn declared_scripts(state: &ScoredState, paths: &FxHashSet<String>) -> Vec<Script> {
    let source = state.run.source();
    let mut scripts = Vec::new();
    for manifest in paths {
        let (dir, file) = manifest.rsplit_once('/').unwrap_or(("", manifest.as_str()));
        if file != "pyproject.toml" && file != "package.json" {
            continue;
        }
        let Some(text) = source.read_to_string(&state.root_dir.join(manifest)) else {
            continue;
        };
        if file == "pyproject.toml" {
            scripts.extend(python_scripts(&text, dir, paths));
        } else {
            scripts.extend(node_scripts(&text, dir));
        }
    }
    scripts.sort_by(|a, b| (&a.name, &a.file).cmp(&(&b.name, &b.file)));
    scripts
}

fn join(dir: &str, rest: &str) -> String {
    let rest = rest.trim_start_matches("./");
    if dir.is_empty() {
        rest.to_string()
    } else {
        format!("{dir}/{rest}")
    }
}

fn python_scripts(text: &str, dir: &str, paths: &FxHashSet<String>) -> Vec<Script> {
    let Ok(table) = toml::from_str::<toml::Table>(text) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for at in [
        &["project", "scripts"][..],
        &["tool", "poetry", "scripts"][..],
    ] {
        let Some(toml::Value::Table(entries)) = crate::snapshot::workspace::toml_at(&table, at)
        else {
            continue;
        };
        for (name, target) in entries {
            let Some(target) = target.as_str() else {
                continue;
            };
            // `pkg.mod:func [extra]`
            let target = target.split_whitespace().next().unwrap_or("");
            let Some((module, function)) = target.split_once(':') else {
                continue;
            };
            let module = module.replace('.', "/");
            let found = ["", "src/"].into_iter().find_map(|root| {
                [format!("{module}.py"), format!("{module}/__init__.py")]
                    .into_iter()
                    .map(|tail| join(dir, &format!("{root}{tail}")))
                    .find(|path| paths.contains(path))
            });
            if let Some(file) = found {
                out.push(Script {
                    name: name.clone(),
                    file,
                    function: function.rsplit('.').next().map(str::to_string),
                });
            }
        }
    }
    out
}

fn node_scripts(text: &str, dir: &str) -> Vec<Script> {
    let Ok(manifest) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    let bins: Vec<(String, String)> = match manifest.get("bin") {
        Some(serde_json::Value::String(path)) => manifest
            .get("name")
            .and_then(|n| n.as_str())
            .map_or_else(Vec::new, |name| {
                vec![(
                    name.rsplit('/').next().unwrap_or(name).to_string(),
                    path.clone(),
                )]
            }),
        Some(serde_json::Value::Object(map)) => map
            .iter()
            .filter_map(|(name, path)| Some((name.clone(), path.as_str()?.to_string())))
            .collect(),
        _ => Vec::new(),
    };
    bins.into_iter()
        .map(|(name, path)| Script {
            name,
            file: join(dir, &path),
            function: None,
        })
        .collect()
}

/// The name as one quoted word, or the first word of a quoted command:
/// `["mytool", "--flag"]`, `'mytool --flag'`.
fn spelled_as_command(text: &str, name: &str) -> bool {
    ['"', '\'']
        .into_iter()
        .any(|q| text.contains(&format!("{q}{name}{q}")) || text.contains(&format!("{q}{name} ")))
}

/// The tests that run a console script: a test file with a line that starts a
/// program and spells the script's name as a command. The function the manifest
/// names (a Python entry), or the file (a Node bin), is what they run.
fn console_runs(
    state: &ScoredState,
    diff_range: Option<&str>,
    spelled: &FxHashMap<&str, &str>,
) -> Vec<Run> {
    let paths: FxHashSet<String> =
        read_or_note(state, state.run.source().list_paths(&state.root_dir))
            .into_iter()
            .collect();
    let scripts = declared_scripts(state, &paths);
    if scripts.is_empty() {
        return Vec::new();
    }
    let mut needles: Vec<String> = scripts.iter().map(|s| s.name.clone()).collect();
    needles.sort();
    needles.dedup();
    let head = diff_range
        .map(crate::snapshot::split_diff_range)
        .and_then(|(_, head)| head);
    let grepped = read_or_note(
        state,
        crate::grep::grep_lines(
            &state.root_dir,
            &state.run.source(),
            &needles,
            head.as_deref(),
        ),
    );
    let mut out = Vec::new();
    for script in scripts {
        let Some(file) = spelled.get(script.file.as_str()) else {
            continue;
        };
        let tests = ordered(
            grepped
                .rows
                .iter()
                .filter(|(path, _, text)| {
                    crate::testfiles::is_test_path(Path::new(path))
                        && *path != script.file
                        && !commented(text)
                        && starts_a_program(text)
                        && spelled_as_command(text, &script.name)
                })
                .map(|(path, line, _)| (path.clone(), *line))
                .collect(),
        );
        if !tests.is_empty() {
            out.push(Run {
                file: (*file).to_string(),
                symbol: script.function,
                tests,
            });
        }
    }
    out
}

/// What a command line can start by its path: a shell script, or a program of a
/// language that has a script form.
fn is_runnable(path: &str) -> bool {
    matches!(
        path.rsplit_once('.').map(|(_, e)| e),
        Some("sh" | "bash" | "zsh" | "py" | "js" | "mjs" | "cjs" | "rb" | "pl")
    )
}

/// The tests that run a changed script by its path: a test file with a line
/// that starts a program and spells the path as a command line does (the same
/// reading `named_by_path` gives a path, which an import is not).
fn script_runs(
    state: &ScoredState,
    diff_range: Option<&str>,
    spelled: &FxHashMap<&str, &str>,
) -> Vec<Run> {
    let targets: Vec<String> = state
        .changed_files
        .iter()
        .map(|p| rel(state, &p.to_string_lossy()))
        .filter(|t| is_runnable(t) && !crate::testfiles::is_test_path(Path::new(t)))
        .filter(|t| spelled.contains_key(t.as_str()))
        .collect();
    if targets.is_empty() {
        return Vec::new();
    }
    let root = state.root_dir.to_string_lossy().replace('\\', "/");
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
    let head = diff_range
        .map(crate::snapshot::split_diff_range)
        .and_then(|(_, head)| head);
    let grepped = read_or_note(
        state,
        crate::grep::grep_lines(
            &state.root_dir,
            &state.run.source(),
            &needles,
            head.as_deref(),
        ),
    );
    let token_re = once_cell::sync::Lazy::force(&super::mentions::PATH_TOKEN);
    let mut out = Vec::new();
    for target in &targets {
        let basename = basename_of(target);
        let tests = grepped
            .rows
            .iter()
            .filter(|(path, _, text)| {
                crate::testfiles::is_test_path(Path::new(path))
                    && path != target
                    && !commented(text)
                    && starts_a_program(text)
                    && token_re.find_iter(text).any(|m| {
                        let token = m.as_str().trim_end_matches('.');
                        token.ends_with(basename.as_str())
                            && matches!(
                                naming(
                                    token,
                                    path,
                                    target,
                                    basenames.get(&basename) == Some(&1),
                                    &root
                                ),
                                Some(
                                    Naming::FullPath
                                        | Naming::RelativePath
                                        | Naming::UniqueBasename
                                )
                            )
                    })
            })
            .map(|(path, line, _)| (path.clone(), *line))
            .collect();
        let tests = ordered(tests);
        if let (false, Some(file)) = (tests.is_empty(), spelled.get(target.as_str())) {
            out.push(Run {
                file: (*file).to_string(),
                symbol: None,
                tests,
            });
        }
    }
    out
}

/// How a test spells the key of an entry a framework makes: the request of a
/// route (a quoted path that ends in the route's, on a line that names its verb,
/// the path's parameters open), the name of a command a program starts, the name
/// of a tool, the quoted name of a job on a line that speaks of schedulers.
enum Spelling {
    Route(regex::Regex, Option<String>),
    Command(String),
    Tool(String),
    /// A scheduled job, named to the scheduler by its function's name.
    Job(String),
}

impl Spelling {
    #[allow(
        clippy::disallowed_methods,
        reason = "an escaped pattern the regex crate refuses leaves the route with no text reach: its caller is listed with no static test link, the cautious side"
    )]
    fn of(kind: EntryKind, verb: Option<&str>, label: &str) -> Option<Self> {
        match kind {
            EntryKind::Route => {
                let segments: Vec<String> = label
                    .trim_matches('/')
                    .split('/')
                    .map(|segment| {
                        if is_parameter(segment) {
                            r#"[^/"'`?#\s]+"#.to_string()
                        } else {
                            regex::escape(segment)
                        }
                    })
                    .collect();
                let pattern = format!(
                    r#"["'`][^"'`\s]*/{}/?(?:[?#][^"'`]*)?["'`]"#,
                    segments.join("/")
                );
                regex::Regex::new(&pattern)
                    .ok()
                    .map(|re| Self::Route(re, verb.map(str::to_string)))
            }
            EntryKind::Command => Some(Self::Command(label.to_string())),
            EntryKind::Tool => Some(Self::Tool(label.to_string())),
            EntryKind::Scheduled => Some(Self::Job(label.to_string())),
            _ => None,
        }
    }

    fn spelled_by(&self, text: &str) -> bool {
        match self {
            Self::Route(path, verb) => {
                path.is_match(text)
                    && verb.as_deref().is_none_or(|verb| {
                        text.split(|c: char| !c.is_alphanumeric())
                            .any(|word| word.eq_ignore_ascii_case(verb))
                    })
            }
            Self::Command(name) => starts_a_program(text) && spelled_as_command(text, name),
            Self::Tool(name) => {
                spelled_as_command(text, name) && text.to_lowercase().contains("tool")
            }
            Self::Job(name) => {
                let line = text.to_lowercase();
                spelled_as_command(text, name)
                    && ["job", "schedul", "cron"]
                        .iter()
                        .any(|word| line.contains(word))
            }
        }
    }
}

/// A segment of a route that stands for any value: `{id}`, `<int:id>`, `:id`, `*`.
fn is_parameter(segment: &str) -> bool {
    segment.starts_with(['{', '<', ':']) || segment == "*"
}

/// The literal start of a route, which a test spells whatever the parameters are.
fn route_needle(label: &str) -> String {
    let literal: Vec<&str> = label
        .trim_matches('/')
        .split('/')
        .take_while(|segment| !is_parameter(segment))
        .collect();
    format!("/{}", literal.join("/"))
}

/// The tests that reach the routes, commands and tools the facts state for the
/// definitions of the changed files: a test file with a line that spells the
/// entry's key as a caller of it does. The framework's name is nobody's concern
/// here (`facts::framework`); only `kind`, `verb` and `label` are read.
fn framework_runs(state: &ScoredState, cells: &Cells, diff_range: Option<&str>) -> Vec<Run> {
    let changed: FxHashSet<String> = state
        .changed_files
        .iter()
        .map(|p| rel(state, &p.to_string_lossy()))
        .collect();
    struct Entry {
        file: String,
        symbol: String,
        spelling: Spelling,
        needle: String,
    }
    let mut entries: Vec<Entry> = Vec::new();
    let mut seen: FxHashSet<(String, String, EntryKind, String)> = FxHashSet::default();
    for f in &state.all_fragments {
        let shown = rel(state, f.path());
        if !changed.contains(&shown) || !f.kind.is_definition_kind() {
            continue;
        }
        let Some(name) = f.symbol_name.as_deref().map(base_symbol) else {
            continue;
        };
        let Some(cell) = cells.of(&shown) else {
            continue;
        };
        for (kind, framework) in
            cell.framework_entries_at(&shown, name, (f.start_line(), f.end_line()))
        {
            // A job's key is its function: the expression it runs on says when,
            // not which.
            let key = if kind == EntryKind::Scheduled {
                Some(name.to_string())
            } else {
                framework.label.clone().filter(|l| !l.is_empty())
            };
            let Some(label) = key else {
                continue;
            };
            let needle = if kind == EntryKind::Route {
                route_needle(&label)
            } else {
                label.clone()
            };
            if needle.trim_matches('/').len() < 3 {
                continue;
            }
            let verb = framework.verb.as_ref().map(|v| v.as_str().to_string());
            let Some(spelling) = Spelling::of(kind, verb.as_deref(), &label) else {
                continue;
            };
            if seen.insert((f.path().to_string(), name.to_string(), kind, label)) {
                entries.push(Entry {
                    file: f.path().to_string(),
                    symbol: name.to_string(),
                    spelling,
                    needle,
                });
            }
        }
    }
    if entries.is_empty() {
        return Vec::new();
    }
    let mut needles: Vec<String> = entries.iter().map(|e| e.needle.clone()).collect();
    needles.sort();
    needles.dedup();
    let head = diff_range
        .map(crate::snapshot::split_diff_range)
        .and_then(|(_, head)| head);
    let grepped = read_or_note(
        state,
        crate::grep::grep_lines(
            &state.root_dir,
            &state.run.source(),
            &needles,
            head.as_deref(),
        ),
    );
    entries
        .into_iter()
        .filter_map(|entry| {
            let tests = ordered(
                grepped
                    .rows
                    .iter()
                    .filter(|(path, _, text)| {
                        crate::testfiles::is_test_path(Path::new(path))
                            && !commented(text)
                            && entry.spelling.spelled_by(text)
                    })
                    .map(|(path, line, _)| (path.clone(), *line))
                    .collect(),
            );
            (!tests.is_empty()).then(|| Run {
                file: entry.file,
                symbol: Some(entry.symbol),
                tests,
            })
        })
        .collect()
}

/// The most strings of changed definitions one answer looks for in the tests.
const MAX_LITERAL_NEEDLES: usize = 64;
/// A string held by more files than this names no one of them: it is a word the
/// project says everywhere.
const MAX_LITERAL_FILES: usize = 3;

/// A string worth looking for whole: long enough, with letters, and shaped like a
/// code, a label or a sentence (`malformed_hlc`, `save-indicator`, `Saving
/// favorites`) rather than one plain word; never an import path.
fn distinctive(literal: &str) -> bool {
    let letters = literal.chars().filter(|c| c.is_alphabetic()).count();
    letters >= 4
        && !(literal.starts_with("./") || literal.starts_with("../"))
        && (literal.contains(['_', '-', ' ', '.', ':', '@'])
            || literal.chars().any(|c| c.is_ascii_digit())
            || literal
                .chars()
                .zip(literal.chars().skip(1))
                .any(|(a, b)| a.is_ascii_lowercase() && b.is_ascii_uppercase()))
}

/// The tests that assert a string a changed definition holds: a test file that
/// spells it, where no more than [`MAX_LITERAL_FILES`] files of the project do.
/// The test reaches the definition the way it reaches one it runs, through what
/// the code says rather than what it calls (an error code, a label), which no
/// resolved use shows.
fn literal_runs(state: &ScoredState, diff_range: Option<&str>) -> Vec<Run> {
    static STRING: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
        regex::Regex::new(r#""([^"\\\n]{8,80})"|'([^'\\\n]{8,80})'"#).expect("string regex")
    });
    let changed: FxHashSet<String> = state
        .changed_files
        .iter()
        .map(|p| rel(state, &p.to_string_lossy()))
        .collect();
    // (the file as fragments spell it, as the answer prints it, the symbol, its strings)
    let mut held: Vec<(String, String, String, Vec<String>)> = Vec::new();
    for f in &state.all_fragments {
        let shown = rel(state, f.path());
        if !changed.contains(&shown)
            || !f.kind.is_definition_kind()
            || crate::testfiles::is_test_path(Path::new(&shown))
            || !super::is_code_file(&shown)
        {
            continue;
        }
        let Some(name) = f.symbol_name.as_deref().map(base_symbol) else {
            continue;
        };
        let mut literals: Vec<String> = f
            .content
            .lines()
            .filter(|line| !commented(line))
            .flat_map(|line| {
                STRING
                    .captures_iter(line)
                    .filter_map(|c| c.get(1).or_else(|| c.get(2)))
                    .map(|m| m.as_str().to_string())
                    .collect::<Vec<_>>()
            })
            .filter(|literal| distinctive(literal))
            .collect();
        literals.sort();
        literals.dedup();
        if !literals.is_empty() {
            held.push((f.path().to_string(), shown, name.to_string(), literals));
        }
    }
    let mut needles: Vec<String> = held
        .iter()
        .flat_map(|(_, _, _, literals)| literals.iter().cloned())
        .collect();
    needles.sort();
    needles.dedup();
    needles.truncate(MAX_LITERAL_NEEDLES);
    if needles.is_empty() {
        return Vec::new();
    }
    let head = diff_range
        .map(crate::snapshot::split_diff_range)
        .and_then(|(_, head)| head);
    let grepped = read_or_note(
        state,
        crate::grep::grep_lines(
            &state.root_dir,
            &state.run.source(),
            &needles,
            head.as_deref(),
        ),
    );
    let mut files_of: FxHashMap<&str, FxHashSet<&str>> = FxHashMap::default();
    for (path, _, text) in &grepped.rows {
        for needle in &needles {
            if text.contains(needle.as_str()) {
                files_of
                    .entry(needle.as_str())
                    .or_default()
                    .insert(path.as_str());
            }
        }
    }
    held.into_iter()
        .filter_map(|(file, shown, symbol, literals)| {
            let shown = shown.as_str();
            let tests = literals
                .iter()
                .filter(|literal| {
                    files_of
                        .get(literal.as_str())
                        .is_some_and(|files| files.len() <= MAX_LITERAL_FILES)
                })
                .flat_map(|literal| {
                    grepped
                        .rows
                        .iter()
                        .filter(move |(path, _, text)| {
                            path != shown
                                && crate::testfiles::is_test_path(Path::new(path))
                                && !commented(text)
                                && text.contains(literal.as_str())
                        })
                        .map(|(path, line, _)| (path.clone(), *line))
                })
                .collect::<Vec<_>>();
            let tests = ordered(tests);
            (!tests.is_empty()).then(|| Run {
                file,
                symbol: Some(symbol),
                tests,
            })
        })
        .collect()
}
