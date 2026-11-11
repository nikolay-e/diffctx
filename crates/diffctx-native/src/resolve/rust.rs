//! Rust: every reference and import of the facts layer bound to the definitions
//! it lands on.
//!
//! A module is a file or an inline `mod`, named by its path from its crate's
//! root (the workspace model places the files: `mod` declarations and
//! `#[path]` included). A name reaches a definition through the scope Rust
//! gives it: an item of the function around it, a `use`, an item of the
//! module, a glob, a crate the workspace holds; a path walks the modules and
//! types it names (a path starting at `crate`, `self`, `super` or `Self`). A method is
//! reached through a value whose type the syntax states (`self`, a typed
//! parameter or `let`, a constructor, what a called function declares it
//! returns, a field with a declared type); a trait object, `impl Tr` or a
//! generic with a bound is every implementer's method, as candidates, never
//! as a resolution; any other value is a candidate from the types the file
//! names.
//!
//! The facts' conventions are in `facts::lang::rust`.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};

use super::{
    Binding, CandidateReason, Definition, Evidence, EvidenceKind, Relation, Resolution,
    UnresolvedReason,
};
use crate::facts::FileFacts;
use crate::facts::{
    Def, DefKey, DefKind, Disambig, Export, Fact, FactsCache, Import, Language, Name, Namespace,
    Receiver, Ref, Scope, UseKind, Visibility, expired, facts_of,
};
use crate::snapshot::workspace::CrateKind;
use crate::snapshot::{
    AnalysisContext, Loss, RepoPath, Result, SideTag, SnapshotError, Stage, WorkspaceModel,
};

/// A call through an untyped receiver is a candidate for at most this many
/// methods; past it the list says nothing a reader could use.
const MAX_CANDIDATES: usize = 24;
/// How far a chain of types may feed one another.
const MAX_DEPTH: usize = 6;
/// How many re-exports and globs a name may pass through.
const MAX_HOPS: usize = 8;

/// Names the language and its standard library bring into every module.
const PRELUDE: &[&str] = &[
    "Option",
    "Some",
    "None",
    "Result",
    "Ok",
    "Err",
    "Vec",
    "String",
    "Box",
    "Default",
    "Drop",
    "Clone",
    "Copy",
    "Send",
    "Sync",
    "Sized",
    "Eq",
    "PartialEq",
    "Ord",
    "PartialOrd",
    "Iterator",
    "IntoIterator",
    "Into",
    "From",
    "AsRef",
    "AsMut",
    "ToString",
    "ToOwned",
    "Fn",
    "FnMut",
    "FnOnce",
    "Extend",
    "DoubleEndedIterator",
    "ExactSizeIterator",
    "TryFrom",
    "TryInto",
    "FromIterator",
    "Debug",
    "Hash",
];

/// The macros the standard library provides.
const STD_MACROS: &[&str] = &[
    "println",
    "print",
    "eprintln",
    "eprint",
    "format",
    "format_args",
    "write",
    "writeln",
    "vec",
    "panic",
    "assert",
    "assert_eq",
    "assert_ne",
    "debug_assert",
    "debug_assert_eq",
    "debug_assert_ne",
    "unreachable",
    "unimplemented",
    "todo",
    "matches",
    "dbg",
    "cfg",
    "include_str",
    "include_bytes",
    "include",
    "env",
    "option_env",
    "concat",
    "stringify",
    "line",
    "file",
    "column",
    "module_path",
    "compile_error",
    "thread_local",
    "try",
];

/// The crates every program may name.
const STD_CRATES: &[&str] = &["std", "core", "alloc", "proc_macro", "test"];

/// The bindings of one snapshot's Rust files.
pub struct RustResolver {
    definitions: Vec<Definition>,
    bindings: Vec<Binding>,
    by_target: HashMap<DefKey, Vec<usize>>,
    losses: Vec<Loss>,
    read: Vec<RepoPath>,
    /// Files of a library crate that a module path declared `pub` from the
    /// crate's root leads to.
    public: HashSet<String>,
    /// The type or trait each member of an `impl` belongs to, wherever the
    /// `impl` is written.
    owners: HashMap<DefKey, DefKey>,
}

impl RustResolver {
    /// Resolves every Rust file of `side`.
    pub fn open(ctx: &AnalysisContext, side: SideTag, cache: &FactsCache) -> Result<Self> {
        let reader = match side {
            SideTag::Base => &ctx.base,
            SideTag::Head => &ctx.head,
        };
        let listing = cache.listing(reader)?;
        let mut files = Vec::new();
        let mut losses = Vec::new();
        let mut read = Vec::new();
        for entry in &listing.entries {
            if Language::from_path(&entry.path) != Some(Language::Rust) {
                continue;
            }
            if expired(&ctx.run) {
                losses.push(Loss::Deadline {
                    stage: Stage::Search,
                });
                break;
            }
            if let Some(file) = facts_of(ctx, side, cache, &entry.path)? {
                if file.language() == Language::Rust {
                    losses.extend(file.losses());
                    read.push(entry.path.clone());
                    files.push(file);
                }
            }
        }
        let index = Index::build(files, &ctx.workspace);
        let public = index
            .files
            .iter()
            .filter(|file| {
                let module = &index.mods[file.module];
                module.public && index.crates[module.krate].kind == CrateKind::Lib
            })
            .map(|file| file.facts.path.as_str().to_string())
            .collect();
        let engine = Engine::new(index, &ctx.workspace);
        let (definitions, bindings, owned) = engine.run();
        // The table is built before the first lookup, so this is a bug of the
        // build itself: said, never an answer from half a table.
        if engine.met_unfinished_table.get() {
            return Err(SnapshotError::Protocol(
                "the trait table was read while it was being built".into(),
            ));
        }
        let mut by_target: HashMap<DefKey, Vec<usize>> = HashMap::new();
        for (at, binding) in bindings.iter().enumerate() {
            for key in binding.targets() {
                by_target.entry(key.clone()).or_default().push(at);
            }
        }
        let owners = owned
            .into_iter()
            .map(|(owner, member, _)| (member, owner))
            .collect();
        Ok(Self {
            definitions,
            bindings,
            by_target,
            losses,
            read,
            public,
            owners,
        })
    }

    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }

    pub fn definitions(&self) -> &[Definition] {
        &self.definitions
    }

    /// The bindings that resolve to `key` or have it among their candidates.
    #[allow(
        clippy::disallowed_methods,
        reason = "a key the index has no entry for has no references"
    )]
    pub fn references_to(&self, key: &DefKey) -> Vec<&Binding> {
        self.by_target
            .get(key)
            .map(|at| at.iter().map(|i| &self.bindings[*i]).collect())
            .unwrap_or_default()
    }

    /// What the answer is partial for: files the extractor could not fully
    /// read, a run cut short.
    pub fn losses(&self) -> &[Loss] {
        &self.losses
    }

    /// The files the resolver read: what an absence may rest on.
    pub fn read_paths(&self) -> &[RepoPath] {
        &self.read
    }

    /// Whether the file is a module of a library crate that every `mod` from
    /// the crate's root declares `pub`: what a removal there is a contract for.
    pub fn is_public_module(&self, file: &str) -> bool {
        self.public.contains(file)
    }

    /// The type or trait a member's `impl` names: the same whichever module the
    /// `impl` is written in.
    pub fn owner_of(&self, member: &DefKey) -> Option<&DefKey> {
        self.owners.get(member)
    }
}

// ---- the index

struct CrateRec {
    name: String,
    kind: CrateKind,
    root_file: String,
    root: usize,
}

struct ModRec {
    krate: usize,
    path: String,
    parent: Option<usize>,
    /// Declared `pub` all the way from the crate root (a crate's root is).
    public: bool,
}

struct ImportRec {
    /// The scope the `use` sits in: the module's inline path and the functions
    /// around it.
    scope: Vec<Name>,
    module: usize,
    /// The path the import names, as written, and the name it binds.
    segs: Vec<Name>,
    local: Option<Name>,
    glob: bool,
    exported: bool,
}

struct FileRec {
    facts: FileFacts,
    module: usize,
    imports: Vec<ImportRec>,
    /// Every name the file defines, imports or refers to.
    names: HashSet<Name>,
    /// The paths of the inline modules the file declares.
    inline: HashSet<Vec<Name>>,
}

struct DefRec {
    file: usize,
    def: Def,
    key: DefKey,
    /// The module the definition's inline modules lead to.
    home: usize,
    /// The container beyond the modules: empty for an item of a module, a type
    /// (or trait) for its member, a function for an item local to it.
    rest: Vec<Name>,
}

impl DefRec {
    fn is_member(&self) -> bool {
        matches!(
            self.def.kind,
            DefKind::Method | DefKind::Field | DefKind::EnumMember
        )
    }
}

