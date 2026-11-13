//! Kotlin: every reference and import of the facts layer bound to the
//! definitions it lands on.
//!
//! The facts state names and the chain written before them (see
//! `facts::lang::kotlin`); this layer walks the chain through what Kotlin itself
//! resolves a name by: the file's own declarations, an import and its alias, the
//! declared package, a wildcard import, the enclosing classes and the instance
//! they hold (an `inner` class sees the one around it), an object or companion
//! used as a qualifier, a receiver's declared type (a local, a parameter, a
//! property of the class or of a source-visible base class, what an initializer
//! or an expression body calls), and the extension functions and properties
//! that add members from outside. What the syntax cannot settle stays a
//! candidate with its reason: a call through a supertype that something
//! overrides, an overloaded name, a wildcard import that is one of several, a
//! receiver nothing types. What it settles against a declaration is rejected.
//! Compiler-grade typing is out of reach: a receiver typed by inference is
//! unresolved, never guessed.
//!
//! Swift reads through the same engine (`resolve::swift`, [`solve_swift`]): its facts
//! take Kotlin's shape, a module is the SwiftPM target a file sits in (the
//! "package": every file of a target sees the others' declarations, and an
//! `import` of a target brings its names), an `extension` adds members to the
//! type of its name from any file of the module, a `static` member is reached
//! through its type, and a member of a conforming type may run behind a call
//! through the protocol without stating `override`.
//!
//! Two projects are linked only through a declared dependency
//! (`WorkspaceModel::may_reference`); `internal` stops at the project, `private`
//! at the file.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::rc::Rc;

use super::index::{Path, scope_path};
use super::jvm::{self, Ent, Found, Head, ImportRec, Jvm, Super, relation_for};
use super::{
    Binding, CandidateReason, Definition, Evidence, EvidenceKind, Relation, Resolution,
    UnresolvedReason, is_callable, plain_relation,
};
use crate::facts::{
    Def, DefKey, DefKind, Disambig, Fact, FactsCache, FileFacts, ImportKind, Language, Name,
    Receiver, Ref, Scope, UseKind, Visibility, expired,
};
use crate::snapshot::workspace::ProjectId;
use crate::snapshot::{AnalysisContext, Loss, RepoPath, Result, SideTag, WorkspaceModel};

/// A call through an untyped receiver is a candidate for at most this many
/// members; past it the list says nothing a reader could use.
const MAX_CANDIDATES: usize = 24;
/// Supertype hops followed to find an inherited member, and links of a chain of
/// values typed by one another.
const MAX_DEPTH: usize = 6;

/// The bindings of one snapshot's Kotlin and Java files, for a Kotlin cell: the
/// Kotlin definitions are its own, the Java files are read through the table of
/// `resolve::jvm`.
pub struct KotlinResolver(Jvm);

impl KotlinResolver {
    /// Resolves every Kotlin and Java file of `side`.
    pub fn open(ctx: &AnalysisContext, side: SideTag, cache: &FactsCache) -> Result<Self> {
        Jvm::open(ctx, side, cache, Language::Kotlin).map(Self)
    }

    pub fn bindings(&self) -> &[Binding] {
        self.0.bindings()
    }

    pub fn definitions(&self) -> &[Definition] {
        self.0.definitions()
    }

    /// The bindings that resolve to `key` or have it among their candidates.
    pub fn references_to(&self, key: &DefKey) -> Vec<&Binding> {
        self.0.references_to(key)
    }

    /// What the answer is partial for: files the extractor could not fully read,
    /// reads that failed, a run cut short.
    pub fn losses(&self) -> &[Loss] {
        self.0.losses()
    }

    /// The files the resolver read: what an absence may rest on.
    pub fn read_paths(&self) -> &[RepoPath] {
        self.0.read_paths()
    }
}

/// The definitions of `files` and what their references land on, the Java files
/// read as the Kotlin engine reads its own.
pub(super) fn solve(
    ctx: &AnalysisContext,
    files: &[FileFacts],
    foreign: &[FileFacts],
) -> (Vec<Definition>, Vec<Binding>, bool) {
    if files.is_empty() {
        return (Vec::new(), Vec::new(), false);
    }
    let index = Index::build(files.to_vec(), foreign.to_vec(), &ctx.workspace, false);
    Engine::new(index, &ctx.workspace).run(ctx)
}

/// The definitions of the Swift `files` and what their references land on.
pub(super) fn solve_swift(
    ctx: &AnalysisContext,
    files: &[FileFacts],
) -> (Vec<Definition>, Vec<Binding>, bool) {
    if files.is_empty() {
        return (Vec::new(), Vec::new(), false);
    }
    let index = Index::build(files.to_vec(), Vec::new(), &ctx.workspace, true);
    Engine::new(index, &ctx.workspace).run(ctx)
}

// ---- the header of a definition

/// What the header text of a definition says (see the facts' conventions).
#[derive(Default, Clone, Debug)]
struct Sig {
    /// The modifiers in front of the keyword: `override`, `abstract`, `inner`...
    words: Vec<String>,
    /// The outer name of the written type of a value, or the return type.
    declared: Option<String>,
    /// `val x = Make()` or `fun f() = repo.make()`: the call that types it.
    init: Option<(Option<String>, String)>,
    /// The outer names of the supertypes a type states.
    supers: Vec<String>,
}

impl Sig {
    fn has(&self, word: &str) -> bool {
        self.words.iter().any(|w| w == word)
    }
}

const KEYWORDS: &[&str] = &[
    "class",
    "interface",
    "object",
    "fun",
    "val",
    "var",
    "typealias",
    "constructor",
    // Swift's.
    "struct",
    "enum",
    "actor",
    "protocol",
    "extension",
    "func",
    "let",
];

/// The header with everything inside brackets blanked, brackets that open kept:
/// what is written at the top of the declaration, in the same positions.
struct Flat {
    chars: Vec<char>,
    depth: Vec<usize>,
    top: Vec<char>,
}

impl Flat {
    fn new(text: &str) -> Self {
        let chars: Vec<char> = text.chars().collect();
        let mut depth = vec![0usize; chars.len() + 1];
        let mut d = 0usize;
        for (i, c) in chars.iter().enumerate() {
            depth[i] = d;
            match c {
                '(' | '[' | '{' | '<' => d += 1,
                ')' | ']' | '}' => d = d.saturating_sub(1),
                '>' if i == 0 || chars[i - 1] != '-' => d = d.saturating_sub(1),
                _ => {}
            }
        }
        let top = chars
            .iter()
            .enumerate()
            .map(|(i, c)| match c {
                '(' | '[' | '{' | '<' if depth[i] == 0 => *c,
                _ if depth[i] == 0 && !matches!(c, ')' | ']' | '}' | '>') => *c,
                _ => ' ',
            })
            .collect();
        Self { chars, depth, top }
    }

    fn text(&self, range: Range<usize>) -> String {
        self.chars[range.start.min(self.chars.len())..range.end.min(self.chars.len())]
            .iter()
            .collect()
    }

    /// The top-level words, with where each starts and ends.
    fn tokens(&self) -> Vec<(usize, usize, String)> {
        let mut out = Vec::new();
        let mut at = 0;
        while at < self.top.len() {
            if self.top[at].is_whitespace() || matches!(self.top[at], '(' | '[' | '{' | '<') {
                at += 1;
                continue;
            }
            let start = at;
            while at < self.top.len()
                && !self.top[at].is_whitespace()
                && !matches!(self.top[at], '(' | '[' | '{' | '<')
            {
                at += 1;
            }
            out.push((start, at, self.top[start..at].iter().collect()));
        }
        out
    }

    /// The first top-level `needle` at or after `from`.
    fn find(&self, from: usize, needle: char) -> Option<usize> {
        (from..self.top.len()).find(|i| self.top[*i] == needle)
    }

    /// The end of the type that starts at `from`: the first top-level ` = `,
    /// ` by `, ` where ` or the end.
    fn type_end(&self, from: usize) -> usize {
        // `next` is the first non-blank at or after `at`, found once for a run
        // of blanks: a header's brackets are blanked, so its runs are long.
        let mut next = from;
        for at in from..self.top.len() {
            if self.top[at] == '=' {
                return at;
            }
            if self.top[at].is_whitespace() {
                next = next.max(at);
                while next < self.top.len() && self.top[next].is_whitespace() {
                    next += 1;
                }
                let rest = &self.top[next..];
                if rest.starts_with(&['b', 'y', ' '])
                    || rest.starts_with(&['w', 'h', 'e', 'r', 'e', ' '])
                    || rest.starts_with(&['=', ' '])
                {
                    return at;
                }
            }
        }
        self.top.len()
    }
}

