//! Go: every reference and import of the facts layer bound to the definitions it
//! lands on.
//!
//! A package is a directory, and every file of it sees every top-level name of
//! the others. An import path is a module path (`go.mod` of the workspace
//! model) and the directory under it; a plain import binds the package's own
//! name (the file's package clause), an explicit one its alias, a dot import
//! the exported names, a blank one nothing. A package under `internal/` is
//! imported only from the tree its parent holds, and a package of another
//! module only through a declared dependency (`WorkspaceModel::may_reference`).
//! A name written `pkg.F` is `F` of the package an import binds; a method is
//! reached through a value whose type the syntax states (a parameter, a typed
//! `var`, a literal, what a called function declares it returns, a field with
//! a declared type), through the fields an embedded struct promotes; a value of
//! an interface type reaches every type whose methods cover the interface's, as
//! candidates, never as a resolution; any other value is a candidate from the
//! types the file can name.
//!
//! The facts' conventions are in `facts::lang::go`.

use std::collections::{HashMap, HashSet};

use super::{
    Binding, CandidateReason, Definition, Evidence, EvidenceKind, Relation, Resolution,
    UnresolvedReason,
};
use crate::facts::FileFacts;
use crate::facts::{
    Def, DefKey, DefKind, Export, Fact, FactsCache, Import, ImportKind, Language, Name, Receiver,
    Ref, Star, UseKind, Visibility, expired, facts_of,
};
use crate::snapshot::{AnalysisContext, Loss, RepoPath, Result, SideTag, Stage, WorkspaceModel};

/// A call through an untyped receiver is a candidate for at most this many
/// methods; past it the list says nothing a reader could use.
const MAX_CANDIDATES: usize = 24;
/// How far a chain of embedded types may reach for a member.
const MAX_DEPTH: usize = 4;

const BUILTIN_FUNCTIONS: &[&str] = &[
    "append", "cap", "clear", "close", "complex", "copy", "delete", "imag", "len", "make", "max",
    "min", "new", "panic", "print", "println", "real", "recover",
];
const BUILTIN_VALUES: &[&str] = &["true", "false", "nil", "iota"];
const BUILTIN_TYPES: &[&str] = &[
    "any",
    "bool",
    "byte",
    "comparable",
    "complex128",
    "complex64",
    "error",
    "float32",
    "float64",
    "int",
    "int16",
    "int32",
    "int64",
    "int8",
    "rune",
    "string",
    "uint",
    "uint16",
    "uint32",
    "uint64",
    "uint8",
    "uintptr",
];

pub struct GoResolver {
    definitions: Vec<Definition>,
    bindings: Vec<Binding>,
    by_target: HashMap<DefKey, Vec<usize>>,
    losses: Vec<Loss>,
    /// Files of a package another module can import: not under `internal/`, not
    /// a program's `main`.
    public: HashSet<String>,
}