#[derive(Default)]
struct Index {
    crates: Vec<CrateRec>,
    mods: Vec<ModRec>,
    mod_ix: HashMap<(usize, String), usize>,
    files: Vec<FileRec>,
    defs: Vec<DefRec>,
    /// Items of a module by name.
    items: HashMap<(usize, Name), Vec<usize>>,
    /// Items local to a function, by file and the function's path.
    locals: HashMap<(usize, Vec<Name>, Name), Vec<usize>>,
    /// Members of a type (or trait) written in a module, by the type's name.
    groups: HashMap<(usize, Name), Vec<usize>>,
    /// Methods by name: what an untyped receiver may be.
    methods: HashMap<Name, Vec<usize>>,
    /// The `macro_rules!` definitions by name.
    macros: HashMap<Name, Vec<usize>>,
    /// The files that hold a module: its own file and the ones with an inline
    /// `mod` for it.
    files_of: HashMap<usize, Vec<usize>>,
    /// The groups of members, by the type's name.
    groups_named: HashMap<Name, Vec<(usize, Vec<usize>)>>,
    /// `impl Tr for X` headers: the file, the module, the type's name and the
    /// trait's reference.
    impls: Vec<(usize, usize, Name, Ref)>,
}

impl Index {
    fn build(files: Vec<FileFacts>, workspace: &WorkspaceModel) -> Self {
        let mut index = Self::default();
        let mut file_mod: HashMap<String, usize> = HashMap::new();
        for project in workspace.projects() {
            for krate in &project.crates {
                let at = index.crates.len();
                index.crates.push(CrateRec {
                    name: krate.name.clone(),
                    kind: krate.kind,
                    root_file: krate.root_file.clone(),
                    root: 0,
                });
                for module in &krate.modules {
                    let id = index.module_at(at, &module.path);
                    if module.path.is_empty() {
                        index.crates[at].root = id;
                    }
                    // A module is public when it and every module above it is
                    // declared `pub` (the tree lists parents first).
                    let above = index.mods[id].parent.is_none_or(|p| index.mods[p].public);
                    index.mods[id].public = above
                        && matches!(
                            module.visibility,
                            crate::snapshot::workspace::Visibility::Pub
                        );
                    file_mod.entry(module.file.clone()).or_insert(id);
                }
            }
        }
        let orphans: HashSet<String> = files
            .iter()
            .map(|f| f.path.as_str().to_string())
            .filter(|p| !file_mod.contains_key(p))
            .collect();
        let mut packages: HashMap<String, usize> = HashMap::new();
        for (at, facts) in files.into_iter().enumerate() {
            let path = facts.path.as_str().to_string();
            let known = file_mod.get(&path).copied();
            let module = match known.or_else(|| index.inferred(&mut packages, &orphans, &path)) {
                Some(id) => id,
                None => {
                    let stem = path
                        .rsplit('/')
                        .next()
                        .unwrap_or(&path)
                        .trim_end_matches(".rs")
                        .to_string();
                    let krate = index.crates.len();
                    index.crates.push(CrateRec {
                        name: stem,
                        kind: CrateKind::Bin,
                        root_file: path.clone(),
                        root: 0,
                    });
                    let id = index.module_at(krate, "");
                    index.crates[krate].root = id;
                    id
                }
            };
            index.add_file(at, facts, module);
        }
        index.settle();
        index
    }

    /// A file no manifest of the workspace lists, laid out as a package is
    /// (`src/lib.rs` or `src/main.rs` above it): the module its path says, in the
    /// crate that root makes. `None` for any other file, which is a crate of its
    /// own.
    fn inferred(
        &mut self,
        packages: &mut HashMap<String, usize>,
        orphans: &HashSet<String>,
        path: &str,
    ) -> Option<usize> {
        let parts: Vec<&str> = path.split('/').collect();
        let src_at = (0..parts.len().saturating_sub(1)).rev().find(|at| {
            let src = parts[..=*at].join("/");
            parts[*at] == "src"
                && ["lib.rs", "main.rs"]
                    .iter()
                    .any(|root| orphans.contains(&format!("{src}/{root}")))
        })?;
        let src = parts[..=src_at].join("/");
        let rel = &parts[src_at + 1..];
        if rel.first() == Some(&"bin") {
            return None;
        }
        // `src/main.rs` is a binary of its own; every other file is a module of
        // the library when there is one. Without a manifest a crate is named as
        // `rustc` names it, after its root file (`lib`, `main`).
        let lib = orphans.contains(&format!("{src}/lib.rs")) && rel != ["main.rs"];
        let root_file = format!("{src}/{}", if lib { "lib.rs" } else { "main.rs" });
        let krate = match packages.get(&root_file) {
            Some(known) => *known,
            None => {
                let at = self.crates.len();
                self.crates.push(CrateRec {
                    name: (if lib { "lib" } else { "main" }).to_string(),
                    kind: if lib { CrateKind::Lib } else { CrateKind::Bin },
                    root_file: root_file.clone(),
                    root: 0,
                });
                let root = self.module_at(at, "");
                self.crates[at].root = root;
                packages.insert(root_file, at);
                at
            }
        };
        let mut segs: Vec<&str> = rel.to_vec();
        let stem = segs.pop()?.strip_suffix(".rs")?;
        match stem {
            "mod" => {}
            "lib" | "main" if segs.is_empty() => {}
            other => segs.push(other),
        }
        Some(self.module_at(krate, &segs.join("::")))
    }

    /// The module of a crate at `path`, made when absent.
    fn module_at(&mut self, krate: usize, path: &str) -> usize {
        if let Some(id) = self.mod_ix.get(&(krate, path.to_string())) {
            return *id;
        }
        let parent = path.rsplit_once("::").map_or_else(
            || (!path.is_empty()).then_some(""),
            |(parent, _)| Some(parent),
        );
        let parent = parent.map(|p| self.module_at(krate, p));
        let id = self.mods.len();
        let public = parent.is_none_or(|p| self.mods[p].public);
        self.mods.push(ModRec {
            krate,
            path: path.to_string(),
            parent,
            public,
        });
        self.mod_ix.insert((krate, path.to_string()), id);
        id
    }

    fn child_module(&mut self, parent: usize, name: &str) -> usize {
        let krate = self.mods[parent].krate;
        let path = if self.mods[parent].path.is_empty() {
            name.to_string()
        } else {
            format!("{}::{name}", self.mods[parent].path)
        };
        self.module_at(krate, &path)
    }

    fn add_file(&mut self, at: usize, facts: FileFacts, module: usize) {
        let mut names: HashSet<Name> = HashSet::new();
        let mut imports = Vec::new();
        let mut inline: HashSet<Vec<Name>> = HashSet::new();
        let mut functions: HashSet<Vec<Name>> = HashSet::new();
        for def in facts.blob.defs() {
            let mut path = def.local.container.clone();
            path.push(def.local.name.clone());
            match def.kind {
                DefKind::Module => {
                    inline.insert(path);
                }
                DefKind::Function | DefKind::Method => {
                    functions.insert(path);
                }
                _ => {}
            }
        }
        let mut recs: Vec<(Def, usize, Vec<Name>)> = Vec::new();
        for fact in &facts.blob.facts {
            match fact {
                Fact::Definition(def) => {
                    names.insert(def.local.name.clone());
                    let container = &def.local.container;
                    let mut home = module;
                    let mut taken = 0;
                    while taken < container.len() && inline.contains(&container[..=taken]) {
                        home = self.child_module(home, container[taken].as_str());
                        taken += 1;
                    }
                    recs.push((def.clone(), home, container[taken..].to_vec()));
                }
                Fact::Reference(r) => {
                    names.insert(r.name.clone());
                    names.extend(r.qualifier.iter().cloned());
                }
                Fact::Import(import) => {
                    let scope = scope_path(&import.enclosing);
                    let home = self.home_of(module, &scope, &inline);
                    self.add_import(import, scope, home, &mut imports, &mut names);
                }
                _ => {}
            }
        }
        // Inline modules a definition does not name (an empty `mod m {}`) still
        // exist; the definitions make them.
        for (def, home, rest) in recs {
            let id = self.defs.len();
            let key = facts.key(&def.local);
            if def.kind == DefKind::Module && rest.is_empty() {
                let id = self.child_module(home, def.local.name.as_str());
                self.mods[id].public &= def.visibility == Visibility::Public;
            }
            let local = !rest.is_empty() && functions.contains(&rest);
            let rec = DefRec {
                file: at,
                def,
                key,
                home,
                rest: rest.clone(),
            };
            if rest.is_empty() {
                self.items
                    .entry((home, rec.def.local.name.clone()))
                    .or_default()
                    .push(id);
                if rec.def.kind == DefKind::Macro {
                    self.macros
                        .entry(rec.def.local.name.clone())
                        .or_default()
                        .push(id);
                }
            } else if local {
                self.locals
                    .entry((
                        at,
                        rec.def.local.container.clone(),
                        rec.def.local.name.clone(),
                    ))
                    .or_default()
                    .push(id);
            } else if rest.len() == 1 && rec.is_member() {
                self.groups
                    .entry((home, rest[0].clone()))
                    .or_default()
                    .push(id);
                if rec.def.kind == DefKind::Method {
                    self.methods
                        .entry(rec.def.local.name.clone())
                        .or_default()
                        .push(id);
                }
            } else if rest.len() == 1 {
                // An associated constant or type.
                self.groups
                    .entry((home, rest[0].clone()))
                    .or_default()
                    .push(id);
            }
            self.defs.push(rec);
        }
        for fact in &facts.blob.facts {
            if let Fact::Reference(r) = fact {
                if r.use_kind == UseKind::Override {
                    if let Scope::Def(subject) = &r.enclosing {
                        if subject.ns == Namespace::Type && subject.disambig.is_none() {
                            let scope = subject.container.clone();
                            let home = self.home_of(module, &scope, &inline);
                            self.impls.push((at, home, subject.name.clone(), r.clone()));
                        }
                    }
                }
            }
        }
        self.files_of.entry(module).or_default().push(at);
        for path in &inline {
            let mut home = module;
            for name in path {
                home = self.child_module(home, name.as_str());
            }
            self.files_of.entry(home).or_default().push(at);
        }
        self.files.push(FileRec {
            facts,
            module,
            imports,
            names,
            inline,
        });
    }

