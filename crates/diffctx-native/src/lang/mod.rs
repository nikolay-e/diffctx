//! The language models impact resolves references through, behind one
//! interface (#506). The core asks for a definition and how a site relates
//! to it, and never names a model's own types: a definition is a handle the
//! model hands out and takes back. A language joins by implementing
//! `LanguageFacts` and registering in `Registry::for_analysis`.

mod bound;
mod go;
mod kotlin;
mod python;
pub(crate) mod rust;
mod shell;
mod sql;

use std::path::Path;
use std::sync::Arc;

use rustc_hash::FxHashMap;

use crate::bindings::{self, Finding, Lang};
use crate::facts::Declaration;
use crate::source::Source;
use crate::types::{Fragment, FragmentId, FragmentKind};

/// A file's declarations as `(name, lines, kind)`, read from its path and
/// text: a page's code is its inline scripts.
type Declarations = fn(&str, &str) -> Vec<(Option<String>, (u32, u32), FragmentKind)>;

fn declarations_of(path: &str) -> Option<Declarations> {
    match bindings::lang_of(path) {
        Some(Lang::Python) => Some(|_, text| python::assignments(text)),
        Some(Lang::Js) => Some(bound::declarations),
        None if sql::is_sql(path) => Some(|_, text| sql::declarations(text)),
        None => None,
    }
}

/// Definitions a model reads that the fragmenter does not cut: Python
/// module- and class-level assignments (#420, #488), JavaScript properties
/// of the global object or of the module's exports (#426), TypeScript
/// interface members and class fields (#488), SQL functions, procedures
/// and views (#459). Pack keeps its fragments as they are, sized for a
/// reader's budget, and an impact run lays these over them; a fragment
/// spanning exactly the definition becomes it, and keeps its kind when it
/// already is a definition's (an arrow assigned to `self.serveShell`).
pub fn overlay_definitions(fragments: &mut Vec<Fragment>, source: &Source) {
    let mut paths: Vec<Arc<str>> = fragments
        .iter()
        .filter(|f| declarations_of(f.path()).is_some())
        .map(|f| Arc::clone(&f.id.path))
        .collect();
    paths.sort();
    paths.dedup();
    let mut at: FxHashMap<FragmentId, usize> = fragments
        .iter()
        .enumerate()
        .map(|(i, f)| (f.id.clone(), i))
        .collect();
    for path in paths {
        let (Some(read), Some(text)) = (
            declarations_of(&path),
            source.read_to_string(Path::new(path.as_ref())),
        ) else {
            continue;
        };
        let lines: Vec<&str> = text.split('\n').collect();
        for (name, (start, end), kind) in read(&path, &text) {
            let id = FragmentId::new(Arc::clone(&path), start, end);
            if let Some(&i) = at.get(&id) {
                let same = &mut fragments[i];
                if same.symbol_name.is_none() && name.is_some() {
                    if !same.kind.is_definition_kind() {
                        same.kind = kind;
                    }
                    same.symbol_name = name;
                }
                continue;
            }
            let Some(content) = lines
                .get(start as usize - 1..end as usize)
                .map(|l| l.join("\n"))
            else {
                continue;
            };
            at.insert(id.clone(), fragments.len());
            fragments.push(Fragment {
                id,
                kind,
                identifiers: crate::types::extract_identifiers(
                    &content,
                    crate::config::tokenization::TOKENIZATION.fragment_min_identifier_length,
                ),
                content: Arc::from(content),
                token_count: 0,
                symbol_name: name,
            });
        }
    }
}

/// The names a file defines that a pattern over its lines does not see:
/// shell functions (`fetch() {`), SQL routines and views, a Python module's
/// assignments, a JavaScript module's definitions and the globals it sets
/// (`self.serveShell = …`). Impact looks for the files that spell them.
pub fn declared_names(path: &str, text: &str) -> Vec<String> {
    let reachable = |declared: &[Declaration]| {
        declared
            .iter()
            .filter(|d| {
                d.scope.is_none()
                    && d.class
                        .as_deref()
                        .is_none_or(|c| c == bindings::GLOBAL_OWNER)
            })
            .map(|d| d.name.clone())
            .collect()
    };
    match bindings::lang_of(path) {
        Some(Lang::Python) => {
            crate::facts::python::facts(text).map_or_else(Vec::new, |f| reachable(&f.declarations))
        }
        Some(Lang::Js) => {
            bindings::js_facts(path, text).map_or_else(Vec::new, |f| reachable(&f.declarations))
        }
        None if sql::is_sql(path) => sql::declarations(text)
            .into_iter()
            .filter_map(|(n, _, _)| n)
            .collect(),
        None => shell::function_names(path, text),
    }
}