impl GoResolver {
    /// Resolves every Go file of `side`.
    pub fn open(ctx: &AnalysisContext, side: SideTag, cache: &FactsCache) -> Result<Self> {
        let reader = match side {
            SideTag::Base => &ctx.base,
            SideTag::Head => &ctx.head,
        };
        let listing = cache.listing(reader)?;
        let mut files = Vec::new();
        let mut losses = Vec::new();
        for entry in &listing.entries {
            if Language::from_path(&entry.path) != Some(Language::Go) {
                continue;
            }
            if expired(&ctx.run) {
                losses.push(Loss::Deadline {
                    stage: Stage::Search,
                });
                break;
            }
            if let Some(file) = facts_of(ctx, side, cache, &entry.path)? {
                if file.language() == Language::Go {
                    losses.extend(file.losses());
                    files.push(file);
                }
            }
        }
        let index = Index::build(files, &ctx.workspace);
        let public = index
            .files
            .iter()
            .filter(|file| {
                let package = &index.pkgs[file.pkg];
                package.name != "main" && !package.dir.split('/').any(|s| s == "internal")
            })
            .map(|file| file.facts.path.as_str().to_string())
            .collect();
        let engine = Engine {
            ix: index,
            workspace: &ctx.workspace,
        };
        let (definitions, bindings) = engine.run();
        let mut by_target: HashMap<DefKey, Vec<usize>> = HashMap::new();
        for (at, binding) in bindings.iter().enumerate() {
            for key in binding.targets() {
                by_target.entry(key.clone()).or_default().push(at);
            }
        }
        Ok(Self {
            definitions,
            bindings,
            by_target,
            losses,
            public,
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

    /// Whether the file is in a package other modules can import: what a removal
    /// there is a contract for.
    pub fn is_public_package(&self, file: &str) -> bool {
        self.public.contains(file)
    }
}

// ---- the index

/// Where an import path leads.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Located {
    /// A package of the workspace, by its index.
    Package(usize),
    /// A module of the workspace with no package of that directory.
    Missing,
    /// The standard library or a module the workspace does not hold.
    External,
}

struct ImportRec {
    /// The name the import gives, or none for a plain one (the package's own).
    alias: Option<Name>,
    path: String,
    dot: bool,
    blank: bool,
    target: Located,
    /// The importer may not name the package: it is under `internal/`.
    barred: bool,
}

struct PkgRec {
    dir: String,
    name: String,
    files: Vec<usize>,
}

struct FileRec {
    facts: FileFacts,
    pkg: usize,
    imports: Vec<ImportRec>,
}

struct DefRec {
    file: usize,
    def: Def,
    key: DefKey,
}

#[derive(Default)]
struct Index {
    pkgs: Vec<PkgRec>,
    files: Vec<FileRec>,
    defs: Vec<DefRec>,
    /// Top-level definitions by package and name.
    items: HashMap<(usize, Name), Vec<usize>>,
    /// Members of a type (methods, fields) by package and the type's name.
    members: HashMap<(usize, Name), Vec<usize>>,
    /// Methods by name: what an untyped receiver may be.
    methods: HashMap<Name, Vec<usize>>,
}

fn dir_of(path: &str) -> String {
    path.rsplit_once('/')
        .map_or(String::new(), |(dir, _)| dir.to_string())
}

/// The name a package is imported by when nothing else says: the last element
/// of its path, past a `/vN` suffix, without `go-`.
fn path_name(path: &str) -> String {
    let mut parts: Vec<&str> = path.split('/').collect();
    if parts.len() > 1
        && parts.last().is_some_and(|last| {
            last.strip_prefix('v')
                .is_some_and(|v| !v.is_empty() && v.chars().all(|c| c.is_ascii_digit()))
        })
    {
        parts.pop();
    }
    let last = parts.last().copied().unwrap_or("");
    let last = last.rsplit_once(".v").map_or(last, |(name, _)| name);
    last.strip_prefix("go-").unwrap_or(last).replace('-', "_")
}

impl Index {
    fn build(files: Vec<FileFacts>, workspace: &WorkspaceModel) -> Self {
        let mut index = Self::default();
        let mut by_dir: HashMap<String, usize> = HashMap::new();
        for (at, facts) in files.into_iter().enumerate() {
            let dir = dir_of(facts.path.as_str());
            let pkg = *by_dir.entry(dir.clone()).or_insert_with(|| {
                index.pkgs.push(PkgRec {
                    dir,
                    name: String::new(),
                    files: Vec::new(),
                });
                index.pkgs.len() - 1
            });
            index.pkgs[pkg].files.push(at);
            index.add_file(at, facts, pkg);
        }
        index.name_packages();
        let mut modules: Vec<(String, String)> = workspace
            .projects()
            .iter()
            .filter_map(|p| Some((p.go_module.clone()?, p.root.clone())))
            .collect();
        modules.sort_by_key(|(module, _)| std::cmp::Reverse(module.len()));
        index.link_imports(&modules, &by_dir);
        index
    }

    fn add_file(&mut self, at: usize, facts: FileFacts, pkg: usize) {
        for def in facts.blob.defs() {
            let id = self.defs.len();
            let key = facts.key(&def.local);
            let container = &def.local.container;
            match container.as_slice() {
                [] => self
                    .items
                    .entry((pkg, def.local.name.clone()))
                    .or_default()
                    .push(id),
                [owner] => {
                    self.members
                        .entry((pkg, owner.clone()))
                        .or_default()
                        .push(id);
                    if def.kind == DefKind::Method {
                        self.methods
                            .entry(def.local.name.clone())
                            .or_default()
                            .push(id);
                    }
                }
                _ => {}
            }
            self.defs.push(DefRec {
                file: at,
                def: def.clone(),
                key,
            });
        }
        self.files.push(FileRec {
            facts,
            pkg,
            imports: Vec::new(),
        });
    }

    /// A package is named by the clause of its files; a directory of `x` and
    /// `x_test` files is `x`.
    #[allow(
        clippy::disallowed_methods,
        reason = "a package none of whose files has a clause has no name: an import of it is named by its path"
    )]
    fn name_packages(&mut self) {
        for at in 0..self.pkgs.len() {
            let mut names: Vec<String> = self.pkgs[at]
                .files
                .iter()
                .flat_map(|f| self.files[*f].facts.blob.facts.iter())
                .filter_map(|fact| match fact {
                    Fact::Export(Export::Default {
                        local: Some(name), ..
                    }) => Some(name.as_str().to_string()),
                    _ => None,
                })
                .collect();
            names.sort();
            names.dedup();
            let own = names
                .iter()
                .find(|n| !n.ends_with("_test"))
                .or_else(|| names.first())
                .cloned()
                .unwrap_or_default();
            self.pkgs[at].name = own;
        }
    }