    /// The module a scope path leads to through the file's inline modules.
    fn home_of(&mut self, module: usize, scope: &[Name], inline: &HashSet<Vec<Name>>) -> usize {
        let mut home = module;
        let mut taken = 0;
        while taken < scope.len() && inline.contains(&scope[..=taken]) {
            home = self.child_module(home, scope[taken].as_str());
            taken += 1;
        }
        home
    }

    fn add_import(
        &mut self,
        import: &Import,
        scope: Vec<Name>,
        module: usize,
        out: &mut Vec<ImportRec>,
        names: &mut HashSet<Name>,
    ) {
        let head: Vec<Name> = if import.module.as_str().is_empty() {
            Vec::new()
        } else {
            import.module.as_str().split("::").map(Name::new).collect()
        };
        names.extend(head.iter().cloned());
        if import.star.is_star() {
            out.push(ImportRec {
                scope,
                module,
                segs: head,
                local: None,
                glob: true,
                exported: false,
            });
            return;
        }
        for (name, alias) in &import.names {
            names.insert(name.clone());
            let mut segs = head.clone();
            segs.push(name.clone());
            out.push(ImportRec {
                scope: scope.clone(),
                module,
                segs,
                local: Some(alias.clone().unwrap_or_else(|| name.clone())),
                glob: false,
                exported: false,
            });
        }
    }

    /// Marks the re-exports and indexes what needs every file read.
    fn settle(&mut self) {
        for ((home, name), ids) in &self.groups {
            self.groups_named
                .entry(name.clone())
                .or_default()
                .push((*home, ids.clone()));
        }
        for file in &mut self.files {
            let exports: Vec<(&Name, Option<&Name>)> = file
                .facts
                .blob
                .facts
                .iter()
                .filter_map(|f| match f {
                    Fact::Export(Export::Named { name, alias, .. }) => Some((name, alias.as_ref())),
                    _ => None,
                })
                .collect();
            let stars = file
                .facts
                .blob
                .facts
                .iter()
                .any(|f| matches!(f, Fact::Export(Export::Star { .. })));
            for import in &mut file.imports {
                if let Some(local) = &import.local {
                    let last = import.segs.last();
                    import.exported = exports.iter().any(|(name, alias)| {
                        Some(*name) == last && alias.unwrap_or(*name) == local
                    });
                } else {
                    import.exported = stars;
                }
            }
        }
    }
}

/// The definitions a scope can name, outermost first; empty for a module.
fn scope_path(scope: &Scope) -> Vec<Name> {
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

// ---- the engine

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Ent {
    Mod(usize),
    Def(usize),
    External,
    Builtin,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Want {
    Value,
    Type,
    Any,
}

/// A type a value has.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Typed {
    /// A struct, enum or alias definition.
    Def(usize),
    /// A trait object, `impl Tr` or a generic with a bound: any implementer.
    Dyn(usize),
    /// A type the repository does not hold.
    External,
    Unknown,
}

enum Outcome {
    Found(usize, EvidenceKind),
    Candidates(Vec<usize>, CandidateReason),
    Unresolved(UnresolvedReason),
}

/// Where a name is asked: the file, the module and the scope path in it.
#[derive(Clone)]
struct Site {
    file: usize,
    module: usize,
    scope: Vec<Name>,
}

/// A member of a type or trait: the owner's key, the member's, its kind.
type Owned = (DefKey, DefKey, DefKind);

struct Engine<'a> {
    ix: Index,
    workspace: &'a WorkspaceModel,
    /// What an import binds, once found.
    imported: RefCell<HashMap<(usize, usize), Option<Ent>>>,
    members: RefCell<HashMap<usize, Vec<usize>>>,
    implemented: RefCell<Traits>,
    /// A trait lookup was made while the trait table was being built, and met no
    /// table: what it answered is not the repository's.
    met_unfinished_table: Cell<bool>,
}

/// Which traits each type implements, and how far it is built.
enum Traits {
    Unbuilt,
    /// Being built: a lookup now sees no table, which is never "no trait".
    Building,
    Built(HashMap<usize, Vec<usize>>),
}

impl<'a> Engine<'a> {
    fn new(ix: Index, workspace: &'a WorkspaceModel) -> Self {
        Self {
            ix,
            workspace,
            imported: RefCell::new(HashMap::new()),
            members: RefCell::new(HashMap::new()),
            implemented: RefCell::new(Traits::Unbuilt),
            met_unfinished_table: Cell::new(false),
        }
    }

    /// The definitions, the bindings, and every (owner, member, kind) of a type
    /// or trait.
    fn run(&self) -> (Vec<Definition>, Vec<Binding>, Vec<Owned>) {
        // Built before any lookup reads it.
        self.trait_table();
        let mut bindings = Vec::new();
        for (f, file) in self.ix.files.iter().enumerate() {
            for fact in &file.facts.blob.facts {
                match fact {
                    Fact::Reference(r) => self.reference(f, r, &mut bindings),
                    Fact::Import(import) => self.import(f, import, &mut bindings),
                    _ => {}
                }
            }
        }
        let definitions = self
            .ix
            .defs
            .iter()
            .map(|rec| Definition {
                file: self.path_of(rec.file).clone(),
                key: rec.key.clone(),
                kind: rec.def.kind,
                span: rec.def.span.clone(),
                name_span: rec.def.name_span.clone(),
            })
            .collect();
        let mut owned = Vec::new();
        for (t, rec) in self.ix.defs.iter().enumerate() {
            if !matches!(
                rec.def.kind,
                DefKind::Struct | DefKind::Enum | DefKind::Trait
            ) {
                continue;
            }
            for m in self.members_of(t) {
                let member = &self.ix.defs[m];
                owned.push((rec.key.clone(), member.key.clone(), member.def.kind));
            }
        }
        (definitions, bindings, owned)
    }

    fn path_of(&self, f: usize) -> &RepoPath {
        &self.ix.files[f].facts.path
    }

    /// Whether a name used in file `f` may refer to the definition: one
    /// project, a declared dependency, or no project at all.
    fn reachable(&self, f: usize, def: usize) -> bool {
        let used = self.ix.defs[def].file;
        used == f
            || self
                .workspace
                .may_reference(self.path_of(f).as_str(), self.path_of(used).as_str())
    }

    fn site(&self, f: usize, scope: &Scope) -> Site {
        let path = scope_path(scope);
        let mut module = self.ix.files[f].module;
        // The inline modules the scope passes through.
        let mut taken = 0;
        while taken < path.len() && self.ix.files[f].inline.contains(&path[..=taken]) {
            match self.child_mod(module, path[taken].as_str()) {
                Some(next) => module = next,
                None => break,
            }
            taken += 1;
        }
        Site {
            file: f,
            module,
            scope: path,
        }
    }

    fn child_mod(&self, parent: usize, name: &str) -> Option<usize> {
        let krate = self.ix.mods[parent].krate;
        let path = if self.ix.mods[parent].path.is_empty() {
            name.to_string()
        } else {
            format!("{}::{name}", self.ix.mods[parent].path)
        };
        let id = *self.ix.mod_ix.get(&(krate, path))?;
        // Only a module this file holds inline, or one it declares.
        Some(id)
    }

    // ---- names

    fn accepts(&self, def: usize, want: Want) -> bool {
        let rec = &self.ix.defs[def];
        match want {
            Want::Any => true,
            Want::Type => rec.def.local.ns == Namespace::Type || rec.def.kind == DefKind::Module,
            Want::Value => {
                rec.def.local.ns == Namespace::Value
                    || matches!(rec.def.kind, DefKind::Struct | DefKind::Macro)
            }
        }
    }

    fn pick(&self, ids: &[usize], want: Want, from: usize) -> Option<usize> {
        ids.iter()
            .copied()
            .filter(|d| self.accepts(*d, want) && self.reachable(from, *d))
            .min_by_key(|d| {
                // The primary definition of a key is the one without twins.
                self.ix.defs[*d].def.local.disambig.is_some()
            })
    }

    /// A module item by name, or a child module.
    fn item_in(&self, module: usize, name: &Name, want: Want, from: usize) -> Option<Ent> {
        if let Some(ids) = self.ix.items.get(&(module, name.clone())) {
            if let Some(d) = self.pick(ids, want, from) {
                // A `mod m` declaration names the module, not a definition.
                if self.ix.defs[d].def.kind == DefKind::Module {
                    if let Some(child) = self.child_mod(module, name.as_str()) {
                        return Some(Ent::Mod(child));
                    }
                }
                return Some(Ent::Def(d));
            }
        }
        if want != Want::Value {
            if let Some(child) = self.child_mod(module, name.as_str()) {
                if self.declared(module, name) {
                    return Some(Ent::Mod(child));
                }
            }
        }
        None
    }

