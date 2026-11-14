//! C: every reference and include of the facts layer bound to the definitions
//! it lands on.
//!
//! The facts state names and the chain written before them (see `facts::lang::c`);
//! this layer finds what C finds a name by: the file's own definitions, what its
//! `#include`s bring (the closure of the quoted includes, read relative to the
//! including file and then through the include roots; the angle ones through the
//! roots alone), a `static` name only in its file or in the header that defines
//! it, and, for a function that is declared and not defined where it is called,
//! the definition the linker finds in the project. A prototype in a header is
//! paired with its definition: a call lands on the definition, and a second
//! binding on the prototype the include shows, so a change to either reaches the
//! callers. `compile_commands.json`, when committed, is read as data for its
//! `-I` directories: a hint, never a build.
//!
//! What the syntax cannot settle stays a candidate with its reason: a name a macro
//! may expand (`Macro`), a call through a pointer, a local or a member (any
//! function whose address is taken: `Dispatch`), a definition under `#if` and a
//! name defined in the project but in no file the caller includes
//! (`ImportUnresolved`). What no include brings and no project file defines is the
//! library's; a quoted include no file answers is an unknown.
//!
//! Two projects are linked only through a declared dependency
//! (`WorkspaceModel::may_reference`). Where the project's own build tree (the
//! directory of its topmost build file) defines an external name, that is the
//! definition its linker finds: a same-named function of another tree is no
//! second candidate.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use super::index::{Path, dir_of, join, normalise};
use super::solved::{Gathered, gather, reader_of};
use super::{
    Binding, CandidateReason, Definition, Evidence, EvidenceKind, Relation, Resolution, Solved,
    UnresolvedReason, is_callable, unresolved,
};
use crate::facts::{
    Def, DefKey, DefKind, Disambig, Fact, FactsCache, FileFacts, ImportKind, Language, LocalKey,
    Name, Receiver, Ref, Scope, Span, Star, UseKind, Visibility, expired,
};
use crate::snapshot::{
    AnalysisContext, Bytes, Loss, RepoPath, Result, SideTag, SnapshotReader, Stage, WorkspaceModel,
};

/// The files that make a directory a build: what the linker of a C project is
/// told its sources by.
const BUILD_FILES: &[&str] = &[
    "Makefile",
    "GNUmakefile",
    "makefile",
    "CMakeLists.txt",
    "meson.build",
    "configure.ac",
    "build.ninja",
    "SConstruct",
    "BUILD.bazel",
];

/// A call through a pointer is a candidate for at most this many functions.
const MAX_CANDIDATES: usize = 24;
/// Links of a chain of members typed by one another, and of typedefs.
const MAX_DEPTH: usize = 6;

/// Words a declaration spells that name no type of the project.
const BUILTIN_WORDS: &[&str] = &[
    "const",
    "volatile",
    "static",
    "extern",
    "register",
    "inline",
    "restrict",
    "unsigned",
    "signed",
    "struct",
    "union",
    "enum",
    "typedef",
    "void",
    "char",
    "short",
    "int",
    "long",
    "float",
    "double",
    "_Bool",
    "bool",
    "_Noreturn",
];

/// The bindings of one snapshot's C files.
pub struct CResolver {
    solved: Solved,
}

impl CResolver {
    /// Resolves every C file of `side`.
    pub fn open(ctx: &AnalysisContext, side: SideTag, cache: &FactsCache) -> Result<Self> {
        let reader = reader_of(ctx, side);
        let mut commands: Vec<Bytes> = Vec::new();
        let Gathered {
            listing,
            files,
            mut losses,
            read,
        } = gather(
            ctx,
            side,
            cache,
            |entry, losses| {
                if entry.path.file_name() == "compile_commands.json" {
                    match reader.read(&entry.path) {
                        Ok(Some(read)) => commands.push(read),
                        Ok(None) => {}
                        Err(_) => losses.push(Loss::ReadFailed {
                            path: entry.path.clone(),
                            side,
                        }),
                    }
                    return false;
                }
                matches!(
                    Language::from_path(&entry.path),
                    Some(Language::C | Language::Cpp)
                )
            },
            |_| true,
        )?;
        let ix = Index::build(files);
        let hints = include_hints(&commands, &ix);
        let builds = build_trees(
            listing
                .entries
                .iter()
                .map(|entry| entry.path.as_str())
                .filter(|path| BUILD_FILES.contains(&path.rsplit('/').next().unwrap_or(path))),
        );
        let engine = Engine::new(ix, hints, builds, &ctx.workspace, reader, side);
        let (definitions, bindings, stopped) = engine.run(ctx);
        losses.extend(engine.losses.take());
        if stopped {
            losses.push(Loss::Deadline {
                stage: Stage::Search,
            });
        }
        Ok(Self {
            solved: Solved::new(definitions, bindings, losses, read),
        })
    }
}

solved_by!(CResolver);

// ---- build trees

/// The directories holding a build file with none above them: the roots of the
/// repository's build trees.
fn build_trees<'a>(build_files: impl Iterator<Item = &'a str>) -> Vec<String> {
    let dirs: HashSet<&str> = build_files.map(dir_of).collect();
    let mut trees: Vec<String> = dirs
        .iter()
        .filter(|dir| {
            let mut above = **dir;
            while !above.is_empty() {
                above = dir_of(above);
                if dirs.contains(above) {
                    return false;
                }
            }
            true
        })
        .map(|dir| (*dir).to_string())
        .collect();
    trees.sort();
    trees
}

// ---- compile_commands.json

/// The include directories a committed `compile_commands.json` states, each the
/// longest suffix of its path that is a directory of the repository: the
/// commands name the build machine's paths, which the repository does not hold.
fn include_hints(commands: &[Bytes], ix: &Index) -> Vec<String> {
    let mut hints: Vec<String> = Vec::new();
    for bytes in commands {
        let Ok(serde_json::Value::Array(entries)) =
            serde_json::from_slice::<serde_json::Value>(bytes)
        else {
            continue;
        };
        for entry in &entries {
            let words: Vec<String> = match (entry.get("arguments"), entry.get("command")) {
                (Some(serde_json::Value::Array(args)), _) => args
                    .iter()
                    .filter_map(|a| a.as_str().map(str::to_string))
                    .collect(),
                (_, Some(serde_json::Value::String(line))) => {
                    line.split_whitespace().map(str::to_string).collect()
                }
                _ => continue,
            };
            let mut at = 0;
            while at < words.len() {
                let word = &words[at];
                let dir = if let Some(rest) = word.strip_prefix("-I") {
                    if rest.is_empty() {
                        at += 1;
                        words.get(at).cloned()
                    } else {
                        Some(rest.to_string())
                    }
                } else if matches!(word.as_str(), "-iquote" | "-isystem" | "-idirafter") {
                    at += 1;
                    words.get(at).cloned()
                } else {
                    None
                };
                at += 1;
                let Some(dir) = dir else { continue };
                let parts: Vec<&str> = dir
                    .split('/')
                    .filter(|p| !p.is_empty() && *p != ".")
                    .collect();
                let found = (0..parts.len())
                    .map(|from| parts[from..].join("/"))
                    .find(|suffix| ix.dirs.contains(suffix));
                if let Some(found) = found {
                    if !hints.contains(&found) {
                        hints.push(found);
                    }
                }
            }
        }
    }
    hints
}

// ---- the header of a definition

/// What the written declaration of a definition says (see the facts' conventions).
#[derive(Default, Clone, Debug)]
struct Sig {
    /// The project type a variable, a field or a function's result names.
    declared: Option<String>,
    /// A field or variable of function-pointer type.
    pointer: bool,
    /// The type a typedef stands for.
    alias_of: Option<String>,
    /// What a function takes: the fewest arguments, and the most (`None`: any
    /// number).
    params: Option<(usize, Option<usize>)>,
    /// A method declared `virtual`.
    virtual_: bool,
    /// What a class lists as its bases, as written.
    bases: Vec<String>,
}

