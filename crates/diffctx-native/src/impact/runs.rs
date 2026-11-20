//! What a test runs that no use of its own reaches, one source per way a runner
//! or a framework enters code: the built binary of a crate, the console script a
//! manifest declares, a script spelled by its path on a command line, the key of
//! a framework's entry (a route, a command, a tool, a job), a string a changed
//! definition holds and a test asserts, a doctest. Each source reads the tests
//! a runner runs ([`DeclaredTests::runs`]) and yields [`Run`]s; a new way in is
//! a new source here, never a second walk.

use std::path::Path;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::facts::EntryKind;
use crate::facts::framework::jvm;
use crate::facts::{Fact, ImportKind, Scope};
use diffctx_base::testfiles::{Bound, DeclaredTests};
use diffctx_snapshot::snapshot::workspace::dir_of;

use super::mentions::{Naming, basename_of, commented, naming, path_search, starts_a_program};
use super::rust;
use super::{
    Cells, Fragment, ScoredState, base_symbol, changed_rels, names_word, read_or_note, rel,
    spelled_lines,
};

/// How a test runs code that no use of its own reaches: `file` as the fragments
/// spell it, `symbol` the one function of the file that runs (none: the whole
/// file runs), `tests` the lines of the tests that run it, as (file, line). No
/// `tests`: a test may run it, and nothing says which (a string it asserts is
/// spelled in too many files to name the definition).
pub(super) struct Run {
    pub(super) file: String,
    pub(super) symbol: Option<String>,
    pub(super) tests: Vec<(String, u32)>,
}

impl Run {
    /// Whether the run enters `f`: its file, and its function when it names one.
    pub(super) fn enters(&self, f: &Fragment) -> bool {
        self.file == f.path()
            && self
                .symbol
                .as_deref()
                .is_none_or(|symbol| f.symbol_name.as_deref().map(base_symbol) == Some(symbol))
    }
}

/// Every run the tests in view make. `spelled` maps the path the answer prints
/// to the path the fragments spell.
pub(super) fn all(
    state: &ScoredState,
    cells: &Cells,
    diff_range: Option<&str>,
    spelled: &FxHashMap<&str, &str>,
    declared: &DeclaredTests,
) -> Vec<Run> {
    let mut runs = binary_runs(state, diff_range);
    runs.extend(swift_product_runs(state, diff_range, declared));
    runs.extend(console_runs(state, diff_range, spelled, declared));
    runs.extend(script_runs(state, diff_range, spelled, declared));
    runs.extend(framework_runs(state, cells, diff_range, spelled, declared));
    runs
}

/// The distinct (file, line) sites, in path then line order.
fn ordered(mut sites: Vec<(String, u32)>) -> Vec<(String, u32)> {
    sites.sort();
    sites.dedup();
    sites
}

/// A doctest runs, in the file it documents, each function of that file its
/// examples call (`python -m doctest` and pytest's `--doctest-modules` read the
/// `>>>` examples, `cargo test --doc` the fenced ones): the test is the example's
/// line. A doctest is a reach of the function it calls and no more: the tests that
/// reach the function by their own calls are searched for as well ([`all`] holds the
/// runs that settle it).
pub(super) fn doctest_runs(state: &ScoredState) -> Vec<Run> {
    let defined: FxHashSet<(&str, &str)> = state
        .all_fragments
        .iter()
        .filter(|f| f.kind.is_definition_kind())
        .filter_map(|f| f.symbol_name.as_deref().map(|n| (f.path(), base_symbol(n))))
        .collect();
    let mut runs = Vec::new();
    let mut rust_read: FxHashSet<&str> = FxHashSet::default();
    let mut python_read: FxHashSet<&str> = FxHashSet::default();
    for f in state.run.scan(&state.all_fragments) {
        let path = f.path();
        if diffctx_base::testfiles::is_test_path(Path::new(path)) {
            continue;
        }
        let shown = rel(state, path);
        if rust::is_rust(path) {
            if !rust_read.insert(path) {
                continue;
            }
            if let Some(text) = state.run.source().read_to_string(Path::new(path)) {
                let calls = diffctx_base::testfiles::rust_doc_example_calls(&text);
                let lines = calls
                    .into_iter()
                    .map(|(row, names)| (row as u32 + 1, names));
                doctest_runs_of(&mut runs, path, &shown, lines, &defined);
            }
        } else if Path::new(path)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("py"))
            && f.content.contains(">>>")
        {
            let calls = diffctx_base::testfiles::doctest_calls(&f.content);
            let lines = calls
                .into_iter()
                .map(|(row, names)| (f.start_line() + row as u32, names));
            doctest_runs_of(&mut runs, path, &shown, lines, &defined);
            if python_read.insert(path) {
                if let Some(text) = state.run.source().read_to_string(Path::new(path)) {
                    let namespace = module_namespace(&shown, &text);
                    imported_doctest_runs(&mut runs, state, &shown, &text, namespace);
                }
            }
        }
    }
    let documents = read_or_note(state, state.run.source().list_paths(&state.root_dir));
    for document in state.run.bounded(&documents).filter(|d| is_document(d)) {
        if let Some(text) = state
            .run
            .source()
            .read_to_string(&state.root_dir.join(document))
            .filter(|text| text.contains(">>>"))
        {
            imported_doctest_runs(&mut runs, state, document, &text, Vec::new());
        }
    }
    runs
}