    /// Whether the module has a `mod name` or a file for it in the workspace.
    fn declared(&self, module: usize, name: &Name) -> bool {
        self.child_mod(module, name.as_str()).is_some()
    }

    /// The name as the scope around `site` gives it.
    fn find(
        &self,
        site: &Site,
        name: &Name,
        want: Want,
        depth: usize,
    ) -> Option<(Ent, EvidenceKind)> {
        if depth > MAX_HOPS {
            return None;
        }
        // An item of a function around the site.
        let file = site.file;
        for len in (0..=site.scope.len()).rev() {
            let outer = &site.scope[..len];
            if let Some(ids) = self.ix.locals.get(&(file, outer.to_vec(), name.clone())) {
                if let Some(d) = self.pick(ids, want, file) {
                    return Some((Ent::Def(d), EvidenceKind::SameScope));
                }
            }
        }
        // A `use` of the scopes around, innermost first, then the module's.
        let rec = &self.ix.files[file];
        let mut scoped: Vec<&ImportRec> = rec
            .imports
            .iter()
            .filter(|i| {
                i.module == site.module
                    && !i.glob
                    && i.local.as_ref() == Some(name)
                    && site.scope.starts_with(&i.scope)
            })
            .collect();
        scoped.sort_by_key(|i| std::cmp::Reverse(i.scope.len()));
        for import in scoped {
            if let Some(ent) = self.import_target(file, import, want, depth) {
                return Some((ent, EvidenceKind::ImportedName));
            }
        }
        if let Some(ent) = self.item_in(site.module, name, want, file) {
            return Some((ent, EvidenceKind::SameScope));
        }
        // Other files of the same module: an inline module is written in one
        // file, and its imports are found there.
        for other in self.ix.files_of.get(&site.module).into_iter().flatten() {
            if *other == file {
                continue;
            }
            for import in self.ix.files[*other]
                .imports
                .iter()
                .filter(|i| i.module == site.module && !i.glob && i.local.as_ref() == Some(name))
            {
                if let Some(ent) = self.import_target(*other, import, want, depth) {
                    return Some((ent, EvidenceKind::ImportedName));
                }
            }
        }
        // Globs.
        for import in rec
            .imports
            .iter()
            .filter(|i| i.module == site.module && i.glob && site.scope.starts_with(&i.scope))
        {
            let Some(Ent::Mod(m)) = self.walk(site, &import.segs, Want::Type, depth + 1) else {
                if let Some(Ent::Def(d)) = self.walk(site, &import.segs, Want::Type, depth + 1) {
                    // `use Enum::*`.
                    if let Some(v) = self.member_named(d, name) {
                        return Some((Ent::Def(v), EvidenceKind::StarImport));
                    }
                }
                continue;
            };
            if let Some(ent) = self.exported_from(m, name, want, file, depth + 1) {
                return Some((ent, EvidenceKind::StarImport));
            }
        }
        if want != Want::Value && PRELUDE.contains(&name.as_str()) {
            return Some((Ent::Builtin, EvidenceKind::SameScope));
        }
        if PRELUDE.contains(&name.as_str()) {
            return Some((Ent::Builtin, EvidenceKind::SameScope));
        }
        None
    }

    /// What module `m` offers under `name`: its items, its re-exports, its globs.
    fn exported_from(
        &self,
        m: usize,
        name: &Name,
        want: Want,
        from: usize,
        depth: usize,
    ) -> Option<Ent> {
        if depth > MAX_HOPS {
            return None;
        }
        if let Some(ent) = self.item_in(m, name, want, from) {
            return Some(ent);
        }
        for f in self.ix.files_of.get(&m).into_iter().flatten().copied() {
            let file = &self.ix.files[f];
            for import in file.imports.iter().filter(|i| i.module == m) {
                if !import.glob && import.local.as_ref() == Some(name) {
                    if let Some(ent) = self.import_target(f, import, want, depth) {
                        return Some(ent);
                    }
                }
            }
            for import in file.imports.iter().filter(|i| i.module == m && i.glob) {
                let site = Site {
                    file: f,
                    module: m,
                    scope: Vec::new(),
                };
                match self.walk(&site, &import.segs, Want::Type, depth + 1) {
                    Some(Ent::Mod(inner)) if inner != m => {
                        if let Some(ent) = self.exported_from(inner, name, want, from, depth + 1) {
                            return Some(ent);
                        }
                    }
                    Some(Ent::Def(d)) => {
                        if let Some(v) = self.member_named(d, name) {
                            return Some(Ent::Def(v));
                        }
                    }
                    _ => {}
                }
            }
        }
        None
    }

    /// What an import binds, found once.
    fn import_target(&self, f: usize, import: &ImportRec, want: Want, depth: usize) -> Option<Ent> {
        let at = import as *const ImportRec as usize;
        let key = (f, at);
        if let Some(known) = self.imported.borrow().get(&key) {
            return *known;
        }
        // Guard against a cycle: the name is unresolved while it is looked up.
        self.imported.borrow_mut().insert(key, None);
        let site = Site {
            file: f,
            module: import.module,
            scope: import.scope.clone(),
        };
        let found = self.walk_import(&site, &import.segs, want, depth + 1);
        self.imported.borrow_mut().insert(key, found);
        found
    }

    /// An import's path: its first segment is a crate, `crate`, `self`,
    /// `super`, or a name of the module around it.
    fn walk_import(&self, site: &Site, segs: &[Name], want: Want, depth: usize) -> Option<Ent> {
        self.walk(site, segs, want, depth)
    }

    /// The head segment of a path.
    fn head(&self, site: &Site, seg: &Name, want: Want, depth: usize) -> Option<Ent> {
        match seg.as_str() {
            "crate" => Some(Ent::Mod(
                self.ix.crates[self.ix.mods[site.module].krate].root,
            )),
            "self" => Some(Ent::Mod(site.module)),
            "super" => self.ix.mods[site.module].parent.map(Ent::Mod),
            "Self" => match self.self_typed(site) {
                Typed::Def(t) => Some(Ent::Def(t)),
                Typed::Dyn(t) => Some(Ent::Def(t)),
                _ => None,
            },
            name => {
                let name = Name::new(name);
                if let Some((ent, _)) = self.find(site, &name, want, depth) {
                    return Some(ent);
                }
                self.crate_named(site, name.as_str())
            }
        }
    }

    /// A crate the workspace holds, a standard crate, or a crate nothing here
    /// declares.
    fn crate_named(&self, site: &Site, name: &str) -> Option<Ent> {
        let own = self.ix.mods[site.module].krate;
        if self.ix.crates[own].name == name {
            // The package's own library from its binary or tests.
            if self.ix.crates[own].kind == CrateKind::Lib {
                return Some(Ent::Mod(self.ix.crates[own].root));
            }
        }
        let user = self.path_of(site.file).as_str();
        let mut found = self
            .ix
            .crates
            .iter()
            .enumerate()
            .filter(|(_, c)| c.name == name)
            .filter(|(_, c)| {
                c.kind == CrateKind::Lib
                    && (self.workspace.may_reference(user, c.root_file.as_str())
                        || c.root_file == user)
            })
            .map(|(i, _)| i);
        if let Some(krate) = found.next() {
            return Some(Ent::Mod(self.ix.crates[krate].root));
        }
        if STD_CRATES.contains(&name) {
            return Some(Ent::External);
        }
        None
    }

    /// A path from `site`: the head, then each segment into what the one before
    /// it names.
    fn walk(&self, site: &Site, segs: &[Name], want: Want, depth: usize) -> Option<Ent> {
        if depth > MAX_HOPS * 2 {
            return None;
        }
        let (first, rest) = segs.split_first()?;
        let head_want = if rest.is_empty() { want } else { Want::Type };
        let mut ent = self.head(site, first, head_want, depth)?;
        for (i, seg) in rest.iter().enumerate() {
            let last = i + 1 == rest.len();
            let step_want = if last { want } else { Want::Type };
            ent = self.step(site, ent, seg, step_want, depth)?;
        }
        Some(ent)
    }

    fn step(&self, site: &Site, ent: Ent, seg: &Name, want: Want, depth: usize) -> Option<Ent> {
        match ent {
            // `super::super::m` and `a::self`: a path leaves a module by `super`.
            Ent::Mod(m) if seg.as_str() == "super" => self.ix.mods[m].parent.map(Ent::Mod),
            Ent::Mod(m) if seg.as_str() == "self" => Some(Ent::Mod(m)),
            Ent::Mod(m) => self.exported_from(m, seg, want, site.file, depth + 1),
            Ent::Def(d) => {
                let kind = self.ix.defs[d].def.kind;
                match kind {
                    DefKind::TypeAlias => {
                        let target = self.alias_target(d, depth + 1)?;
                        self.step(site, target, seg, want, depth + 1)
                    }
                    DefKind::Struct | DefKind::Enum | DefKind::Trait => {
                        self.member_named(d, seg).map(Ent::Def)
                    }
                    DefKind::Module => None,
                    _ => None,
                }
            }
            Ent::External => Some(Ent::External),
            Ent::Builtin => Some(Ent::Builtin),
        }
    }