/// The outer name of a written type: `a.b.List<T>?` is `List`; a function type
/// has none.
fn outer_name(written: &str) -> Option<String> {
    let mut text = written.trim();
    while let Some(rest) = text.strip_prefix("suspend ") {
        text = rest.trim_start();
    }
    if text.starts_with('(') || text.is_empty() {
        return None;
    }
    let cut = text
        .find(['<', '(', '?', ' ', '=', ',', '{'])
        .unwrap_or(text.len());
    let last = text[..cut].rsplit('.').next()?.trim_matches('`');
    (!last.is_empty()
        && last
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '$'))
    .then(|| last.to_string())
}

/// Whether the token is the keyword the declaration is introduced by: Kotlin's
/// `enum class` and Swift's `class func` and `class var` have a modifier that
/// spells one.
fn is_keyword(tokens: &[(usize, usize, String)], at: usize) -> bool {
    let word = tokens[at].2.as_str();
    let next = tokens.get(at + 1).map(|(_, _, t)| t.as_str());
    match word {
        "enum" => next != Some("class"),
        "class" => !matches!(next, Some("func" | "var" | "let")),
        _ => KEYWORDS.contains(&word),
    }
}

fn parse_sig(kind: DefKind, text: &str) -> Sig {
    let flat = Flat::new(text);
    let tokens = flat.tokens();
    let Some(at) = (0..tokens.len()).find(|i| is_keyword(&tokens, *i)) else {
        return Sig::default();
    };
    let mut sig = Sig {
        words: tokens[..at]
            .iter()
            .filter(|(_, _, t)| !t.starts_with('@'))
            .map(|(_, _, t)| t.clone())
            .collect(),
        ..Sig::default()
    };
    let keyword_end = tokens[at].1;
    // The call a trailing ` = f()` states.
    let mut last_equals = None;
    for i in 1..flat.top.len().saturating_sub(1) {
        if flat.top[i] == '='
            && flat.top[i - 1] == ' '
            && flat.top[i + 1] == ' '
            && flat.depth[i] == 0
        {
            last_equals = Some(i);
        }
    }
    let mut body_end = flat.chars.len();
    if let Some(equals) = last_equals {
        let call = flat.text(equals + 2..flat.chars.len());
        if let Some(callee) = call.trim().strip_suffix("()") {
            let ident = |s: &str| {
                !s.is_empty()
                    && s.chars()
                        .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
            };
            let init = match callee.split_once('.') {
                Some((receiver, name)) if ident(receiver) && ident(name) => {
                    Some((Some(receiver.to_string()), name.to_string()))
                }
                None if ident(callee) => Some((None, callee.to_string())),
                _ => None,
            };
            if init.is_some() {
                sig.init = init;
                body_end = equals;
            }
        }
    }
    match kind {
        DefKind::Property | DefKind::Variable | DefKind::Constant | DefKind::Field => {
            if let Some(colon) = flat.find(keyword_end, ':').filter(|c| *c < body_end) {
                let end = flat.type_end(colon + 1).min(body_end);
                sig.declared = outer_name(&flat.text(colon + 1..end));
            }
        }
        DefKind::Function | DefKind::Method => {
            if let Some(open) = flat.find(keyword_end, '(') {
                let close = (open + 1..flat.chars.len())
                    .find(|i| flat.chars[*i] == ')' && flat.depth[*i] == 1);
                if let Some(close) = close {
                    let rest = flat.text(close + 1..body_end);
                    if rest.trim_start().starts_with(':') {
                        let colon = close + 1 + rest.find(':').unwrap_or(0);
                        let end = flat.type_end(colon + 1).min(body_end);
                        sig.declared = outer_name(&flat.text(colon + 1..end));
                    } else if let Some(arrow) = rest.find("->") {
                        // Swift: `func f(…) async throws -> Track`.
                        let from = close + 1 + rest[..arrow].chars().count() + 2;
                        let end = flat.type_end(from).min(body_end);
                        sig.declared = outer_name(&flat.text(from..end));
                    }
                }
            }
        }
        DefKind::Class
        | DefKind::Interface
        | DefKind::Object
        | DefKind::Enum
        | DefKind::Struct
        | DefKind::Trait => {
            if let Some(colon) = flat.find(keyword_end, ':') {
                let end = flat.type_end(colon + 1).min(body_end);
                let mut part_start = colon + 1;
                for i in colon + 1..=end {
                    if i == end || flat.top[i] == ',' {
                        if let Some(name) = outer_name(&flat.text(part_start..i)) {
                            sig.supers.push(name);
                        }
                        part_start = i + 1;
                    }
                }
            }
        }
        _ => {}
    }
    sig
}

// ---- the index

struct DefRec {
    file: usize,
    def: Def,
    path: Path,
    key: DefKey,
    sig: Sig,
}

struct FileRec {
    facts: FileFacts,
    package: String,
    project: Option<ProjectId>,
    imports: Vec<ImportRec>,
    /// Where each local name is visible.
    locals: HashMap<Name, Vec<Range<u32>>>,
    /// Definitions by the path of the scope that holds them.
    children: HashMap<Path, Vec<usize>>,
    by_path: HashMap<Path, Vec<usize>>,
    /// Every name the file refers to.
    names: HashSet<Name>,
    /// A Java file: its declarations are read as Kotlin's, its references are
    /// the Java engine's.
    foreign: bool,
}

#[derive(Default)]
struct Index {
    files: Vec<FileRec>,
    defs: Vec<DefRec>,
    /// Every type, by name.
    types: HashMap<Name, Vec<usize>>,
    /// The declarations of a package, by package and name.
    tops: HashMap<(String, Name), Vec<usize>>,
    /// The packages declared, and every prefix of one.
    packages: HashSet<String>,
    /// Extensions by name.
    extensions: HashMap<Name, Vec<usize>>,
    /// Members of types by name: what an untyped receiver may be.
    members: HashMap<Name, Vec<usize>>,
    /// Members that state `override`, by name. In Swift every member: a type
    /// that conforms to a protocol or inherits states no `override` for what it
    /// implements.
    overriders: HashMap<Name, Vec<usize>>,
    /// The `extension`s of a type, by its name: the definitions that add members
    /// and conformances to a type declared elsewhere in the module.
    ext_types: HashMap<Name, Vec<usize>>,
    /// The files are Swift: a module is a target, an `internal` name is the
    /// module's.
    swift: bool,
}

fn is_type(kind: DefKind) -> bool {
    matches!(
        kind,
        DefKind::Class
            | DefKind::Interface
            | DefKind::Object
            | DefKind::Enum
            | DefKind::TypeAlias
            | DefKind::Struct
            | DefKind::Trait
    )
}

fn is_extension(def: &Def) -> bool {
    matches!(def.local.disambig, Some(Disambig::ImplOf(_)))
}

fn is_value(kind: DefKind) -> bool {
    matches!(
        kind,
        DefKind::Property | DefKind::Variable | DefKind::Constant | DefKind::Field
    )
}

impl Index {
    fn build(
        files: Vec<FileFacts>,
        foreign: Vec<FileFacts>,
        workspace: &WorkspaceModel,
        swift: bool,
    ) -> Self {
        let mut index = Self {
            swift,
            ..Self::default()
        };
        let neighbours = foreign.into_iter().map(|facts| (facts, true));
        for (facts, neighbour) in files
            .into_iter()
            .map(|facts| (facts, false))
            .chain(neighbours)
        {
            let at = index.files.len();
            let project = workspace.project_of(facts.path.as_str());
            // A Swift file declares no package: its module is its target's.
            let mut package = if swift {
                facts.module.as_str().to_string()
            } else {
                String::new()
            };
            let blob = std::sync::Arc::clone(&facts.blob);
            let projected = if neighbour {
                jvm::java_as_kotlin(&facts)
            } else {
                Vec::new()
            };
            let mut file = FileRec {
                facts,
                package: String::new(),
                project,
                imports: Vec::new(),
                locals: HashMap::new(),
                children: HashMap::new(),
                by_path: HashMap::new(),
                names: HashSet::new(),
                foreign: neighbour,
            };
            for fact in &blob.facts {
                match fact {
                    Fact::Import(import) if import.kind == ImportKind::SideEffect => {
                        package = import.module.as_str().to_string();
                    }
                    Fact::Import(import) => {
                        let module = import.module.as_str();
                        let segments = |name: &str| -> Vec<String> {
                            module
                                .split('.')
                                .filter(|s| !s.is_empty())
                                .chain(std::iter::once(name))
                                .map(str::to_string)
                                .collect()
                        };
                        if import.star.is_star() {
                            file.imports.push(ImportRec {
                                local: None,
                                path: module
                                    .split('.')
                                    .filter(|s| !s.is_empty())
                                    .map(str::to_string)
                                    .collect(),
                                wildcard: true,
                                site: import.site.clone(),
                                scope: import.enclosing.clone(),
                            });
                        }
                        for (name, alias) in &import.names {
                            file.imports.push(ImportRec {
                                local: Some(alias.clone().unwrap_or_else(|| name.clone())),
                                path: segments(name.as_str()),
                                wildcard: false,
                                site: import.site.clone(),
                                scope: import.enclosing.clone(),
                            });
                        }
                    }
                    Fact::LocalBinding(local) => {
                        file.locals
                            .entry(local.name.clone())
                            .or_default()
                            .push(local.site.bytes.clone());
                    }
                    Fact::Reference(r) => {
                        file.names.insert(r.name.clone());
                    }
                    _ => {}
                }
            }
            file.package = package.clone();
            index.files.push(file);
            let mut prefix = String::new();
            for segment in package.split('.').filter(|s| !s.is_empty()) {
                if !prefix.is_empty() {
                    prefix.push('.');
                }
                prefix.push_str(segment);
                index.packages.insert(prefix.clone());
            }
            if neighbour {
                for item in projected {
                    index.add_def(at, &item.def, item.key, &package);
                }
                continue;
            }
            for fact in &blob.facts {
                if let Fact::Definition(def) = fact {
                    let key = index.files[at].facts.key(&def.local);
                    index.add_def(at, def, key, &package);
                }
            }
        }
        index
    }

