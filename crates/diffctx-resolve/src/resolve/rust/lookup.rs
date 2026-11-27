//! The name lookups the engine repeats most: each indexed by what it is asked
//! for and probed by `&str`, so a probe allocates nothing, and a glob import's
//! walk answered once per place it is asked from.

use std::cell::{Cell, RefCell};

use rustc_hash::{FxHashMap, FxHashSet};

use diffctx_facts::facts::Name;

use super::{Engine, Ent, FileRec, ImportRec, Site, Want};

/// Items of a module, by name.
#[derive(Default)]
pub(super) struct Items(FxHashMap<usize, FxHashMap<String, Vec<usize>>>);

impl Items {
    pub(super) fn get(&self, module: usize, name: &str) -> Option<&Vec<usize>> {
        self.0.get(&module)?.get(name)
    }

    pub(super) fn push(&mut self, module: usize, name: &Name, id: usize) {
        self.0
            .entry(module)
            .or_default()
            .entry(name.as_str().to_string())
            .or_default()
            .push(id);
    }
}

/// Items local to a function, by file and name, each with the function's path.
#[derive(Default)]
pub(super) struct Locals(FxHashMap<usize, FxHashMap<String, Vec<(Vec<Name>, Vec<usize>)>>>);

impl Locals {
    pub(super) fn push(&mut self, file: usize, scope: &[Name], name: &Name, id: usize) {
        let by_scope = self
            .0
            .entry(file)
            .or_default()
            .entry(name.as_str().to_string())
            .or_default();
        match by_scope.iter_mut().find(|(path, _)| path == scope) {
            Some((_, ids)) => ids.push(id),
            None => by_scope.push((scope.to_vec(), vec![id])),
        }
    }

    /// Innermost first, so the order is the one the scopes around give the name.
    pub(super) fn settle(&mut self) {
        for by_scope in self.0.values_mut().flat_map(FxHashMap::values_mut) {
            by_scope.sort_by_key(|(path, _)| std::cmp::Reverse(path.len()));
        }
    }

    /// What the functions around `scope` hold under `name`, innermost first.
    pub(super) fn around<'a>(
        &'a self,
        file: usize,
        name: &str,
        scope: &'a [Name],
    ) -> impl Iterator<Item = &'a Vec<usize>> {
        self.0
            .get(&file)
            .and_then(|names| names.get(name))
            .into_iter()
            .flatten()
            .filter(move |(path, _)| scope.starts_with(path))
            .map(|(_, ids)| ids)
    }
}

/// The child module of each module, by its last segment.
#[derive(Default)]
pub(super) struct Children(FxHashMap<usize, FxHashMap<String, usize>>);

impl Children {
    pub(super) fn add(&mut self, parent: usize, path: &str, id: usize) {
        let name = path.rsplit("::").next().unwrap_or(path);
        self.0
            .entry(parent)
            .or_default()
            .insert(name.to_string(), id);
    }

    pub(super) fn get(&self, parent: usize, name: &str) -> Option<usize> {
        self.0.get(&parent)?.get(name).copied()
    }
}

/// A file's imports by the name they bind, and its globs.
#[derive(Default)]
pub(super) struct ImportsIndex {
    by_local: FxHashMap<String, Vec<usize>>,
    globs: Vec<usize>,
}

impl ImportsIndex {
    pub(super) fn of(imports: &[ImportRec]) -> Self {
        let mut index = Self::default();
        for (at, import) in imports.iter().enumerate() {
            match &import.local {
                Some(local) if !import.glob => index
                    .by_local
                    .entry(local.as_str().to_string())
                    .or_default()
                    .push(at),
                _ if import.glob => index.globs.push(at),
                _ => {}
            }
        }
        index
    }
}

impl FileRec {
    /// The non-glob imports that bind `name`, in the order they are written.
    pub(super) fn imports_named(&self, name: &str) -> impl Iterator<Item = &ImportRec> {
        self.by_name
            .by_local
            .get(name)
            .into_iter()
            .flatten()
            .map(|at| &self.imports[*at])
    }

    pub(super) fn globs(&self) -> impl Iterator<Item = &ImportRec> {
        self.by_name.globs.iter().map(|at| &self.imports[*at])
    }
}

/// What a lookup met that it reads differently once the lookup that holds it
/// has finished: a name being resolved, a type whose members are being found,
/// the trait table being built. A walk that met none of them is the answer
/// anywhere; one that did is not remembered.
#[derive(Default)]
pub(super) struct Guards {
    open: RefCell<FxHashSet<(u8, usize, usize)>>,
    met: Cell<u32>,
}

pub(super) const IMPORT: u8 = 0;
pub(super) const MEMBERS: u8 = 1;

impl Guards {
    pub(super) fn enter(&self, kind: u8, a: usize, b: usize) {
        self.open.borrow_mut().insert((kind, a, b));
    }

    pub(super) fn leave(&self, kind: u8, a: usize, b: usize) {
        self.open.borrow_mut().remove(&(kind, a, b));
    }

    /// A remembered answer is read: it is a guard's placeholder while still open.
    pub(super) fn read(&self, kind: u8, a: usize, b: usize) {
        if self.open.borrow().contains(&(kind, a, b)) {
            self.unfinished();
        }
    }

    pub(super) fn unfinished(&self) {
        self.met.set(self.met.get() + 1);
    }
}

/// What a glob import's path walked to, by the import, the scope it is asked
/// from and the depth of the walk: a cut by `MAX_HOPS` is a function of depth.
#[derive(Default)]
pub(super) struct GlobWalks(RefCell<FxHashMap<usize, Vec<(Vec<Name>, usize, Option<Ent>)>>>);

impl GlobWalks {
    fn get(&self, import: usize, scope: &[Name], depth: usize) -> Option<Option<Ent>> {
        self.0
            .borrow()
            .get(&import)?
            .iter()
            .find(|(path, at, _)| *at == depth && path == scope)
            .map(|(_, _, found)| *found)
    }

    fn put(&self, import: usize, scope: &[Name], depth: usize, found: Option<Ent>) {
        self.0
            .borrow_mut()
            .entry(import)
            .or_default()
            .push((scope.to_vec(), depth, found));
    }
}

impl Engine<'_> {
    /// What the path of glob `import` (written in file `f`) names when walked
    /// from `scope` of its module at `depth`.
    pub(super) fn glob_target(
        &self,
        f: usize,
        import: &ImportRec,
        scope: &[Name],
        depth: usize,
    ) -> Option<Ent> {
        let at = std::ptr::from_ref(import) as usize;
        if let Some(known) = self.globs.get(at, scope, depth) {
            return known;
        }
        let before = self.guards.met.get();
        let site = Site {
            file: f,
            module: import.module,
            scope: scope.to_vec(),
        };
        let found = self.walk(&site, &import.segs, Want::Type, depth);
        if self.guards.met.get() == before {
            self.globs.put(at, scope, depth, found);
        }
        found
    }
}
