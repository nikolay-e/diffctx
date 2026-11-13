//! Java: every reference and import of the facts layer bound to the definitions
//! it lands on.
//!
//! The facts state names and the chain written before them (see
//! `facts::lang::java`); this layer walks the chain through what Java itself
//! resolves a name by: the file's own types, a single-type import, the declared
//! package, an on-demand import, the enclosing classes and the instance they hold
//! (an inner class sees the one around it, a `static` member only the statics), a
//! type or a static import used as a qualifier, a receiver's declared type (a
//! local, a parameter, a field of the class or of a source-visible base class,
//! what an initializer or a method returns). What the syntax cannot settle stays
//! a candidate with its reason: an overloaded name the argument count does not
//! settle, a call through a supertype that a subtype overrides, a receiver
//! nothing types. What it settles against a declaration is rejected. Compiler-
//! grade typing is out of reach: a receiver typed by inference is unresolved,
//! never guessed.
//!
//! Two projects are linked only through a declared dependency
//! (`WorkspaceModel::may_reference`); a package-private member stops at its
//! package, a `private` one at its file.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::rc::Rc;

use super::index::{Path, scope_path};
use super::jvm::{self, Ent, Found, Head, ImportRec, Super, relation_for};
use super::{
    Binding, CandidateReason, Definition, Evidence, EvidenceKind, Relation, Resolution, Solved,
    UnresolvedReason, is_callable, plain_relation,
};
use crate::facts::{
    Def, DefKey, DefKind, Fact, FactsCache, FileFacts, ImportKind, Language, Name, Receiver, Ref,
    Scope, UseKind, Visibility, expired,
};
use crate::snapshot::workspace::ProjectId;
use crate::snapshot::{
    AnalysisContext, Bytes, Loss, Result, SideTag, SnapshotReader, WorkspaceModel,
};

/// A call through an untyped receiver is a candidate for at most this many
/// members; past it the list says nothing a reader could use.
const MAX_CANDIDATES: usize = 24;
/// Supertype hops followed to find an inherited member, and links of a chain of
/// values typed by one another.
const MAX_DEPTH: usize = 6;

/// The bindings of one snapshot's Java and Kotlin files, for a Java cell: the Java
/// definitions are its own, the Kotlin files are read through the table of
/// `resolve::jvm`.
pub struct JavaResolver {
    solved: Solved,
}

impl JavaResolver {
    /// Resolves every Java and Kotlin file of `side`.
    pub fn open(ctx: &AnalysisContext, side: SideTag, cache: &FactsCache) -> Result<Self> {
        Solved::open_jvm(ctx, side, cache, Language::Java).map(|solved| Self { solved })
    }
}

solved_by!(JavaResolver);

/// The definitions of `files` and what their references land on, the Kotlin files
/// read as the Java engine reads its own; the losses of reading their texts.
pub(super) fn solve(
    ctx: &AnalysisContext,
    side: SideTag,
    reader: &SnapshotReader,
    files: &[FileFacts],
    foreign: &[FileFacts],
) -> (Vec<Definition>, Vec<Binding>, bool, Vec<Loss>) {
    if files.is_empty() {
        return (Vec::new(), Vec::new(), false, Vec::new());
    }
    let index = Index::build(files.to_vec(), foreign.to_vec(), &ctx.workspace);
    let engine = Engine::new(index, &ctx.workspace, reader, side);
    let (definitions, bindings, stopped) = engine.run(ctx);
    (definitions, bindings, stopped, engine.losses.take())
}

// ---- the header of a definition

/// What the header text of a definition says (see the facts' conventions).
#[derive(Default, Clone, Debug)]
struct Sig {
    /// The modifiers in front of the type or keyword: `static`, `final`...
    words: Vec<String>,
    /// `class`, `interface`, `enum` or `record` for a type.
    keyword: Option<String>,
    /// The outer name of the written type of a value, or the return type.
    declared: Option<String>,
    /// The outer names of the supertypes a type states.
    supers: Vec<String>,
    /// The parameters of a method or a constructor: how many, and whether the
    /// last is variadic.
    params: Option<(usize, bool)>,
}

impl Sig {
    fn has(&self, word: &str) -> bool {
        self.words.iter().any(|w| w == word)
    }
}

const MODIFIERS: &[&str] = &[
    "public",
    "protected",
    "private",
    "static",
    "final",
    "abstract",
    "synchronized",
    "native",
    "transient",
    "volatile",
    "strictfp",
    "default",
    "sealed",
    "non-sealed",
];