/// Whether a `--symbol` qualifier names the owner a model states for a
/// definition: a SQL schema (`lib.refresh_book`). `None` when the model
/// states none.
pub fn stated_owner(path: &str, line: u32, owner: &str, source: &Source) -> Option<bool> {
    if !sql::is_sql(path) {
        return None;
    }
    let text = source.read_to_string(Path::new(path))?;
    Some(sql::schema_at(&text, line).as_deref() == Some(&owner.to_lowercase()))
}

/// What a model can answer about a definition and its uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Relation {
    /// Which sites reference or call the definition.
    Callers,
    /// Every line of a file where a use could reach it, read off the syntax
    /// rather than waiting for a graph edge.
    Uses,
    /// Classes implementing an abstract member.
    Implementations,
    /// Whether a public declaration's signature moved since the base.
    PublicApi,
    /// Definitions the range removed, read off the facts at the base.
    Removals,
    /// Whether a file still binds a name from another file.
    Imports,
}

/// How far an answer about a language is promised (T5). Tier 1 is the
/// declared set whose definitions, callers and test reach are being made
/// precise. Tier 2 is read by a model of the registry or by another graph and
/// answers with candidates. Every other language is Tier 3: not modelled, and
/// no answer about it claims an absence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    One,
    Two,
    Three,
}

/// The README's Tier table: a row per language as the README names it, with
/// the language names `languages.rs` gives its files, or the extension of a
/// language it does not name (`.astro`). Tier 3 has no rows.
#[allow(
    dead_code,
    reason = "tests/test_tiers_readme.py reads the README's table off this one"
)]
pub const TIERS: &[(Tier, &str, &[&str])] = &[
    (Tier::One, "Python", &["python"]),
    (
        Tier::One,
        "TypeScript/JavaScript",
        &["typescript", "tsx", "javascript", "jsx"],
    ),
    (Tier::One, "Rust", &["rust"]),
    (Tier::One, "Kotlin", &["kotlin"]),
    (Tier::Two, "Go", &["go"]),
    (Tier::Two, "Java", &["java"]),
    (Tier::Two, "Swift", &["swift"]),
    (Tier::Two, "SQL", &["sql"]),
    (Tier::Two, "shell", &["bash", "zsh"]),
    (Tier::Two, "YAML/k8s/Helm/Argo", &["yaml"]),
    (Tier::Two, "HTML/Astro", &["html", "astro"]),
];

/// A definition one model resolved, as that model's own handle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DefId(u32);

/// A fragment as a model sees it: its repo-relative path and the fragments
/// of its file, which hold its owner and scope.
pub struct At<'a> {
    pub path: &'a str,
    pub frag: &'a Fragment,
    pub file: &'a [&'a Fragment],
}