fn words(text: &str) -> Vec<&str> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == ':'))
        .filter(|w| w.chars().any(char::is_alphanumeric))
        .collect()
}

/// The first type the words name, when a declarator name follows it.
fn type_word(text: &str) -> Option<String> {
    let named: Vec<&str> = words(text)
        .into_iter()
        .filter(|w| !BUILTIN_WORDS.contains(w))
        .collect();
    (named.len() >= 2).then(|| named[0].to_string())
}

fn parse_sig(kind: DefKind, text: &str) -> Sig {
    let mut sig = Sig::default();
    match kind {
        DefKind::Field | DefKind::Variable => {
            sig.pointer = text.contains("(*");
            if !sig.pointer {
                sig.declared = type_word(text);
            }
        }
        DefKind::Function | DefKind::Method | DefKind::Constructor => {
            let body = strip_template(text);
            let head = body.split('(').next().unwrap_or("");
            sig.declared = type_word(head);
            sig.virtual_ = words(head).contains(&"virtual");
            sig.params = params_of(body);
        }
        DefKind::Class | DefKind::Struct => {
            let body = strip_template(text);
            if let Some((_, bases)) = body.split_once(" : ") {
                sig.bases = split_top(bases, ',')
                    .iter()
                    .filter_map(|b| {
                        let name = words(b).into_iter().find(|w| {
                            !matches!(*w, "public" | "protected" | "private" | "virtual")
                        })?;
                        Some(name.split('<').next().unwrap_or(name).to_string())
                    })
                    .collect();
            }
        }
        DefKind::TypeAlias if text.trim_start().starts_with("using ") => {
            sig.alias_of = text
                .split_once('=')
                .and_then(|(_, rhs)| words(rhs).into_iter().find(|w| !BUILTIN_WORDS.contains(w)))
                .map(|w| w.to_string());
        }
        DefKind::TypeAlias => {
            let body = text.trim().strip_prefix("typedef").unwrap_or(text);
            let ws: Vec<&str> = words(body)
                .into_iter()
                .filter(|w| !matches!(*w, "const" | "volatile"))
                .collect();
            let tagged = matches!(ws.first(), Some(&"struct" | &"union" | &"enum"));
            sig.alias_of = if tagged {
                ws.get(1).map(|w| (*w).to_string())
            } else if ws.len() >= 2 && !BUILTIN_WORDS.contains(&ws[0]) {
                Some(ws[0].to_string())
            } else {
                None
            };
        }
        _ => {}
    }
    sig
}

// ---- the index

struct DefRec {
    file: usize,
    def: Def,
    key: DefKey,
    sig: Sig,
    /// The namespaces and classes the definition is in, outermost first.
    scope: Vec<String>,
}

/// A `using` declaration or directive (C++).
struct UsingRec {
    /// The namespace block it sits in.
    scope: Vec<String>,
    module: Vec<String>,
    /// The name a declaration takes; none for a directive.
    name: Option<String>,
    /// `namespace alias = module;`: the spelling the namespace takes here.
    alias: Option<String>,
}

struct IncRec {
    module: String,
    quoted: bool,
    site: Span,
    scope: Scope,
}

struct FileRec {
    facts: FileFacts,
    dir: String,
    includes: Vec<IncRec>,
    usings: Vec<UsingRec>,
    children: HashMap<Path, Vec<usize>>,
    /// The file is C++: its extension, a namespace, a class or a `using`.
    cpp: bool,
}

#[derive(Default)]
struct Index {
    files: Vec<FileRec>,
    defs: Vec<DefRec>,
    by_file: HashMap<String, usize>,
    /// Every directory of the repository that holds a C file somewhere below.
    dirs: HashSet<String>,
    /// File-scope definitions by name.
    top: HashMap<String, Vec<usize>>,
    /// The names read as values: a function among them has its address taken.
    taken: HashSet<String>,
    /// Every definition by its qualified name (`a::B::f`).
    qual: HashMap<String, Vec<usize>>,
    /// Every definition by its short name.
    by_short: HashMap<String, Vec<usize>>,
    /// A definition by its file and key.
    by_local: HashMap<(usize, LocalKey), usize>,
}

fn is_cpp_path(path: &str) -> bool {
    matches!(
        path.rsplit_once('.').map(|(_, e)| e),
        Some("cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" | "c++" | "h++" | "ipp" | "tpp")
    )
}

fn is_header(path: &str) -> bool {
    path.ends_with(".h")
}

impl Index {
    fn build(files: Vec<FileFacts>) -> Self {
        let mut index = Self::default();
        for facts in files {
            let at = index.files.len();
            let blob = std::sync::Arc::clone(&facts.blob);
            let path = facts.path.as_str().to_string();
            let mut dir = dir_of(&path);
            loop {
                if !index.dirs.insert(dir.to_string()) || dir.is_empty() {
                    break;
                }
                dir = dir_of(dir);
            }
            index.by_file.insert(path.clone(), at);
            let mut file = FileRec {
                dir: dir_of(&path).to_string(),
                facts,
                includes: Vec::new(),
                usings: Vec::new(),
                children: HashMap::new(),
                cpp: is_cpp_path(&path),
            };
            for fact in &blob.facts {
                match fact {
                    Fact::Import(import) if import.kind == ImportKind::Require => {
                        file.includes.push(IncRec {
                            module: import.module.as_str().to_string(),
                            quoted: import.relative > 0,
                            site: import.site.clone(),
                            scope: import.enclosing.clone(),
                        })
                    }
                    Fact::Import(import) => {
                        file.cpp = true;
                        let module: Vec<String> = import
                            .module
                            .as_str()
                            .split("::")
                            .filter(|p| !p.is_empty())
                            .map(str::to_string)
                            .collect();
                        let scope = match &import.enclosing {
                            Scope::Def(key) => key
                                .container
                                .iter()
                                .chain([&key.name])
                                .map(|n| n.as_str().to_string())
                                .collect(),
                            _ => Vec::new(),
                        };
                        if let Star::Alias(alias) = &import.star {
                            file.usings.push(UsingRec {
                                scope,
                                module,
                                name: None,
                                alias: Some(alias.as_str().to_string()),
                            });
                            continue;
                        }
                        let names: Vec<Option<String>> = if import.names.is_empty() {
                            vec![None]
                        } else {
                            import
                                .names
                                .iter()
                                .map(|(n, _)| Some(n.as_str().to_string()))
                                .collect()
                        };
                        for name in names {
                            file.usings.push(UsingRec {
                                scope: scope.clone(),
                                module: module.clone(),
                                name,
                                alias: None,
                            });
                        }
                    }
                    Fact::Reference(r)
                        if r.use_kind == UseKind::Read
                            && matches!(r.receiver, Receiver::None)
                            && r.qualifier.is_empty() =>
                    {
                        index.taken.insert(r.name.as_str().to_string());
                    }
                    Fact::Reference(r) if r.use_kind == UseKind::PassAsValue => {
                        index.taken.insert(r.name.as_str().to_string());
                    }
                    _ => {}
                }
            }
            if blob.facts.iter().any(|f| {
                matches!(f, Fact::Definition(d) if matches!(d.kind, DefKind::Module | DefKind::Class))
            }) {
                file.cpp = true;
            }
            index.files.push(file);
            for fact in &blob.facts {
                if let Fact::Definition(def) = fact {
                    index.add_def(at, def);
                }
            }
        }
        index
    }