    // ---- types

    fn type_named(&self, site: &Site, name: &Name) -> Option<usize> {
        match self.find(site, name, Want::Type, 0)?.0 {
            Ent::Def(d) => self.follow_alias(d, 0),
            _ => None,
        }
    }

    fn follow_alias(&self, d: usize, depth: usize) -> Option<usize> {
        if self.ix.defs[d].def.kind != DefKind::TypeAlias {
            return Some(d);
        }
        if depth > MAX_DEPTH {
            return None;
        }
        match self.alias_target(d, depth + 1)? {
            Ent::Def(t) => self.follow_alias(t, depth + 1),
            _ => None,
        }
    }

    /// `type A = B<..>;`: what `B` names where the alias is written.
    fn alias_target(&self, d: usize, depth: usize) -> Option<Ent> {
        let rec = &self.ix.defs[d];
        let text = rec.def.signature.as_deref()?;
        let after = text.split_once('=')?.1;
        let path = type_path(after)?;
        let site = self.def_site(d);
        self.walk(&site, &path, Want::Type, depth + 1)
    }

    fn def_site(&self, d: usize) -> Site {
        let rec = &self.ix.defs[d];
        Site {
            file: rec.file,
            module: rec.home,
            scope: rec.def.local.container.clone(),
        }
    }

    /// The type a written type name stands for, where `site` writes it.
    fn typed(&self, site: &Site, text: &str, depth: usize) -> Typed {
        if depth > MAX_DEPTH {
            return Typed::Unknown;
        }
        if let Some(tr) = text.strip_prefix("dyn ") {
            let segs = path_names(tr);
            return match self.walk(site, &segs, Want::Type, 0) {
                Some(Ent::Def(d)) if self.ix.defs[d].def.kind == DefKind::Trait => Typed::Dyn(d),
                Some(Ent::External | Ent::Builtin) => Typed::External,
                _ => Typed::Unknown,
            };
        }
        let segs = path_names(text);
        if segs.is_empty() {
            return Typed::Unknown;
        }
        match self.walk(site, &segs, Want::Type, 0) {
            Some(Ent::Def(d)) => match self.ix.defs[d].def.kind {
                DefKind::TypeAlias => match self.follow_alias(d, 0) {
                    Some(t) => self.typed_def(t),
                    None => Typed::Unknown,
                },
                _ => self.typed_def(d),
            },
            Some(Ent::External | Ent::Builtin) => Typed::External,
            _ => Typed::Unknown,
        }
    }

    fn typed_def(&self, d: usize) -> Typed {
        match self.ix.defs[d].def.kind {
            DefKind::Trait => Typed::Dyn(d),
            DefKind::Struct | DefKind::Enum => Typed::Def(d),
            _ => Typed::Unknown,
        }
    }

    // ---- members

    /// Every member of a type or trait: the methods, fields and variants of
    /// its impls in the crate, and a trait's own items.
    fn members_of(&self, t: usize) -> Vec<usize> {
        if let Some(known) = self.members.borrow().get(&t) {
            return known.clone();
        }
        // A cycle through a glob finds the type without members.
        self.members.borrow_mut().insert(t, Vec::new());
        let rec = &self.ix.defs[t];
        let name = rec.def.local.name.clone();
        let mut found = Vec::new();
        for (home, ids) in self.ix.groups_named.get(&name).into_iter().flatten() {
            // The group is this type's when the name resolves to it from there.
            let first = ids[0];
            let site = Site {
                file: self.ix.defs[first].file,
                module: *home,
                scope: Vec::new(),
            };
            let same = self.ix.mods[*home].krate == self.ix.mods[rec.home].krate
                || self.reachable(self.ix.defs[first].file, t);
            if !same {
                continue;
            }
            let here = matches!(
                self.find(&site, &name, Want::Type, 0),
                Some((Ent::Def(d), _)) if self.follow_alias(d, 0) == Some(t) || d == t
            );
            if here {
                found.extend(ids.iter().copied());
            }
        }
        found.sort_unstable();
        found.dedup();
        self.members.borrow_mut().insert(t, found.clone());
        found
    }

    fn member_named(&self, t: usize, name: &Name) -> Option<usize> {
        let kind = self.ix.defs[t].def.kind;
        let all = self.members_of(t);
        let named: Vec<usize> = all
            .iter()
            .copied()
            .filter(|m| self.ix.defs[*m].def.local.name == *name)
            .collect();
        // An inherent member first, then a trait impl's.
        let pick = named
            .iter()
            .copied()
            .min_by_key(|m| self.ix.defs[*m].def.local.disambig.is_some());
        if pick.is_some() || kind == DefKind::Trait {
            return pick;
        }
        self.provided_by_trait(t, name)
    }

    /// A default method a trait the type implements provides.
    fn provided_by_trait(&self, t: usize, name: &Name) -> Option<usize> {
        for tr in self.traits_of(t) {
            if let Some(m) = self
                .members_of(tr)
                .into_iter()
                .find(|m| self.ix.defs[*m].def.local.name == *name)
            {
                return Some(m);
            }
        }
        None
    }

