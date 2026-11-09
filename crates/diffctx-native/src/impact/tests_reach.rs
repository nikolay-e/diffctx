//! Which tests reach a definition.
//!
//! A test reaches code in two ways. It uses it: the test, or a function it calls
//! (two hops up the uses, [`MAX_TEST_HOPS`]), holds a resolved use of the
//! definition, which a cell of the definition's language answers
//! ([`Cell::users`](crate::decide::Cell::users)); and it runs it: the built binary
//! of a crate, the console script a manifest declares, a script spelled by its
//! path on a command line. Nothing calls those, so each is a [`Run`] the test
//! makes of one entry.
//!
//! The ranking graph answers for the files no cell reads, as `TestIndex` did for
//! all of them; it goes with the graph's last reader. A claim that a definition
//! has no test link is printed only where `any` says tests are in view.

use std::cell::RefCell;
use std::path::Path;
use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::facts::EntryKind;

use super::mentions::{Naming, commented, naming, starts_a_program};
use super::{
    Cells, EdgeCategory, Fragment, FragmentId, FragmentKind, Link, MAX_TEST_HOPS, ScoredState,
    base_symbol, cell_caller, code_text, lang, lifted_caller, names_module, names_word,
    read_or_note, rel, same_runtime,
};

/// How a test runs code that no use of its own reaches: `file` as the fragments
/// spell it, `symbol` the one function of the file that runs (none: the whole
/// file runs), `test` the file of the test that runs it.
struct Run {
    file: String,
    symbol: Option<String>,
    test: String,
}

/// What a use of a definition leads to one hop up: the fragments that hold it,
/// and a test file whose own code does with no fragment to name it.
#[derive(Default)]
struct Up<'a> {
    fragments: Vec<&'a Fragment>,
    test: Option<String>,
}