    fn add_def(&mut self, at: usize, def: &Def) {
        let key = self.files[at].facts.key(&def.local);
        let sig = parse_sig(def.kind, def.signature.as_deref().unwrap_or(""));
        let id = self.defs.len();
        self.files[at]
            .children
            .entry(def.local.container.clone())
            .or_default()
            .push(id);
        // An enumerator is global whatever enum holds it.
        let global = def.local.container.is_empty()
            || (def.kind == DefKind::EnumMember && def.local.container.len() == 1);
        if global {
            self.top
                .entry(def.local.name.as_str().to_string())
                .or_default()
                .push(id);
        }
        let scope: Vec<String> = def
            .local
            .container
            .iter()
            .map(|n| n.as_str().to_string())
            .collect();
        let qualified = scope
            .iter()
            .map(String::as_str)
            .chain([def.local.name.as_str()])
            .collect::<Vec<_>>()
            .join("::");
        self.qual.entry(qualified).or_default().push(id);
        self.by_short
            .entry(def.local.name.as_str().to_string())
            .or_default()
            .push(id);
        self.by_local.insert((at, def.local.clone()), id);
        self.defs.push(DefRec {
            file: at,
            def: def.clone(),
            key,
            sig,
            scope,
        });
    }

    fn file_path(&self, file: usize) -> &str {
        self.files[file].facts.path.as_str()
    }
}

// ---- the engine

/// What a chain of members stands for at a step.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Ent {
    /// A struct or union of the project.
    Struct(usize),
    /// A type the project does not define.
    Foreign,
    Unknown,
}

/// What an include lands on.
#[derive(Clone, Debug)]
enum Inc {
    File(usize),
    /// Several files answer the path equally.
    Ambiguous,
    /// No file of the project: the library's.
    System,
    Missing,
}

struct Engine<'w> {
    ix: Index,
    roots: Vec<String>,
    builds: Vec<String>,
    workspace: &'w WorkspaceModel,
    includes: RefCell<HashMap<(usize, usize), Inc>>,
    closures: RefCell<HashMap<usize, Rc<Vec<usize>>>>,
    reader: &'w SnapshotReader,
    side: SideTag,
    texts: RefCell<HashMap<usize, Option<Bytes>>>,
    losses: RefCell<Vec<Loss>>,
}

fn relation_for(use_kind: UseKind, target: DefKind) -> Relation {
    match (use_kind, target) {
        (UseKind::Read, DefKind::Function) => Relation::PassesAsValue,
        (UseKind::Call, _) => Relation::Calls,
        (UseKind::Read, _) => Relation::Reads,
        (UseKind::Write, _) => Relation::Writes,
        (UseKind::TypeUse, _) => Relation::TypeUses,
        (UseKind::PassAsValue, _) => Relation::PassesAsValue,
        (UseKind::Instantiate | UseKind::Decorate, _) => Relation::Instantiates,
        (UseKind::Override, _) => Relation::Overrides,
    }
}

impl<'w> Engine<'w> {
    fn new(
        ix: Index,
        hints: Vec<String>,
        builds: Vec<String>,
        workspace: &'w WorkspaceModel,
        reader: &'w SnapshotReader,
        side: SideTag,
    ) -> Self {
        let roots = Self::roots_of(&ix, &hints);
        Self {
            ix,
            roots,
            builds,
            workspace,
            includes: RefCell::default(),
            closures: RefCell::default(),
            reader,
            side,
            texts: RefCell::default(),
            losses: RefCell::default(),
        }
    }