/// Where a use sits: whole lines, and the fragment around them when there
/// is one (a line a text search found has none).
pub struct UseSite<'a> {
    pub path: &'a str,
    pub lines: (u32, u32),
    pub frag: Option<(&'a Fragment, &'a [&'a Fragment])>,
}

/// A class member implementing an abstract one.
pub struct Implementer {
    pub path: String,
    pub name: String,
    pub start_line: u32,
}

/// A definition the range removed, as the base's facts state it.
pub struct RemovedDef {
    pub def: DefId,
    pub path: String,
    pub name: String,
    pub lines: (u32, u32),
    pub public: bool,
}

/// Every method that answers `None` means this model does not resolve that
/// relation, and `modelled` does not list it: the caller treats the
/// relation as not modelled for the language, never as nothing found.
pub trait LanguageFacts {
    fn modelled(&self) -> &'static [Relation];
    fn covers(&self, path: &str) -> bool;
    fn definition(&self, at: &At) -> Option<DefId>;
    fn classify(&self, site: &UseSite, def: DefId) -> Option<Finding>;
    /// Whether a test file reaches a caller's file.
    fn reaches_file(&self, test: &str, caller: &str) -> bool;
    fn use_lines(&self, _path: &str, _def: DefId) -> Option<Vec<u32>> {
        None
    }
    /// Whether code in `path` can name the definition at all: a function
    /// local to another is named only inside it (#546), a private item only
    /// where its language's visibility reaches (#599).
    fn names_from(&self, _def: DefId, _path: &str) -> bool {
        true
    }
    fn implementers(&self, _def: DefId, _paths: &[String]) -> Option<Vec<Implementer>> {
        None
    }
    fn annotations(&self, _def: DefId) -> Option<Vec<String>> {
        None
    }
    /// The trait a member implements or declares, whose calls may arrive
    /// through a dispatch no model follows.
    fn dispatched(&self, _def: DefId) -> Option<String> {
        None
    }
    fn public_change(&self, _def: DefId, _base: &str) -> Option<bool> {
        None
    }
    /// `pairs` are (path at the base, path at the head or `None` when
    /// deleted) of every file the range changed.
    fn removed(&self, _base: &str, _pairs: &[(String, Option<String>)]) -> Option<Vec<RemovedDef>> {
        None
    }
    /// Whether `importer`, whose text is `text`, binds `name` from `from`.
    fn binds(&self, _importer: &str, _text: &str, _name: &str, _from: &str) -> Option<bool> {
        None
    }
    /// Whether `path`, whose parse broke, spells `name` where the grammar
    /// could not read it: a call there is not seen. A name only in the
    /// lines it read was resolved like any other.
    fn names_unread(&self, _path: &str, _name: &str) -> Option<bool> {
        None
    }
}

pub struct Registry<'a> {
    models: Vec<Box<dyn LanguageFacts + 'a>>,
}

impl<'a> Registry<'a> {
    /// The models of one analysis. `files` is every path an import may
    /// name, deleted and renamed-away ones included.
    pub fn for_analysis(
        root: &'a Path,
        source: &'a Source,
        files: rustc_hash::FxHashSet<String>,
    ) -> Self {
        let mut models = bound::models(root, source, files.clone());
        models.push(Box::new(shell::ShellModel::new(root, source, &files)));
        models.push(Box::new(kotlin::KotlinModel::swift(root, source, &files)));
        models.push(Box::new(kotlin::KotlinModel::java(root, source)));
        models.push(Box::new(python::PythonModel::new(
            root,
            source,
            files.clone(),
        )));
        models.push(Box::new(sql::SqlModel::new(root, source)));
        models.push(Box::new(kotlin::KotlinModel::kotlin(root, source)));
        models.push(Box::new(go::GoModel::new(root, source)));
        models.push(Box::new(rust::RustModel::new(root, source, files)));
        Self { models }
    }

    pub fn for_path(&self, path: &str) -> Option<&(dyn LanguageFacts + 'a)> {
        self.models
            .iter()
            .find(|m| m.covers(path))
            .map(AsRef::as_ref)
    }

    /// The model both files belong to: a reference resolves within one.
    pub fn for_pair(&self, a: &str, b: &str) -> Option<&(dyn LanguageFacts + 'a)> {
        self.for_path(a).filter(|m| m.covers(b))
    }

    pub fn models(&self) -> impl Iterator<Item = &(dyn LanguageFacts + 'a)> {
        self.models.iter().map(AsRef::as_ref)
    }

    /// Whether the model of `path` answers `relation`.
    pub fn answers(&self, path: &str, relation: Relation) -> bool {
        self.for_path(path)
            .is_some_and(|m| m.modelled().contains(&relation))
    }