    fn add_def(&mut self, at: usize, def: &Def, key: DefKey, package: &str) {
        let mut path = def.local.container.clone();
        path.push(def.local.name.clone());
        let sig = parse_sig(def.kind, def.signature.as_deref().unwrap_or(""));
        let id = self.defs.len();
        let name = def.local.name.clone();
        let container_empty = def.local.container.is_empty();
        let file = &mut self.files[at];
        file.children
            .entry(def.local.container.clone())
            .or_default()
            .push(id);
        file.by_path.entry(path.clone()).or_default().push(id);
        let extension = is_extension(def);
        // A Swift `extension T` is no type of its own.
        let extends_type = extension && is_type(def.kind);
        if is_type(def.kind) && !extends_type {
            self.types.entry(name.clone()).or_default().push(id);
        }
        if container_empty && !extends_type {
            self.tops
                .entry((package.to_string(), name.clone()))
                .or_default()
                .push(id);
        }
        if extends_type {
            self.ext_types.entry(name.clone()).or_default().push(id);
        } else if extension {
            self.extensions.entry(name.clone()).or_default().push(id);
        } else if !container_empty && !is_type(def.kind) {
            self.members.entry(name.clone()).or_default().push(id);
        }
        if sig.has("override")
            || (self.swift && !container_empty && !is_type(def.kind) && !extension)
        {
            self.overriders.entry(name).or_default().push(id);
        }
        self.defs.push(DefRec {
            file: at,
            def: def.clone(),
            path,
            key,
            sig,
        });
    }

    /// The definition a path names: the primary of its twins.
    fn owner(&self, file: usize, path: &[Name]) -> Option<usize> {
        let ids = self.files[file].by_path.get(path)?;
        ids.iter()
            .copied()
            .find(|id| self.defs[*id].def.local.disambig.is_none())
            .or_else(|| ids.first().copied())
    }

    fn file_path(&self, file: usize) -> &str {
        self.files[file].facts.path.as_str()
    }
}

// ---- the engine

/// The instances a bare name may be a member of.
enum Implicit {
    Class(usize, bool),
    Foreign(Name),
}

struct Engine<'w> {
    ix: Index,
    workspace: &'w WorkspaceModel,
    supers: RefCell<HashMap<usize, Rc<Vec<Super>>>>,
}

impl<'w> Engine<'w> {
    fn new(ix: Index, workspace: &'w WorkspaceModel) -> Self {
        Self {
            ix,
            workspace,
            supers: RefCell::default(),
        }
    }

    fn run(&self, ctx: &AnalysisContext) -> (Vec<Definition>, Vec<Binding>, bool) {
        let mut bindings = Vec::new();
        let mut stopped = false;
        'files: for (f, file) in self.ix.files.iter().enumerate() {
            if file.foreign {
                continue;
            }
            for fact in &file.facts.blob.facts {
                if expired(&ctx.run) {
                    stopped = true;
                    break 'files;
                }
                if let Fact::Reference(r) = fact {
                    self.reference(f, r, &mut bindings);
                }
            }
            for import in &file.imports {
                self.import(f, import, &mut bindings);
            }
        }
        let definitions = self
            .ix
            .defs
            .iter()
            .filter(|rec| !self.ix.files[rec.file].foreign)
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

    // ---- reach

    /// Whether a name used in file `f` may be the definition: the projects allow
    /// it, `private` stays in its file, `internal` in its project.
    fn reach(&self, f: usize, id: usize) -> std::result::Result<(), UnresolvedReason> {
        let rec = &self.ix.defs[id];
        if rec.file == f {
            return Ok(());
        }
        if !self
            .workspace
            .may_reference(self.ix.file_path(f), self.ix.file_path(rec.file))
        {
            return Err(UnresolvedReason::OutsideProject);
        }
        match rec.def.visibility {
            Visibility::Private => Err(UnresolvedReason::NoMember),
            Visibility::Crate
                if !self.ix.swift
                    && self.ix.files[f].project != self.ix.files[rec.file].project =>
            {
                Err(UnresolvedReason::OutsideProject)
            }
            _ => Ok(()),
        }
    }

    /// Of the definitions, those the file may use; the reason when none is.
    fn reachable(
        &self,
        f: usize,
        ids: Vec<usize>,
    ) -> std::result::Result<Vec<usize>, UnresolvedReason> {
        let mut blocked = None;
        let kept: Vec<usize> = ids
            .into_iter()
            .filter(|id| match self.reach(f, *id) {
                Ok(()) => true,
                Err(reason) => {
                    blocked = Some(reason);
                    false
                }
            })
            .collect();
        match (kept.is_empty(), blocked) {
            (true, Some(reason)) => Err(reason),
            _ => Ok(kept),
        }
    }

    // ---- types