    fn run(&self, ctx: &AnalysisContext) -> (Vec<Definition>, Vec<Binding>, bool) {
        let mut bindings = Vec::new();
        let mut stopped = false;
        'files: for (f, file) in self.ix.files.iter().enumerate() {
            for fact in &file.facts.blob.facts {
                if expired(&ctx.run) {
                    stopped = true;
                    break 'files;
                }
                if let Fact::Reference(r) = fact {
                    self.reference(f, r, &mut bindings);
                }
            }
            for (at, include) in file.includes.iter().enumerate() {
                self.include_binding(f, at, include, &mut bindings);
            }
        }
        let definitions = self
            .ix
            .defs
            .iter()
            .map(|rec| Definition {
                file: self.ix.files[rec.file].facts.path.clone(),
                key: rec.key.clone(),
                kind: rec.def.kind,
                span: rec.def.span.clone(),
                name_span: rec.def.name_span.clone(),
            })
            .collect();
        (definitions, bindings, stopped)
    }

    // ---- includes

    /// The directories an include is looked for in after the including file's own:
    /// the hints, the `include`-like directories, the repository root.
    fn roots_of(ix: &Index, hints: &[String]) -> Vec<String> {
        let mut roots: Vec<String> = hints.to_vec();
        let mut named: Vec<&String> = ix
            .dirs
            .iter()
            .filter(|d| {
                matches!(
                    d.rsplit('/').next(),
                    Some("include" | "inc" | "includes" | "headers")
                )
            })
            .collect();
        named.sort_by_key(|d| (d.matches('/').count(), d.to_string()));
        roots.extend(named.into_iter().cloned());
        roots.push(String::new());
        roots
    }

    fn resolve_include(&self, f: usize, at: usize) -> Inc {
        if let Some(known) = self.includes.borrow().get(&(f, at)) {
            return known.clone();
        }
        let file = &self.ix.files[f];
        let include = &file.includes[at];
        let mut candidates: Vec<String> = Vec::new();
        if include.quoted {
            candidates.push(normalise(&join(&file.dir, &include.module)));
        }
        candidates.extend(
            self.roots
                .iter()
                .map(|root| normalise(&join(root, &include.module))),
        );
        let found = candidates
            .iter()
            .find_map(|c| self.ix.by_file.get(c).copied());
        let result = match found {
            Some(target) => Inc::File(target),
            None => {
                let wanted = normalise(&include.module);
                let suffix = format!("/{wanted}");
                let matches: Vec<usize> = self
                    .ix
                    .by_file
                    .iter()
                    .filter(|(path, _)| **path == wanted || path.ends_with(&suffix))
                    .map(|(_, at)| *at)
                    .collect();
                match matches.as_slice() {
                    [one] => Inc::File(*one),
                    [] if include.quoted => Inc::Missing,
                    [] => Inc::System,
                    _ => Inc::Ambiguous,
                }
            }
        };
        self.includes.borrow_mut().insert((f, at), result.clone());
        result
    }

    /// The file and the files it includes, transitively.
    fn closure(&self, f: usize) -> Rc<Vec<usize>> {
        if let Some(known) = self.closures.borrow().get(&f) {
            return Rc::clone(known);
        }
        let mut seen: Vec<usize> = vec![f];
        let mut at = 0;
        while at < seen.len() {
            let file = seen[at];
            at += 1;
            for i in 0..self.ix.files[file].includes.len() {
                if let Inc::File(target) = self.resolve_include(file, i) {
                    if !seen.contains(&target) {
                        seen.push(target);
                    }
                }
            }
        }
        let seen = Rc::new(seen);
        self.closures.borrow_mut().insert(f, Rc::clone(&seen));
        seen
    }

    fn include_binding(&self, f: usize, at: usize, include: &IncRec, out: &mut Vec<Binding>) {
        let resolution = match self.resolve_include(f, at) {
            Inc::File(target) => match RepoPath::new(self.ix.file_path(target)) {
                Ok(file) => Resolution::Module {
                    file,
                    evidence: Evidence::new(EvidenceKind::ImportedName),
                },
                Err(_) => return,
            },
            Inc::System => unresolved(UnresolvedReason::External),
            Inc::Missing => unresolved(UnresolvedReason::MissingModule),
            Inc::Ambiguous => unresolved(UnresolvedReason::Opaque),
        };
        out.push(Binding {
            file: self.ix.files[f].facts.path.clone(),
            site: include.site.clone(),
            from: include.scope.clone(),
            relation: Relation::Imports,
            name: Name::new(include.module.rsplit('/').next().unwrap_or(&include.module)),
            resolution,
            entry_guard: false,
        });
    }

    // ---- names

    /// Whether code in file `f` can name the definition: it is in the file, or in
    /// a header the file includes; a `static` function of a source file stays in
    /// it.
    fn sees(&self, f: usize, id: usize, closure: &[usize]) -> bool {
        let rec = &self.ix.defs[id];
        if rec.file == f {
            return true;
        }
        if !closure.contains(&rec.file) {
            return false;
        }
        rec.def.visibility != Visibility::Private || is_header(self.ix.file_path(rec.file))
    }

    fn may_reach(&self, f: usize, id: usize) -> bool {
        self.workspace.may_reference(
            self.ix.file_path(f),
            self.ix.file_path(self.ix.defs[id].file),
        )
    }

    /// The file-scope definitions of the name that `f` sees.
    fn visible(&self, f: usize, name: &str, wanted: fn(DefKind) -> bool) -> Vec<usize> {
        let closure = self.closure(f);
        self.ix
            .top
            .get(name)
            .into_iter()
            .flatten()
            .copied()
            .filter(|id| wanted(self.ix.defs[*id].def.kind) && self.sees(f, *id, &closure))
            .collect()
    }

    /// The build tree a file is in, when a build file stands above it.
    fn build_of(&self, file: usize) -> Option<&str> {
        let path = self.ix.file_path(file);
        self.builds
            .iter()
            .find(|root| {
                root.is_empty()
                    || path
                        .strip_prefix(root.as_str())
                        .is_some_and(|rest| rest.starts_with('/'))
            })
            .map(String::as_str)
    }

    /// The external definitions of a function: what the linker finds. Where the
    /// caller's own build defines the name, its linker finds that one.
    fn linked(&self, f: usize, name: &str) -> Vec<usize> {
        let found: Vec<usize> = self
            .ix
            .top
            .get(name)
            .into_iter()
            .flatten()
            .copied()
            .filter(|id| {
                let rec = &self.ix.defs[*id];
                rec.def.kind == DefKind::Function
                    && rec.def.body_hash != 0
                    && rec.def.visibility != Visibility::Private
                    && self.may_reach(f, *id)
            })
            .collect();
        let own: Vec<usize> = match self.build_of(f) {
            Some(tree) => found
                .iter()
                .copied()
                .filter(|id| self.build_of(self.ix.defs[*id].file) == Some(tree))
                .collect(),
            None => Vec::new(),
        };
        if own.is_empty() { found } else { own }
    }

    ground_methods!();

    /// Of definitions of one name, the ones that are the same thing in a
    /// different place: a definition under `#if` stands beside the others.
    fn settle(&self, ids: Vec<usize>, how: EvidenceKind) -> Resolution {
        let conditional = ids.iter().any(|i| {
            matches!(
                self.ix.defs[*i].def.local.disambig,
                Some(Disambig::Cfg(ref cond)) if cond != "declaration"
            )
        });
        match ids.as_slice() {
            [] => unresolved(UnresolvedReason::NoBinding),
            [one] if !conditional => self.resolved(*one, how),
            many => self.candidates(many, CandidateReason::Overloaded),
        }
    }

    /// Why a name nothing found is not bound: the library's, or an unknown when
    /// the project defines the name somewhere else.
    fn missing(&self, name: &str) -> Resolution {
        if self.ix.top.contains_key(name) {
            unresolved(UnresolvedReason::NoBinding)
        } else {
            unresolved(UnresolvedReason::External)
        }
    }

    // ---- references

    fn reference(&self, f: usize, r: &Ref, out: &mut Vec<Binding>) {
        let name = r.name.as_str();
        let make = |relation: Relation, resolution: Resolution| Binding {
            file: self.ix.files[f].facts.path.clone(),
            site: r.site.clone(),
            from: r.enclosing.clone(),
            relation,
            name: r.name.clone(),
            resolution,
            entry_guard: false,
        };
        if self.ix.files[f].cpp {
            self.cpp_reference(f, r, out, &make);
            return;
        }
        match r.use_kind {
            UseKind::TypeUse => {
                let found = self.visible(f, name, |k| {
                    matches!(k, DefKind::Struct | DefKind::Enum | DefKind::TypeAlias)
                });
                let resolution = self.pick_type(found, name);
                out.push(make(Relation::TypeUses, resolution));
            }
            _ if !matches!(r.receiver, Receiver::None) || !r.qualifier.is_empty() => {
                let (relation, resolution) = self.member(f, r);
                out.push(make(relation, resolution));
            }
            _ => self.bare(f, r, out, &make),
        }
    }

    /// A struct or enum beats the typedef of the same name.
    fn pick_type(&self, found: Vec<usize>, name: &str) -> Resolution {
        let concrete: Vec<usize> = found
            .iter()
            .copied()
            .filter(|i| self.ix.defs[*i].def.kind != DefKind::TypeAlias)
            .collect();
        let pool = if concrete.is_empty() { found } else { concrete };
        match pool.as_slice() {
            [] => self.missing(name),
            _ => self.settle(pool, EvidenceKind::ImportedName),
        }
    }

    fn bare(
        &self,
        f: usize,
        r: &Ref,
        out: &mut Vec<Binding>,
        make: &dyn Fn(Relation, Resolution) -> Binding,
    ) {
        let name = r.name.as_str();
        let call = r.use_kind == UseKind::Call;
        let macros = self.visible(f, name, |k| k == DefKind::Macro);
        if !macros.is_empty() {
            let mut all = macros;
            all.extend(self.visible(f, name, |k| k == DefKind::Function));
            out.push(make(
                relation_for(r.use_kind, DefKind::Macro),
                self.candidates(&all, CandidateReason::Macro),
            ));
            return;
        }
        let functions = self.visible(f, name, |k| k == DefKind::Function);
        let linked = self.linked(f, name);
        // A global holding a function pointer, called by its name.
        if call
            && functions.is_empty()
            && linked.is_empty()
            && !self.visible(f, name, |k| k == DefKind::Variable).is_empty()
        {
            out.push(make(Relation::Calls, self.indirect(f)));
            return;
        }
        if call || !functions.is_empty() || !linked.is_empty() {
            self.function_use(f, r, functions, linked, out, make);
            return;
        }
        let values = self.visible(f, name, |k| {
            matches!(k, DefKind::Variable | DefKind::EnumMember)
        });
        if !values.is_empty() {
            // A definition beats the `extern` declaration of the same variable.
            let defined: Vec<usize> = values
                .iter()
                .copied()
                .filter(|i| self.ix.defs[*i].def.body_hash != 0)
                .collect();
            let pool = if defined.is_empty() { values } else { defined };
            let kind = self.ix.defs[pool[0]].def.kind;
            out.push(make(
                relation_for(r.use_kind, kind),
                self.settle(pool, EvidenceKind::ImportedName),
            ));
            return;
        }
        let outside: Vec<usize> = self
            .ix
            .top
            .get(name)
            .into_iter()
            .flatten()
            .copied()
            .filter(|i| {
                let rec = &self.ix.defs[*i];
                matches!(rec.def.kind, DefKind::Variable | DefKind::EnumMember)
                    && rec.def.visibility != Visibility::Private
                    && rec.def.body_hash != 0
                    && self.may_reach(f, *i)
            })
            .collect();
        if !outside.is_empty() {
            out.push(make(
                relation_for(r.use_kind, DefKind::Variable),
                self.candidates(&outside, CandidateReason::ImportUnresolved),
            ));
            return;
        }
        out.push(make(
            relation_for(r.use_kind, DefKind::Variable),
            self.missing(name),
        ));
    }

    /// A call or a use as a value of a function name: the definition, and the
    /// prototype the includes show beside it.
    fn function_use(
        &self,
        f: usize,
        r: &Ref,
        visible: Vec<usize>,
        linked: Vec<usize>,
        out: &mut Vec<Binding>,
        make: &dyn Fn(Relation, Resolution) -> Binding,
    ) {
        let relation = relation_for(r.use_kind, DefKind::Function);
        let name = r.name.as_str();
        if visible.is_empty() && linked.is_empty() {
            out.push(make(relation, self.missing(name)));
            return;
        }
        if visible.is_empty() {
            out.push(make(
                relation,
                self.candidates(&linked, CandidateReason::ImportUnresolved),
            ));
            return;
        }
        // A `static` function of the file is the one the name means.
        let own: Vec<usize> = visible
            .iter()
            .copied()
            .filter(|i| {
                let rec = &self.ix.defs[*i];
                rec.file == f && rec.def.visibility == Visibility::Private
            })
            .collect();
        if !own.is_empty() {
            out.push(make(relation, self.settle(own, EvidenceKind::SameScope)));
            return;
        }
        let defined: Vec<usize> = visible
            .iter()
            .copied()
            .filter(|i| self.ix.defs[*i].def.body_hash != 0)
            .collect();
        let primary: Vec<usize> = if !linked.is_empty() {
            linked.clone()
        } else if !defined.is_empty() {
            defined
        } else {
            visible.clone()
        };
        let how = if primary.iter().all(|i| self.ix.defs[*i].file == f) {
            EvidenceKind::SameScope
        } else {
            EvidenceKind::ImportedName
        };
        out.push(make(relation, self.settle(primary.clone(), how)));
        // The prototype a header shows is a second place the call depends on.
        let declarations: Vec<usize> = visible
            .into_iter()
            .filter(|i| self.ix.defs[*i].def.body_hash == 0 && !primary.contains(i))
            .collect();
        if declarations.len() == 1 {
            out.push(make(
                relation,
                self.resolved(declarations[0], EvidenceKind::ImportedName),
            ));
        }
    }

    // ---- members

    /// A tag or a typedef name as the struct it stands for.
    fn struct_named(&self, f: usize, name: &str, depth: usize) -> Ent {
        if depth > MAX_DEPTH {
            return Ent::Unknown;
        }
        let found = self.visible(f, name, |k| {
            matches!(k, DefKind::Struct | DefKind::TypeAlias | DefKind::Enum)
        });
        let concrete = found
            .iter()
            .copied()
            .find(|i| self.ix.defs[*i].def.kind == DefKind::Struct);
        if let Some(id) = concrete {
            return Ent::Struct(id);
        }
        let alias = found
            .iter()
            .copied()
            .find(|i| self.ix.defs[*i].def.kind == DefKind::TypeAlias);
        match alias.and_then(|i| self.ix.defs[i].sig.alias_of.clone()) {
            Some(next) if next != name => {
                let rec = &self.ix.defs[alias.unwrap_or(0)];
                self.struct_named(rec.file, &next, depth + 1)
            }
            _ if found.is_empty() => Ent::Foreign,
            _ => Ent::Unknown,
        }
    }

    fn field_of(&self, structure: usize, name: &str) -> Vec<usize> {
        let rec = &self.ix.defs[structure];
        let container = vec![rec.def.local.name.clone()];
        self.ix.files[rec.file]
            .children
            .get(&container)
            .into_iter()
            .flatten()
            .copied()
            .filter(|i| {
                let d = &self.ix.defs[*i].def;
                d.kind == DefKind::Field && d.local.name.as_str() == name
            })
            .collect()
    }

    /// What the type a declaration writes is, seen from the file that writes it.
    fn declared_ent(&self, id: usize) -> Ent {
        let rec = &self.ix.defs[id];
        match &rec.sig.declared {
            Some(name) => self.struct_named(rec.file, name, 0),
            None => Ent::Unknown,
        }
    }

    fn member(&self, f: usize, r: &Ref) -> (Relation, Resolution) {
        let plain = relation_for(r.use_kind, DefKind::Field);
        let call = r.use_kind == UseKind::Call;
        let mut chain: Vec<&Name> = r.qualifier.iter().collect();
        let mut entity = match &r.receiver {
            Receiver::Typed(t) | Receiver::New(t) => self.struct_named(f, t.as_str(), 0),
            Receiver::CallResult(path) => path.last().map_or(Ent::Unknown, |name| {
                let fns = self.visible(f, name.as_str(), |k| k == DefKind::Function);
                match fns.as_slice() {
                    [one] => self.declared_ent(*one),
                    _ => Ent::Unknown,
                }
            }),
            Receiver::None => {
                // The first name is a global; the rest are members.
                let head = chain.first().map(|n| n.as_str()).unwrap_or("");
                let globals = self.visible(f, head, |k| k == DefKind::Variable);
                if !chain.is_empty() {
                    chain.remove(0);
                }
                match globals.as_slice() {
                    [] => {
                        if self.ix.top.contains_key(head) {
                            Ent::Unknown
                        } else {
                            Ent::Foreign
                        }
                    }
                    [one, ..] => self.declared_ent(*one),
                }
            }
            _ => Ent::Unknown,
        };
        for name in chain {
            entity = match entity {
                Ent::Struct(s) => match self.field_of(s, name.as_str()).first() {
                    Some(field) => self.declared_ent(*field),
                    None => Ent::Unknown,
                },
                other => other,
            };
        }
        match entity {
            Ent::Struct(structure) => {
                let fields = self.field_of(structure, r.name.as_str());
                match fields.as_slice() {
                    [] => (plain, unresolved(UnresolvedReason::NoMember)),
                    [_] if call => (Relation::Calls, self.indirect(f)),
                    [one] => (plain, self.resolved(*one, EvidenceKind::TypedReceiver)),
                    many => (plain, self.candidates(many, CandidateReason::Overloaded)),
                }
            }
            Ent::Foreign => (plain, unresolved(UnresolvedReason::External)),
            Ent::Unknown if call => (Relation::Calls, self.indirect(f)),
            Ent::Unknown => (plain, unresolved(UnresolvedReason::UntypedRead)),
        }
    }

    /// A call through a pointer: any function whose address the project takes.
    fn indirect(&self, f: usize) -> Resolution {
        let closure = self.closure(f);
        let pool: Vec<usize> = self
            .ix
            .defs
            .iter()
            .enumerate()
            .filter(|(id, rec)| {
                rec.def.kind == DefKind::Function
                    && rec.def.body_hash != 0
                    && self.ix.taken.contains(rec.def.local.name.as_str())
                    && self.may_reach(f, *id)
                    && (rec.def.visibility != Visibility::Private || self.sees(f, *id, &closure))
            })
            .map(|(id, _)| id)
            .collect();
        match pool.len() {
            0 => unresolved(UnresolvedReason::NoCandidates),
            n if n > MAX_CANDIDATES => unresolved(UnresolvedReason::TooManyCandidates),
            _ => self.candidates(&pool, CandidateReason::Dispatch),
        }
    }
}