    fn locate(
        &self,
        modules: &[(String, String)],
        by_dir: &HashMap<String, usize>,
        path: &str,
    ) -> Located {
        for (module, root) in modules {
            let rest = if path == module {
                Some("")
            } else {
                path.strip_prefix(module.as_str())
                    .and_then(|rest| rest.strip_prefix('/'))
            };
            let Some(rest) = rest else {
                continue;
            };
            let dir = match (root.is_empty(), rest.is_empty()) {
                (true, _) => rest.to_string(),
                (false, true) => root.clone(),
                (false, false) => format!("{root}/{rest}"),
            };
            return by_dir
                .get(&dir)
                .map_or(Located::Missing, |p| Located::Package(*p));
        }
        Located::External
    }

    fn link_imports(&mut self, modules: &[(String, String)], by_dir: &HashMap<String, usize>) {
        for at in 0..self.files.len() {
            let own_dir = self.pkgs[self.files[at].pkg].dir.clone();
            let mut recs = Vec::new();
            for fact in &self.files[at].facts.blob.facts {
                let Fact::Import(import) = fact else {
                    continue;
                };
                let path = import.module.as_str().to_string();
                let target = self.locate(modules, by_dir, &path);
                let barred = match target {
                    Located::Package(p) => !internal_visible(&own_dir, &self.pkgs[p].dir),
                    _ => false,
                };
                recs.push(ImportRec {
                    alias: import.default.clone(),
                    path,
                    dot: import.star == Star::Glob,
                    blank: import.kind == ImportKind::SideEffect,
                    target,
                    barred,
                });
            }
            self.files[at].imports = recs;
        }
    }
}

/// A package under `internal/` is imported only from the tree its parent holds.
fn internal_visible(importer_dir: &str, target_dir: &str) -> bool {
    let parts: Vec<&str> = target_dir.split('/').collect();
    let Some(at) = parts.iter().rposition(|p| *p == "internal") else {
        return true;
    };
    let parent = parts[..at].join("/");
    parent.is_empty() || importer_dir == parent || importer_dir.starts_with(&format!("{parent}/"))
}

// ---- resolution

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Want {
    Type,
    Any,
}

/// A type a value has.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Typed {
    /// A struct or a defined type.
    Def(usize),
    /// An interface: any type that has its methods.
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

struct Engine<'a> {
    ix: Index,
    workspace: &'a WorkspaceModel,
}

fn is_ident(token: &str) -> bool {
    token
        .chars()
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_')
}

/// The tokens past the first group of `open ... close` at the front.
fn after_group<'s, 't>(tokens: &'s [&'t str], open: &str, close: &str) -> &'s [&'t str] {
    if tokens.first() != Some(&open) {
        return tokens;
    }
    let mut depth = 0usize;
    for (at, token) in tokens.iter().enumerate() {
        if *token == open {
            depth += 1;
        } else if *token == close {
            depth -= 1;
            if depth == 0 {
                return &tokens[at + 1..];
            }
        }
    }
    &[]
}