/// Prose a doctest runner reads (`--doctest-glob`) or a reader runs by hand.
fn is_document(path: &str) -> bool {
    Path::new(path).extension().is_some_and(|ext| {
        ["rst", "md", "markdown", "txt"]
            .iter()
            .any(|document| ext.eq_ignore_ascii_case(document))
    })
}

/// What the module-level imports of a Python file bind: the namespace its doctests
/// run in. A relative import is the package of `shown` moved up `relative - 1`
/// levels, as the resolver reads it; a star import binds no name.
fn module_namespace(shown: &str, text: &str) -> Vec<Bound> {
    let Some(blob) = crate::facts::local_facts(shown, text) else {
        return Vec::new();
    };
    let mut bound = Vec::new();
    for fact in &blob.facts {
        let Fact::Import(import) = fact else { continue };
        if import.enclosing != Scope::Module
            || import.kind != ImportKind::Static
            || import.star.is_star()
        {
            continue;
        }
        let module = if import.relative == 0 {
            import.module.as_str().to_string()
        } else {
            let mut package = dir_of(shown);
            for _ in 1..import.relative {
                package = dir_of(package);
            }
            let package = package.replace('/', ".");
            [package.as_str(), import.module.as_str()]
                .iter()
                .filter(|part| !part.is_empty())
                .copied()
                .collect::<Vec<_>>()
                .join(".")
        };
        if module.is_empty() {
            continue;
        }
        if import.names.is_empty() {
            let (local, module) = match &import.default {
                Some(alias) => (alias.as_str(), module.as_str()),
                None => {
                    let head = module.split('.').next().unwrap_or(&module);
                    (head, head)
                }
            };
            bound.push(Bound {
                local: local.to_string(),
                module: module.to_string(),
                name: None,
            });
        }
        for (name, alias) in &import.names {
            bound.push(Bound {
                local: alias.as_ref().unwrap_or(name).to_string(),
                module: module.clone(),
                name: Some(name.to_string()),
            });
        }
    }
    bound
}

/// The run each `>>>` example of `shown` makes of a function another file defines,
/// the example's own imports saying which file.
fn imported_doctest_runs(
    runs: &mut Vec<Run>,
    state: &ScoredState,
    shown: &str,
    text: &str,
    namespace: Vec<Bound>,
) {
    for (row, module, name) in diffctx_base::testfiles::imported_doctest_calls(text, namespace) {
        let module_path = module.replace('.', "/");
        let places = [
            format!("{module_path}.py"),
            format!("{module_path}/__init__.py"),
        ];
        for f in state.all_fragments.iter().filter(|f| {
            f.kind.is_definition_kind()
                && f.symbol_name.as_deref().map(base_symbol) == Some(name.as_str())
        }) {
            let defined_in = rel(state, f.path());
            if defined_in != shown
                && places
                    .iter()
                    .any(|place| defined_in == *place || defined_in.ends_with(&format!("/{place}")))
            {
                runs.push(Run {
                    file: f.path().to_string(),
                    symbol: Some(name.clone()),
                    tests: vec![(shown.to_string(), row as u32 + 1)],
                });
            }
        }
    }
}