    /// The traits a type has an `impl` for, in the repository. Empty while the
    /// table is not built, which `met_unfinished_table` records.
    #[allow(
        clippy::disallowed_methods,
        reason = "a type the built table has no row for has no `impl` of a trait in the repository"
    )]
    fn traits_of(&self, t: usize) -> Vec<usize> {
        if !self.trait_table() {
            return Vec::new();
        }
        match &*self.implemented.borrow() {
            Traits::Built(table) => table.get(&t).cloned().unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    /// Which traits each type implements, found once, before any lookup reads it
    /// (`run` builds it first). `false` when it is asked for during its own build:
    /// the lookup then meets no table, and the engine says so instead of reading
    /// an empty one as complete.
    fn trait_table(&self) -> bool {
        match &*self.implemented.borrow() {
            Traits::Built(_) => return true,
            Traits::Building => {
                self.met_unfinished_table.set(true);
                return false;
            }
            Traits::Unbuilt => {}
        }
        *self.implemented.borrow_mut() = Traits::Building;
        let mut table: HashMap<usize, Vec<usize>> = HashMap::new();
        for (file, home, type_name, tr) in &self.ix.impls {
            let site = Site {
                file: *file,
                module: *home,
                scope: Vec::new(),
            };
            let Some(subject) = self.type_named(&site, type_name) else {
                continue;
            };
            let mut segs: Vec<Name> = tr.qualifier.clone();
            segs.push(tr.name.clone());
            if let Some(Ent::Def(d)) = self.walk(&site, &segs, Want::Type, 0) {
                if self.ix.defs[d].def.kind == DefKind::Trait {
                    table.entry(subject).or_default().push(d);
                }
            }
        }
        *self.implemented.borrow_mut() = Traits::Built(table);
        true
    }

    /// The types that implement a trait; `None` when the table is not built.
    fn implementers(&self, tr: usize) -> Option<Vec<usize>> {
        if !self.trait_table() {
            return None;
        }
        let Traits::Built(table) = &*self.implemented.borrow() else {
            return None;
        };
        let mut out: Vec<usize> = table
            .iter()
            .filter(|(_, traits)| traits.contains(&tr))
            .map(|(t, _)| *t)
            .collect();
        out.sort_unstable();
        Some(out)
    }

    /// The type a field declares: the first path of the type written after the
    /// colon.
    fn field_type(&self, field: usize, depth: usize) -> Typed {
        let rec = &self.ix.defs[field];
        let Some(text) = rec.def.signature.as_deref() else {
            return Typed::Unknown;
        };
        let Some((_, ty)) = text.split_once(':') else {
            return Typed::Unknown;
        };
        let Some(written) = type_text(ty) else {
            return Typed::Unknown;
        };
        // The owner's generics are not in the signature: a single capital
        // letter is a parameter of it.
        if is_generic_name(&written) {
            return Typed::Unknown;
        }
        self.typed(&self.def_site(field), &written, depth + 1)
    }

    /// What a function declares it returns, `Result`/`Option` unwrapped on request.
    fn return_type(&self, f: usize, unwrapped: bool, depth: usize) -> Typed {
        let rec = &self.ix.defs[f];
        let Some(text) = rec.def.signature.as_deref() else {
            return Typed::Unknown;
        };
        let Some(ret) = return_text(text) else {
            return Typed::Unknown;
        };
        let inner = if unwrapped {
            unwrap_text(&ret).unwrap_or(ret)
        } else {
            ret
        };
        let Some(written) = type_text(&inner) else {
            return Typed::Unknown;
        };
        if written == "Self" {
            return match rec.rest.last() {
                Some(owner) => {
                    let site = self.def_site(f);
                    self.type_named(&site, owner)
                        .map_or(Typed::Unknown, |t| self.typed_def(t))
                }
                None => Typed::Unknown,
            };
        }
        if is_generic_name(&written) {
            return Typed::Unknown;
        }
        self.typed(&self.def_site(f), &written, depth + 1)
    }

    // ---- references

    fn relation_of(&self, kind: UseKind, target: Option<usize>) -> Relation {
        let constructor = target.is_some_and(|t| {
            matches!(
                self.ix.defs[t].def.kind,
                DefKind::Struct | DefKind::EnumMember
            )
        });
        match kind {
            UseKind::Call if constructor => Relation::Instantiates,
            UseKind::Call | UseKind::Decorate => Relation::Calls,
            UseKind::Read => Relation::Reads,
            UseKind::Write => Relation::Writes,
            UseKind::TypeUse => Relation::TypeUses,
            // A name handed on is a function passed by name, or a plain read of
            // a constant or a field in an argument position.
            UseKind::PassAsValue => match target {
                Some(t) if !self.is_callable(t) => Relation::Reads,
                _ => Relation::PassesAsValue,
            },
            UseKind::Instantiate => Relation::Instantiates,
            UseKind::Override => Relation::Overrides,
        }
    }

    fn is_callable(&self, d: usize) -> bool {
        matches!(
            self.ix.defs[d].def.kind,
            DefKind::Function
                | DefKind::Method
                | DefKind::Struct
                | DefKind::EnumMember
                | DefKind::Macro
        )
    }

    fn reference(&self, f: usize, r: &Ref, out: &mut Vec<Binding>) {
        self.bind(f, r, out);
        if r.receiver != Receiver::None
            && r.use_kind != UseKind::Override
            && !r.name.as_str().ends_with('!')
        {
            for (name, resolution) in self.chain_reads(f, r) {
                out.push(Binding {
                    file: self.path_of(f).clone(),
                    site: r.site.clone(),
                    from: r.enclosing.clone(),
                    relation: Relation::Reads,
                    name,
                    resolution,
                    entry_guard: false,
                });
            }
        }
        // `Type::name` and `Enum::Variant` also use the type they are written
        // through.
        if r.receiver == Receiver::None
            && !r.qualifier.is_empty()
            && r.use_kind != UseKind::TypeUse
            && r.use_kind != UseKind::Override
            && !r.name.as_str().ends_with('!')
            && r.qualifier.as_slice() != [Name::new("Self")]
        {
            let site = self.site(f, &r.enclosing);
            if let Some(Ent::Def(d)) = self.walk(&site, &r.qualifier, Want::Type, 0) {
                if matches!(
                    self.ix.defs[d].def.kind,
                    DefKind::Struct | DefKind::Enum | DefKind::Trait | DefKind::TypeAlias
                ) {
                    let last = r
                        .qualifier
                        .last()
                        .cloned()
                        .unwrap_or_else(|| r.name.clone());
                    out.push(Binding {
                        file: self.path_of(f).clone(),
                        site: r.site.clone(),
                        from: r.enclosing.clone(),
                        relation: Relation::Reads,
                        name: last,
                        resolution: self.def_resolution(f, d, EvidenceKind::ModuleAttribute),
                        entry_guard: false,
                    });
                }
            }
        }
    }

    fn bind(&self, f: usize, r: &Ref, out: &mut Vec<Binding>) {
        let outcome = self.outcome(f, r);
        let (resolution, target) = match outcome {
            Outcome::Found(d, kind) => {
                if !self.reachable(f, d) {
                    (
                        Resolution::Unresolved {
                            reason: UnresolvedReason::OutsideProject,
                        },
                        None,
                    )
                } else {
                    (
                        Resolution::Resolved {
                            target: self.ix.defs[d].key.clone(),
                            evidence: Evidence::new(kind),
                        },
                        Some(d),
                    )
                }
            }
            Outcome::Candidates(ids, reason) => {
                let ids: Vec<usize> = ids.into_iter().filter(|d| self.reachable(f, *d)).collect();
                if ids.is_empty() {
                    (
                        Resolution::Unresolved {
                            reason: UnresolvedReason::NoCandidates,
                        },
                        None,
                    )
                } else {
                    (
                        Resolution::Candidate {
                            targets: ids.iter().map(|d| self.ix.defs[*d].key.clone()).collect(),
                            reason,
                        },
                        None,
                    )
                }
            }
            Outcome::Unresolved(reason) => (Resolution::Unresolved { reason }, None),
        };
        out.push(Binding {
            file: self.path_of(f).clone(),
            site: r.site.clone(),
            from: r.enclosing.clone(),
            relation: self.relation_of(r.use_kind, target),
            name: Name::new(r.name.as_str().trim_end_matches('!')),
            resolution,
            entry_guard: false,
        });
    }

    fn outcome(&self, f: usize, r: &Ref) -> Outcome {
        let site = self.site(f, &r.enclosing);
        if r.name.as_str().ends_with('!') {
            return self.macro_outcome(&site, r);
        }
        if r.use_kind == UseKind::Override {
            return self.override_outcome(&site, r);
        }
        match &r.receiver {
            Receiver::None => self.path_outcome(&site, r),
            receiver => self.method_outcome(&site, r, receiver),
        }
    }

    fn outcome_of(&self, ent: Option<Ent>, kind: EvidenceKind) -> Outcome {
        match ent {
            Some(Ent::Def(d)) => Outcome::Found(d, kind),
            Some(Ent::Mod(_)) => Outcome::Unresolved(UnresolvedReason::Local),
            Some(Ent::External) => Outcome::Unresolved(UnresolvedReason::External),
            Some(Ent::Builtin) => Outcome::Unresolved(UnresolvedReason::Builtin),
            None => Outcome::Unresolved(UnresolvedReason::NoBinding),
        }
    }

    fn path_outcome(&self, site: &Site, r: &Ref) -> Outcome {
        let want = match r.use_kind {
            UseKind::TypeUse => Want::Type,
            UseKind::Call | UseKind::PassAsValue => Want::Value,
            UseKind::Instantiate => Want::Value,
            _ => Want::Any,
        };
        let mut segs: Vec<Name> = r.qualifier.clone();
        segs.push(r.name.clone());
        let bare = segs.len() == 1;
        let ent = self.walk(site, &segs, want, 0);
        let kind = if bare {
            self.find(site, &r.name, want, 0)
                .map_or(EvidenceKind::SameScope, |(_, k)| k)
        } else {
            EvidenceKind::ModuleAttribute
        };
        if ent.is_none() && !bare {
            // A path whose head the repository does not hold is a crate's.
            if let Some(head) = segs.first() {
                if self.head(site, head, Want::Type, 0).is_none() {
                    return Outcome::Unresolved(UnresolvedReason::External);
                }
            }
            return Outcome::Unresolved(UnresolvedReason::NoBinding);
        }
        self.outcome_of(ent, kind)
    }

    fn macro_outcome(&self, site: &Site, r: &Ref) -> Outcome {
        let name = Name::new(r.name.as_str().trim_end_matches('!'));
        let mut found: Vec<usize> = Vec::new();
        let krate = self.ix.mods[site.module].krate;
        if !r.qualifier.is_empty() {
            let segs = r.qualifier.clone();
            if let Some(Ent::Mod(m)) = self.walk(site, &segs, Want::Type, 0) {
                if let Some(ids) = self.ix.items.get(&(m, name.clone())) {
                    found.extend(ids.iter().copied());
                }
            } else {
                return Outcome::Unresolved(UnresolvedReason::External);
            }
        } else {
            for d in self.ix.macros.get(&name).into_iter().flatten() {
                let same_crate = self.ix.mods[self.ix.defs[*d].home].krate == krate;
                if same_crate || self.reachable(site.file, *d) {
                    found.push(*d);
                }
            }
        }
        found.retain(|d| self.ix.defs[*d].def.kind == DefKind::Macro);
        found.sort_unstable();
        found.dedup();
        match found.as_slice() {
            [] if STD_MACROS.contains(&name.as_str()) => {
                Outcome::Unresolved(UnresolvedReason::Builtin)
            }
            [] => Outcome::Unresolved(UnresolvedReason::External),
            [only] => Outcome::Found(*only, EvidenceKind::SameScope),
            many => Outcome::Candidates(many.to_vec(), CandidateReason::UntypedReceiver),
        }
    }

    /// `impl Tr for X`: the type implements the trait; a method of it replaces
    /// the trait's.
    fn override_outcome(&self, site: &Site, r: &Ref) -> Outcome {
        let is_method = matches!(&r.enclosing, Scope::Def(k) if k.ns == Namespace::Value);
        if !is_method {
            let mut segs = r.qualifier.clone();
            segs.push(r.name.clone());
            let ent = self.walk(site, &segs, Want::Type, 0);
            return self.outcome_of(ent, EvidenceKind::Overridden);
        }
        let Some(tr) = self.trait_of_path(site, &r.qualifier) else {
            return Outcome::Unresolved(UnresolvedReason::External);
        };
        match self.members_of(tr).into_iter().find(|m| {
            self.ix.defs[*m].def.local.name == r.name
                && self.ix.defs[*m].def.local.disambig.is_none()
        }) {
            Some(m) => Outcome::Found(m, EvidenceKind::Overridden),
            None => Outcome::Unresolved(UnresolvedReason::NoMember),
        }
    }

    fn trait_of_path(&self, site: &Site, path: &[Name]) -> Option<usize> {
        match self.walk(site, path, Want::Type, 0)? {
            Ent::Def(d) if self.ix.defs[d].def.kind == DefKind::Trait => Some(d),
            _ => None,
        }
    }

    // ---- method calls

    /// Where a method chain starts, how many of its qualifier's names the start
    /// already takes, and how it is known; `None` where the head is a local
    /// nothing types.
    fn chain_start(
        &self,
        site: &Site,
        r: &Ref,
        receiver: &Receiver,
    ) -> Option<(Typed, usize, EvidenceKind)> {
        Some(match receiver {
            Receiver::SelfType => (self.self_typed(site), 0, EvidenceKind::SelfReceiver),
            Receiver::Typed(t) => (
                self.typed(site, t.as_str(), 0),
                0,
                EvidenceKind::TypedReceiver,
            ),
            Receiver::New(t) => (
                self.typed(site, t.as_str(), 0),
                0,
                EvidenceKind::Constructed,
            ),
            Receiver::CallResult(path) => {
                (self.call_result(site, path), 0, EvidenceKind::FactoryReturn)
            }
            // `Unit.method()`: the first name of the chain is a unit struct, which
            // is its own value; the names after it are fields.
            Receiver::Unknown => {
                let head = r.qualifier.first()?;
                let unit = self.unit_struct(site, head)?;
                (Typed::Def(unit), 1, EvidenceKind::TypedReceiver)
            }
            Receiver::Module(_) | Receiver::None => return None,
        })
    }

    fn method_outcome(&self, site: &Site, r: &Ref, receiver: &Receiver) -> Outcome {
        let Some((start, skip, evidence)) = self.chain_start(site, r, receiver) else {
            return self.untyped_outcome(site, r);
        };
        // The fields written after the receiver.
        let mut at = start;
        for field in r.qualifier.iter().skip(skip) {
            at = match at {
                Typed::Def(t) => match self.member_named(t, field) {
                    Some(m) if self.ix.defs[m].def.kind == DefKind::Field => self.field_type(m, 0),
                    _ => Typed::Unknown,
                },
                _ => Typed::Unknown,
            };
        }
        match at {
            Typed::Def(t) => match self.member_named(t, &r.name) {
                Some(m) => {
                    let inherited = !matches!(self.ix.defs[m].rest.first(), Some(n) if *n == self.ix.defs[t].def.local.name);
                    Outcome::Found(
                        m,
                        if inherited {
                            EvidenceKind::Inherited
                        } else {
                            evidence
                        },
                    )
                }
                None => Outcome::Unresolved(UnresolvedReason::NoMember),
            },
            Typed::Dyn(tr) => self.dispatched(site, tr, &r.name),
            Typed::External => Outcome::Unresolved(UnresolvedReason::ExternalBase),
            Typed::Unknown => self.untyped_outcome(site, r),
        }
    }

    /// The fields a chain passes through on its way to its last name are read:
    /// `e.request.url` reads the field `request`. Where nothing types the value
    /// each is an untyped read like the last name; a type the repository does
    /// not hold and a trait object say nothing of ours. A member its type lacks
    /// is the line a removal strands: it is bound to nothing, as the last name is.
    fn chain_reads(&self, f: usize, r: &Ref) -> Vec<(Name, Resolution)> {
        let site = self.site(f, &r.enclosing);
        let (mut at, skip, evidence) = match self.chain_start(&site, r, &r.receiver) {
            Some(start) => start,
            None if r.receiver == Receiver::Unknown => (Typed::Unknown, 1, EvidenceKind::SameScope),
            None => return Vec::new(),
        };
        let fields = r.qualifier.get(skip..).unwrap_or(&[]);
        let mut reads = Vec::new();
        for (i, field) in fields.iter().enumerate() {
            match at {
                Typed::Def(t) => {
                    let found = self.member_named(t, field);
                    let Some(m) = found.filter(|m| self.ix.defs[*m].def.kind == DefKind::Field)
                    else {
                        if found.is_none() {
                            reads.push((
                                field.clone(),
                                Resolution::Unresolved {
                                    reason: UnresolvedReason::NoMember,
                                },
                            ));
                        }
                        break;
                    };
                    if self.reachable(f, m) {
                        reads.push((
                            field.clone(),
                            Resolution::Resolved {
                                target: self.ix.defs[m].key.clone(),
                                evidence: Evidence::new(evidence),
                            },
                        ));
                    }
                    at = self.field_type(m, 0);
                }
                Typed::Dyn(_) | Typed::External => break,
                Typed::Unknown => {
                    reads.extend(fields[i..].iter().map(|name| {
                        (
                            name.clone(),
                            Resolution::Unresolved {
                                reason: UnresolvedReason::UntypedRead,
                            },
                        )
                    }));
                    break;
                }
            }
        }
        reads
    }

    /// The struct a capitalised name stands for as a value, as `Unit` does for a
    /// unit struct. A name that is a local is lower case and no struct.
    fn unit_struct(&self, site: &Site, name: &Name) -> Option<usize> {
        if !name.as_str().starts_with(char::is_uppercase) {
            return None;
        }
        match self.find(site, name, Want::Value, 0)?.0 {
            Ent::Def(d) if self.ix.defs[d].def.kind == DefKind::Struct => self.follow_alias(d, 0),
            _ => None,
        }
    }

    /// The type of `self` where the site is a method.
    fn self_typed(&self, site: &Site) -> Typed {
        // The method's container is the type or trait the `impl` or the trait
        // names.
        let Some(owner) = self.owner_name(site) else {
            return Typed::Unknown;
        };
        let owner_site = Site {
            file: site.file,
            module: site.module,
            scope: Vec::new(),
        };
        match self.find(&owner_site, &owner, Want::Type, 0) {
            Some((Ent::Def(d), _)) => match self.follow_alias(d, 0) {
                Some(t) => self.typed_def(t),
                None => Typed::Unknown,
            },
            Some((Ent::External | Ent::Builtin, _)) => Typed::External,
            _ => Typed::Unknown,
        }
    }

    /// The type name that holds the method the site is in: the container before
    /// the method's own name.
    fn owner_name(&self, site: &Site) -> Option<Name> {
        // The scope path is the method's container then its name; closures and
        // nested functions lengthen it. Walk out until a name that has members.
        for len in (1..=site.scope.len()).rev() {
            if len >= 2 {
                let owner = &site.scope[len - 2];
                if self.ix.groups.contains_key(&(site.module, owner.clone())) {
                    return Some(owner.clone());
                }
            }
        }
        None
    }

    fn call_result(&self, site: &Site, path: &[Name]) -> Typed {
        let unwrapped = path.last().is_some_and(|n| n.as_str() == "?");
        let names: Vec<Name> = path.iter().filter(|n| n.as_str() != "?").cloned().collect();
        match self.walk(site, &names, Want::Value, 0) {
            Some(Ent::Def(f))
                if matches!(
                    self.ix.defs[f].def.kind,
                    DefKind::Function | DefKind::Method
                ) =>
            {
                self.return_type(f, unwrapped, 0)
            }
            Some(Ent::External | Ent::Builtin) => Typed::External,
            _ => Typed::Unknown,
        }
    }

    /// A call through a trait object or a bound: the trait's method and every
    /// implementer's, as candidates.
    fn dispatched(&self, _site: &Site, tr: usize, name: &Name) -> Outcome {
        let mut targets: Vec<usize> = Vec::new();
        if let Some(m) = self
            .members_of(tr)
            .into_iter()
            .find(|m| self.ix.defs[*m].def.local.name == *name)
        {
            targets.push(m);
        }
        let trait_name = self.ix.defs[tr].def.local.name.clone();
        // A set of implementers read before the table was built is not the
        // repository's: named as a receiver the resolver could not follow.
        let Some(implementers) = self.implementers(tr) else {
            return Outcome::Unresolved(UnresolvedReason::Opaque);
        };
        for implementer in implementers {
            for m in self.members_of(implementer) {
                let rec = &self.ix.defs[m];
                let by_trait = matches!(
                    &rec.def.local.disambig,
                    Some(Disambig::ImplOf(t)) if t.as_str().rsplit("::").next() == Some(trait_name.as_str())
                );
                if rec.def.local.name == *name && by_trait {
                    targets.push(m);
                }
            }
        }
        targets.sort_unstable();
        targets.dedup();
        if targets.is_empty() {
            return Outcome::Unresolved(UnresolvedReason::NoMember);
        }
        Outcome::Candidates(targets, CandidateReason::Dispatch)
    }

    /// A method called on a value nothing types: any method of that name on a
    /// type the file names.
    fn untyped_outcome(&self, site: &Site, r: &Ref) -> Outcome {
        let file = &self.ix.files[site.file];
        let Some(all) = self.ix.methods.get(&r.name) else {
            // No `impl` of the repository has a method of this name: the call is
            // the standard library's or a dependency's (`items.join(..)` beside a
            // free function `join`), and a free function is never called with
            // method syntax.
            return if r.use_kind == UseKind::Call {
                Outcome::Unresolved(UnresolvedReason::External)
            } else {
                Outcome::Unresolved(UnresolvedReason::UntypedRead)
            };
        };
        let mut targets: Vec<usize> = all
            .iter()
            .copied()
            .filter(|m| self.reachable(site.file, *m))
            .filter(|m| self.visible_from(*m, site))
            .filter(|m| {
                let rec = &self.ix.defs[*m];
                rec.file == site.file
                    || rec
                        .rest
                        .first()
                        .is_some_and(|owner| file.names.contains(owner))
                    || matches!(
                        &rec.def.local.disambig,
                        Some(Disambig::ImplOf(t)) if file.names.contains(&Name::new(t.as_str().rsplit("::").next().unwrap_or("")))
                    )
            })
            .collect();
        targets.sort_unstable();
        targets.dedup();
        if targets.is_empty() {
            return Outcome::Unresolved(UnresolvedReason::NoCandidates);
        }
        if targets.len() > MAX_CANDIDATES {
            return Outcome::Unresolved(UnresolvedReason::TooManyCandidates);
        }
        Outcome::Candidates(targets, CandidateReason::UntypedReceiver)
    }

    /// A private item is named only in its module and the ones below it; a
    /// `pub(crate)` one in its crate (#599).
    fn visible_from(&self, def: usize, site: &Site) -> bool {
        let rec = &self.ix.defs[def];
        let home = &self.ix.mods[rec.home];
        let here = &self.ix.mods[site.module];
        match rec.def.visibility {
            Visibility::Private => {
                here.krate == home.krate
                    && (here.path == home.path
                        || home.path.is_empty()
                        || here.path.starts_with(&format!("{}::", home.path)))
            }
            Visibility::Crate => here.krate == home.krate,
            _ => true,
        }
    }

    // ---- imports

    fn import(&self, f: usize, import: &Import, out: &mut Vec<Binding>) {
        let site = self.site(f, &import.enclosing);
        let head: Vec<Name> = if import.module.as_str().is_empty() {
            Vec::new()
        } else {
            import.module.as_str().split("::").map(Name::new).collect()
        };
        let exported = self.ix.files[f].facts.blob.facts.iter().any(|fact| {
            matches!(fact, Fact::Export(e) if match e {
                Export::Named { site: s, .. } | Export::Star { site: s, .. } => *s == import.site,
                Export::Default { .. } => false,
            })
        });
        let relation = if exported {
            Relation::Reexports
        } else {
            Relation::Imports
        };
        let mut push = |name: &Name, resolution: Resolution| {
            out.push(Binding {
                file: self.path_of(f).clone(),
                site: import.site.clone(),
                from: import.enclosing.clone(),
                relation,
                name: name.clone(),
                resolution,
                entry_guard: false,
            });
        };
        if import.star.is_star() {
            let name = head.last().cloned().unwrap_or_else(|| Name::new("*"));
            let resolution = match self.walk(&site, &head, Want::Type, 0) {
                Some(Ent::Mod(m)) => match self.mod_file(m) {
                    Some(file) => Resolution::Module {
                        file,
                        evidence: Evidence::new(EvidenceKind::StarImport),
                    },
                    None => Resolution::Unresolved {
                        reason: UnresolvedReason::MissingModule,
                    },
                },
                Some(Ent::Def(d)) => self.def_resolution(f, d, EvidenceKind::StarImport),
                Some(Ent::External | Ent::Builtin) => Resolution::Unresolved {
                    reason: UnresolvedReason::External,
                },
                None => Resolution::Unresolved {
                    reason: self.unresolved_head(&site, &head),
                },
            };
            push(&name, resolution);
            return;
        }
        for (name, _) in &import.names {
            let mut segs = head.clone();
            segs.push(name.clone());
            let resolution = match self.walk(&site, &segs, Want::Any, 0) {
                Some(Ent::Def(d)) => self.def_resolution(f, d, EvidenceKind::ImportedName),
                Some(Ent::Mod(m)) => match self.mod_file(m) {
                    Some(file) => Resolution::Module {
                        file,
                        evidence: Evidence::new(EvidenceKind::ImportedName),
                    },
                    None => Resolution::Unresolved {
                        reason: UnresolvedReason::MissingModule,
                    },
                },
                Some(Ent::External | Ent::Builtin) => Resolution::Unresolved {
                    reason: UnresolvedReason::External,
                },
                None => Resolution::Unresolved {
                    reason: self.unresolved_head(&site, &segs),
                },
            };
            push(name, resolution);
        }
    }

    fn def_resolution(&self, f: usize, d: usize, kind: EvidenceKind) -> Resolution {
        if self.reachable(f, d) {
            Resolution::Resolved {
                target: self.ix.defs[d].key.clone(),
                evidence: Evidence::new(kind),
            }
        } else {
            Resolution::Unresolved {
                reason: UnresolvedReason::OutsideProject,
            }
        }
    }

    /// Why a path found nothing: the crate it starts at is not the
    /// repository's, or a name inside the repository's is missing.
    fn unresolved_head(&self, site: &Site, segs: &[Name]) -> UnresolvedReason {
        match segs.first() {
            Some(first) if self.head(site, first, Want::Type, 0).is_none() => {
                UnresolvedReason::External
            }
            _ => UnresolvedReason::NoBinding,
        }
    }

    fn mod_file(&self, m: usize) -> Option<RepoPath> {
        self.ix
            .files_of
            .get(&m)
            .and_then(|files| files.first())
            .map(|f| self.path_of(*f).clone())
    }
}

// ---- type text

/// The segments of a type path as text: `a::b::T` is `[a, b, T]`; generics and
/// lifetimes are cut.
fn path_names(text: &str) -> Vec<Name> {
    let head: &str = text.split(['<', ' ', '(', ',']).next().unwrap_or(text);
    head.split("::")
        .filter(|s| !s.is_empty())
        .map(Name::new)
        .collect()
}

/// The first type path in `text` after references, `mut`, `dyn`, `impl` and
/// the smart pointers that deref to their content.
fn type_path(text: &str) -> Option<Vec<Name>> {
    let written = type_text(text)?;
    let written = written.strip_prefix("dyn ").unwrap_or(&written).to_string();
    let names = path_names(&written);
    (!names.is_empty()).then_some(names)
}

/// The type a written type stands for, as the extractor writes receivers:
/// `a::T`, or `dyn Tr` for a trait object.
fn type_text(text: &str) -> Option<String> {
    let mut rest = text.trim();
    loop {
        let before = rest;
        rest = rest.trim_start_matches('&').trim_start();
        if let Some(lifetime) = rest.strip_prefix('\'') {
            rest = lifetime
                .split_once(char::is_whitespace)
                .map_or("", |(_, r)| r)
                .trim_start();
        }
        rest = rest.strip_prefix("mut ").unwrap_or(rest).trim_start();
        rest = rest.strip_prefix("*const ").unwrap_or(rest).trim_start();
        rest = rest.strip_prefix("*mut ").unwrap_or(rest).trim_start();
        if before == rest {
            break;
        }
    }
    let dynamic = rest
        .strip_prefix("dyn ")
        .or_else(|| rest.strip_prefix("impl "));
    if let Some(tr) = dynamic {
        let path = tr.split(['<', '+', ' ', ',']).next()?.trim();
        return (!path.is_empty()).then(|| format!("dyn {path}"));
    }
    for pointer in ["Box<", "Rc<", "Arc<"] {
        if let Some(inner) = rest.strip_prefix(pointer) {
            let inner = inner.strip_suffix('>').unwrap_or(inner);
            return type_text(inner);
        }
    }
    let path = rest
        .split(['<', ' ', ',', ';', '{', '(', ')', '['])
        .next()?
        .trim();
    (!path.is_empty() && !path.starts_with(|c: char| c.is_ascii_digit())).then(|| path.to_string())
}

/// The text after `->` of a function signature, up to a `where` clause.
fn return_text(signature: &str) -> Option<String> {
    let mut depth = 0i32;
    let bytes = signature.as_bytes();
    let mut at = 0;
    while at + 1 < bytes.len() {
        match bytes[at] {
            b'(' | b'[' | b'<' => depth += 1,
            b')' | b']' => depth -= 1,
            b'>' if at > 0 && bytes[at - 1] != b'-' => depth -= 1,
            b'-' if bytes[at + 1] == b'>' && depth == 0 => {
                let rest = &signature[at + 2..];
                let rest = rest.split(" where ").next().unwrap_or(rest);
                return Some(rest.trim().to_string());
            }
            _ => {}
        }
        at += 1;
    }
    None
}

/// What `Result<T, E>` and `Option<T>` hold.
fn unwrap_text(text: &str) -> Option<String> {
    let inner = text
        .strip_prefix("Result<")
        .or_else(|| text.strip_prefix("Option<"))
        .or_else(|| {
            text.split_once("::")
                .and_then(|(_, r)| r.strip_prefix("Result<"))
        })?;
    let inner = inner.strip_suffix('>').unwrap_or(inner);
    // The first argument, up to a comma at depth zero.
    let mut depth = 0i32;
    for (i, c) in inner.char_indices() {
        match c {
            '<' | '(' | '[' => depth += 1,
            '>' | ')' | ']' => depth -= 1,
            ',' if depth == 0 => return Some(inner[..i].trim().to_string()),
            _ => {}
        }
    }
    Some(inner.trim().to_string())
}

/// A single capital letter, or one with digits: a type parameter.
fn is_generic_name(text: &str) -> bool {
    let mut chars = text.chars();
    chars.next().is_some_and(|c| c.is_ascii_uppercase()) && chars.all(|c| c.is_ascii_digit())
}