// ---- C++

/// The text after a leading `template <...>` header.
fn strip_template(text: &str) -> &str {
    let t = text.trim_start();
    let Some(rest) = t.strip_prefix("template") else {
        return t;
    };
    let rest = rest.trim_start();
    if !rest.starts_with('<') {
        return t;
    }
    let mut depth = 0usize;
    for (i, c) in rest.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return rest[i + 1..].trim_start();
                }
            }
            _ => {}
        }
    }
    t
}

/// `text` split at `separator` where no bracket is open.
fn split_top(text: &str, separator: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut from = 0;
    for (i, c) in text.char_indices() {
        match c {
            '(' | '[' | '{' | '<' => depth += 1,
            ')' | ']' | '}' | '>' => depth = depth.saturating_sub(1),
            c if c == separator && depth == 0 => {
                parts.push(&text[from..i]);
                from = i + c.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&text[from..]);
    parts
}

/// How many arguments a function written in `text` takes: at least, at most.
fn params_of(text: &str) -> Option<(usize, Option<usize>)> {
    let open = text.find('(')?;
    let mut depth = 0usize;
    let mut close = None;
    for (i, c) in text[open..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    close = Some(open + i);
                    break;
                }
            }
            _ => {}
        }
    }
    let inside = text[open + 1..close?].trim();
    if inside.is_empty() || inside == "void" {
        return Some((0, Some(0)));
    }
    let parts = split_top(inside, ',');
    let variadic = parts.iter().any(|p| p.contains("..."));
    let min = parts
        .iter()
        .filter(|p| !p.contains('=') && !p.contains("..."))
        .count();
    let all = parts.iter().filter(|p| !p.contains("...")).count();
    Some((min, (!variadic).then_some(all)))
}