/// The run each called name of `file` that the file defines makes of it.
fn doctest_runs_of<'t>(
    runs: &mut Vec<Run>,
    file: &str,
    shown: &str,
    calls: impl Iterator<Item = (u32, Vec<&'t str>)>,
    defined: &FxHashSet<(&str, &str)>,
) {
    for (line, names) in calls {
        for name in names.into_iter().filter(|n| defined.contains(&(file, *n))) {
            runs.push(Run {
                file: file.to_string(),
                symbol: Some(name.to_string()),
                tests: vec![(shown.to_string(), line)],
            });
        }
    }
}

/// The curried registration calls of `f`, one per statement: the call and the
/// name of the definition of its own file it registers (`tool("name")(fn)`).
pub(super) fn curried_registrations(f: &Fragment) -> impl Iterator<Item = (&str, &str)> {
    static CURRIED: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
        regex::Regex::new(r"^\s*([A-Za-z_][\w.]*\([^()]*\))\(\s*([A-Za-z_][\w.]*)\s*\)\s*$")
            .expect("curried registration regex")
    });
    let python = f.path().ends_with(".py") || f.path().ends_with(".pyi");
    f.content
        .lines()
        .take_while(move |_| python)
        .filter_map(|l| {
            let caps = CURRIED.captures(l)?;
            Some((caps.get(1)?.as_str(), caps.get(2)?.as_str()))
        })
}

/// The entries of name-keyed dicts in `f` (`{"log_vital": domain.log_vital}`), a
/// registry a dispatcher reads by key: each key, the name its value spells and
/// the line the value sits on.
pub(super) fn dict_registrations(f: &Fragment) -> impl Iterator<Item = (&str, &str, u32)> {
    static ENTRY: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
        regex::Regex::new(
            r#"["']([\w.:/-]{3,})["']\s*:\s*([A-Za-z_]\w*(?:\.[A-Za-z_]\w*)*)\s*[,}]"#,
        )
        .expect("dict registration regex")
    });
    let python = f.path().ends_with(".py");
    ENTRY
        .captures_iter(if python { &f.content } else { "" })
        .filter_map(move |c| {
            let value = c.get(2)?;
            let line = f.start_line() + f.content[..value.start()].matches('\n').count() as u32;
            Some((c.get(1)?.as_str(), value.as_str(), line))
        })
}

/// The dict entries of `f` that register `name`, the definition `target`: a
/// value that names it through its module (`domain.log_vital` for
/// `srv/domain.py`) or by name in its own file, on a line where the resolver
/// landed a use of it. A parameter or local of the same name
/// (`def payload(user): return {"user": user}`) is no use of it and registers
/// nothing.
pub(super) fn dict_registrations_of<'f>(
    state: &ScoredState,
    cells: &Cells,
    f: &'f Fragment,
    target: &Fragment,
    name: &str,
) -> Vec<(&'f str, &'f str, u32)> {
    let stem = Path::new(target.path())
        .file_stem()
        .and_then(|s| s.to_str());
    let named: Vec<(&str, &str, u32)> = dict_registrations(f)
        .filter(|(_, value, _)| match value.rsplit_once('.') {
            Some((module, last)) => last == name && module.rsplit('.').next() == stem,
            None => *value == name && f.path() == target.path(),
        })
        .collect();
    if named.is_empty() {
        return named;
    }
    let (shown, user) = (rel(state, target.path()), rel(state, f.path()));
    let landed: Vec<u32> = cells.of(&shown).map_or_else(Vec::new, |cell| {
        cell.users(&shown, name, (target.start_line(), target.end_line()))
            .into_iter()
            .filter(|(file, _)| *file == user)
            .map(|(_, line)| line)
            .collect()
    });
    named
        .into_iter()
        .filter(|(_, _, line)| landed.contains(line))
        .collect()
}

