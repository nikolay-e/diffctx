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

pub use python::overlay_assignments;

use std::path::Path;

use crate::bindings::Finding;
use crate::source::Source;
use crate::types::Fragment;

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
    fn implementers(&self, _def: DefId, _paths: &[String]) -> Option<Vec<Implementer>> {
        None
    }
    fn annotations(&self, _def: DefId) -> Option<Vec<String>> {
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
        models.push(Box::new(python::PythonModel::new(root, source, files)));
        models.push(Box::new(kotlin::KotlinModel::new(root, source)));
        models.push(Box::new(go::GoModel::new(root, source)));
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