fn is_type_kind(kind: DefKind) -> bool {
    matches!(
        kind,
        DefKind::Class | DefKind::Struct | DefKind::Enum | DefKind::TypeAlias
    )
}

fn is_value_kind(kind: DefKind) -> bool {
    matches!(
        kind,
        DefKind::Field | DefKind::Variable | DefKind::Constant | DefKind::EnumMember
    )
}

fn is_callable_or_type(kind: DefKind) -> bool {
    is_callable(kind) || is_type_kind(kind)
}

fn is_value_or_callable(kind: DefKind) -> bool {
    is_value_kind(kind) || is_callable(kind)
}

fn cpp_relation(use_kind: UseKind, target: DefKind) -> Relation {
    match (use_kind, target) {
        (UseKind::Call, DefKind::Class | DefKind::Struct | DefKind::TypeAlias) => {
            Relation::Instantiates
        }
        _ => relation_for(use_kind, target),
    }
}

impl Engine<'_> {
    file_text!();

    /// The arguments the call at `r` passes, read off the text; none where no
    /// parenthesis follows the name.
    fn args_at(&self, f: usize, r: &Ref) -> Option<usize> {
        let text = self.text(f)?;
        let mut at = r.site.bytes.end as usize;
        while text.get(at).is_some_and(u8::is_ascii_whitespace) {
            at += 1;
        }
        if text.get(at) == Some(&b'<') {
            let mut depth = 0usize;
            while let Some(&c) = text.get(at) {
                match c {
                    b'<' => depth += 1,
                    b'>' => {
                        depth = depth.saturating_sub(1);
                        if depth == 0 {
                            at += 1;
                            break;
                        }
                    }
                    _ => {}
                }
                at += 1;
            }
            while text.get(at).is_some_and(u8::is_ascii_whitespace) {
                at += 1;
            }
        }
        if text.get(at) != Some(&b'(') {
            return None;
        }
        at += 1;
        let (mut depth, mut commas, mut any) = (0usize, 0usize, false);
        while let Some(&c) = text.get(at) {
            match c {
                b'"' | b'\'' => {
                    any = true;
                    at += 1;
                    while text.get(at).is_some_and(|d| *d != c) {
                        if text[at] == b'\\' {
                            at += 1;
                        }
                        at += 1;
                    }
                }
                b'(' | b'[' | b'{' => {
                    depth += 1;
                    any = true;
                }
                b')' | b']' | b'}' => {
                    if depth == 0 {
                        return Some(if any { commas + 1 } else { 0 });
                    }
                    depth -= 1;
                }
                b',' if depth == 0 => commas += 1,
                c if !c.is_ascii_whitespace() => any = true,
                _ => {}
            }
            at += 1;
        }
        None
    }

    /// Whether the name at `r` is written after `.` or `->`.
    fn after_member_operator(&self, f: usize, r: &Ref) -> bool {
        let Some(text) = self.text(f) else {
            return false;
        };
        let mut at = r.site.bytes.start as usize;
        while at > 0 && text[at - 1].is_ascii_whitespace() {
            at -= 1;
        }
        at > 0 && (text[at - 1] == b'.' || (text[at - 1] == b'>' && at > 1 && text[at - 2] == b'-'))
    }

    /// The namespaces and classes a reference is written in: its definition's
    /// container, and the definition itself where it holds names.
    fn lookup_scope(&self, f: usize, enclosing: &Scope) -> Vec<String> {
        match enclosing {
            Scope::Def(key) => {
                let mut path: Vec<String> = key
                    .container
                    .iter()
                    .map(|n| n.as_str().to_string())
                    .collect();
                let holds = self.ix.by_local.get(&(f, key.clone())).is_some_and(|id| {
                    matches!(
                        self.ix.defs[*id].def.kind,
                        DefKind::Module | DefKind::Class | DefKind::Struct
                    )
                });
                if holds {
                    path.push(key.name.as_str().to_string());
                }
                path
            }
            Scope::Callback { parent, .. } => self.lookup_scope(f, parent),
            Scope::Module => Vec::new(),
        }
    }

    fn class_at(&self, path: &[String]) -> Option<usize> {
        if path.is_empty() {
            return None;
        }
        self.ix
            .qual
            .get(&path.join("::"))?
            .iter()
            .copied()
            .find(|i| matches!(self.ix.defs[*i].def.kind, DefKind::Class | DefKind::Struct))
    }

    fn class_of_scope(&self, scope: &[String]) -> Option<usize> {
        (1..=scope.len())
            .rev()
            .find_map(|k| self.class_at(&scope[..k]))
    }

    /// The definitions the name stands for from a scope: the innermost scope that
    /// holds it (its class and the bases', its namespace, a `using` there, the
    /// ones around), or the one a qualifier names.
    fn cpp_find(
        &self,
        f: usize,
        scope: &[String],
        qualifier: &[Name],
        name: &str,
        wanted: fn(DefKind) -> bool,
    ) -> (Vec<usize>, EvidenceKind) {
        let closure = self.closure(f);
        let visible = |id: usize| wanted(self.ix.defs[id].def.kind) && self.sees(f, id, &closure);
        let rooted = qualifier.first().is_some_and(|n| n.as_str().is_empty());
        let mut written: Vec<String> = qualifier
            .iter()
            .map(|n| n.as_str().to_string())
            .filter(|n| !n.is_empty())
            .collect();
        let aliased = closure.iter().find_map(|file| {
            self.ix.files[*file].usings.iter().find(|using| {
                using.alias.is_some()
                    && using.alias.as_ref() == written.first()
                    && scope.starts_with(&using.scope)
            })
        });
        if let Some(using) = aliased {
            written.splice(..1, using.module.iter().cloned());
        }
        let prefixes: Vec<Vec<String>> = if rooted {
            vec![Vec::new()]
        } else {
            (0..=scope.len())
                .rev()
                .map(|k| scope[..k].to_vec())
                .collect()
        };
        let lookup = |path: &[String]| -> Vec<usize> {
            let key = path
                .iter()
                .map(String::as_str)
                .chain([name])
                .collect::<Vec<_>>()
                .join("::");
            self.ix
                .qual
                .get(&key)
                .into_iter()
                .flatten()
                .copied()
                .filter(|i| visible(*i))
                .collect()
        };
        for prefix in &prefixes {
            let mut path = prefix.clone();
            path.extend(written.iter().cloned());
            let direct = lookup(&path);
            if !direct.is_empty() {
                let how = if written.is_empty() {
                    EvidenceKind::SameScope
                } else {
                    EvidenceKind::ModuleAttribute
                };
                return (direct, how);
            }
            if let Some(class) = self.class_at(&path) {
                if let Some((ids, _)) = self.members_named(class, name, wanted, 0) {
                    return (ids, EvidenceKind::Inherited);
                }
            }
            if written.is_empty() && !rooted {
                for file in closure.iter() {
                    for using in &self.ix.files[*file].usings {
                        if using.alias.is_some()
                            || using.scope != *prefix
                            || using.name.as_deref().is_some_and(|n| n != name)
                        {
                            continue;
                        }
                        for base in [Vec::new(), prefix.clone()] {
                            let mut at = base;
                            at.extend(using.module.iter().cloned());
                            let found = lookup(&at);
                            if !found.is_empty() {
                                return (found, EvidenceKind::ImportedName);
                            }
                        }
                    }
                }
            }
        }
        (Vec::new(), EvidenceKind::SameScope)
    }

    /// The classes a class lists as bases, found from where it is defined.
    fn bases_of(&self, class: usize) -> Vec<usize> {
        let rec = &self.ix.defs[class];
        let mut found = Vec::new();
        for base in &rec.sig.bases {
            if let Some(id) = self.type_class(rec.file, &rec.scope, base, 0) {
                if id != class {
                    found.push(id);
                }
            }
        }
        found
    }

    /// The class a written type names, through typedefs and aliases.
    fn type_class(&self, f: usize, scope: &[String], written: &str, depth: usize) -> Option<usize> {
        if depth > MAX_DEPTH {
            return None;
        }
        let mut parts: Vec<&str> = written.split("::").collect();
        let name = parts.pop()?;
        let qualifier: Vec<Name> = parts.into_iter().map(Name::new).collect();
        let (ids, _) = self.cpp_find(f, scope, &qualifier, name, is_type_kind);
        let id = ids.first().copied()?;
        let rec = &self.ix.defs[id];
        match rec.def.kind {
            DefKind::Class | DefKind::Struct => Some(id),
            DefKind::TypeAlias => {
                let next = rec.sig.alias_of.as_deref()?;
                self.type_class(rec.file, &rec.scope, next, depth + 1)
            }
            _ => None,
        }
    }

    /// The members called `name` of the class or of the first base that has any.
    fn members_named(
        &self,
        class: usize,
        name: &str,
        wanted: fn(DefKind) -> bool,
        depth: usize,
    ) -> Option<(Vec<usize>, bool)> {
        if depth > MAX_DEPTH {
            return None;
        }
        let rec = &self.ix.defs[class];
        let key = rec
            .scope
            .iter()
            .map(String::as_str)
            .chain([rec.def.local.name.as_str(), name])
            .collect::<Vec<_>>()
            .join("::");
        let own: Vec<usize> = self
            .ix
            .qual
            .get(&key)
            .into_iter()
            .flatten()
            .copied()
            .filter(|i| wanted(self.ix.defs[*i].def.kind))
            .collect();
        if !own.is_empty() {
            return Some((own, false));
        }
        self.bases_of(class)
            .into_iter()
            .find_map(|b| self.members_named(b, name, wanted, depth + 1))
            .map(|(ids, _)| (ids, true))
    }

    fn is_subclass(&self, sub: usize, sup: usize, depth: usize) -> bool {
        sub == sup
            || (depth < MAX_DEPTH
                && self
                    .bases_of(sub)
                    .into_iter()
                    .any(|b| self.is_subclass(b, sup, depth + 1)))
    }

    /// Whether a class, or one it inherits from, has a base the project does not
    /// hold: a member not found may be there.
    fn open_above(&self, class: usize, depth: usize) -> bool {
        let rec = &self.ix.defs[class];
        depth < MAX_DEPTH
            && (self.bases_of(class).len() < rec.sig.bases.len()
                || self
                    .bases_of(class)
                    .into_iter()
                    .any(|b| self.open_above(b, depth + 1)))
    }

    /// What the type a value declares is, from where the declaration is.
    fn declared_class(&self, id: usize) -> Option<usize> {
        let rec = &self.ix.defs[id];
        let declared = rec.sig.declared.as_deref()?;
        self.type_class(rec.file, &rec.scope, declared, 0)
    }

    /// The overloads that take as many arguments as the call passes.
    fn fitting(&self, f: usize, r: &Ref, ids: Vec<usize>) -> Vec<usize> {
        if ids.len() < 2 {
            return ids;
        }
        let Some(passed) = self.args_at(f, r) else {
            return ids;
        };
        let fits: Vec<usize> = ids
            .iter()
            .copied()
            .filter(|i| match self.ix.defs[*i].sig.params {
                Some((min, max)) => passed >= min && max.is_none_or(|m| passed <= m),
                None => true,
            })
            .collect();
        if fits.is_empty() { ids } else { fits }
    }

    /// A call or use that found definitions: the ones with a body, and the
    /// declaration beside them; a virtual one reached through a class is also
    /// whatever overrides it below.
    #[allow(clippy::too_many_arguments)]
    fn cpp_result(
        &self,
        f: usize,
        r: &Ref,
        ids: Vec<usize>,
        how: EvidenceKind,
        class: Option<usize>,
        out: &mut Vec<Binding>,
        make: &dyn Fn(Relation, Resolution) -> Binding,
    ) {
        let kind = self.ix.defs[ids[0]].def.kind;
        let relation = cpp_relation(r.use_kind, kind);
        let ids = if is_callable(kind) {
            let linked = self.linked_cpp(f, &ids);
            self.fitting(f, r, ids.into_iter().chain(linked).collect())
        } else {
            ids
        };
        let defined: Vec<usize> = ids
            .iter()
            .copied()
            .filter(|i| self.ix.defs[*i].def.body_hash != 0)
            .collect();
        let primary = if defined.is_empty() {
            ids.clone()
        } else {
            defined
        };
        let conditional = primary.iter().any(|i| {
            matches!(
                self.ix.defs[*i].def.local.disambig,
                Some(Disambig::Cfg(ref cond)) if cond != "declaration"
            )
        });
        let virtual_ = ids.iter().any(|i| self.ix.defs[*i].sig.virtual_);
        let resolution = match primary.as_slice() {
            [one] if !conditional => {
                let overriding = match class {
                    Some(class) if virtual_ => self.overriders(class, *one),
                    _ => Vec::new(),
                };
                if overriding.is_empty() {
                    self.resolved(*one, how)
                } else {
                    let mut all = primary.clone();
                    all.extend(overriding);
                    self.candidates(&all, CandidateReason::Dispatch)
                }
            }
            many => self.candidates(many, CandidateReason::Overloaded),
        };
        out.push(make(relation, resolution));
        let declarations: Vec<usize> = ids
            .into_iter()
            .filter(|i| self.ix.defs[*i].def.body_hash == 0 && !primary.contains(i))
            .collect();
        if let [one] = declarations.as_slice() {
            out.push(make(
                relation,
                self.resolved(*one, EvidenceKind::ImportedName),
            ));
        }
    }

    /// The definitions the linker finds for declarations a file sees without one:
    /// those of the same qualified name in files it does not include.
    fn linked_cpp(&self, f: usize, ids: &[usize]) -> Vec<usize> {
        if ids.iter().any(|i| self.ix.defs[*i].def.body_hash != 0) {
            return Vec::new();
        }
        let first = &self.ix.defs[ids[0]];
        let key = first
            .scope
            .iter()
            .map(String::as_str)
            .chain([first.def.local.name.as_str()])
            .collect::<Vec<_>>()
            .join("::");
        self.ix
            .qual
            .get(&key)
            .into_iter()
            .flatten()
            .copied()
            .filter(|i| {
                let rec = &self.ix.defs[*i];
                !ids.contains(i)
                    && is_callable(rec.def.kind)
                    && rec.def.body_hash != 0
                    && rec.def.visibility != Visibility::Private
                    && self.may_reach(f, *i)
            })
            .collect()
    }

    /// The methods of the member's name in the classes below `class`.
    fn overriders(&self, class: usize, member: usize) -> Vec<usize> {
        let name = self.ix.defs[member].def.local.name.as_str();
        let own_scope = &self.ix.defs[member].scope;
        self.ix
            .by_short
            .get(name)
            .into_iter()
            .flatten()
            .copied()
            .filter(|i| {
                let other = &self.ix.defs[*i];
                other.def.kind == DefKind::Method
                    && other.scope != *own_scope
                    && self
                        .class_at(&other.scope)
                        .is_some_and(|c| c != class && self.is_subclass(c, class, 0))
            })
            .collect()
    }

    /// The methods of that name any class the file can hold a value of has.
    fn untyped_methods(&self, f: usize, name: &str) -> Resolution {
        let closure = self.closure(f);
        let pool: Vec<usize> = self
            .ix
            .by_short
            .get(name)
            .into_iter()
            .flatten()
            .copied()
            .filter(|i| {
                let rec = &self.ix.defs[*i];
                rec.def.kind == DefKind::Method
                    && self.may_reach(f, *i)
                    && (rec.file == f || closure.contains(&rec.file))
            })
            .collect();
        match pool.len() {
            0 => unresolved(UnresolvedReason::NoCandidates),
            n if n > MAX_CANDIDATES => unresolved(UnresolvedReason::TooManyCandidates),
            _ => self.candidates(&pool, CandidateReason::UntypedReceiver),
        }
    }

    fn cpp_reference(
        &self,
        f: usize,
        r: &Ref,
        out: &mut Vec<Binding>,
        make: &dyn Fn(Relation, Resolution) -> Binding,
    ) {
        let scope = self.lookup_scope(f, &r.enclosing);
        let name = r.name.as_str();
        if r.use_kind == UseKind::TypeUse {
            let (ids, how) = self.cpp_find(f, &scope, &r.qualifier, name, is_type_kind);
            let resolution = match ids.as_slice() {
                [] => self.cpp_missing(name),
                _ => self.settle(self.prefer_concrete(ids), how),
            };
            out.push(make(Relation::TypeUses, resolution));
            return;
        }
        let member = matches!(
            r.receiver,
            Receiver::SelfType | Receiver::Typed(_) | Receiver::CallResult(_) | Receiver::New(_)
        ) || (matches!(r.receiver, Receiver::None) && !r.qualifier.is_empty())
            || (matches!(r.receiver, Receiver::Unknown)
                && (!r.qualifier.is_empty() || self.after_member_operator(f, r)));
        if member {
            self.cpp_member(f, &scope, r, out, make);
            return;
        }
        if matches!(r.receiver, Receiver::Unknown) {
            // A call through a local: a pointer or a lambda.
            out.push(make(Relation::Calls, self.indirect(f)));
            return;
        }
        if r.qualifier.is_empty() {
            let macros = self.visible(f, name, |k| k == DefKind::Macro);
            if !macros.is_empty() {
                out.push(make(
                    relation_for(r.use_kind, DefKind::Macro),
                    self.candidates(&macros, CandidateReason::Macro),
                ));
                return;
            }
        }
        let wanted: fn(DefKind) -> bool = match r.use_kind {
            UseKind::Instantiate | UseKind::Decorate => is_type_kind,
            UseKind::Call => is_callable_or_type,
            _ => is_value_or_callable,
        };
        let (ids, how) = self.cpp_find(f, &scope, &r.qualifier, name, wanted);
        if ids.is_empty() {
            let adl: Vec<usize> = if r.use_kind == UseKind::Call && r.qualifier.is_empty() {
                self.ix
                    .by_short
                    .get(name)
                    .into_iter()
                    .flatten()
                    .copied()
                    .filter(|i| {
                        let rec = &self.ix.defs[*i];
                        is_callable(rec.def.kind) && !rec.scope.is_empty() && self.may_reach(f, *i)
                    })
                    .collect()
            } else {
                Vec::new()
            };
            let resolution = if adl.is_empty() {
                self.cpp_missing(name)
            } else {
                self.candidates(&adl, CandidateReason::Adl)
            };
            out.push(make(
                cpp_relation(r.use_kind, DefKind::Function),
                resolution,
            ));
            return;
        }
        let class = self.class_of_scope(&scope);
        self.cpp_result(f, r, self.prefer_concrete(ids), how, class, out, make);
    }

    /// A class or struct beats the alias of the same name.
    fn prefer_concrete(&self, ids: Vec<usize>) -> Vec<usize> {
        let concrete: Vec<usize> = ids
            .iter()
            .copied()
            .filter(|i| self.ix.defs[*i].def.kind != DefKind::TypeAlias)
            .collect();
        if concrete.is_empty() { ids } else { concrete }
    }

    fn cpp_missing(&self, name: &str) -> Resolution {
        if self.ix.by_short.contains_key(name) {
            unresolved(UnresolvedReason::NoBinding)
        } else {
            unresolved(UnresolvedReason::External)
        }
    }

    /// A call, read or write through `a.m`, `a->m`, `this->m` or an implicit
    /// `this`.
    fn cpp_member(
        &self,
        f: usize,
        scope: &[String],
        r: &Ref,
        out: &mut Vec<Binding>,
        make: &dyn Fn(Relation, Resolution) -> Binding,
    ) {
        let name = r.name.as_str();
        let call = r.use_kind == UseKind::Call;
        let plain = cpp_relation(r.use_kind, DefKind::Field);
        let mut chain: Vec<&Name> = r.qualifier.iter().collect();
        let (mut class, mut foreign) = (None, false);
        match &r.receiver {
            Receiver::SelfType => class = self.class_of_scope(scope),
            Receiver::Typed(t) | Receiver::New(t) => {
                class = self.type_class(f, scope, t.as_str(), 0);
                let short = t
                    .as_str()
                    .rsplit_once("::")
                    .map_or(t.as_str(), |(_, short)| short);
                foreign = class.is_none() && !self.ix.by_short.contains_key(short);
            }
            Receiver::CallResult(path) => {
                if let Some(function) = path.last() {
                    let (ids, _) = self.cpp_find(f, scope, &[], function.as_str(), is_callable);
                    class = match ids.as_slice() {
                        [one] => self.declared_class(*one),
                        _ => None,
                    };
                }
            }
            Receiver::None => {
                let head = chain.first().map_or("", |n| n.as_str());
                let (ids, _) = self.cpp_find(f, scope, &[], head, is_value_kind);
                if !chain.is_empty() {
                    chain.remove(0);
                }
                class = ids.first().and_then(|i| self.declared_class(*i));
            }
            _ => {}
        }
        for step in chain {
            class = class.and_then(|c| {
                let (ids, _) = self.members_named(c, step.as_str(), is_value_kind, 0)?;
                self.declared_class(*ids.first()?)
            });
        }
        match class {
            Some(class) => {
                let wanted: fn(DefKind) -> bool = if call {
                    is_callable
                } else {
                    is_value_or_callable
                };
                match self.members_named(class, name, wanted, 0) {
                    Some((ids, inherited)) => {
                        let how = if inherited {
                            EvidenceKind::Inherited
                        } else {
                            EvidenceKind::TypedReceiver
                        };
                        self.cpp_result(f, r, ids, how, Some(class), out, make);
                    }
                    None => {
                        let reason = if self.open_above(class, 0) {
                            UnresolvedReason::ExternalBase
                        } else {
                            UnresolvedReason::NoMember
                        };
                        out.push(make(plain, unresolved(reason)));
                    }
                }
            }
            None if foreign => out.push(make(plain, unresolved(UnresolvedReason::External))),
            None if call => out.push(make(Relation::Calls, self.untyped_methods(f, name))),
            None => out.push(make(plain, unresolved(UnresolvedReason::UntypedRead))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_declaration_gives_the_project_type_it_names() {
        assert_eq!(
            parse_sig(DefKind::Field, "struct node *next")
                .declared
                .as_deref(),
            Some("node")
        );
        assert_eq!(parse_sig(DefKind::Field, "int value").declared, None);
        assert_eq!(
            parse_sig(DefKind::Variable, "static Node table[8]")
                .declared
                .as_deref(),
            Some("Node")
        );
        let pointer = parse_sig(DefKind::Field, "void (*cb)(int)");
        assert!(pointer.pointer && pointer.declared.is_none());
        assert_eq!(
            parse_sig(DefKind::Function, "static Node *make(void)")
                .declared
                .as_deref(),
            Some("Node")
        );
    }

    #[test]
    fn a_typedef_stands_for_a_tag_or_another_typedef() {
        assert_eq!(
            parse_sig(DefKind::TypeAlias, "typedef struct node Node")
                .alias_of
                .as_deref(),
            Some("node")
        );
        assert_eq!(
            parse_sig(DefKind::TypeAlias, "typedef Node Item")
                .alias_of
                .as_deref(),
            Some("Node")
        );
        assert_eq!(
            parse_sig(DefKind::TypeAlias, "typedef unsigned long Word").alias_of,
            None
        );
    }

    #[test]
    fn paths_lose_their_dot_segments() {
        assert_eq!(normalise("src/./a/../b.h"), "src/b.h");
    }
}