/// The Rust binaries in the analysed fragments, keyed by their entry file
/// as fragments spell it, each run by one test of its crate that runs
/// the built binary. `src/main.rs` is the package's binary, named in
/// `Cargo.toml`; `src/bin/<name>.rs` is `<name>`.
fn binary_runs(state: &ScoredState, diff_range: Option<&str>) -> Vec<Run> {
    let mut entries: Vec<(String, String, String)> = Vec::new();
    let mut seen: FxHashSet<&str> = FxHashSet::default();
    for f in state.run.scan(&state.all_fragments) {
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
    let needles = vec!["CARGO_BIN_EXE".to_string(), "cargo_bin".to_string()];
    let grepped = spelled_lines(state, diff_range, &needles);
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

/// The SwiftPM executables in the analysed fragments: an `.executableTarget(name:
/// "<name>")` of a `Package.swift` builds the product `<name>` from the
/// `main.swift` of `Sources/<name>` (or of the target's `path:`), whose top-level
/// code is what runs. A test runs it by the built product's path
/// (`.build/release/<name>`) or by `swift run <name>`.
fn swift_product_runs(
    state: &ScoredState,
    diff_range: Option<&str>,
    declared: &DeclaredTests,
) -> Vec<Run> {
    static TARGET: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
        regex::Regex::new(
            r#"\.executableTarget\(\s*name:\s*"([^"]+)"(?:[^()]*?\bpath:\s*"([^"]+)")?"#,
        )
        .expect("executable target regex")
    });
    let mut manifests: FxHashMap<String, Vec<(String, String)>> = FxHashMap::default();
    let mut entries: Vec<(String, String)> = Vec::new();
    let mut seen: FxHashSet<&str> = FxHashSet::default();
    for f in state.run.scan(&state.all_fragments) {
        let own = f.path();
        if !seen.insert(own) || !own.ends_with("main.swift") {
            continue;
        }
        let path = rel(state, own);
        let dirs = path.match_indices('/').map(|(at, _)| &path[..=at]);
        for dir in std::iter::once("").chain(dirs) {
            let targets = manifests.entry(dir.to_string()).or_insert_with(|| {
                let manifest = state
                    .run
                    .source()
                    .read_to_string(&state.root_dir.join(format!("{dir}Package.swift")));
                manifest.map_or_else(Vec::new, |text| {
                    TARGET
                        .captures_iter(&text)
                        .map(|c| {
                            let name = c[1].to_string();
                            let source = c.get(2).map_or_else(
                                || format!("Sources/{name}"),
                                |p| p.as_str().trim_matches('/').to_string(),
                            );
                            (name, format!("{dir}{source}/main.swift"))
                        })
                        .collect()
                })
            });
            if let Some((name, _)) = targets.iter().find(|(_, main)| *main == path) {
                entries.push((own.to_string(), name.clone()));
            }
        }
    }
    if entries.is_empty() {
        return Vec::new();
    }
    let needles = vec![".build/".to_string(), "swift run".to_string()];
    let grepped = spelled_lines(state, diff_range, &needles);
    entries
        .into_iter()
        .filter_map(|(own, name)| {
            let tests = ordered(
                grepped
                    .rows
                    .iter()
                    .filter(|(path, _, text)| {
                        declared.runs(path) && !commented(text) && runs_product(text, &name)
                    })
                    .map(|(path, line, _)| (path.clone(), *line))
                    .collect(),
            );
            (!tests.is_empty()).then_some(Run {
                file: own,
                symbol: None,
                tests,
            })
        })
        .collect()
}

