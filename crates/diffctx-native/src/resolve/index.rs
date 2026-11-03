//! The definitions, scopes and imports of every Python file of a snapshot,
//! indexed for the questions the resolver asks.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ops::Range;

use crate::facts::{
    Def, DefKey, DefKind, Disambig, EntryKind, Export, Fact, FileFacts, Import, ImportKind,
    LocalKind, Name, Scope,
};
use crate::snapshot::WorkspaceModel;
use crate::snapshot::workspace::ProjectId;

/// The names from the outermost definition to the one in question.
pub(super) type Path = Vec<Name>;

/// The definitions a scope can name, outermost first; empty for a module.
pub(super) fn scope_path(scope: &Scope) -> Path {
    match scope {
        Scope::Module => Vec::new(),
        Scope::Def(key) => {
            let mut path = key.container.clone();
            path.push(key.name.clone());
            path
        }
        Scope::Callback { parent, .. } => scope_path(parent),
    }
}

pub(super) struct DefRec {
    pub file: usize,
    pub def: Def,
    pub path: Path,
    pub key: DefKey,
    /// Where the definition sits among its file's facts.
    pub fact: usize,
}

/// What an import statement binds.
pub(super) enum Spec {
    /// A module, bound whole: `import a.b as c`, or `import a.b` which binds
    /// its head `a`.
    Module { relative: u32, dotted: String },
    /// `from module import name`.
    Member {
        relative: u32,
        module: String,
        name: Name,
    },
    /// `from module import *`.
    Star { relative: u32, module: String },
}

pub(super) struct ImportRec {
    /// The scope the import statement sits in.
    pub scope: Path,
    /// The name it binds; none for a star import.
    pub local: Option<Name>,
    pub spec: Spec,
}

pub(super) struct FileRec {
    pub facts: FileFacts,
    pub dir: String,
    pub is_init: bool,
    pub project: Option<ProjectId>,
    /// Definitions by the path of the scope that holds them.
    pub children: HashMap<Path, Vec<usize>>,
    /// Definitions by their own path (twins share one).
    pub by_path: HashMap<Path, Vec<usize>>,
    /// The names bound as locals in a scope, parameters included.
    pub locals: HashMap<Path, HashSet<Name>>,
    /// The first parameter of each function-like scope.
    pub first_param: HashMap<Path, Name>,
    /// Where a local variable is bound, by scope and name: fact indices.
    pub variables: HashMap<(Path, Name), Vec<usize>>,
    pub imports: Vec<ImportRec>,
    /// The bytes of the `if __name__ == "__main__":` statements.
    pub guards: Vec<Range<u32>>,
    /// Literal and computed paths put on `sys.path`.
    pub mentions: Vec<String>,
    /// Names the module lists in `__all__`.
    pub exports: HashSet<Name>,
}

#[derive(Default)]
pub(super) struct Index {
    pub files: Vec<FileRec>,
    pub defs: Vec<DefRec>,
    pub by_file: HashMap<String, usize>,
    /// Directories holding a Python file anywhere below.
    pub dirs: HashSet<String>,
    /// Methods and nested classes by name: what an untyped receiver may be.
    pub members: HashMap<Name, Vec<usize>>,
    /// The import roots of each file, once found.
    pub roots_cache: RefCell<HashMap<usize, Vec<String>>>,
}

pub(super) fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

pub(super) fn join(dir: &str, rest: &str) -> String {
    match (dir.is_empty(), rest.is_empty()) {
        (_, true) => dir.to_string(),
        (true, false) => rest.to_string(),
        (false, false) => format!("{dir}/{rest}"),
    }
}

impl Index {
    pub(super) fn build(files: Vec<FileFacts>, workspace: &WorkspaceModel) -> Self {
        let mut index = Self::default();
        for (at, facts) in files.into_iter().enumerate() {
            let path = facts.path.as_str().to_string();
            index.by_file.insert(path.clone(), at);
            let mut dir = dir_of(&path);
            loop {
                if !index.dirs.insert(dir.to_string()) || dir.is_empty() {
                    break;
                }
                dir = dir_of(dir);
            }
            let project = workspace.project_of(&path);
            let is_init = path.rsplit('/').next() == Some("__init__.py");
            let file = FileRec {
                dir: dir_of(&path).to_string(),
                is_init,
                project,
                facts,
                children: HashMap::new(),
                by_path: HashMap::new(),
                locals: HashMap::new(),
                first_param: HashMap::new(),
                variables: HashMap::new(),
                imports: Vec::new(),
                guards: Vec::new(),
                mentions: Vec::new(),
                exports: HashSet::new(),
            };
            index.files.push(file);
            index.read(at);
        }
        index
    }