impl Engine<'_> {
    fn run(&self) -> (Vec<Definition>, Vec<Binding>) {
        let mut bindings = Vec::new();
        for (f, file) in self.ix.files.iter().enumerate() {
            for fact in &file.facts.blob.facts {
                match fact {
                    Fact::Reference(r) => self.bind(f, r, &mut bindings),
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
        (definitions, bindings)
    }

    fn path_of(&self, f: usize) -> &RepoPath {
        &self.ix.files[f].facts.path
    }

    fn pkg_of(&self, d: usize) -> usize {
        self.ix.files[self.ix.defs[d].file].pkg
    }

    /// Whether a name used in file `f` may refer to the definition: one project,
    /// a declared dependency, or no project at all.
    fn reachable(&self, f: usize, def: usize) -> bool {
        let used = self.ix.defs[def].file;
        used == f
            || self
                .workspace
                .may_reference(self.path_of(f).as_str(), self.path_of(used).as_str())
    }

    fn tokens(&self, d: usize) -> Vec<&str> {
        self.ix.defs[d]
            .def
            .signature
            .as_deref()
            .unwrap_or("")
            .split_whitespace()
            .collect()
    }

    // ---- names

    /// The import of `f` that binds `name` as a package.
    fn import_named(&self, f: usize, name: &str) -> Option<&ImportRec> {
        self.ix.files[f].imports.iter().find(|i| {
            !i.dot
                && !i.blank
                && match (&i.alias, i.target) {
                    (Some(alias), _) => alias.as_str() == name,
                    (None, Located::Package(p)) => {
                        let own = &self.ix.pkgs[p].name;
                        if own.is_empty() {
                            path_name(&i.path) == name
                        } else {
                            own == name
                        }
                    }
                    (None, _) => path_name(&i.path) == name,
                }
        })
    }

    /// A top-level definition of a package by name.
    fn item(&self, pkg: usize, name: &str, want: Want) -> Option<usize> {
        self.ix
            .items
            .get(&(pkg, Name::new(name)))?
            .iter()
            .copied()
            .filter(|d| {
                want == Want::Any
                    || matches!(
                        self.ix.defs[*d].def.kind,
                        DefKind::Struct | DefKind::Interface | DefKind::TypeAlias
                    )
            })
            .min_by_key(|d| self.ix.defs[*d].def.local.disambig.is_some())
    }

    /// An exported definition of a package.
    fn exported(&self, pkg: usize, name: &str, want: Want) -> Option<usize> {
        name.starts_with(char::is_uppercase)
            .then(|| self.item(pkg, name, want))
            .flatten()
    }

    /// A bare name as the file's scope gives it: the package's, a dot import's.
    fn find_name(&self, f: usize, name: &str, want: Want) -> Option<(usize, EvidenceKind)> {
        if let Some(d) = self.item(self.ix.files[f].pkg, name, want) {
            return Some((d, EvidenceKind::SameScope));
        }
        self.ix.files[f]
            .imports
            .iter()
            .filter(|i| i.dot && !i.barred)
            .filter_map(|i| match i.target {
                Located::Package(p) => self.exported(p, name, want),
                _ => None,
            })
            .next()
            .map(|d| (d, EvidenceKind::StarImport))
    }

    // ---- types

    /// The type tokens name, where file `f` writes them.
    fn type_of_tokens(&self, f: usize, tokens: &[&str]) -> Typed {
        let mut tokens = tokens;
        while tokens.first() == Some(&"*") {
            tokens = &tokens[1..];
        }
        match tokens {
            [name] => self.named_type(f, None, name),
            [package, ".", name] => self.named_type(f, Some(package), name),
            [package, ".", name, "[", ..] => self.named_type(f, Some(package), name),
            [name, "[", ..] if is_ident(name) => self.named_type(f, None, name),
            _ => Typed::Unknown,
        }
    }

    fn type_of_text(&self, f: usize, text: &str) -> Typed {
        match text.split_once('.') {
            Some((package, name)) => self.named_type(f, Some(package), name),
            None => self.named_type(f, None, text),
        }
    }

    fn named_type(&self, f: usize, package: Option<&str>, name: &str) -> Typed {
        match package {
            Some(package) => match self.import_named(f, package) {
                Some(import) => match import.target {
                    Located::Package(p) if !import.barred => self
                        .exported(p, name, Want::Type)
                        .map_or(Typed::Unknown, |d| self.typed_def(d, 0)),
                    Located::External => Typed::External,
                    _ => Typed::Unknown,
                },
                None => Typed::Unknown,
            },
            None => {
                if BUILTIN_TYPES.contains(&name) {
                    return Typed::External;
                }
                self.find_name(f, name, Want::Type)
                    .map_or(Typed::Unknown, |(d, _)| self.typed_def(d, 0))
            }
        }
    }

    /// What a type definition is, following an alias to its target.
    fn typed_def(&self, d: usize, depth: usize) -> Typed {
        let rec = &self.ix.defs[d];
        match rec.def.kind {
            DefKind::Interface => Typed::Dyn(d),
            DefKind::Struct => Typed::Def(d),
            DefKind::TypeAlias if depth < MAX_DEPTH => {
                let tokens = self.tokens(d);
                let name = rec.def.local.name.as_str();
                let after = tokens
                    .iter()
                    .position(|t| *t == name)
                    .map_or(&[][..], |at| &tokens[at + 1..]);
                let after = after_group(after, "[", "]");
                match after {
                    // `type A = B`: A is B.
                    ["=", target @ ..] => match self.type_of_tokens(rec.file, target) {
                        Typed::Def(t) | Typed::Dyn(t) => self.typed_def(t, depth + 1),
                        other => other,
                    },
                    // `type A B`: a type of its own, whose methods are its own.
                    _ => Typed::Def(d),
                }
            }
            _ => Typed::Unknown,
        }
    }

    /// The type a value of this definition has: a variable's or field's declared
    /// type, a type's own for a method expression.
    fn value_type(&self, d: usize) -> Typed {
        let rec = &self.ix.defs[d];
        match rec.def.kind {
            DefKind::Variable | DefKind::Constant | DefKind::Field => {
                let tokens = self.tokens(d);
                let mut rest: &[&str] = tokens.get(1..).unwrap_or(&[]);
                // `x , y int`: the names before the type.
                while rest.first() == Some(&",") && rest.len() > 2 {
                    rest = &rest[2..];
                }
                let end = rest.iter().position(|t| *t == "=").unwrap_or(rest.len());
                let written = &rest[..end];
                if rec.def.kind == DefKind::Field && self.is_embedded(d) {
                    return self.type_of_tokens(rec.file, &tokens);
                }
                if written.is_empty() {
                    Typed::Unknown
                } else {
                    self.type_of_tokens(rec.file, written)
                }
            }
            DefKind::Struct | DefKind::Interface | DefKind::TypeAlias => self.typed_def(d, 0),
            _ => Typed::Unknown,
        }
    }

    /// What a function or method declares it returns first.
    fn return_type(&self, d: usize) -> Typed {
        let rec = &self.ix.defs[d];
        let tokens = self.tokens(d);
        let mut rest: &[&str] = &tokens;
        if rest.first() == Some(&"func") {
            rest = &rest[1..];
        }
        if rec.def.kind == DefKind::Method {
            rest = after_group(rest, "(", ")");
        }
        // The name, then type parameters, then the parameters.
        rest = rest.get(1..).unwrap_or(&[]);
        rest = after_group(rest, "[", "]");
        rest = after_group(rest, "(", ")");
        if rest.is_empty() {
            return Typed::Unknown;
        }
        let first: &[&str] = if rest.first() == Some(&"(") {
            let end = rest
                .iter()
                .position(|t| *t == ",")
                .unwrap_or(rest.len() - 1);
            let mut elem = &rest[1..end.max(1)];
            if elem.last() == Some(&")") {
                elem = &elem[..elem.len() - 1];
            }
            // `(r Result, err error)`: a result may be named.
            if elem.len() >= 2 && elem[1] != "." && elem[1] != "[" && is_ident(elem[0]) {
                elem = &elem[1..];
            }
            elem
        } else {
            rest
        };
        self.type_of_tokens(rec.file, first)
    }

    // ---- members

    fn members_of(&self, t: usize) -> &[usize] {
        static NONE: [usize; 0] = [];
        self.ix
            .members
            .get(&(self.pkg_of(t), self.ix.defs[t].def.local.name.clone()))
            .map_or(&NONE[..], Vec::as_slice)
    }

    fn is_embedded(&self, field: usize) -> bool {
        let tokens = self.tokens(field);
        let name = self.ix.defs[field].def.local.name.as_str();
        !(tokens.first() == Some(&name)
            && tokens.len() >= 2
            && tokens[1] != "."
            && tokens[1] != "[")
    }

    /// A method or field of a type, found through its embedded fields.
    fn find_member(&self, t: usize, name: &str, depth: usize) -> Option<usize> {
        let own = self.members_of(t);
        if let Some(m) = own
            .iter()
            .copied()
            .find(|m| self.ix.defs[*m].def.local.name.as_str() == name)
        {
            return Some(m);
        }
        if depth >= MAX_DEPTH {
            return None;
        }
        own.iter()
            .copied()
            .filter(|m| self.ix.defs[*m].def.kind == DefKind::Field && self.is_embedded(*m))
            .find_map(|m| match self.value_type(m) {
                Typed::Def(e) | Typed::Dyn(e) => self.find_member(e, name, depth + 1),
                _ => None,
            })
    }

    /// The names of the methods an interface requires, embedded interfaces'
    /// included.
    fn required(&self, i: usize, depth: usize) -> Vec<String> {
        let mut names: Vec<String> = self
            .members_of(i)
            .iter()
            .filter(|m| self.ix.defs[**m].def.kind == DefKind::Method)
            .map(|m| self.ix.defs[*m].def.local.name.as_str().to_string())
            .collect();
        if depth < MAX_DEPTH {
            let tokens = self.tokens(i);
            let mut at = tokens
                .iter()
                .position(|t| *t == "{")
                .map_or(tokens.len(), |a| a + 1);
            while at < tokens.len() && tokens[at] != "}" {
                let (package, name, next) = if tokens.get(at + 1) == Some(&".") {
                    (Some(tokens[at]), tokens.get(at + 2).copied(), at + 3)
                } else {
                    (None, tokens.get(at).copied(), at + 1)
                };
                if let Some(name) = name.filter(|n| is_ident(n)) {
                    if let Typed::Dyn(e) = self.named_type(self.ix.defs[i].file, package, name) {
                        names.extend(self.required(e, depth + 1));
                    }
                }
                at = next;
            }
        }
        names.sort();
        names.dedup();
        names
    }

    /// A call through an interface: the interface's method and every concrete
    /// type's that has all the methods the interface asks.
    fn dispatched(&self, i: usize, name: &str) -> Outcome {
        let mut targets: Vec<usize> = Vec::new();
        if let Some(m) = self
            .members_of(i)
            .iter()
            .copied()
            .find(|m| self.ix.defs[*m].def.local.name.as_str() == name)
        {
            targets.push(m);
        }
        let required = self.required(i, 0);
        for m in self.ix.methods.get(&Name::new(name)).into_iter().flatten() {
            let rec = &self.ix.defs[*m];
            let Some(owner) = rec.def.local.container.first() else {
                continue;
            };
            let pkg = self.pkg_of(*m);
            let Some(t) = self.item(pkg, owner.as_str(), Want::Type) else {
                continue;
            };
            if matches!(self.ix.defs[t].def.kind, DefKind::Interface) {
                continue;
            }
            let has = |needed: &String| {
                self.ix
                    .members
                    .get(&(pkg, owner.clone()))
                    .into_iter()
                    .flatten()
                    .any(|x| {
                        self.ix.defs[*x].def.kind == DefKind::Method
                            && self.ix.defs[*x].def.local.name.as_str() == needed
                    })
            };
            if required.iter().all(has) {
                targets.push(*m);
            }
        }
        targets.sort_unstable();
        targets.dedup();
        if targets.is_empty() {
            return Outcome::Unresolved(UnresolvedReason::NoMember);
        }
        Outcome::Candidates(targets, CandidateReason::Dispatch)
    }

    /// A member of `start`, after the fields written on the way.
    fn member_outcome(
        &self,
        f: usize,
        start: Typed,
        fields: &[Name],
        r: &Ref,
        evidence: EvidenceKind,
    ) -> Outcome {
        let mut at = start;
        for field in fields {
            at = match at {
                Typed::Def(t) | Typed::Dyn(t) => match self.find_member(t, field.as_str(), 0) {
                    Some(m) if self.ix.defs[m].def.kind == DefKind::Field => self.value_type(m),
                    _ => Typed::Unknown,
                },
                _ => Typed::Unknown,
            };
        }
        match at {
            Typed::Def(t) => match self.find_member(t, r.name.as_str(), 0) {
                Some(m) => {
                    let inherited = self.ix.defs[m].def.local.container.first()
                        != Some(&self.ix.defs[t].def.local.name);
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
            Typed::Dyn(i) => match self.find_member(i, r.name.as_str(), 0) {
                Some(m) if self.ix.defs[m].def.kind == DefKind::Field => {
                    Outcome::Found(m, evidence)
                }
                _ => self.dispatched(i, r.name.as_str()),
            },
            Typed::External => Outcome::Unresolved(UnresolvedReason::ExternalBase),
            Typed::Unknown => self.untyped_outcome(f, r),
        }
    }

    // ---- references

    fn relation_of(&self, kind: UseKind, target: Option<usize>) -> Relation {
        let constructor = target.is_some_and(|t| self.ix.defs[t].def.kind == DefKind::Struct);
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
            DefKind::Function | DefKind::Method | DefKind::Struct
        )
    }

    fn bind(&self, f: usize, r: &Ref, out: &mut Vec<Binding>) {
        let (resolution, target) = match self.outcome(f, r) {
            Outcome::Found(d, kind) => {
                if self.reachable(f, d) {
                    (
                        Resolution::Resolved {
                            target: self.ix.defs[d].key.clone(),
                            evidence: Evidence::new(kind),
                        },
                        Some(d),
                    )
                } else {
                    (
                        Resolution::Unresolved {
                            reason: UnresolvedReason::OutsideProject,
                        },
                        None,
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
            name: r.name.clone(),
            resolution,
            entry_guard: false,
        });
    }

    fn outcome(&self, f: usize, r: &Ref) -> Outcome {
        match &r.receiver {
            Receiver::None => self.path_outcome(f, r),
            Receiver::Typed(t) => {
                let start = self.type_of_text(f, t.as_str());
                self.member_outcome(f, start, &r.qualifier, r, EvidenceKind::TypedReceiver)
            }
            Receiver::CallResult(path) => {
                let start = self.call_result(f, path);
                self.member_outcome(f, start, &r.qualifier, r, EvidenceKind::FactoryReturn)
            }
            Receiver::New(t) => {
                let start = self.type_of_text(f, t.as_str());
                self.member_outcome(f, start, &r.qualifier, r, EvidenceKind::Constructed)
            }
            Receiver::SelfType | Receiver::Module(_) | Receiver::Unknown => {
                self.untyped_outcome(f, r)
            }
        }
    }

    /// What calling the function a path names returns.
    fn call_result(&self, f: usize, path: &[Name]) -> Typed {
        let target = match path {
            [name] => self.find_name(f, name.as_str(), Want::Any).map(|(d, _)| d),
            [package, name] => self
                .import_named(f, package.as_str())
                .filter(|i| !i.barred)
                .and_then(|i| match i.target {
                    Located::Package(p) => self.exported(p, name.as_str(), Want::Any),
                    _ => None,
                }),
            _ => None,
        };
        let Some(d) = target else {
            return match path {
                [package, _]
                    if self
                        .import_named(f, package.as_str())
                        .is_some_and(|i| i.target == Located::External) =>
                {
                    Typed::External
                }
                _ => Typed::Unknown,
            };
        };
        match self.ix.defs[d].def.kind {
            DefKind::Function => self.return_type(d),
            // `T(x)` is a conversion to the type.
            DefKind::Struct | DefKind::Interface | DefKind::TypeAlias => self.typed_def(d, 0),
            _ => Typed::Unknown,
        }
    }

    fn path_outcome(&self, f: usize, r: &Ref) -> Outcome {
        let Some((head, rest)) = r.qualifier.split_first() else {
            return self.bare_outcome(f, r);
        };
        if let Some(import) = self.import_named(f, head.as_str()) {
            return match import.target {
                Located::External => Outcome::Unresolved(UnresolvedReason::External),
                Located::Missing => Outcome::Unresolved(UnresolvedReason::MissingModule),
                Located::Package(_) if import.barred => {
                    Outcome::Unresolved(UnresolvedReason::OutsideProject)
                }
                Located::Package(p) => {
                    let first = rest.first().unwrap_or(&r.name);
                    let Some(item) = self.exported(p, first.as_str(), Want::Any) else {
                        return Outcome::Unresolved(UnresolvedReason::NoBinding);
                    };
                    if rest.is_empty() {
                        Outcome::Found(item, EvidenceKind::ModuleAttribute)
                    } else {
                        let start = self.value_type(item);
                        self.member_outcome(f, start, &rest[1..], r, EvidenceKind::ModuleAttribute)
                    }
                }
            };
        }
        // A package-level variable written first: `config.Port`.
        if let Some((item, _)) = self.find_name(f, head.as_str(), Want::Any) {
            let start = self.value_type(item);
            return self.member_outcome(f, start, rest, r, EvidenceKind::SameScope);
        }
        Outcome::Unresolved(UnresolvedReason::NoBinding)
    }

    fn bare_outcome(&self, f: usize, r: &Ref) -> Outcome {
        let name = r.name.as_str();
        if let Some((d, kind)) = self.find_name(f, name, Want::Any) {
            return Outcome::Found(d, kind);
        }
        if BUILTIN_FUNCTIONS.contains(&name)
            || BUILTIN_VALUES.contains(&name)
            || BUILTIN_TYPES.contains(&name)
        {
            return Outcome::Unresolved(UnresolvedReason::Builtin);
        }
        Outcome::Unresolved(UnresolvedReason::NoBinding)
    }

    /// A method called on a value nothing types: any method of that name on a
    /// type the file can name.
    fn untyped_outcome(&self, f: usize, r: &Ref) -> Outcome {
        let Some(all) = self.ix.methods.get(&r.name) else {
            // No type of the repository has a method of this name: the call is the
            // standard library's or a dependency's, and a function is never
            // called with method syntax.
            return if r.use_kind == UseKind::Call {
                Outcome::Unresolved(UnresolvedReason::External)
            } else {
                Outcome::Unresolved(UnresolvedReason::UntypedRead)
            };
        };
        let file = &self.ix.files[f];
        let named: HashSet<usize> = file
            .imports
            .iter()
            .filter(|i| !i.blank && !i.barred)
            .filter_map(|i| match i.target {
                Located::Package(p) => Some(p),
                _ => None,
            })
            .collect();
        let mut targets: Vec<usize> = all
            .iter()
            .copied()
            .filter(|m| self.reachable(f, *m))
            .filter(|m| {
                let pkg = self.pkg_of(*m);
                (pkg == file.pkg
                    || (named.contains(&pkg) && r.name.as_str().starts_with(char::is_uppercase)))
                    && self.ix.defs[*m].def.visibility != Visibility::Private
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

    // ---- imports

    fn import(&self, f: usize, import: &Import, out: &mut Vec<Binding>) {
        if import.kind == ImportKind::SideEffect {
            return;
        }
        let Some(rec) = self.ix.files[f]
            .imports
            .iter()
            .find(|i| i.path == import.module.as_str() && i.alias == import.default)
        else {
            return;
        };
        let name = import
            .default
            .clone()
            .unwrap_or_else(|| Name::new(path_name(&rec.path).as_str()));
        let evidence = if rec.dot {
            EvidenceKind::StarImport
        } else {
            EvidenceKind::ImportedName
        };
        let resolution = match rec.target {
            Located::Package(_) if rec.barred => Resolution::Unresolved {
                reason: UnresolvedReason::OutsideProject,
            },
            Located::Package(p) => {
                let first = self.ix.pkgs[p]
                    .files
                    .iter()
                    .map(|file| self.path_of(*file))
                    .min_by(|a, b| a.as_str().cmp(b.as_str()))
                    .cloned();
                match first {
                    Some(file) => Resolution::Module {
                        file,
                        evidence: Evidence::new(evidence),
                    },
                    None => Resolution::Unresolved {
                        reason: UnresolvedReason::MissingModule,
                    },
                }
            }
            Located::Missing => Resolution::Unresolved {
                reason: UnresolvedReason::MissingModule,
            },
            Located::External => Resolution::Unresolved {
                reason: UnresolvedReason::External,
            },
        };
        out.push(Binding {
            file: self.path_of(f).clone(),
            site: import.site.clone(),
            from: import.enclosing.clone(),
            relation: Relation::Imports,
            name,
            resolution,
            entry_guard: false,
        });
    }
}