/// A command line that starts the SwiftPM product `name`: its built path, or
/// `swift run name`.
fn runs_product(text: &str, name: &str) -> bool {
    let words: Vec<&str> = text
        .split(|c: char| {
            c.is_whitespace() || matches!(c, '"' | '\'' | '`' | '(' | ')' | ',' | '[' | ']' | '=')
        })
        .filter(|w| !w.is_empty())
        .collect();
    words
        .iter()
        .any(|w| w.contains(".build/") && w.rsplit('/').next() == Some(name))
        || words
            .windows(3)
            .any(|w| w[0] == "swift" && w[1] == "run" && w[2] == name)
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
        let Some(toml::Value::Table(entries)) =
            diffctx_snapshot::snapshot::workspace::toml_at(&table, at)
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
pub(super) fn spelled_as_command(text: &str, name: &str) -> bool {
    ['"', '\'']
        .into_iter()
        .any(|q| text.contains(&format!("{q}{name}{q}")) || text.contains(&format!("{q}{name} ")))
}

/// The tests that run a console script: a test file with a line that starts or
/// locates a program and spells the script's name as a command. The function the manifest
/// names (a Python entry), or the file (a Node bin), is what they run.
fn console_runs(
    state: &ScoredState,
    diff_range: Option<&str>,
    spelled: &FxHashMap<&str, &str>,
    declared: &DeclaredTests,
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
    let grepped = spelled_lines(state, diff_range, &needles);
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
                    declared.runs(path)
                        && *path != script.file
                        && !commented(text)
                        && (starts_a_program(text) || locates_a_program(text))
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

/// A line that finds an installed console script to run it: beside the
/// interpreter (`Path(sys.executable).with_name("pkg")`) or on the PATH
/// (`shutil.which("pkg")`). `sys.executable, "-m", "pkg"` runs the package's
/// `__main__`, not the script.
fn locates_a_program(text: &str) -> bool {
    text.contains("which(")
        || (text.contains("sys.executable") && !text.contains("\"-m\"") && !text.contains("'-m'"))
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
    declared: &DeclaredTests,
) -> Vec<Run> {
    let targets: Vec<String> = changed_rels(state)
        .filter(|t| is_runnable(t) && !declared.runs(t))
        .filter(|t| spelled.contains_key(t.as_str()))
        .collect();
    if targets.is_empty() {
        return Vec::new();
    }
    let (root, basenames, needles) = path_search(state, &targets);
    let grepped = spelled_lines(state, diff_range, &needles);
    let token_re = once_cell::sync::Lazy::force(&super::mentions::PATH_TOKEN);
    let mut out = Vec::new();
    for target in &targets {
        let basename = basename_of(target);
        let tests = grepped
            .rows
            .iter()
            .filter(|(path, _, text)| {
                declared.runs(path)
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
/// the path's parameters open), the name of a command a program starts, or a
/// quoted name on a line that speaks of what registers it (a tool, a scheduled
/// job, whatever a registration call is named).
enum Spelling {
    Route(regex::Regex, Option<String>),
    Command(String),
    Keyed(String, Vec<String>),
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
            EntryKind::Tool => Some(Self::keyed(label, &["tool"])),
            EntryKind::Scheduled => Some(Self::keyed(label, &["job", "schedul", "cron"])),
            EntryKind::Migration => Some(Self::keyed(label, &["upgrade", "downgrade"])),
            _ => None,
        }
    }

    fn spelled_by(&self, text: &str) -> bool {
        match self {
            Self::Route(path, verb) => {
                path.is_match(text)
                    && verb.as_deref().is_none_or(|verb| {
                        text.split(|c: char| !c.is_alphanumeric())
                            .any(|word| names_verb(word, verb))
                    })
            }
            Self::Command(name) => starts_a_program(text) && spelled_as_command(text, name),
            Self::Keyed(key, words) => {
                let line = text.to_lowercase();
                spelled_as_command(text, key) && words.iter().any(|word| line.contains(word))
            }
        }
    }

    fn keyed(key: &str, words: &[&str]) -> Self {
        Self::Keyed(
            key.to_string(),
            words.iter().map(ToString::to_string).collect(),
        )
    }
}

/// A word of a request line that is the verb, or a template client's call that
/// carries it (`getForObject`, `postForEntity`).
fn names_verb(word: &str, verb: &str) -> bool {
    word.eq_ignore_ascii_case(verb)
        || word
            .get(..verb.len())
            .zip(word.get(verb.len()..))
            .is_some_and(|(head, rest)| head.eq_ignore_ascii_case(verb) && rest.starts_with("For"))
}

/// A route's path with the constants of other files it names put in, none when
/// one cannot be read: the declaring file is no fragment of the run, or two
/// of its fragments disagree. Any fragment of the file is read, since a
/// constant can sit inside its class's fragment.
fn settled(state: &ScoredState, label: &str) -> Option<String> {
    jvm::resolve_constants(label, |owner, name| {
        let file = owner.replace('.', "/");
        let mut found: Vec<String> = state
            .all_fragments
            .iter()
            .filter(|f| {
                let path = rel(state, f.path());
                path.strip_suffix(".java")
                    .or_else(|| path.strip_suffix(".kt"))
                    .is_some_and(|stem| stem == file || stem.ends_with(&format!("/{file}")))
            })
            .filter_map(|f| jvm::constant_literal(&f.content, name))
            .collect();
        found.sort();
        found.dedup();
        (found.len() == 1).then(|| found.remove(0))
    })
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
/// analysed definitions, whatever the diff touched, since the walk up from a
/// change enters them: a test file with a line that spells the entry's key as a
/// caller of it does. The framework's name is nobody's concern
/// here (`facts::framework`); only `kind`, `verb` and `label` are read.
fn framework_runs(
    state: &ScoredState,
    cells: &Cells,
    diff_range: Option<&str>,
    spelled: &FxHashMap<&str, &str>,
    declared: &DeclaredTests,
) -> Vec<Run> {
    struct Entry {
        file: String,
        symbol: String,
        spelling: Spelling,
        needle: String,
    }
    let mut entries: Vec<Entry> = Vec::new();
    let mut seen: FxHashSet<(String, String, EntryKind, String)> = FxHashSet::default();
    let mut taken = 0u64;
    for f in state.run.scan(&state.all_fragments) {
        taken += 1;
        let shown = rel(state, f.path());
        if !f.kind.is_definition_kind() {
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
                framework
                    .label
                    .clone()
                    .filter(|l| !l.is_empty())
                    .and_then(|label| settled(state, &label))
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
    diffctx_base::profile::count("impact.tests.fragments", taken);
    // A registration call no facts rule knows (`tool("name")(fn)`) is keyed by its
    // first string and spelled beside the callee's name.
    for f in state.run.scan(&state.all_fragments) {
        for (call, symbol) in curried_registrations(f) {
            let Some((callee, arguments)) = call.split_once('(') else {
                continue;
            };
            let Some(key) = arguments.split(['"', '\'']).nth(1) else {
                continue;
            };
            let word = callee.rsplit('.').next().unwrap_or(callee).to_lowercase();
            if key.len() >= 3
                && seen.insert((
                    f.path().to_string(),
                    symbol.to_string(),
                    EntryKind::Tool,
                    key.to_string(),
                ))
            {
                entries.push(Entry {
                    file: f.path().to_string(),
                    symbol: symbol.to_string(),
                    spelling: Spelling::keyed(key, &[&word]),
                    needle: key.to_string(),
                });
            }
        }
    }
    // A handler a name-keyed dict registers is spelled by its key beside a
    // function the registry's file defines (`call_tool("log_vital", …)`).
    for f in state.run.scan(&state.all_fragments) {
        for (key, value, _) in dict_registrations(f) {
            let name = value.rsplit('.').next().unwrap_or(value);
            let Some(target) = state.all_fragments.iter().find(|t| {
                t.kind.is_definition_kind()
                    && t.symbol_name.as_deref().map(base_symbol) == Some(name)
                    && dict_registrations_of(state, cells, f, t, name)
                        .iter()
                        .any(|(registered, _, _)| *registered == key)
            }) else {
                continue;
            };
            let source = state.run.source().read_to_string(Path::new(f.path()));
            let words: Vec<String> = source
                .iter()
                .flat_map(|text| text.lines())
                .filter_map(|l| {
                    l.trim_start()
                        .trim_start_matches("async ")
                        .strip_prefix("def ")
                })
                .filter_map(|l| l.split('(').next())
                .map(|n| n.trim().to_lowercase())
                .filter(|n| n.len() >= 3)
                .collect();
            if seen.insert((
                target.path().to_string(),
                name.to_string(),
                EntryKind::Tool,
                key.to_string(),
            )) {
                entries.push(Entry {
                    file: target.path().to_string(),
                    symbol: name.to_string(),
                    spelling: Spelling::Keyed(key.to_string(), words),
                    needle: key.to_string(),
                });
            }
        }
    }
    // A browser test that visits a route of a client-side router's table runs the
    // definition that holds the route and the component the route renders.
    let mut read: FxHashSet<&str> = FxHashSet::default();
    for f in state.run.scan(&state.all_fragments) {
        let script = matches!(
            diffctx_base::languages::get_language_for_file(f.path()),
            Some("javascript" | "typescript" | "tsx" | "jsx")
        );
        if !(script && f.content.contains("path") && read.insert(f.path())) {
            continue;
        }
        let Some(text) = state.run.source().read_to_string(Path::new(f.path())) else {
            continue;
        };
        let shown = rel(state, f.path());
        for (line, label, component) in router_routes(&text) {
            let needle = route_needle(&label);
            if needle.trim_matches('/').len() < 3 {
                continue;
            }
            let holder = state
                .all_fragments
                .iter()
                .filter(|h| h.path() == f.path() && h.start_line() <= line && h.end_line() >= line)
                .filter_map(|h| Some((h.line_count(), h.symbol_name.as_deref().map(base_symbol)?)))
                .min()
                .map(|(_, name)| (f.path().to_string(), name.to_string()));
            let entered = rendered(state, &shown, &text, &component, spelled);
            for (file, symbol) in holder.into_iter().chain(entered) {
                let Some(spelling) = Spelling::of(EntryKind::Route, None, &label) else {
                    continue;
                };
                if seen.insert((
                    file.clone(),
                    symbol.clone(),
                    EntryKind::Route,
                    label.clone(),
                )) {
                    entries.push(Entry {
                        file,
                        symbol,
                        spelling,
                        needle: needle.clone(),
                    });
                }
            }
        }
    }
    if entries.is_empty() {
        return Vec::new();
    }
    let mut needles: Vec<String> = entries.iter().map(|e| e.needle.clone()).collect();
    needles.sort();
    needles.dedup();
    let grepped = spelled_lines(state, diff_range, &needles);
    entries
        .into_iter()
        .filter_map(|entry| {
            let tests = ordered(
                grepped
                    .rows
                    .iter()
                    .filter(|(path, _, text)| {
                        declared.runs(path) && !commented(text) && entry.spelling.spelled_by(text)
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

/// The routes of a React Router table in `text` (a file that imports
/// `react-router`), each its line, its path and the component it renders: a `<Route
/// path=".." element={<Page />}>` element or a `{ path: "..", element: <Page /> }`
/// object (`Component` alike). A route is the text from one `<Route` or `path:`
/// to the next.
fn router_routes(text: &str) -> Vec<(u32, String, String)> {
    static START: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
        regex::Regex::new(r"<Route\b|\bpath\s*:").expect("route start regex")
    });
    static PATH: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
        regex::Regex::new(r#"\bpath\s*[=:]\s*\{?\s*["'`]([^"'`]+)["'`]"#).expect("route path regex")
    });
    static RENDERS: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
        regex::Regex::new(r"\b(?:element\s*[=:]\s*\{?\s*<\s*|Component\s*[=:]\s*\{?\s*)([A-Z]\w*)")
            .expect("route component regex")
    });
    if !text.contains("react-router") {
        return Vec::new();
    }
    let starts: Vec<usize> = START.find_iter(text).map(|m| m.start()).collect();
    starts
        .iter()
        .enumerate()
        .filter_map(|(i, &start)| {
            let route = &text[start..starts.get(i + 1).copied().unwrap_or(text.len())];
            let path = PATH.captures(route)?.get(1)?.as_str().to_string();
            let component = RENDERS.captures(route)?.get(1)?.as_str().to_string();
            let line = u32::try_from(text[..start].matches('\n').count() + 1).unwrap_or(u32::MAX);
            Some((line, path, component))
        })
        .collect()
}

/// The definition a route renders as `component` names it in the router's file
/// (`shown` as the answer prints it): the one a relative module exports under
/// it — by name (`import { Page }`), or as its default (`import Page from`,
/// `lazy(() => import(…))`) — with its file as fragments spell it.
fn rendered(
    state: &ScoredState,
    shown: &str,
    text: &str,
    component: &str,
    spelled: &FxHashMap<&str, &str>,
) -> Vec<(String, String)> {
    static DEFAULT: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
        regex::Regex::new(
            r#"import\s+(\w+)\s*(?:,\s*\{[^}]*\})?\s*from\s*["']([^"']+)["']|\b(\w+)\s*=\s*(?:React\.)?lazy\(\s*(?:async\s*)?\(\s*\)\s*=>\s*import\(\s*["']([^"']+)["']"#,
        )
        .expect("default import regex")
    });
    static NAMED: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
        regex::Regex::new(r#"import\s*(?:\w+\s*,\s*)?\{([^}]*)\}\s*from\s*["']([^"']+)["']"#)
            .expect("named import regex")
    });
    static EXPORTED: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
        regex::Regex::new(r"export\s+default\s+(?:async\s+)?(?:function\s*\*?\s*|class\s+)?(\w+)")
            .expect("default export regex")
    });
    let mut found: Vec<(String, String)> = Vec::new();
    let module = |spec: &str| {
        let dir = shown.rsplit_once('/').map_or("", |(dir, _)| dir);
        let base = spec
            .starts_with('.')
            .then(|| relative(dir, spec))
            .flatten()?;
        [
            "",
            ".tsx",
            ".ts",
            ".jsx",
            ".js",
            "/index.tsx",
            "/index.ts",
            "/index.jsx",
            "/index.js",
        ]
        .iter()
        .find_map(|tail| spelled.get(format!("{base}{tail}").as_str()).copied())
    };
    let defaults = DEFAULT.captures_iter(text).filter_map(|c| {
        let (local, spec) = c.get(1).zip(c.get(2)).or_else(|| c.get(3).zip(c.get(4)))?;
        (local.as_str() == component).then_some(spec.as_str())
    });
    for spec in defaults {
        let Some(target) = module(spec) else {
            continue;
        };
        let source = state.run.source().read_to_string(Path::new(target));
        if let Some(name) = source.as_deref().and_then(|s| EXPORTED.captures(s)) {
            found.push((target.to_string(), name[1].to_string()));
        }
    }
    for c in NAMED.captures_iter(text) {
        let exported = c[1].split(',').find_map(|binding| {
            let mut words = binding.split_whitespace();
            let name = words.next()?;
            let local = match (words.next(), words.next()) {
                (Some("as"), Some(local)) => local,
                _ => name,
            };
            (local == component).then(|| name.to_string())
        });
        if let Some((target, name)) = module(&c[2]).zip(exported) {
            found.push((target.to_string(), name));
        }
    }
    found
}

/// `spec` (`./a`, `../b/c`) from the directory `dir`, none when it climbs above the
/// repository.
fn relative(dir: &str, spec: &str) -> Option<String> {
    let mut parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
    for part in spec.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

/// The most strings of changed definitions one answer looks for in the tests;
/// past it the run says it was limited.
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
pub(super) fn literal_runs(
    state: &ScoredState,
    diff_range: Option<&str>,
    declared: &DeclaredTests,
) -> Vec<Run> {
    static STRING: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
        regex::Regex::new(r#""([^"\\\n]{8,80})"|'([^'\\\n]{8,80})'"#).expect("string regex")
    });
    let changed: FxHashSet<String> = changed_rels(state).collect();
    // (the file as fragments spell it, as the answer prints it, the symbol, its strings)
    let mut held: Vec<(String, String, String, Vec<String>)> = Vec::new();
    for f in state.run.scan(&state.all_fragments) {
        let shown = rel(state, f.path());
        if !changed.contains(&shown)
            || !f.kind.is_definition_kind()
            || declared.runs(&shown)
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
    if needles.len() > MAX_LITERAL_NEEDLES {
        state
            .run
            .note(diffctx_snapshot::resource::LimitReason::CandidateLimit);
        needles.truncate(MAX_LITERAL_NEEDLES);
    }
    if needles.is_empty() {
        return Vec::new();
    }
    let grepped = spelled_lines(state, diff_range, &needles);
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
            let spells = |literal: &str, path: &str, text: &str| {
                path != shown && declared.runs(path) && !commented(text) && text.contains(literal)
            };
            let (clear, shared): (Vec<&String>, Vec<&String>) = literals.iter().partition(|l| {
                files_of
                    .get(l.as_str())
                    .is_some_and(|files| files.len() <= MAX_LITERAL_FILES)
            });
            let tests = ordered(
                clear
                    .into_iter()
                    .flat_map(|literal| {
                        grepped
                            .rows
                            .iter()
                            .filter(move |(path, _, text)| spells(literal, path, text))
                            .map(|(path, line, _)| (path.clone(), *line))
                    })
                    .collect(),
            );
            // A test that spells a string too many files spell may assert this
            // definition's: no run is made of it, and none is ruled out.
            (!tests.is_empty()
                || shared
                    .into_iter()
                    .any(|l| grepped.rows.iter().any(|(p, _, t)| spells(l, p, t))))
            .then(|| Run {
                file,
                symbol: Some(symbol),
                tests,
            })
        })
        .collect()
}