    /// The named type as the file at `scope` means it.
    fn resolve_type(&self, f: usize, scope: &[Name], name: &Name) -> Found {
        let file = &self.ix.files[f];
        let typed = |ids: &[usize]| -> Vec<usize> {
            ids.iter()
                .copied()
                .filter(|id| {
                    let rec = &self.ix.defs[*id];
                    is_type(rec.def.kind) && !is_extension(&rec.def) && rec.def.local.name == *name
                })
                .collect()
        };
        for depth in (0..=scope.len()).rev() {
            let local = file.children.get(&scope[..depth]).map(|ids| typed(ids));
            if let Some(ids) = local.filter(|ids| !ids.is_empty()) {
                return Found::Defs(ids, EvidenceKind::SameScope);
            }
        }
        if let Some(import) = file.imports.iter().find(|i| i.local.as_ref() == Some(name)) {
            let ids = typed(&self.import_targets(import));
            return if ids.is_empty() {
                Found::External
            } else {
                self.pick(f, ids, EvidenceKind::ImportedName)
            };
        }
        #[allow(
            clippy::disallowed_methods,
            reason = "a package with no top-level declaration of the name has none in view"
        )]
        let here = self
            .ix
            .tops
            .get(&(file.package.clone(), name.clone()))
            .map(|ids| typed(ids))
            .unwrap_or_default();
        if !here.is_empty() {
            return self.pick(f, here, EvidenceKind::SameScope);
        }
        for import in file.imports.iter().filter(|i| i.wildcard) {
            let ids = typed(&self.wildcard_targets(import, name));
            if !ids.is_empty() {
                return self.pick(f, ids, EvidenceKind::StarImport);
            }
        }
        Found::Nothing
    }

    fn pick(&self, f: usize, ids: Vec<usize>, how: EvidenceKind) -> Found {
        match self.reachable(f, ids) {
            Ok(kept) if !kept.is_empty() => Found::Defs(kept, how),
            Ok(_) => Found::Nothing,
            Err(reason) => Found::Blocked(reason),
        }
    }

    /// The definitions an explicit import names: a declaration of a package, or
    /// a member or nested type of a type.
    fn import_targets(&self, import: &ImportRec) -> Vec<usize> {
        let Some((name, owners)) = import.path.split_last() else {
            return Vec::new();
        };
        self.named_in(owners, &Name::new(name.as_str()))
    }

    /// The declarations named `name` in a package or type, `owners` being the
    /// dotted path: the segments up to the first capitalised one are the package.
    fn named_in(&self, owners: &[String], name: &Name) -> Vec<usize> {
        // A Swift module is named like a type (`Shop`): what is a declared module
        // is no type.
        let split = if self.ix.swift && self.ix.packages.contains(&owners.join(".")) {
            owners.len()
        } else {
            owners
                .iter()
                .position(|s| s.chars().next().is_some_and(char::is_uppercase))
                .unwrap_or(owners.len())
        };
        let package = owners[..split].join(".");
        let types: Vec<Name> = owners[split..]
            .iter()
            .map(|s| Name::new(s.as_str()))
            .collect();
        if types.is_empty() {
            #[allow(
                clippy::disallowed_methods,
                reason = "a package with no top-level declaration of the name has none"
            )]
            let tops = self
                .ix
                .tops
                .get(&(package, name.clone()))
                .cloned()
                .unwrap_or_default();
            return tops;
        }
        let Some(first) = self.ix.types.get(&types[0]) else {
            return Vec::new();
        };
        let mut holders: Vec<usize> = first
            .iter()
            .copied()
            .filter(|id| {
                let rec = &self.ix.defs[*id];
                rec.def.local.container.is_empty() && self.ix.files[rec.file].package == package
            })
            .collect();
        for segment in &types[1..] {
            holders = holders
                .into_iter()
                .flat_map(|h| self.children_of(h))
                .filter(|c| {
                    let rec = &self.ix.defs[*c];
                    is_type(rec.def.kind) && rec.def.local.name == *segment
                })
                .collect();
        }
        holders
            .into_iter()
            .flat_map(|h| self.importable_children(h))
            .filter(|c| self.ix.defs[*c].def.local.name == *name)
            .collect()
    }

    /// What an import names under a type: its children, and for a Java type its
    /// statics, which this reading holds in a `Companion` that Java does not
    /// spell: `import shop.Pricing.total` is the static `total` of `Pricing`.
    fn importable_children(&self, holder: usize) -> Vec<usize> {
        let mut kids = self.children_of(holder);
        if !self.ix.files[self.ix.defs[holder].file].foreign {
            return kids;
        }
        let companions: Vec<usize> = kids
            .iter()
            .copied()
            .filter(|c| {
                let rec = &self.ix.defs[*c];
                rec.def.kind == DefKind::Object && rec.def.local.name.as_str() == "Companion"
            })
            .collect();
        for companion in companions {
            kids.extend(self.children_of(companion));
        }
        kids
    }

    #[allow(
        clippy::disallowed_methods,
        reason = "a declaration with no row of members has no members"
    )]
    fn children_of(&self, holder: usize) -> Vec<usize> {
        let rec = &self.ix.defs[holder];
        let mut kids = self.ix.files[rec.file]
            .children
            .get(&rec.path)
            .cloned()
            .unwrap_or_default();
        // What the extensions of the type, in the module's other files, add.
        if self.ix.swift {
            for ext in self
                .ix
                .ext_types
                .get(&rec.def.local.name)
                .into_iter()
                .flatten()
            {
                let ext = &self.ix.defs[*ext];
                if ext.file != rec.file
                    && ext.def.local.container == rec.def.local.container
                    && self.ix.files[ext.file].package == self.ix.files[rec.file].package
                {
                    kids.extend(
                        self.ix.files[ext.file]
                            .children
                            .get(&ext.path)
                            .into_iter()
                            .flatten()
                            .copied(),
                    );
                }
            }
        }
        kids
    }

    /// What a wildcard import brings under `name`: the declarations of a package,
    /// or the members of a type.
    fn wildcard_targets(&self, import: &ImportRec, name: &Name) -> Vec<usize> {
        self.named_in(&import.path, name)
    }

    // ---- the supertypes

    fn supers_of(&self, class: usize) -> Rc<Vec<Super>> {
        if let Some(known) = self.supers.borrow().get(&class) {
            return Rc::clone(known);
        }
        // A cycle is cut where it closes.
        self.supers.borrow_mut().insert(class, Rc::default());
        let rec = &self.ix.defs[class];
        let scope: Path = rec.def.local.container.clone();
        let mut out = Vec::new();
        let mut inside = scope.clone();
        inside.push(rec.def.local.name.clone());
        let mut stated: Vec<(usize, &[String])> = vec![(rec.file, rec.sig.supers.as_slice())];
        // The conformances a Swift extension of the type adds.
        if self.ix.swift {
            for id in self
                .ix
                .ext_types
                .get(&rec.def.local.name)
                .into_iter()
                .flatten()
            {
                let ext = &self.ix.defs[*id];
                if *id != class
                    && ext.def.local.container == rec.def.local.container
                    && self.ix.files[ext.file].package == self.ix.files[rec.file].package
                {
                    stated.push((ext.file, ext.sig.supers.as_slice()));
                }
            }
        }
        for (file, names) in stated {
            for name in names {
                let name = Name::new(name.as_str());
                let found = match self.resolve_type(file, &inside, &name) {
                    Found::Defs(ids, _) => Super::Repo(ids[0]),
                    _ => Super::Foreign(name),
                };
                if !out.contains(&found) {
                    out.push(found);
                }
            }
        }
        let out = Rc::new(out);
        self.supers.borrow_mut().insert(class, Rc::clone(&out));
        out
    }

    /// Whether `sub` is `sup` or inherits from it.
    fn is_subtype(&self, sub: usize, sup: usize, depth: usize) -> bool {
        sub == sup
            || (depth < MAX_DEPTH
                && self.supers_of(sub).iter().any(|s| match s {
                    Super::Repo(id) => self.is_subtype(*id, sup, depth + 1),
                    Super::Foreign(_) => false,
                }))
    }

    /// Whether the type inherits from a type of that name outside the
    /// repository.
    fn has_foreign_super(&self, class: usize, name: &Name, depth: usize) -> bool {
        depth < MAX_DEPTH
            && self.supers_of(class).iter().any(|s| match s {
                Super::Foreign(n) => n == name,
                Super::Repo(id) => self.has_foreign_super(*id, name, depth + 1),
            })
    }

    /// Whether some type the class inherits from lives outside the repository,
    /// where an unfound member most likely is.
    fn open_above(&self, class: usize, depth: usize) -> bool {
        depth < MAX_DEPTH
            && self.supers_of(class).iter().any(|s| match s {
                Super::Foreign(_) => true,
                Super::Repo(id) => self.open_above(*id, depth + 1),
            })
    }

    // ---- members

    /// The members called `name` of the class, of its companion, or of what it
    /// inherits; the first level that has any. `statics` leaves out a class's
    /// own instance members.
    fn members_of(
        &self,
        class: usize,
        name: &Name,
        statics: bool,
        depth: usize,
    ) -> Option<(Vec<usize>, bool)> {
        if depth > MAX_DEPTH {
            return None;
        }
        let rec = &self.ix.defs[class];
        let kids = self.children_of(class);
        let own: Vec<usize> = kids
            .iter()
            .copied()
            .filter(|c| {
                let k = &self.ix.defs[*c];
                k.def.local.name == *name
                    && (!is_type(k.def.kind) || k.def.kind == DefKind::Object)
                    // A Swift type's own members are reached through the type
                    // when they are `static` or `class` ones.
                    && (!(self.ix.swift && statics) || k.sig.has("static") || k.sig.has("class"))
            })
            .collect();
        let singleton = rec.def.kind == DefKind::Object || self.ix.swift;
        if !own.is_empty() && (!statics || singleton) {
            return Some((own, false));
        }
        for c in &kids {
            let k = &self.ix.defs[*c];
            if k.def.kind == DefKind::Object && k.sig.has("companion") {
                if let Some(found) = self.members_of(*c, name, false, depth + 1) {
                    return Some(found);
                }
            }
        }
        if statics && !singleton {
            return None;
        }
        for s in self.supers_of(class).iter() {
            if let Super::Repo(id) = s {
                if let Some((ids, _)) = self.members_of(*id, name, statics, depth + 1) {
                    return Some((ids, true));
                }
            }
        }
        None
    }

    // ---- scope

    fn implicit(&self, f: usize, scope: &[Name]) -> Vec<Implicit> {
        let mut out: Vec<Implicit> = Vec::new();
        let mut instance = true;
        for depth in (1..=scope.len()).rev() {
            let Some(id) = self.ix.owner(f, &scope[..depth]) else {
                continue;
            };
            let rec = &self.ix.defs[id];
            if let Some(Disambig::ImplOf(receiver)) = &rec.def.local.disambig {
                let name = Name::new(receiver.as_str());
                match self.resolve_type(f, &scope[..depth - 1], &name) {
                    Found::Defs(ids, _) => out.push(Implicit::Class(ids[0], true)),
                    _ => out.push(Implicit::Foreign(name)),
                }
                continue;
            }
            if matches!(
                rec.def.kind,
                DefKind::Class
                    | DefKind::Interface
                    | DefKind::Enum
                    | DefKind::Object
                    | DefKind::Struct
            ) {
                let singleton = rec.def.kind == DefKind::Object;
                out.push(Implicit::Class(id, instance || singleton));
                instance = instance && rec.sig.has("inner");
            }
        }
        out
    }

    fn local_visible(&self, f: usize, name: &Name, at: u32) -> bool {
        self.ix.files[f]
            .locals
            .get(name)
            .is_some_and(|ranges| ranges.iter().any(|r| r.contains(&at)))
    }

    // ---- values and their types

    /// The type of a definition that holds a value: the written one, or what its
    /// initializer calls.
    fn value_type(&self, id: usize, depth: usize) -> Ent {
        if depth > MAX_DEPTH {
            return Ent::Unknown;
        }
        let rec = &self.ix.defs[id];
        let mut scope = rec.def.local.container.clone();
        if is_extension(&rec.def) {
            scope.pop();
        }
        if let Some(written) = &rec.sig.declared {
            return self.type_ent(rec.file, &scope, &Name::new(written.as_str()));
        }
        match &rec.sig.init {
            Some((receiver, callee)) => {
                let mut chain: Vec<Name> = receiver.iter().map(|r| Name::new(r.as_str())).collect();
                chain.push(Name::new(callee.as_str()));
                self.call_result(rec.file, &scope, &chain, depth + 1)
            }
            None => Ent::Unknown,
        }
    }

    fn type_ent(&self, f: usize, scope: &[Name], name: &Name) -> Ent {
        match self.resolve_type(f, scope, name) {
            Found::Defs(ids, _) => self.alias_target(ids[0]),
            Found::Blocked(_) => Ent::Outside,
            _ => Ent::Foreign(name.clone()),
        }
    }

    /// An alias stands for what it aliases when that is a type of the repository.
    fn alias_target(&self, id: usize) -> Ent {
        Ent::Instance(id)
    }

    /// The value a call makes: an instance of the class it constructs, or what
    /// the function it calls returns.
    fn call_result(&self, f: usize, scope: &[Name], chain: &[Name], depth: usize) -> Ent {
        if depth > MAX_DEPTH {
            return Ent::Unknown;
        }
        let Some((callee, before)) = chain.split_last() else {
            return Ent::Unknown;
        };
        if before.is_empty() {
            if let Found::Defs(ids, _) = self.resolve_type(f, scope, callee) {
                return Ent::Instance(ids[0]);
            }
            for implicit in self.implicit(f, scope) {
                if let Implicit::Class(class, _) = implicit {
                    if let Some((ids, _)) = self.members_of(class, callee, false, 0) {
                        if let Some(fun) =
                            ids.iter().find(|i| is_callable(self.ix.defs[**i].def.kind))
                        {
                            return self.value_type(*fun, depth);
                        }
                    }
                }
            }
            return match self.top_level(f, callee, is_callable) {
                Found::Defs(ids, _) => self.value_type(ids[0], depth),
                Found::External => Ent::External,
                _ if callee.as_str().starts_with(char::is_uppercase) => {
                    Ent::Foreign(callee.clone())
                }
                _ => Ent::Unknown,
            };
        }
        let mut entity = self.head_entity(f, scope, &before[0], depth + 1);
        for name in &before[1..] {
            entity = self.step(f, scope, &entity, name, depth + 1);
        }
        match &entity {
            Ent::Instance(class) | Ent::Type(class) => {
                match self.members_of(*class, callee, false, 0) {
                    Some((ids, _)) => ids
                        .iter()
                        .find(|i| is_callable(self.ix.defs[**i].def.kind))
                        .map_or(Ent::Unknown, |fun| self.value_type(*fun, depth)),
                    None => Ent::Unknown,
                }
            }
            _ => Ent::Unknown,
        }
    }

    /// What the first name of a chain stands for.
    fn head_entity(&self, f: usize, scope: &[Name], name: &Name, depth: usize) -> Ent {
        for implicit in self.implicit(f, scope) {
            if let Implicit::Class(class, instance) = implicit {
                if let Some((ids, _)) = self.members_of(class, name, !instance, 0) {
                    if let Some(id) = ids.iter().find(|i| {
                        let k = self.ix.defs[**i].def.kind;
                        is_value(k) || k == DefKind::Object
                    }) {
                        return self.entity_of_member(*id, depth);
                    }
                }
            }
        }
        if let Found::Defs(ids, _) = self.top_level(f, name, is_value) {
            return self.value_type(ids[0], depth);
        }
        match self.resolve_type(f, scope, name) {
            Found::Defs(ids, _) => return Ent::Type(ids[0]),
            Found::Blocked(_) => return Ent::Outside,
            _ => {}
        }
        let text = name.as_str();
        if self.ix.packages.contains(text) {
            return Ent::Package(text.to_string());
        }
        if text.starts_with(char::is_uppercase) {
            Ent::External
        } else {
            Ent::Unknown
        }
    }

    fn entity_of_member(&self, id: usize, depth: usize) -> Ent {
        if self.ix.defs[id].def.kind == DefKind::Object {
            Ent::Type(id)
        } else {
            self.value_type(id, depth)
        }
    }

    /// The next step of a chain: a property of an instance, a nested type or a
    /// member of an object, a declaration of a package.
    fn step(&self, f: usize, scope: &[Name], from: &Ent, name: &Name, depth: usize) -> Ent {
        match from {
            Ent::Instance(class) | Ent::Type(class) => {
                let statics = matches!(from, Ent::Type(_));
                if let Some((ids, _)) = self.members_of(*class, name, statics, 0) {
                    if let Some(id) = ids.first() {
                        return self.entity_of_member(*id, depth);
                    }
                }
                if statics {
                    let nested = self.children_of(*class).into_iter().find(|c| {
                        let k = &self.ix.defs[*c];
                        is_type(k.def.kind) && k.def.local.name == *name
                    });
                    if let Some(id) = nested {
                        return Ent::Type(id);
                    }
                }
                match self.extension_for(f, from, name, is_value) {
                    Some(id) => self.value_type(id, depth),
                    None => Ent::Unknown,
                }
            }
            Ent::Foreign(_) => match self.extension_for(f, from, name, is_value) {
                Some(id) => self.value_type(id, depth),
                None => Ent::Unknown,
            },
            Ent::Package(package) => {
                let dotted = format!("{package}.{name}");
                if let Some(ids) = self.ix.tops.get(&(package.clone(), name.clone())) {
                    if let Some(ty) = ids.iter().find(|i| is_type(self.ix.defs[**i].def.kind)) {
                        return Ent::Type(*ty);
                    }
                    if let Some(v) = ids.iter().find(|i| is_value(self.ix.defs[**i].def.kind)) {
                        return self.value_type(*v, depth);
                    }
                }
                if self.ix.packages.contains(&dotted) {
                    Ent::Package(dotted)
                } else {
                    let _ = (f, scope);
                    Ent::Unknown
                }
            }
            Ent::Outside => Ent::Outside,
            Ent::External => Ent::External,
            Ent::Unknown => Ent::Unknown,
        }
    }

    // ---- top-level names

    /// A declaration of the file's package or one it imports, called `name`.
    fn top_level(&self, f: usize, name: &Name, wanted: fn(DefKind) -> bool) -> Found {
        let file = &self.ix.files[f];
        let fits = |ids: &[usize]| -> Vec<usize> {
            ids.iter()
                .copied()
                .filter(|id| {
                    let rec = &self.ix.defs[*id];
                    wanted(rec.def.kind) && !is_extension(&rec.def)
                })
                .collect()
        };
        if let Some(import) = file.imports.iter().find(|i| i.local.as_ref() == Some(name)) {
            let named = self.import_targets(import);
            let ids = fits(&named);
            return if named.is_empty() {
                Found::External
            } else if ids.is_empty() {
                Found::Nothing
            } else {
                self.pick(f, ids, EvidenceKind::ImportedName)
            };
        }
        #[allow(
            clippy::disallowed_methods,
            reason = "a file with no top-level declaration has none in scope"
        )]
        let mine: Vec<usize> = file
            .children
            .get(&Vec::new())
            .map(|ids| fits(ids))
            .unwrap_or_default()
            .into_iter()
            .filter(|id| self.ix.defs[*id].def.local.name == *name)
            .collect();
        if !mine.is_empty() {
            return Found::Defs(mine, EvidenceKind::SameScope);
        }
        #[allow(
            clippy::disallowed_methods,
            reason = "a package with no top-level declaration of the name has none in view"
        )]
        let here = self
            .ix
            .tops
            .get(&(file.package.clone(), name.clone()))
            .map(|ids| fits(ids))
            .unwrap_or_default();
        if !here.is_empty() {
            return self.pick(f, here, EvidenceKind::SameScope);
        }
        let mut wildcards: Vec<usize> = Vec::new();
        for import in file.imports.iter().filter(|i| i.wildcard) {
            wildcards.extend(fits(&self.wildcard_targets(import, name)));
        }
        if !wildcards.is_empty() {
            return self.pick(f, wildcards, EvidenceKind::StarImport);
        }
        Found::Nothing
    }

    // ---- extensions

    /// An extension called `name` that the file sees and whose receiver type
    /// fits the entity.
    fn extension_for(
        &self,
        f: usize,
        from: &Ent,
        name: &Name,
        wanted: fn(DefKind) -> bool,
    ) -> Option<usize> {
        let ids = self.ix.extensions.get(name)?;
        let file = &self.ix.files[f];
        ids.iter().copied().find(|id| {
            let rec = &self.ix.defs[*id];
            if !wanted(rec.def.kind) || !self.extension_visible(f, *id, file) {
                return false;
            }
            let Some(Disambig::ImplOf(receiver)) = &rec.def.local.disambig else {
                return false;
            };
            let receiver = Name::new(receiver.as_str());
            match from {
                Ent::Instance(class) | Ent::Type(class) => {
                    let wanted_type = self.resolve_type(
                        rec.file,
                        &rec.def.local.container[..rec.def.local.container.len().saturating_sub(1)],
                        &receiver,
                    );
                    match wanted_type {
                        Found::Defs(types, _) => {
                            types.iter().any(|t| self.is_subtype(*class, *t, 0))
                        }
                        _ => {
                            self.ix.defs[*class].def.local.name == receiver
                                || self.has_foreign_super(*class, &receiver, 0)
                        }
                    }
                }
                Ent::Foreign(own) => *own == receiver,
                _ => false,
            }
        })
    }

    /// An extension is seen from its own file, its package, an import of it, or
    /// a wildcard over its package.
    fn extension_visible(&self, f: usize, id: usize, file: &FileRec) -> bool {
        let rec = &self.ix.defs[id];
        if rec.file == f || self.reach(f, id).is_err() {
            return rec.file == f;
        }
        let there = &self.ix.files[rec.file];
        if there.package == file.package {
            return true;
        }
        file.imports.iter().any(|i| {
            if i.wildcard {
                i.path.join(".") == there.package
            } else {
                i.local.as_ref() == Some(&rec.def.local.name)
                    && i.path[..i.path.len() - 1].join(".") == there.package
            }
        })
    }

    // ---- references

    fn reference(&self, f: usize, r: &Ref, out: &mut Vec<Binding>) {
        let file = &self.ix.files[f];
        let scope = scope_path(&r.enclosing);
        let (relation, resolution) = match r.use_kind {
            UseKind::Override => self.override_of(f, r),
            UseKind::TypeUse => self.type_use(f, &scope, r),
            _ => self.value_use(f, &scope, r),
        };
        out.push(Binding {
            file: file.facts.path.clone(),
            site: r.site.clone(),
            from: r.enclosing.clone(),
            relation,
            name: r.name.clone(),
            resolution,
            entry_guard: false,
        });
    }

    fn type_use(&self, f: usize, scope: &[Name], r: &Ref) -> (Relation, Resolution) {
        let relation = Relation::TypeUses;
        if r.qualifier.is_empty() {
            return (
                relation,
                match self.resolve_type(f, scope, &r.name) {
                    Found::Defs(ids, how) => self.resolved_or_overloaded(&ids, how, &r.name, false),
                    Found::Blocked(reason) => unresolved(reason),
                    Found::External => unresolved(UnresolvedReason::External),
                    Found::Nothing if r.name.as_str().starts_with(char::is_uppercase) => {
                        unresolved(UnresolvedReason::External)
                    }
                    Found::Nothing => unresolved(UnresolvedReason::NoBinding),
                },
            );
        }
        let mut entity = self.head_entity(f, scope, &r.qualifier[0], 0);
        for name in &r.qualifier[1..] {
            entity = self.step(f, scope, &entity, name, 0);
        }
        let resolution = match entity {
            Ent::Type(class) | Ent::Instance(class) => {
                let nested = self.children_of(class).into_iter().find(|c| {
                    let k = &self.ix.defs[*c];
                    is_type(k.def.kind) && k.def.local.name == r.name
                });
                match nested {
                    Some(id) => self.resolved(id, EvidenceKind::ModuleAttribute),
                    None => unresolved(UnresolvedReason::NoMember),
                }
            }
            Ent::Package(package) => {
                let ids: Vec<usize> = self
                    .ix
                    .tops
                    .get(&(package, r.name.clone()))
                    .into_iter()
                    .flatten()
                    .copied()
                    .filter(|i| is_type(self.ix.defs[*i].def.kind))
                    .collect();
                match self.pick(f, ids, EvidenceKind::ModuleAttribute) {
                    Found::Defs(ids, how) => self.resolved_or_overloaded(&ids, how, &r.name, false),
                    Found::Blocked(reason) => unresolved(reason),
                    _ => unresolved(UnresolvedReason::External),
                }
            }
            Ent::Outside => unresolved(UnresolvedReason::OutsideProject),
            _ => unresolved(UnresolvedReason::External),
        };
        (relation, resolution)
    }

    fn resolved(&self, id: usize, how: EvidenceKind) -> Resolution {
        Resolution::Resolved {
            target: self.ix.defs[id].key.clone(),
            evidence: Evidence::new(how),
        }
    }

    /// One definition resolves; several of one name (overloads, twins in two
    /// files) are a candidate each.
    fn resolved_or_overloaded(
        &self,
        ids: &[usize],
        how: EvidenceKind,
        _name: &Name,
        _call: bool,
    ) -> Resolution {
        match ids {
            [one] => self.resolved(*one, how),
            [] => unresolved(UnresolvedReason::NoBinding),
            many => self.candidates(many, CandidateReason::Overloaded),
        }
    }

    fn candidates(&self, ids: &[usize], reason: CandidateReason) -> Resolution {
        let mut targets: Vec<DefKey> = Vec::new();
        for id in ids {
            let key = self.ix.defs[*id].key.clone();
            if !targets.contains(&key) {
                targets.push(key);
            }
        }
        Resolution::Candidate { targets, reason }
    }

    fn value_use(&self, f: usize, scope: &[Name], r: &Ref) -> (Relation, Resolution) {
        let plain = plain_relation(r.use_kind);
        let call = matches!(r.use_kind, UseKind::Call | UseKind::PassAsValue);
        let (entity, how) = match self.chain_head(f, scope, r) {
            Head::Resolved(entity, how) => (entity, how),
            Head::Bare => return self.bare(f, scope, r, plain),
        };
        let resolution = match &entity {
            Ent::Instance(class) | Ent::Type(class) => {
                let statics = matches!(entity, Ent::Type(_));
                match self.members_of(*class, &r.name, statics, 0) {
                    Some((ids, inherited)) => {
                        let how = if inherited {
                            EvidenceKind::Inherited
                        } else {
                            how
                        };
                        return self.member_result(f, r, *class, &ids, how, statics);
                    }
                    None => {
                        let nested = statics
                            .then(|| {
                                self.children_of(*class).into_iter().find(|c| {
                                    let k = &self.ix.defs[*c];
                                    is_type(k.def.kind) && k.def.local.name == r.name
                                })
                            })
                            .flatten();
                        if let Some(id) = nested {
                            return (
                                relation_for(r.use_kind, self.ix.defs[id].def.kind),
                                self.resolved(id, EvidenceKind::ModuleAttribute),
                            );
                        }
                        match self.extension_for(f, &entity, &r.name, |_| true) {
                            Some(id) => {
                                return (
                                    relation_for(r.use_kind, self.ix.defs[id].def.kind),
                                    self.resolved_checked(f, id, EvidenceKind::TypedReceiver),
                                );
                            }
                            None if self.open_above(*class, 0) => {
                                unresolved(UnresolvedReason::ExternalBase)
                            }
                            None => unresolved(UnresolvedReason::NoMember),
                        }
                    }
                }
            }
            Ent::Foreign(_) => match self.extension_for(f, &entity, &r.name, |_| true) {
                Some(id) => {
                    return (
                        relation_for(r.use_kind, self.ix.defs[id].def.kind),
                        self.resolved_checked(f, id, EvidenceKind::TypedReceiver),
                    );
                }
                None => unresolved(UnresolvedReason::External),
            },
            Ent::Package(package) => {
                let ids: Vec<usize> = self
                    .ix
                    .tops
                    .get(&(package.clone(), r.name.clone()))
                    .into_iter()
                    .flatten()
                    .copied()
                    .filter(|i| !is_extension(&self.ix.defs[*i].def))
                    .collect();
                match self.pick(f, ids, EvidenceKind::ModuleAttribute) {
                    Found::Defs(ids, how) => {
                        let kind = self.ix.defs[ids[0]].def.kind;
                        return (
                            relation_for(r.use_kind, kind),
                            self.callable_resolution(&ids, how),
                        );
                    }
                    Found::Blocked(reason) => unresolved(reason),
                    _ => unresolved(UnresolvedReason::External),
                }
            }
            Ent::Outside => unresolved(UnresolvedReason::OutsideProject),
            Ent::External => unresolved(UnresolvedReason::External),
            Ent::Unknown => return (plain, self.untyped(f, r, call)),
        };
        (plain, resolution)
    }

    fn resolved_checked(&self, f: usize, id: usize, how: EvidenceKind) -> Resolution {
        match self.reach(f, id) {
            Ok(()) => self.resolved(id, how),
            Err(reason) => unresolved(reason),
        }
    }

    /// The resolution of a name that may be several overloads.
    fn callable_resolution(&self, ids: &[usize], how: EvidenceKind) -> Resolution {
        match ids {
            [one] => self.resolved(*one, how),
            many => self.candidates(many, CandidateReason::Overloaded),
        }
    }

    /// A member of a type found for a use: overloads are a candidate each; a
    /// member something overrides, reached through the supertype, may run in the
    /// override.
    fn member_result(
        &self,
        f: usize,
        r: &Ref,
        class: usize,
        ids: &[usize],
        how: EvidenceKind,
        statics: bool,
    ) -> (Relation, Resolution) {
        let kind = self.ix.defs[ids[0]].def.kind;
        let relation = relation_for(r.use_kind, kind);
        let ids = match self.reachable(f, ids.to_vec()) {
            Ok(kept) if !kept.is_empty() => kept,
            Ok(_) => return (relation, unresolved(UnresolvedReason::NoMember)),
            Err(reason) => return (relation, unresolved(reason)),
        };
        if ids.len() > 1 && ids.iter().all(|i| is_callable(self.ix.defs[*i].def.kind)) {
            return (relation, self.candidates(&ids, CandidateReason::Overloaded));
        }
        let member = ids[0];
        if statics {
            return (relation, self.resolved(member, how));
        }
        let implementations = self.implementations(class, member);
        if implementations.is_empty() {
            return (relation, self.resolved(member, how));
        }
        let mut all = vec![member];
        all.extend(implementations);
        (relation, self.candidates(&all, CandidateReason::Dispatch))
    }

    /// The members that state `override` of the member's name in a type below the
    /// one the call is made through.
    fn implementations(&self, class: usize, member: usize) -> Vec<usize> {
        let name = &self.ix.defs[member].def.local.name;
        let holder = self.holder_of(member);
        let Some(overriders) = self.ix.overriders.get(name) else {
            return Vec::new();
        };
        overriders
            .iter()
            .copied()
            .filter(|id| *id != member)
            .filter(|id| {
                let rec = &self.ix.defs[*id];
                let Some(owner) = self.holder_of(*id) else {
                    return false;
                };
                rec.def.kind == self.ix.defs[member].def.kind
                    && owner != class
                    && self.is_subtype(owner, class, 0)
                    && holder.is_none_or(|h| h != owner)
            })
            .collect()
    }

    /// The type a member is declared in.
    fn holder_of(&self, member: usize) -> Option<usize> {
        let rec = &self.ix.defs[member];
        let container = &rec.def.local.container;
        if container.is_empty() {
            return None;
        }
        self.ix
            .owner(rec.file, container)
            .filter(|h| is_type(self.ix.defs[*h].def.kind))
    }

    /// A call or read through a receiver nothing types: the members of that name
    /// of the types the file can hold a value of.
    fn untyped(&self, f: usize, r: &Ref, call: bool) -> Resolution {
        if !call {
            return unresolved(UnresolvedReason::UntypedRead);
        }
        let pool: Vec<usize> = self
            .ix
            .members
            .get(&r.name)
            .into_iter()
            .flatten()
            .copied()
            .filter(|id| is_callable(self.ix.defs[*id].def.kind))
            .filter(|id| self.reach(f, *id).is_ok() && self.may_hold(f, *id))
            .collect();
        match pool.len() {
            0 => unresolved(UnresolvedReason::NoCandidates),
            n if n > MAX_CANDIDATES => unresolved(UnresolvedReason::TooManyCandidates),
            _ => self.candidates(&pool, CandidateReason::UntypedReceiver),
        }
    }

    /// Whether a file can hold a value of the member's type without spelling it:
    /// it is that file, shares the package, or names the type or one it inherits
    /// from.
    fn may_hold(&self, f: usize, member: usize) -> bool {
        let rec = &self.ix.defs[member];
        let file = &self.ix.files[f];
        if rec.file == f || self.ix.files[rec.file].package == file.package {
            return true;
        }
        let Some(owner) = self.holder_of(member) else {
            return true;
        };
        let mut related: Vec<Name> = vec![self.ix.defs[owner].def.local.name.clone()];
        related.extend(self.supers_of(owner).iter().map(|s| match s {
            Super::Repo(id) => self.ix.defs[*id].def.local.name.clone(),
            Super::Foreign(n) => n.clone(),
        }));
        related.iter().any(|n| file.names.contains(n))
            || file.imports.iter().any(|i| {
                i.path
                    .last()
                    .is_some_and(|l| related.iter().any(|n| n.as_str() == l))
            })
    }

    // ---- bare names

    fn bare(&self, f: usize, scope: &[Name], r: &Ref, plain: Relation) -> (Relation, Resolution) {
        // `this.x`.
        if matches!(r.receiver, Receiver::SelfType) {
            for implicit in self.implicit(f, scope) {
                if let Implicit::Class(class, true) = implicit {
                    if let Some((ids, inherited)) = self.members_of(class, &r.name, false, 0) {
                        let how = if inherited {
                            EvidenceKind::Inherited
                        } else {
                            EvidenceKind::SelfReceiver
                        };
                        return self.member_result(f, r, class, &ids, how, false);
                    }
                    break;
                }
            }
            return (plain, unresolved(UnresolvedReason::NoMember));
        }
        if matches!(r.receiver, Receiver::Unknown) {
            return (
                plain,
                self.untyped(
                    f,
                    r,
                    matches!(r.use_kind, UseKind::Call | UseKind::PassAsValue),
                ),
            );
        }
        if let Receiver::CallResult(chain) = &r.receiver {
            let entity = self.call_result(f, scope, chain, 0);
            return self.apply(f, scope, r, entity, EvidenceKind::Constructed);
        }
        if self.local_visible(f, &r.name, r.site.bytes.start) {
            return (plain, unresolved(UnresolvedReason::Local));
        }
        // The enclosing instances: their members (inherited ones too), then the
        // extensions of the receivers a function adds.
        for implicit in self.implicit(f, scope) {
            match implicit {
                Implicit::Class(class, instance) => {
                    if let Some((ids, inherited)) = self.members_of(class, &r.name, !instance, 0) {
                        let how = if inherited {
                            EvidenceKind::Inherited
                        } else {
                            EvidenceKind::SameScope
                        };
                        return self.member_result(f, r, class, &ids, how, false);
                    }
                    if let Some(id) =
                        self.extension_for(f, &Ent::Instance(class), &r.name, |_| true)
                    {
                        return (
                            relation_for(r.use_kind, self.ix.defs[id].def.kind),
                            self.resolved_checked(f, id, EvidenceKind::SelfReceiver),
                        );
                    }
                }
                Implicit::Foreign(name) => {
                    if let Some(id) = self.extension_for(f, &Ent::Foreign(name), &r.name, |_| true)
                    {
                        return (
                            relation_for(r.use_kind, self.ix.defs[id].def.kind),
                            self.resolved_checked(f, id, EvidenceKind::SelfReceiver),
                        );
                    }
                }
            }
        }
        match self.top_level(f, &r.name, |k| !is_type(k)) {
            Found::Defs(ids, how) => {
                let kind = self.ix.defs[ids[0]].def.kind;
                return (
                    relation_for(r.use_kind, kind),
                    self.callable_resolution(&ids, how),
                );
            }
            Found::Blocked(reason) => return (plain, unresolved(reason)),
            Found::External => return (plain, unresolved(UnresolvedReason::External)),
            Found::Nothing => {}
        }
        match self.resolve_type(f, scope, &r.name) {
            Found::Defs(ids, how) => {
                let kind = self.ix.defs[ids[0]].def.kind;
                (
                    relation_for(r.use_kind, kind),
                    self.resolved_or_overloaded(&ids, how, &r.name, true),
                )
            }
            Found::Blocked(reason) => (plain, unresolved(reason)),
            Found::External => (plain, unresolved(UnresolvedReason::External)),
            Found::Nothing if r.name.as_str().starts_with(char::is_uppercase) => {
                (plain, unresolved(UnresolvedReason::External))
            }
            Found::Nothing => (plain, unresolved(UnresolvedReason::NoBinding)),
        }
    }

    fn apply(
        &self,
        f: usize,
        scope: &[Name],
        r: &Ref,
        entity: Ent,
        how: EvidenceKind,
    ) -> (Relation, Resolution) {
        let _ = scope;
        let plain = plain_relation(r.use_kind);
        let call = matches!(r.use_kind, UseKind::Call | UseKind::PassAsValue);
        match &entity {
            Ent::Instance(class) | Ent::Type(class) => {
                let statics = matches!(entity, Ent::Type(_));
                match self.members_of(*class, &r.name, statics, 0) {
                    Some((ids, inherited)) => {
                        let how = if inherited {
                            EvidenceKind::Inherited
                        } else {
                            how
                        };
                        self.member_result(f, r, *class, &ids, how, statics)
                    }
                    None => match self.extension_for(f, &entity, &r.name, |_| true) {
                        Some(id) => (
                            relation_for(r.use_kind, self.ix.defs[id].def.kind),
                            self.resolved_checked(f, id, how),
                        ),
                        None if self.open_above(*class, 0) => {
                            (plain, unresolved(UnresolvedReason::ExternalBase))
                        }
                        None => (plain, unresolved(UnresolvedReason::NoMember)),
                    },
                }
            }
            Ent::Foreign(_) => match self.extension_for(f, &entity, &r.name, |_| true) {
                Some(id) => (
                    relation_for(r.use_kind, self.ix.defs[id].def.kind),
                    self.resolved_checked(f, id, how),
                ),
                None => (plain, unresolved(UnresolvedReason::External)),
            },
            Ent::Outside => (plain, unresolved(UnresolvedReason::OutsideProject)),
            Ent::External | Ent::Package(_) => (plain, unresolved(UnresolvedReason::External)),
            Ent::Unknown => (plain, self.untyped(f, r, call)),
        }
    }

    /// What the first name of a reference's chain stands for.
    fn chain_head(&self, f: usize, scope: &[Name], r: &Ref) -> Head {
        if r.qualifier.is_empty() {
            return Head::Bare;
        }
        let mut entity = match &r.receiver {
            Receiver::Typed(written) => (
                self.type_ent(f, scope, &Name::new(written.as_str())),
                EvidenceKind::TypedReceiver,
            ),
            Receiver::CallResult(chain) => (
                self.call_result(f, scope, chain, 0),
                EvidenceKind::Constructed,
            ),
            Receiver::Unknown => (Ent::Unknown, EvidenceKind::TypedReceiver),
            Receiver::SelfType => {
                let mut found = (Ent::Unknown, EvidenceKind::SelfReceiver);
                for implicit in self.implicit(f, scope) {
                    if let Implicit::Class(class, true) = implicit {
                        found = (Ent::Instance(class), EvidenceKind::SelfReceiver);
                        break;
                    }
                }
                found
            }
            Receiver::None | Receiver::Module(_) | Receiver::New(_) => (
                self.head_entity(f, scope, &r.qualifier[0], 0),
                EvidenceKind::ModuleAttribute,
            ),
        };
        // A typed or constructed head is the first name of the chain itself; the
        // self receiver stands before it.
        let skip = usize::from(!matches!(r.receiver, Receiver::SelfType));
        for name in r.qualifier.iter().skip(skip) {
            entity.0 = self.step(f, scope, &entity.0, name, 0);
        }
        Head::Resolved(entity.0, entity.1)
    }

    // ---- overrides

    fn override_of(&self, f: usize, r: &Ref) -> (Relation, Resolution) {
        let Scope::Def(key) = &r.enclosing else {
            return (Relation::Overrides, unresolved(UnresolvedReason::NoMember));
        };
        let mut path = key.container.clone();
        path.push(key.name.clone());
        let member = self.ix.owner(f, &path);
        let holder = member.and_then(|m| self.holder_of(m));
        let Some((member, holder)) = member.zip(holder) else {
            return (Relation::Overrides, unresolved(UnresolvedReason::External));
        };
        let kind = self.ix.defs[member].def.kind;
        let mut found: Vec<usize> = Vec::new();
        for s in self.supers_of(holder).iter() {
            if let Super::Repo(id) = s {
                if let Some((ids, _)) = self.members_of(*id, &r.name, false, 0) {
                    found.extend(
                        ids.into_iter()
                            .filter(|i| self.ix.defs[*i].def.kind == kind),
                    );
                }
            }
        }
        let resolution = match found.as_slice() {
            [] => unresolved(UnresolvedReason::External),
            [one] => self.resolved(*one, EvidenceKind::Overridden),
            many => self.candidates(many, CandidateReason::Overloaded),
        };
        (Relation::Overrides, resolution)
    }

    // ---- imports

    fn import(&self, f: usize, import: &ImportRec, out: &mut Vec<Binding>) {
        let file = &self.ix.files[f];
        let make = |name: Name, resolution: Resolution| Binding {
            file: file.facts.path.clone(),
            site: import.site.clone(),
            from: import.scope.clone(),
            relation: Relation::Imports,
            name,
            resolution,
            entry_guard: false,
        };
        if import.wildcard {
            let package = import.path.join(".");
            if !self.ix.packages.contains(&package) {
                out.push(make(
                    Name::new(package.as_str()),
                    unresolved(UnresolvedReason::External),
                ));
            }
            return;
        }
        let Some(local) = import.path.last() else {
            return;
        };
        let name = Name::new(local.as_str());
        let ids = self.import_targets(import);
        let resolution = match self.pick(f, ids, EvidenceKind::ImportedName) {
            Found::Defs(ids, how) => self.callable_resolution(&ids, how),
            Found::Blocked(reason) => unresolved(reason),
            _ => {
                let owners = &import.path[..import.path.len() - 1];
                let first = owners
                    .iter()
                    .position(|s| s.chars().next().is_some_and(char::is_uppercase))
                    .unwrap_or(owners.len());
                if self.ix.packages.contains(&owners[..first].join(".")) {
                    unresolved(UnresolvedReason::NoBinding)
                } else {
                    unresolved(UnresolvedReason::External)
                }
            }
        };
        out.push(make(name, resolution));
    }
}