const TYPE_KEYWORDS: &[&str] = &["class", "interface", "enum", "record"];

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
                ')' | ']' | '}' | '>' => d = d.saturating_sub(1),
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

    /// The top-level words, with where each starts and ends.
    fn tokens(&self) -> Vec<(usize, usize, String)> {
        let mut out = Vec::new();
        let mut at = 0;
        let separator = |c: char| c.is_whitespace() || matches!(c, '(' | '[' | '{' | '<' | ',');
        while at < self.top.len() {
            if separator(self.top[at]) {
                at += 1;
                continue;
            }
            let start = at;
            while at < self.top.len() && !separator(self.top[at]) {
                at += 1;
            }
            out.push((start, at, self.top[start..at].iter().collect()));
        }
        out
    }

    /// The first non-blank top-level character at or after `from`.
    fn next_char(&self, from: usize) -> Option<char> {
        self.top[from.min(self.top.len())..]
            .iter()
            .copied()
            .find(|c| !c.is_whitespace())
    }
}

/// The outer name of a written type: `a.b.List` is `List`; a primitive is its
/// own name, which no type of the repository has.
fn outer_name(written: &str) -> Option<String> {
    let text = written.trim().trim_end_matches("...");
    let last = text.rsplit('.').next()?;
    (!last.is_empty()
        && last
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '$'))
    .then(|| last.to_string())
}