pub(super) struct TestsReach<'a> {
    cells: &'a Cells,
    /// The fragments of each file, by the path the answer prints.
    by_rel: FxHashMap<String, Vec<&'a Fragment>>,
    /// The path the answer prints for each path a fragment spells.
    rel_of: FxHashMap<&'a str, String>,
    /// Source path -> one test path linked to it by a test edge of the ranking
    /// graph: the channel of the files no cell reads.
    by_path: FxHashMap<String, String>,
    test_fragments: Vec<&'a Fragment>,
    /// Functions outside test files an attribute marks as tests: a Rust
    /// `#[test]` in an inline `mod tests` (#528).
    inline_tests: FxHashSet<&'a FragmentId>,
    /// Test files the size cap kept out, with their code.
    unread: Vec<(String, Arc<str>)>,
    /// Entries a test runs: binaries (`CARGO_BIN_EXE_<name>`, `cargo_bin("<name>")`:
    /// #391, #678), console scripts, scripts spelled by their path.
    runs: Vec<Run>,
    /// The repository holds a test file the walk never reached: tests the
    /// change does not touch and nothing links to are still tests in view
    /// (#123), so an absent link is a finding and not a blank.
    repo_tests: bool,
    /// `tested_by` per fragment: callers repeat across changed symbols, and
    /// each answer walks every test fragment.
    memo: RefCell<FxHashMap<FragmentId, Option<(String, Option<String>)>>>,
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
            if read_by_a_cell(source.path.as_ref()) {
                return;
            }
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
        let spelled: FxHashMap<&str, &str> = rel_of
            .iter()
            .map(|(path, shown)| (shown.as_str(), *path))
            .collect();
        let mut runs = binary_runs(state, diff_range);
        runs.extend(console_runs(state, diff_range, &spelled));
        runs.extend(script_runs(state, diff_range, &spelled));
        runs.extend(framework_runs(state, cells, diff_range));
        let repo_tests = test_fragments.is_empty()
            && by_path.is_empty()
            && runs.is_empty()
            && repo_holds_test_file(state);
        Self {
            cells,
            by_rel,
            rel_of,
            by_path,
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
            || !self.by_path.is_empty()
            || !self.runs.is_empty()
            || self.repo_tests
    }

    pub(super) fn unread_naming(&self, caller: &Fragment, own: &str) -> Option<String> {
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

    pub(super) fn is_test(&self, f: &Fragment) -> bool {
        crate::testfiles::is_test_path(Path::new(f.path())) || self.inline_tests.contains(&f.id)
    }

    /// The cell that reads the file of `f`, with the path the answer prints.
    fn cell_of(&self, f: &Fragment) -> Option<(&'a crate::decide::Cell, &str)> {
        let shown = self.rel_of.get(f.path())?;
        Some((self.cells.of(shown)?, shown.as_str()))
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
        // A chunk inside a long function is named `main[88]`; the test
        // names `main`.
        let name = caller.symbol_name.as_deref().map(base_symbol);
        if let Some(run) = self.runs.iter().find(|run| {
            run.file == caller.path()
                && run
                    .symbol
                    .as_deref()
                    .is_none_or(|symbol| name == Some(symbol))
        }) {
            return Some(run.test.clone());
        }
        let name = name.filter(|s| s.len() >= 3)?;
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
    pub(super) fn tested_by(
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

    /// One hop up the uses of `f`: a cell's resolved uses where a cell reads the
    /// file, the ranking graph's semantic edges, held to `link`, where none does.
    fn up(
        &self,
        f: &'a Fragment,
        state: &ScoredState,
        by_id: &FxHashMap<&FragmentId, &'a Fragment>,
        link: &Link,
    ) -> Up<'a> {
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
                        up.test = Some(file);
                    }
                    None => {}
                }
            }
            return up;
        }
        let graph = &state.scoring_result.graph;
        graph.for_each_reverse_neighbor(&f.id, |src, _| {
            if graph.edge_category(src, &f.id) != Some(EdgeCategory::Semantic)
                || !graph.is_forward(src, &f.id)
            {
                return;
            }
            let Some(user) = by_id.get(src).copied() else {
                return;
            };
            if link(user, f) {
                up.fragments.push(user);
            }
        });
        up
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
            for &f in &frontier {
                let up = self.up(f, state, by_id, link);
                if let Some(test) = up.test {
                    return Some((test, None));
                }
                next.extend(up.fragments.into_iter().filter(|u| seen.insert(&u.id)));
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
        self.by_path.contains_key(caller.path())
            || self.test_fragments.iter().any(|t| reaches_file(t, caller))
    }
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
        return Vec::new();
    }
    let head = diff_range
        .map(crate::git::split_diff_range)
        .and_then(|(_, head)| head);
    let needles = vec!["CARGO_BIN_EXE".to_string(), "cargo_bin".to_string()];
    let grepped = read_or_note(
        state,
        crate::git::grep_lines(
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
            out.push(Run {
                file: own,
                symbol: Some("main".to_string()),
                test,
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
            .map(|name| {
                vec![(
                    name.rsplit('/').next().unwrap_or(name).to_string(),
                    path.clone(),
                )]
            })
            .unwrap_or_default(),
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
        .map(crate::git::split_diff_range)
        .and_then(|(_, head)| head);
    let grepped = read_or_note(
        state,
        crate::git::grep_lines(
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
        let test = grepped
            .rows
            .iter()
            .filter(|(path, _, text)| {
                crate::testfiles::is_test_path(Path::new(path))
                    && *path != script.file
                    && !commented(text)
                    && starts_a_program(text)
                    && spelled_as_command(text, &script.name)
            })
            .map(|(path, _, _)| path.clone())
            .min();
        if let Some(test) = test {
            out.push(Run {
                file: (*file).to_string(),
                symbol: script.function,
                test,
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
        .map(crate::git::split_diff_range)
        .and_then(|(_, head)| head);
    let grepped = read_or_note(
        state,
        crate::git::grep_lines(
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
        let test = grepped
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
            .map(|(path, _, _)| path.clone())
            .min();
        if let (Some(test), Some(file)) = (test, spelled.get(target.as_str())) {
            out.push(Run {
                file: (*file).to_string(),
                symbol: None,
                test,
            });
        }
    }
    out
}

/// How a test spells the key of an entry a framework makes: the request of a
/// route (a quoted path that ends in the route's, on a line that names its verb,
/// the path's parameters open), the name of a command a program starts, the name
/// of a tool.
enum Spelling {
    Route(regex::Regex, Option<String>),
    Command(String),
    Tool(String),
}

impl Spelling {
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
            let Some(label) = framework.label.filter(|l| !l.is_empty()) else {
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
        .map(crate::git::split_diff_range)
        .and_then(|(_, head)| head);
    let grepped = read_or_note(
        state,
        crate::git::grep_lines(
            &state.root_dir,
            &state.run.source(),
            &needles,
            head.as_deref(),
        ),
    );
    entries
        .into_iter()
        .filter_map(|entry| {
            let test = grepped
                .rows
                .iter()
                .filter(|(path, _, text)| {
                    crate::testfiles::is_test_path(Path::new(path))
                        && !commented(text)
                        && entry.spelling.spelled_by(text)
                })
                .map(|(path, _, _)| path.clone())
                .min()?;
            Some(Run {
                file: entry.file,
                symbol: Some(entry.symbol),
                test,
            })
        })
        .collect()
}