fn unresolved(reason: UnresolvedReason) -> Resolution {
    Resolution::Unresolved { reason }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_class_header_gives_its_modifiers_and_supertypes() {
        let sig = parse_sig(
            DefKind::Class,
            "class Cart(private val repo: Repo, plain: Int) : Base(), Repo",
        );
        assert_eq!(sig.supers, ["Base", "Repo"]);
        assert!(sig.words.is_empty());
        let sig = parse_sig(DefKind::Enum, "enum class Color(val rgb: Int) : Paint");
        assert_eq!(sig.words, ["enum"]);
        assert_eq!(sig.supers, ["Paint"]);
        let sig = parse_sig(DefKind::Class, "class Box<T : Bound>(val v: T)");
        assert!(sig.supers.is_empty(), "a bound is no supertype: {sig:?}");
    }

    #[test]
    fn a_value_gives_its_written_type_or_the_call_that_types_it() {
        let sig = parse_sig(DefKind::Property, "private val repo: Repo");
        assert_eq!(sig.words, ["private"]);
        assert_eq!(sig.declared.as_deref(), Some("Repo"));
        let sig = parse_sig(DefKind::Property, "val default = Cart()");
        assert_eq!(sig.declared, None);
        assert_eq!(sig.init, Some((None, "Cart".to_string())));
        let sig = parse_sig(DefKind::Property, "val client = factory.build()");
        assert_eq!(
            sig.init,
            Some((Some("factory".to_string()), "build".to_string()))
        );
    }

    #[test]
    fn a_function_gives_its_return_type_past_its_parameters() {
        let sig = parse_sig(DefKind::Method, "override fun find(id: Int): String");
        assert_eq!(sig.words, ["override"]);
        assert_eq!(sig.declared.as_deref(), Some("String"));
        let sig = parse_sig(
            DefKind::Function,
            "fun <T> Box<T>.map(f: (T) -> T): List<T>",
        );
        assert_eq!(sig.declared.as_deref(), Some("List"));
        let sig = parse_sig(DefKind::Function, "fun client() = Repo()");
        assert_eq!(sig.declared, None);
        assert_eq!(sig.init, Some((None, "Repo".to_string())));
        let sig = parse_sig(DefKind::Function, "fun f(x: Int = g()) = h()");
        assert_eq!(
            sig.init,
            Some((None, "h".to_string())),
            "a default is no body"
        );
    }

    #[test]
    fn the_outer_name_of_a_written_type() {
        assert_eq!(outer_name("a.b.List<T>?").as_deref(), Some("List"));
        assert_eq!(outer_name(" String? ").as_deref(), Some("String"));
        assert_eq!(outer_name("(A) -> B"), None);
        assert_eq!(outer_name("suspend () -> Unit"), None);
    }
}