fn parse_sig(kind: DefKind, text: &str) -> Sig {
    let flat = Flat::new(text);
    let tokens: Vec<(usize, usize, String)> = flat
        .tokens()
        .into_iter()
        .filter(|(_, _, t)| !t.starts_with('@'))
        .collect();
    let first = tokens
        .iter()
        .position(|(_, _, t)| !MODIFIERS.contains(&t.as_str()));
    let mut sig = Sig {
        words: tokens[..first.unwrap_or(tokens.len())]
            .iter()
            .map(|(_, _, t)| t.clone())
            .collect(),
        ..Sig::default()
    };
    let Some(first) = first else {
        return sig;
    };
    match kind {
        DefKind::Class | DefKind::Interface | DefKind::Enum | DefKind::Struct => {
            if TYPE_KEYWORDS.contains(&tokens[first].2.as_str()) {
                sig.keyword = Some(tokens[first].2.clone());
            }
            let mut listing = false;
            for (_, _, token) in &tokens[(first + 1).min(tokens.len())..] {
                match token.as_str() {
                    "extends" | "implements" => listing = true,
                    "permits" | "throws" => listing = false,
                    _ if listing => sig.supers.extend(outer_name(token)),
                    _ => {}
                }
            }
        }
        DefKind::Method | DefKind::Constructor | DefKind::Function => {
            let named = tokens
                .iter()
                .position(|(_, end, _)| flat.next_char(*end) == Some('('));
            if let Some(named) = named {
                if kind != DefKind::Constructor && named > 0 {
                    let (_, end, written) = &tokens[named - 1];
                    if !MODIFIERS.contains(&written.as_str()) && flat.next_char(*end) != Some('[') {
                        sig.declared = outer_name(written);
                    }
                }
                let open = (tokens[named].1..flat.chars.len()).find(|i| flat.chars[*i] == '(');
                if let Some(open) = open {
                    let close = (open + 1..flat.chars.len())
                        .find(|i| flat.chars[*i] == ')' && flat.depth[*i] == flat.depth[open] + 1);
                    if let Some(close) = close {
                        let inside: String = flat.chars[open + 1..close].iter().collect();
                        let level = flat.depth[open] + 1;
                        let commas = (open + 1..close)
                            .filter(|i| flat.chars[*i] == ',' && flat.depth[*i] == level)
                            .count();
                        let count = if inside.trim().is_empty() {
                            0
                        } else {
                            commas + 1
                        };
                        sig.params = Some((count, inside.contains("...")));
                    }
                }
            }
        }
        DefKind::Field | DefKind::Constant | DefKind::Property | DefKind::Variable => {
            if flat.next_char(tokens[first].1) != Some('[') {
                sig.declared = outer_name(&tokens[first].2);
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
    /// A Kotlin file: its declarations are read as Java's, its references are
    /// the Kotlin engine's.
    foreign: bool,
}

#[derive(Default)]
struct Index {
    files: Vec<FileRec>,
    defs: Vec<DefRec>,
    /// Every type, by name.
    types: HashMap<Name, Vec<usize>>,
    /// The types of a package, by package and name.
    tops: HashMap<(String, Name), Vec<usize>>,
    /// The packages declared, and every prefix of one.
    packages: HashSet<String>,
    /// Members of types by name: what an untyped receiver may be.
    members: HashMap<Name, Vec<usize>>,
}

fn is_type(kind: DefKind) -> bool {
    matches!(
        kind,
        DefKind::Class | DefKind::Interface | DefKind::Enum | DefKind::Struct
    )
}

fn is_value(kind: DefKind) -> bool {
    matches!(
        kind,
        DefKind::Property
            | DefKind::Variable
            | DefKind::Constant
            | DefKind::Field
            | DefKind::EnumMember
    )
}

impl Index {
    fn build(files: Vec<FileFacts>, foreign: Vec<FileFacts>, workspace: &WorkspaceModel) -> Self {
        let mut index = Self::default();
        let neighbours = foreign.into_iter().map(|facts| (facts, true));
        for (facts, neighbour) in files
            .into_iter()
            .map(|facts| (facts, false))
            .chain(neighbours)
        {
            let at = index.files.len();
            let project = workspace.project_of(facts.path.as_str());
            let mut package = String::new();
            let blob = std::sync::Arc::clone(&facts.blob);
            let projected = if neighbour {
                jvm::kotlin_as_java(&facts)
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
        if is_type(def.kind) {
            self.types.entry(name.clone()).or_default().push(id);
        }
        if container_empty {
            self.tops
                .entry((package.to_string(), name.clone()))
                .or_default()
                .push(id);
        } else if !is_type(def.kind) {
            self.members.entry(name).or_default().push(id);
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

struct Engine<'w> {
    ix: Index,
    workspace: &'w WorkspaceModel,
    reader: &'w SnapshotReader,
    side: SideTag,
    texts: RefCell<HashMap<usize, Option<Bytes>>>,
    supers: RefCell<HashMap<usize, Rc<Vec<Super>>>>,
    losses: RefCell<Vec<Loss>>,
}

impl<'w> Engine<'w> {
    fn new(
        ix: Index,
        workspace: &'w WorkspaceModel,
        reader: &'w SnapshotReader,
        side: SideTag,
    ) -> Self {
        Self {
            ix,
            workspace,
            reader,
            side,
            texts: RefCell::default(),
            supers: RefCell::default(),
            losses: RefCell::default(),
        }
    }

    ground_methods!();
    jvm_engine_methods!();

    fn text(&self, f: usize) -> Option<Bytes> {
        if let Some(known) = self.texts.borrow().get(&f) {
            return known.clone();
        }
        let path = &self.ix.files[f].facts.path;
        let read = match self.reader.read(path) {
            Ok(read) => read,
            Err(_) => {
                self.losses.borrow_mut().push(Loss::ReadFailed {
                    path: path.clone(),
                    side: self.side,
                });
                None
            }
        };
        self.texts.borrow_mut().insert(f, read.clone());
        read
    }

    // ---- reach

    /// Whether a name used in file `f` may be the definition: the projects allow
    /// it, `private` stays in its file, package-private in its package.
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
            Visibility::Package
                if self.ix.files[f].package != self.ix.files[rec.file].package
                    || self.ix.files[f].project != self.ix.files[rec.file].project =>
            {
                Err(UnresolvedReason::NoMember)
            }
            _ => Ok(()),
        }
    }

    // ---- types

    /// The named type as the file at `scope` means it: a type of the scopes
    /// around, a single-type import, the package's, an on-demand import's.
    fn resolve_type(&self, f: usize, scope: &[Name], name: &Name) -> Found {
        let file = &self.ix.files[f];
        let typed = |ids: &[usize]| -> Vec<usize> {
            ids.iter()
                .copied()
                .filter(|id| {
                    let rec = &self.ix.defs[*id];
                    is_type(rec.def.kind) && rec.def.local.name == *name
                })
                .collect()
        };
        for depth in (0..=scope.len()).rev() {
            let local = file.children.get(&scope[..depth]).map(|ids| typed(ids));
            if let Some(ids) = local.filter(|ids| !ids.is_empty()) {
                return Found::Defs(ids, EvidenceKind::SameScope);
            }
        }
        // A member type a base class passes down.
        for depth in (1..=scope.len()).rev() {
            let Some(holder) = self.ix.owner(f, &scope[..depth]) else {
                continue;
            };
            if !is_type(self.ix.defs[holder].def.kind) {
                continue;
            }
            for s in self.supers_of(holder).iter() {
                if let Super::Repo(id) = s {
                    let inherited: Vec<usize> = self
                        .nested_types(*id, name, 0)
                        .into_iter()
                        .filter(|t| self.reach(f, *t).is_ok())
                        .collect();
                    if !inherited.is_empty() {
                        return Found::Defs(inherited, EvidenceKind::Inherited);
                    }
                }
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
            reason = "a package with no type of the name has none in view"
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

    /// The types called `name` nested in `class` or in what it inherits.
    fn nested_types(&self, class: usize, name: &Name, depth: usize) -> Vec<usize> {
        if depth > MAX_DEPTH {
            return Vec::new();
        }
        let own: Vec<usize> = self
            .children_of(class)
            .into_iter()
            .filter(|c| {
                let k = &self.ix.defs[*c];
                is_type(k.def.kind) && k.def.local.name == *name
            })
            .collect();
        if !own.is_empty() {
            return own;
        }
        self.supers_of(class)
            .iter()
            .filter_map(|s| match s {
                Super::Repo(id) => Some(self.nested_types(*id, name, depth + 1)),
                Super::Foreign(_) => None,
            })
            .find(|found| !found.is_empty())
            .unwrap_or_else(Vec::new)
    }

    /// The declarations named `name` in a package or type, `owners` being the
    /// dotted path: the segments up to the first capitalised one are the package.
    fn named_in(&self, owners: &[String], name: &Name) -> Vec<usize> {
        let split = owners
            .iter()
            .position(|s| s.chars().next().is_some_and(char::is_uppercase))
            .unwrap_or(owners.len());
        let package = owners[..split].join(".");
        let types: Vec<Name> = owners[split..]
            .iter()
            .map(|s| Name::new(s.as_str()))
            .collect();
        if types.is_empty() {
            #[allow(
                clippy::disallowed_methods,
                reason = "a package with no type of the name has none"
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
            .flat_map(|h| self.members_named(h, name))
            .collect()
    }

    /// The members and nested types called `name` of a type or of what it
    /// inherits.
    fn members_named(&self, holder: usize, name: &Name) -> Vec<usize> {
        let own: Vec<usize> = self
            .children_of(holder)
            .into_iter()
            .filter(|c| self.ix.defs[*c].def.local.name == *name)
            .collect();
        if !own.is_empty() {
            return own;
        }
        self.members_of(holder, name, true, 0)
            .map_or_else(Vec::new, |(ids, _)| ids)
    }

    #[allow(
        clippy::disallowed_methods,
        reason = "a declaration with no row of members has no members"
    )]
    fn children_of(&self, holder: usize) -> Vec<usize> {
        let rec = &self.ix.defs[holder];
        self.ix.files[rec.file]
            .children
            .get(&rec.path)
            .cloned()
            .unwrap_or_default()
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
        for name in &rec.sig.supers {
            let name = Name::new(name.as_str());
            let mut inside = scope.clone();
            inside.push(rec.def.local.name.clone());
            match self.resolve_type(rec.file, &inside, &name) {
                Found::Defs(ids, _) if ids[0] != class => out.push(Super::Repo(ids[0])),
                Found::Defs(..) => {}
                _ => out.push(Super::Foreign(name)),
            }
        }
        let out = Rc::new(out);
        self.supers.borrow_mut().insert(class, Rc::clone(&out));
        out
    }

    // ---- members

    /// Whether a member is reached through its type's name: declared `static`, an
    /// enum constant, or a field of an interface.
    fn is_static(&self, id: usize) -> bool {
        let rec = &self.ix.defs[id];
        rec.sig.has("static")
            || rec.def.kind == DefKind::EnumMember
            || (is_value(rec.def.kind)
                && self
                    .holder_of(id)
                    .is_some_and(|h| self.ix.defs[h].def.kind == DefKind::Interface))
    }

    /// The members called `name` of the class, or of what it inherits; the
    /// first level that has any. `statics` leaves out the instance members.
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
        let own: Vec<usize> = self
            .children_of(class)
            .into_iter()
            .filter(|c| {
                let k = &self.ix.defs[*c];
                k.def.local.name == *name
                    && !is_type(k.def.kind)
                    && (!statics || self.is_static(*c))
            })
            .collect();
        if !own.is_empty() {
            return Some((own, false));
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

    /// Whether a type sees the instance of the type around it: an anonymous
    /// class and a class nested without `static` do; an enum, a record and an
    /// interface never.
    fn is_inner(&self, id: usize) -> bool {
        let rec = &self.ix.defs[id];
        rec.def.kind == DefKind::Class
            && (rec.def.local.name.as_str().starts_with("new ")
                || (!rec.sig.has("static") && rec.sig.keyword.as_deref() != Some("record")))
    }

    // ---- scope

    /// The types around the scope whose members a bare name may be, innermost
    /// first, each with whether the instance is in reach.
    fn implicit(&self, f: usize, scope: &[Name]) -> Vec<(usize, bool)> {
        let mut out: Vec<(usize, bool)> = Vec::new();
        let mut instance = true;
        for depth in (1..=scope.len()).rev() {
            let Some(id) = self.ix.owner(f, &scope[..depth]) else {
                continue;
            };
            let rec = &self.ix.defs[id];
            if is_type(rec.def.kind) {
                out.push((id, instance));
                instance = instance && self.is_inner(id);
            } else if rec.sig.has("static") {
                instance = false;
            }
        }
        out
    }

    // ---- values and their types

    /// The type of a definition that holds a value: the written one, or the
    /// enum an enum constant is of.
    fn value_type(&self, id: usize) -> Ent {
        let rec = &self.ix.defs[id];
        if rec.def.kind == DefKind::EnumMember {
            return self.holder_of(id).map_or(Ent::Unknown, Ent::Instance);
        }
        match &rec.sig.declared {
            Some(written) => self.type_ent(
                rec.file,
                &rec.def.local.container,
                &Name::new(written.as_str()),
            ),
            None => Ent::Unknown,
        }
    }

    fn type_ent(&self, f: usize, scope: &[Name], name: &Name) -> Ent {
        match self.resolve_type(f, scope, name) {
            Found::Defs(ids, _) => Ent::Instance(ids[0]),
            Found::Blocked(_) => Ent::Outside,
            _ => Ent::Foreign(name.clone()),
        }
    }

    /// The value a call makes: an instance of the class it constructs, or what
    /// the method it calls returns.
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
            for (class, _) in self.implicit(f, scope) {
                if let Some((ids, _)) = self.members_of(class, callee, false, 0) {
                    if let Some(method) =
                        ids.iter().find(|i| is_callable(self.ix.defs[**i].def.kind))
                    {
                        return self.value_type(*method);
                    }
                }
            }
            return Ent::Unknown;
        }
        let mut entity = self.head_entity(f, scope, &before[0]);
        for name in &before[1..] {
            entity = self.step(f, &entity, name);
        }
        match &entity {
            Ent::Instance(class) | Ent::Type(class) => {
                match self.members_of(*class, callee, false, 0) {
                    Some((ids, _)) => ids
                        .iter()
                        .find(|i| is_callable(self.ix.defs[**i].def.kind))
                        .map_or(Ent::Unknown, |method| self.value_type(*method)),
                    None => Ent::Unknown,
                }
            }
            _ => Ent::Unknown,
        }
    }

    /// What the first name of a chain stands for.
    fn head_entity(&self, f: usize, scope: &[Name], name: &Name) -> Ent {
        for (class, instance) in self.implicit(f, scope) {
            if let Some((ids, _)) = self.members_of(class, name, !instance, 0) {
                if let Some(id) = ids.iter().find(|i| is_value(self.ix.defs[**i].def.kind)) {
                    return self.value_type(*id);
                }
            }
        }
        if let Found::Defs(ids, _) = self.top_level(f, name, is_value) {
            return self.value_type(ids[0]);
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

    /// The next step of a chain: a field of an instance, a nested type or a
    /// static member of a type, a type of a package.
    fn step(&self, f: usize, from: &Ent, name: &Name) -> Ent {
        match from {
            Ent::Instance(class) | Ent::Type(class) => {
                let statics = matches!(from, Ent::Type(_));
                if let Some((ids, _)) = self.members_of(*class, name, statics, 0) {
                    if let Some(id) = ids.iter().find(|i| is_value(self.ix.defs[**i].def.kind)) {
                        return self.value_type(*id);
                    }
                }
                if statics {
                    if let Some(id) = self.nested_types(*class, name, 0).first() {
                        return Ent::Type(*id);
                    }
                }
                Ent::Unknown
            }
            Ent::Package(package) => {
                let dotted = format!("{package}.{name}");
                if let Some(ids) = self.ix.tops.get(&(package.clone(), name.clone())) {
                    if let Some(ty) = ids.first() {
                        return Ent::Type(*ty);
                    }
                }
                if self.ix.packages.contains(&dotted) {
                    Ent::Package(dotted)
                } else {
                    let _ = f;
                    Ent::Unknown
                }
            }
            Ent::Foreign(_) => Ent::Unknown,
            Ent::Outside => Ent::Outside,
            Ent::External => Ent::External,
            Ent::Unknown => Ent::Unknown,
        }
    }

    // ---- static imports

    /// A static member the file imports under `name`, by name or by an
    /// on-demand import of its type.
    fn top_level(&self, f: usize, name: &Name, wanted: fn(DefKind) -> bool) -> Found {
        let file = &self.ix.files[f];
        let fits = |ids: &[usize]| -> Vec<usize> {
            ids.iter()
                .copied()
                .filter(|id| wanted(self.ix.defs[*id].def.kind) && self.is_static(*id))
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
        let mut wildcards: Vec<usize> = Vec::new();
        for import in file.imports.iter().filter(|i| i.wildcard) {
            wildcards.extend(fits(&self.wildcard_targets(import, name)));
        }
        if !wildcards.is_empty() {
            return self.pick(f, wildcards, EvidenceKind::StarImport);
        }
        Found::Nothing
    }

    // ---- references

    fn type_use(&self, f: usize, scope: &[Name], r: &Ref) -> (Relation, Resolution) {
        let relation = Relation::TypeUses;
        if r.qualifier.is_empty() {
            return (
                relation,
                match self.resolve_type(f, scope, &r.name) {
                    Found::Defs(ids, how) => self.resolved_or_candidates(&ids, how),
                    Found::Blocked(reason) => unresolved(reason),
                    Found::External => unresolved(UnresolvedReason::External),
                    Found::Nothing if r.name.as_str().starts_with(char::is_uppercase) => {
                        unresolved(UnresolvedReason::External)
                    }
                    Found::Nothing => unresolved(UnresolvedReason::NoBinding),
                },
            );
        }
        let mut entity = self.head_entity(f, scope, &r.qualifier[0]);
        for name in &r.qualifier[1..] {
            entity = self.step(f, &entity, name);
        }
        let resolution = match entity {
            Ent::Type(class) | Ent::Instance(class) => {
                match self.nested_types(class, &r.name, 0).first() {
                    Some(id) => self.resolved(*id, EvidenceKind::ModuleAttribute),
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
                    Found::Defs(ids, how) => self.resolved_or_candidates(&ids, how),
                    Found::Blocked(reason) => unresolved(reason),
                    _ => unresolved(UnresolvedReason::External),
                }
            }
            Ent::Outside => unresolved(UnresolvedReason::OutsideProject),
            _ => unresolved(UnresolvedReason::External),
        };
        (relation, resolution)
    }

    /// One definition resolves; several of one name (twins in two files) are a
    /// candidate each.
    fn resolved_or_candidates(&self, ids: &[usize], how: EvidenceKind) -> Resolution {
        match ids {
            [one] => self.resolved(*one, how),
            [] => unresolved(UnresolvedReason::NoBinding),
            many => self.candidates(many, CandidateReason::Overloaded),
        }
    }

    /// The number of arguments the call whose name is `r` passes, read off the
    /// text; `None` where none is written (a method reference).
    fn arguments(&self, f: usize, r: &Ref) -> Option<usize> {
        let text = self.text(f)?;
        let mut at = r.site.bytes.end as usize;
        while text.get(at).is_some_and(u8::is_ascii_whitespace) {
            at += 1;
        }
        if text.get(at) != Some(&b'(') {
            return None;
        }
        let (mut depth, mut commas, mut empty) = (0usize, 0usize, true);
        let mut quote: Option<u8> = None;
        while let Some(&c) = text.get(at) {
            if let Some(q) = quote {
                if c == b'\\' {
                    at += 1;
                } else if c == q {
                    quote = None;
                }
            } else {
                match c {
                    b'"' | b'\'' => {
                        quote = Some(c);
                        empty = false;
                    }
                    b'(' | b'[' | b'{' => {
                        depth += 1;
                        if depth > 1 {
                            empty = false;
                        }
                    }
                    b')' | b']' | b'}' => {
                        depth = depth.saturating_sub(1);
                        if depth == 0 {
                            return Some(if empty { 0 } else { commas + 1 });
                        }
                    }
                    b',' if depth == 1 => commas += 1,
                    b'<' if depth == 1 && generic_open(&text, at) => {
                        // `new Map<String, Integer>()`: the commas inside are the
                        // type's.
                        let mut nested = 0usize;
                        while let Some(&d) = text.get(at) {
                            match d {
                                b'<' => nested += 1,
                                b'>' => {
                                    nested = nested.saturating_sub(1);
                                    if nested == 0 {
                                        break;
                                    }
                                }
                                b';' | b')' => break,
                                _ => {}
                            }
                            at += 1;
                        }
                        empty = false;
                    }
                    c if depth == 1 && !c.is_ascii_whitespace() => empty = false,
                    _ => {}
                }
            }
            at += 1;
        }
        None
    }

    /// Of the callables, those whose parameter list the call can pass its
    /// arguments to; all of them when the count says nothing.
    fn fitting(&self, f: usize, r: &Ref, ids: Vec<usize>) -> Vec<usize> {
        let Some(count) = self.arguments(f, r) else {
            return ids;
        };
        let fits: Vec<usize> = ids
            .iter()
            .copied()
            .filter(|id| match self.ix.defs[*id].sig.params {
                Some((params, variadic)) => params == count || (variadic && count + 1 >= params),
                None => true,
            })
            .collect();
        if fits.is_empty() { ids } else { fits }
    }

    fn value_use(&self, f: usize, scope: &[Name], r: &Ref) -> (Relation, Resolution) {
        let plain = plain_relation(r.use_kind);
        let call = matches!(r.use_kind, UseKind::Call | UseKind::PassAsValue);
        let (entity, how) = match self.chain_head(f, scope, r) {
            Head::Resolved(entity, how) => (entity, how),
            Head::Bare => return self.bare(f, scope, r, plain),
        };
        self.apply(f, r, entity, how, plain, call)
    }

    /// What a reference lands on given what its chain stands for before the name.
    fn apply(
        &self,
        f: usize,
        r: &Ref,
        entity: Ent,
        how: EvidenceKind,
        plain: Relation,
        call: bool,
    ) -> (Relation, Resolution) {
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
                    None => {
                        let nested = statics
                            .then(|| self.nested_types(*class, &r.name, 0).first().copied())
                            .flatten();
                        if let Some(id) = nested {
                            return (
                                relation_for(r.use_kind, self.ix.defs[id].def.kind),
                                self.resolved(id, EvidenceKind::ModuleAttribute),
                            );
                        }
                        if self.open_above(*class, 0) {
                            (plain, unresolved(UnresolvedReason::ExternalBase))
                        } else {
                            (plain, unresolved(UnresolvedReason::NoMember))
                        }
                    }
                }
            }
            Ent::Foreign(_) | Ent::External => (plain, unresolved(UnresolvedReason::External)),
            Ent::Package(package) => {
                let ids: Vec<usize> = self
                    .ix
                    .tops
                    .get(&(package.clone(), r.name.clone()))
                    .into_iter()
                    .flatten()
                    .copied()
                    .collect();
                match self.pick(f, ids, EvidenceKind::ModuleAttribute) {
                    Found::Defs(ids, how) => {
                        let kind = self.ix.defs[ids[0]].def.kind;
                        (
                            relation_for(r.use_kind, kind),
                            self.resolved_or_candidates(&ids, how),
                        )
                    }
                    Found::Blocked(reason) => (plain, unresolved(reason)),
                    _ => (plain, unresolved(UnresolvedReason::External)),
                }
            }
            Ent::Outside => (plain, unresolved(UnresolvedReason::OutsideProject)),
            Ent::Unknown => (plain, self.untyped(f, r, call)),
        }
    }

    /// A member of a type found for a use: overloads the argument count does not
    /// settle are a candidate each; a member something overrides, reached through
    /// the supertype, may run in the override.
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
        let mut ids = match self.reachable(f, ids.to_vec()) {
            Ok(kept) if !kept.is_empty() => kept,
            Ok(_) => return (relation, unresolved(UnresolvedReason::NoMember)),
            Err(reason) => return (relation, unresolved(reason)),
        };
        if ids.len() > 1 && ids.iter().all(|i| is_callable(self.ix.defs[*i].def.kind)) {
            ids = self.fitting(f, r, ids);
            if ids.len() > 1 {
                return (relation, self.candidates(&ids, CandidateReason::Overloaded));
            }
        }
        let member = ids[0];
        if statics || !is_callable(self.ix.defs[member].def.kind) {
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

    /// The methods of the member's name and arity in a type below the one the
    /// call is made through: each may run in place of it. A `private`, `static`
    /// or `final` member is never replaced.
    fn implementations(&self, class: usize, member: usize) -> Vec<usize> {
        let rec = &self.ix.defs[member];
        if rec.def.kind != DefKind::Method
            || rec.sig.has("private")
            || rec.sig.has("static")
            || rec.sig.has("final")
        {
            return Vec::new();
        }
        let holder = self.holder_of(member);
        let Some(named) = self.ix.members.get(&rec.def.local.name) else {
            return Vec::new();
        };
        named
            .iter()
            .copied()
            .filter(|id| *id != member)
            .filter(|id| {
                let other = &self.ix.defs[*id];
                let Some(owner) = self.holder_of(*id) else {
                    return false;
                };
                other.def.kind == DefKind::Method
                    && !other.sig.has("static")
                    && other.sig.params.map(|p| p.0) == rec.sig.params.map(|p| p.0)
                    && owner != class
                    && self.is_subtype(owner, class, 0)
                    && holder.is_none_or(|h| h != owner)
            })
            .collect()
    }

    /// A call or read through a receiver nothing types: the methods of that name
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
            .filter(|id| self.ix.defs[*id].def.kind == DefKind::Method)
            .filter(|id| self.reach(f, *id).is_ok() && self.may_hold(f, *id))
            .collect();
        match pool.len() {
            0 => unresolved(UnresolvedReason::NoCandidates),
            n if n > MAX_CANDIDATES => unresolved(UnresolvedReason::TooManyCandidates),
            _ => self.candidates(&pool, CandidateReason::UntypedReceiver),
        }
    }

    // ---- bare names

    fn bare(&self, f: usize, scope: &[Name], r: &Ref, plain: Relation) -> (Relation, Resolution) {
        // `this.x`.
        if matches!(r.receiver, Receiver::SelfType) {
            for (class, instance) in self.implicit(f, scope) {
                if instance {
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
            let call = matches!(r.use_kind, UseKind::Call | UseKind::PassAsValue);
            return self.apply(f, r, entity, EvidenceKind::Constructed, plain, call);
        }
        // `new T(..)` names a type, whatever a member of the class around it is
        // called.
        if r.use_kind != UseKind::Instantiate {
            if self.local_visible(f, &r.name, r.site.bytes.start) {
                return (plain, unresolved(UnresolvedReason::Local));
            }
            // The enclosing types' members, inherited ones too, then the static
            // imports.
            for (class, instance) in self.implicit(f, scope) {
                if let Some((ids, inherited)) = self.members_of(class, &r.name, !instance, 0) {
                    let how = if inherited {
                        EvidenceKind::Inherited
                    } else {
                        EvidenceKind::SameScope
                    };
                    return self.member_result(f, r, class, &ids, how, false);
                }
            }
            match self.top_level(f, &r.name, |k| !is_type(k)) {
                Found::Defs(ids, how) => {
                    let kind = self.ix.defs[ids[0]].def.kind;
                    let ids = if is_callable(kind) {
                        self.fitting(f, r, ids)
                    } else {
                        ids
                    };
                    return (
                        relation_for(r.use_kind, kind),
                        self.resolved_or_candidates(&ids, how),
                    );
                }
                Found::Blocked(reason) => return (plain, unresolved(reason)),
                Found::External => return (plain, unresolved(UnresolvedReason::External)),
                Found::Nothing => {}
            }
        }
        match self.resolve_type(f, scope, &r.name) {
            Found::Defs(ids, how) => {
                let kind = self.ix.defs[ids[0]].def.kind;
                (
                    relation_for(r.use_kind, kind),
                    self.resolved_or_candidates(&ids, how),
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
                for (class, instance) in self.implicit(f, scope) {
                    if instance {
                        found = (Ent::Instance(class), EvidenceKind::SelfReceiver);
                        break;
                    }
                }
                found
            }
            Receiver::None | Receiver::Module(_) | Receiver::New(_) => (
                self.head_entity(f, scope, &r.qualifier[0]),
                EvidenceKind::ModuleAttribute,
            ),
        };
        // A typed or constructed head is the first name of the chain itself; the
        // self receiver stands before it.
        let skip = usize::from(!matches!(r.receiver, Receiver::SelfType));
        for name in r.qualifier.iter().skip(skip) {
            entity.0 = self.step(f, &entity.0, name);
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
        let params = self.ix.defs[member].sig.params.map(|p| p.0);
        let mut found: Vec<usize> = Vec::new();
        for s in self.supers_of(holder).iter() {
            if let Super::Repo(id) = s {
                if let Some((ids, _)) = self.members_of(*id, &r.name, false, 0) {
                    found.extend(ids.into_iter().filter(|i| {
                        let other = &self.ix.defs[*i];
                        other.def.kind == DefKind::Method && other.sig.params.map(|p| p.0) == params
                    }));
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
            let named_type = import
                .path
                .last()
                .is_some_and(|s| s.chars().next().is_some_and(char::is_uppercase));
            let known = if named_type {
                let owners = &import.path[..import.path.len() - 1];
                let last = Name::new(import.path[import.path.len() - 1].as_str());
                !self.named_in(owners, &last).is_empty()
            } else {
                self.ix.packages.contains(&package)
            };
            if !known {
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
            Found::Defs(ids, how) => self.resolved_or_candidates(&ids, how),
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

/// Whether the `<` at `at` opens type arguments rather than comparing: it
/// follows a name and stands before another.
fn generic_open(text: &[u8], at: usize) -> bool {
    at > 0
        && (text[at - 1].is_ascii_alphanumeric() || text[at - 1] == b'_')
        && text
            .get(at + 1)
            .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'?')
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
            "@Service public final class Store<T> extends Base<T> implements Repo, a.b.Closeable",
        );
        assert_eq!(sig.words, ["public", "final"]);
        assert_eq!(sig.keyword.as_deref(), Some("class"));
        assert_eq!(sig.supers, ["Base", "Repo", "Closeable"]);
    }

    #[test]
    fn an_interface_extends_several_and_a_record_is_a_keyword() {
        let sig = parse_sig(DefKind::Interface, "public interface Port extends A, B<C>");
        assert_eq!(sig.supers, ["A", "B"]);
        let record = parse_sig(DefKind::Class, "record Pair(int a, int b) implements Cmp");
        assert_eq!(record.keyword.as_deref(), Some("record"));
        assert_eq!(record.supers, ["Cmp"]);
    }

    #[test]
    fn a_method_gives_its_return_type_and_its_parameters() {
        let sig = parse_sig(
            DefKind::Method,
            "@Override public static <T> List<T> find(Map<String, Long> by, int... rest) throws IOException",
        );
        assert_eq!(sig.words, ["public", "static"]);
        assert_eq!(sig.declared.as_deref(), Some("List"));
        assert_eq!(sig.params, Some((2, true)));
        let none = parse_sig(DefKind::Method, "void run()");
        assert_eq!(
            (none.declared.as_deref(), none.params),
            (Some("void"), Some((0, false)))
        );
        let array = parse_sig(DefKind::Method, "String[] names()");
        assert_eq!(array.declared, None);
    }

    #[test]
    fn a_constructor_has_no_return_type_and_a_field_its_written_type() {
        let ctor = parse_sig(DefKind::Constructor, "public Store(Repo repo, int size)");
        assert_eq!((ctor.declared, ctor.params), (None, Some((2, false))));
        let field = parse_sig(
            DefKind::Field,
            "private final Map<String, Repo> repos = new HashMap<>();",
        );
        assert_eq!(field.declared.as_deref(), Some("Map"));
        assert!(field.has("final"));
    }
}