    /// The tier of `path`: its language's row of `TIERS`, and at least Tier 2
    /// for a file a model of this registry reads, so a model that joins the
    /// registry cannot leave its language "not modelled".
    #[allow(
        dead_code,
        reason = "read by this module's tests until a door states it"
    )]
    pub fn tier(&self, path: &str) -> Tier {
        let language = crate::languages::get_language_for_file(path);
        let extension = Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        let listed = TIERS
            .iter()
            .find(|(_, _, names)| {
                names
                    .iter()
                    .any(|n| Some(*n) == language || n.eq_ignore_ascii_case(extension))
            })
            .map_or(Tier::Three, |(tier, _, _)| *tier);
        if listed == Tier::Three && self.for_path(path).is_some() {
            Tier::Two
        } else {
            listed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_language_sits_in_two_rows() {
        let mut seen = std::collections::BTreeSet::new();
        for (_, _, names) in TIERS {
            for name in *names {
                assert!(seen.insert(*name), "{name} is in two rows of TIERS");
            }
        }
    }

    #[test]
    fn every_name_is_a_language_the_engine_detects_or_an_extension_it_does_not() {
        let detected: std::collections::BTreeSet<&str> = crate::languages::EXTENSION_TO_LANGUAGE
            .values()
            .copied()
            .collect();
        for (_, label, names) in TIERS {
            for name in *names {
                assert!(
                    detected.contains(name) || *name == "astro",
                    "{label}: {name} names no language"
                );
            }
        }
    }

    #[test]
    fn a_path_is_tiered_by_its_language_and_by_the_models_that_read_it() {
        let root = tempfile::tempdir().expect("tempdir");
        let source = Source::default();
        let registry =
            Registry::for_analysis(root.path(), &source, rustc_hash::FxHashSet::default());
        let expected = [
            ("app/main.py", Tier::One),
            ("web/index.ts", Tier::One),
            ("src/lib.rs", Tier::One),
            ("src/App.kt", Tier::One),
            ("cmd/main.go", Tier::Two),
            ("src/Main.java", Tier::Two),
            ("Sources/App.swift", Tier::Two),
            ("db/q.sql", Tier::Two),
            ("bin/run.sh", Tier::Two),
            ("deploy/app.yaml", Tier::Two),
            ("web/index.html", Tier::Two),
            ("src/pages/index.astro", Tier::Two),
            ("lib/users.rb", Tier::Three),
            ("src/Users.php", Tier::Three),
            ("src/Users.cs", Tier::Three),
            ("src/Users.scala", Tier::Three),
        ];
        for (path, tier) in expected {
            assert_eq!(registry.tier(path), tier, "{path}");
        }
    }

    #[test]
    fn tier_one_is_exactly_the_declared_set() {
        let one: Vec<&str> = TIERS
            .iter()
            .filter(|(tier, _, _)| *tier == Tier::One)
            .map(|(_, label, _)| *label)
            .collect();
        assert_eq!(one, ["Python", "TypeScript/JavaScript", "Rust", "Kotlin"]);
    }
}

/// Definitions a model handed out, interned so asking twice for the same
/// one returns the same handle.
struct Table<K, V> {
    keys: std::cell::RefCell<rustc_hash::FxHashMap<K, DefId>>,
    values: std::cell::RefCell<Vec<std::rc::Rc<V>>>,
}

impl<K: std::hash::Hash + Eq, V> Table<K, V> {
    fn new() -> Self {
        Self {
            keys: Default::default(),
            values: Default::default(),
        }
    }

    fn intern(&self, key: K, make: impl FnOnce() -> Option<V>) -> Option<DefId> {
        if let Some(id) = self.keys.borrow().get(&key) {
            return Some(*id);
        }
        let value = make()?;
        let mut values = self.values.borrow_mut();
        let id = DefId(u32::try_from(values.len()).ok()?);
        values.push(std::rc::Rc::new(value));
        self.keys.borrow_mut().insert(key, id);
        Some(id)
    }

    fn push(&self, value: V) -> Option<DefId> {
        let mut values = self.values.borrow_mut();
        let id = DefId(u32::try_from(values.len()).ok()?);
        values.push(std::rc::Rc::new(value));
        Some(id)
    }

    fn get(&self, id: DefId) -> std::rc::Rc<V> {
        self.values.borrow()[id.0 as usize].clone()
    }
}