    fn read(&mut self, at: usize) {
        let blob = std::sync::Arc::clone(&self.files[at].facts.blob);
        for (fact_at, fact) in blob.facts.iter().enumerate() {
            match fact {
                Fact::Definition(def) => self.add_def(at, fact_at, def),
                Fact::LocalBinding(local) => {
                    let scope = scope_path(&local.enclosing);
                    let file = &mut self.files[at];
                    file.locals
                        .entry(scope.clone())
                        .or_default()
                        .insert(local.name.clone());
                    if local.kind == LocalKind::Parameter {
                        file.first_param
                            .entry(scope.clone())
                            .or_insert_with(|| local.name.clone());
                    }
                    if local.kind == LocalKind::Variable {
                        file.variables
                            .entry((scope, local.name.clone()))
                            .or_default()
                            .push(fact_at);
                    }
                }
                Fact::Import(import) => self.add_import(at, import),
                Fact::Export(Export::Named {
                    name, from: None, ..
                }) => {
                    self.files[at].exports.insert(name.clone());
                }
                Fact::EntryPoint(entry) if entry.kind == EntryKind::ScriptGuard => {
                    self.files[at].guards.push(entry.site.bytes.clone());
                }
                Fact::PathMention(mention) => {
                    self.files[at].mentions.push(mention.text.clone());
                }
                _ => {}
            }
        }
        // Methods and nested classes by name, once the owners are known.
        let ids: Vec<usize> = self
            .defs
            .iter()
            .enumerate()
            .filter(|(_, d)| d.file == at)
            .map(|(i, _)| i)
            .collect();
        for id in ids {
            let rec = &self.defs[id];
            if rec.path.len() < 2 {
                continue;
            }
            let owner_path = &rec.path[..rec.path.len() - 1];
            let owner_is_class = self.files[at]
                .by_path
                .get(owner_path)
                .is_some_and(|owners| {
                    owners
                        .iter()
                        .any(|o| self.defs[*o].def.kind == DefKind::Class)
                });
            if owner_is_class && matches!(rec.def.kind, DefKind::Method | DefKind::Class) {
                let name = rec.def.local.name.clone();
                self.members.entry(name).or_default().push(id);
            }
        }
    }

    fn add_def(&mut self, at: usize, fact_at: usize, def: &Def) {
        let key = self.files[at].facts.key(&def.local);
        let mut path = def.local.container.clone();
        path.push(def.local.name.clone());
        let id = self.defs.len();
        self.defs.push(DefRec {
            file: at,
            def: def.clone(),
            path: path.clone(),
            key,
            fact: fact_at,
        });
        let file = &mut self.files[at];
        file.children
            .entry(def.local.container.clone())
            .or_default()
            .push(id);
        file.by_path.entry(path).or_default().push(id);
    }

    fn add_import(&mut self, at: usize, import: &Import) {
        if matches!(import.kind, ImportKind::Dynamic | ImportKind::SideEffect) {
            return;
        }
        let scope = scope_path(&import.enclosing);
        let module = import.module.as_str().to_string();
        let file = &mut self.files[at];
        if import.names.is_empty() && import.star.is_none() && import.relative == 0 {
            // `import a.b.c` binds `a`; `import a.b.c as x` binds `x` to the whole.
            let (local, dotted) = match &import.default {
                Some(alias) => (alias.clone(), module),
                None => {
                    let head = module.split('.').next().unwrap_or("").to_string();
                    (Name::new(head.as_str()), head)
                }
            };
            file.imports.push(ImportRec {
                scope,
                local: Some(local),
                spec: Spec::Module {
                    relative: 0,
                    dotted,
                },
            });
            return;
        }
        if import.star.is_some() {
            file.imports.push(ImportRec {
                scope,
                local: None,
                spec: Spec::Star {
                    relative: import.relative,
                    module,
                },
            });
            return;
        }
        for (name, alias) in &import.names {
            file.imports.push(ImportRec {
                scope: scope.clone(),
                local: Some(alias.clone().unwrap_or_else(|| name.clone())),
                spec: Spec::Member {
                    relative: import.relative,
                    module: module.clone(),
                    name: name.clone(),
                },
            });
        }
    }

    /// The definition a path names: the primary of its twins.
    pub(super) fn owner(&self, file: usize, path: &[Name]) -> Option<usize> {
        let ids = self.files[file].by_path.get(path)?;
        self.primary(ids)
    }

    /// Of definitions sharing one key apart from their disambiguator: the one
    /// the code means by the name. A `TYPE_CHECKING` twin and an `@overload`
    /// stub stand behind it.
    pub(super) fn primary(&self, ids: &[usize]) -> Option<usize> {
        ids.iter()
            .copied()
            .find(|id| self.defs[*id].def.local.disambig.is_none())
            .or_else(|| {
                ids.iter()
                    .copied()
                    .find(|id| !matches!(self.defs[*id].def.local.disambig, Some(Disambig::Cfg(_))))
            })
            .or_else(|| ids.first().copied())
    }

    pub(super) fn has_init(&self, dir: &str) -> bool {
        self.by_file.contains_key(&join(dir, "__init__.py"))
    }

    /// The file of module `rest` (`a/b`) under the import root `root`.
    pub(super) fn find_module(&self, root: &str, rest: &str) -> Option<usize> {
        let base = join(root, rest);
        if rest.is_empty() {
            return self.by_file.get(&join(root, "__init__.py")).copied();
        }
        [
            format!("{base}.py"),
            format!("{base}/__init__.py"),
            format!("{base}.pyi"),
        ]
        .iter()
        .find_map(|candidate| self.by_file.get(candidate).copied())
    }
}
