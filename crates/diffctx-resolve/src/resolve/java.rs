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
use std::rc::Rc;

use super::index::{Path, scope_path};
use super::jvm::{
    self, DefRec, Ent, FileRec, Found, Head, ImportRec, Super, declare_packages, relation_for,
};
use super::{
    Binding, CandidateReason, Definition, Evidence, EvidenceKind, Relation, Resolution, Solved,
    UnresolvedReason, is_callable, is_value, plain_relation, unresolved,
};
use diffctx_facts::facts::{
    Def, DefKey, DefKind, Fact, FactsCache, FileFacts, Language, Name, Receiver, Ref, Scope, Star,
    UseKind, Visibility, expired,
};
use diffctx_snapshot::snapshot::{
    AnalysisContext, Bytes, Loss, Result, SideTag, SnapshotReader, WorkspaceModel,
};

mod header;
use header::parse_sig;

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
) -> Solution {
    if files.is_empty() {
        return Solution::default();
    }
    let index = Index::build(files.to_vec(), foreign.to_vec(), &ctx.workspace);
    let engine = Engine::new(index, &ctx.workspace, reader, side);
    let (definitions, bindings, stopped) = engine.run(ctx);
    Solution {
        definitions,
        bindings,
        stopped,
        losses: engine.losses.take(),
        open_routes: engine.open_routes(),
        serialized: engine.serialized_getters(),
    }
}

/// What the Java engine settled: the bindings and, for each public method a class
/// outside the repository could reach through an interface, that interface.
#[derive(Default)]
pub(super) struct Solution {
    pub definitions: Vec<Definition>,
    pub bindings: Vec<Binding>,
    pub stopped: bool,
    pub losses: Vec<Loss>,
    pub open_routes: HashMap<DefKey, String>,
    /// The getters Jackson calls: members of a class it serializes.
    pub serialized: HashSet<DefKey>,
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
    /// The type parameters the header declares, each with the written types of
    /// its bounds: `<T extends A & B>`.
    type_params: Vec<(String, Vec<String>)>,
    /// The classes `@ExtensionMethod(A.class, b.B.class)` names, as written.
    extensions: Vec<String>,
    /// The outer names of the types a method returns and of every type argument
    /// it writes, at any depth.
    returns: Vec<String>,
}

impl Sig {
    fn has(&self, word: &str) -> bool {
        self.words.iter().any(|w| w == word)
    }
}

const MAPPINGS: [&str; 6] = [
    "GetMapping",
    "PostMapping",
    "PutMapping",
    "PatchMapping",
    "DeleteMapping",
    "RequestMapping",
];

/// A public instance method `getX()` or `isX()` that returns a value.
fn is_getter(rec: &DefRec<Sig>) -> bool {
    let name = rec.def.local.name.as_str();
    let property = name
        .strip_prefix("get")
        .or_else(|| name.strip_prefix("is"))
        .and_then(|rest| rest.chars().next());
    property.is_some_and(char::is_uppercase)
        && rec.def.visibility == Visibility::Public
        && !rec.sig.has("static")
        && rec.sig.params == Some((0, false))
        && rec.sig.declared.as_deref().is_some_and(|t| t != "void")
}

// ---- the index

#[derive(Default)]
struct Index {
    files: Vec<FileRec>,
    defs: Vec<DefRec<Sig>>,
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
            let blob = std::sync::Arc::clone(&facts.blob);
            let projected = if neighbour {
                jvm::kotlin_as_java(&facts)
            } else {
                Vec::new()
            };
            let file = FileRec::read(facts, neighbour, workspace, String::new());
            let package = file.package.clone();
            index.files.push(file);
            declare_packages(&mut index.packages, &package);
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
    direct_subtypes: RefCell<Option<Rc<HashMap<usize, Vec<usize>>>>>,
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
            direct_subtypes: RefCell::default(),
            losses: RefCell::default(),
        }
    }

    ground_methods!();
    jvm_engine_methods!();

    file_text!();

    // ---- serialization

    /// Whether the annotation `name` of file `f` is Jackson's: imported from
    /// `com.fasterxml.jackson.annotation`, by name or on demand.
    fn jackson_annotation(&self, f: usize, name: &Name) -> bool {
        self.ix.files[f].facts.blob.facts.iter().any(|fact| {
            let Fact::Import(import) = fact else {
                return false;
            };
            import.module.as_str() == "com.fasterxml.jackson.annotation"
                && (import.names.iter().any(|(n, _)| n == name)
                    || (import.star == Star::Glob
                        && (name.as_str().starts_with("Json")
                            || name.as_str().starts_with("Jackson"))))
        })
    }

    /// The getters Jackson calls: the public, parameterless, non-void `getX` and
    /// `isX` of a class it serializes. A class is serialized when it or a member
    /// states a Jackson annotation, or a mapped method of a `@RestController` (or
    /// one stating `@ResponseBody`) returns it.
    fn serialized_getters(&self) -> HashSet<DefKey> {
        let defs = &self.ix.defs;
        let states = |rec: &DefRec<Sig>, names: &[&str]| {
            rec.def
                .decorators
                .iter()
                .any(|d| names.contains(&d.as_str()))
        };
        let mut classes: HashSet<usize> = HashSet::new();
        for (id, rec) in defs.iter().enumerate() {
            if is_type(rec.def.kind) {
                let members = self.ix.files[rec.file].children.get(&rec.path);
                let touched = std::iter::once(rec)
                    .chain(members.into_iter().flatten().map(|m| &defs[*m]))
                    .any(|r| {
                        r.def
                            .decorators
                            .iter()
                            .any(|d| self.jackson_annotation(rec.file, d))
                    });
                if touched {
                    classes.insert(id);
                }
            }
            if rec.def.kind != DefKind::Method || !states(rec, &MAPPINGS) {
                continue;
            }
            let owner = self.ix.owner(rec.file, &rec.def.local.container);
            let body = states(rec, &["ResponseBody"])
                || owner.is_some_and(|o| states(&defs[o], &["RestController", "ResponseBody"]));
            if !body {
                continue;
            }
            for returned in &rec.sig.returns {
                if let Found::Defs(ids, _) = self.resolve_type(
                    rec.file,
                    &rec.def.local.container,
                    &Name::new(returned.as_str()),
                ) {
                    classes.extend(ids);
                }
            }
        }
        // A serialized class carries the getters it inherits from a project base.
        let mut pending: Vec<usize> = classes.iter().copied().collect();
        while let Some(class) = pending.pop() {
            for sup in self.supers_of(class).iter() {
                if let Super::Repo(id) = sup {
                    if classes.insert(*id) {
                        pending.push(*id);
                    }
                }
            }
        }
        defs.iter()
            .filter(|rec| rec.def.kind == DefKind::Method && is_getter(rec))
            .filter(|rec| {
                self.ix
                    .owner(rec.file, &rec.def.local.container)
                    .is_some_and(|o| classes.contains(&o))
            })
            .map(|rec| rec.key.clone())
            .collect()
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
            Some(written) => self.bound_ent(
                rec.file,
                &rec.def.local.container,
                Some(id),
                &Name::new(written.as_str()),
            ),
            None => Ent::Unknown,
        }
    }

    fn type_ent(&self, f: usize, scope: &[Name], name: &Name) -> Ent {
        if let Some((head, rest)) = name.as_str().split_once('.') {
            return self.qualified_type_ent(f, scope, head, rest);
        }
        match self.resolve_type(f, scope, name) {
            Found::Defs(ids, _) => Ent::Instance(ids[0]),
            Found::Blocked(_) => Ent::Outside,
            Found::Nothing if self.declared_type(name) => Ent::Unknown,
            _ => Ent::Foreign(name.clone()),
        }
    }

    /// An instance of a type written with its qualifier: `Outer.Nested` through
    /// the outer type, `a.b.Outer` through the package.
    fn qualified_type_ent(&self, f: usize, scope: &[Name], head: &str, rest: &str) -> Ent {
        let last = rest.rsplit('.').next().unwrap_or(rest);
        let mut entity = match self.resolve_type(f, scope, &Name::new(head)) {
            Found::Defs(ids, _) => Ent::Type(ids[0]),
            Found::Blocked(_) => return Ent::Outside,
            _ if self.ix.packages.contains(head) => Ent::Package(head.to_string()),
            Found::Nothing if self.declared_type(&Name::new(head)) => return Ent::Unknown,
            _ => return Ent::Foreign(Name::new(last)),
        };
        for part in rest.split('.') {
            let part = Name::new(part);
            entity = match entity {
                Ent::Type(class) => self
                    .nested_types(class, &part, 0)
                    .first()
                    .map_or(Ent::Unknown, |id| Ent::Type(*id)),
                other => self.step(f, &other, &part),
            };
        }
        match entity {
            Ent::Type(id) => Ent::Instance(id),
            _ => Ent::Foreign(Name::new(last)),
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
        if text.starts_with(char::is_uppercase) && !self.declared_type(name) {
            Ent::External
        } else {
            Ent::Unknown
        }
    }

    /// A name no import and no package in reach binds is a library's only while the
    /// repository declares no type of it: one that does is a file whose package
    /// clause or imports do not reach it, and what it names is unknown.
    fn declared_type(&self, name: &Name) -> bool {
        self.ix.types.contains_key(name)
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
                    Found::Nothing
                        if r.name.as_str().starts_with(char::is_uppercase)
                            && !self.declared_type(&r.name) =>
                    {
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
    /// arguments, and `receivers` more, to; all of them when the count says nothing.
    fn fitting(&self, f: usize, r: &Ref, ids: Vec<usize>, receivers: usize) -> Vec<usize> {
        let Some(count) = self.arguments(f, r).map(|count| count + receivers) else {
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
        let found = self.member_use(f, scope, r);
        self.extension_use(f, scope, r, found)
    }

    /// Lombok reads `x.shout()` inside a class annotated `@ExtensionMethod(Ext.class)`
    /// as `Ext.shout(x)` when the receiver's own type has no method of that name: a
    /// call through a receiver that does not answer it is the extension's, and one
    /// that does stays the receiver's.
    fn extension_use(
        &self,
        f: usize,
        scope: &[Name],
        r: &Ref,
        found: (Relation, Resolution),
    ) -> (Relation, Resolution) {
        let unanswered = matches!(
            found.1,
            Resolution::Unresolved {
                reason: UnresolvedReason::External
                    | UnresolvedReason::NoMember
                    | UnresolvedReason::NoCandidates
            }
        );
        let through_receiver = !matches!(
            r.receiver,
            Receiver::None | Receiver::Module(_) | Receiver::New(_) | Receiver::SelfType
        );
        if r.use_kind != UseKind::Call || !unanswered || !through_receiver {
            return found;
        }
        let mut ids: Vec<usize> = Vec::new();
        let mut unseen = false;
        for (class, _) in self.implicit(f, scope) {
            let rec = &self.ix.defs[class];
            let mut inside = rec.def.local.container.clone();
            inside.push(rec.def.local.name.clone());
            for written in &rec.sig.extensions {
                match self.type_ent(rec.file, &inside, &Name::new(written.as_str())) {
                    Ent::Instance(extension) => {
                        let statics = self.members_of(extension, &r.name, true, 0);
                        ids.extend(statics.into_iter().flat_map(|(ids, _)| ids));
                    }
                    // The repository declares the class, but this file cannot see it.
                    Ent::Unknown => unseen = true,
                    _ => {}
                }
            }
        }
        ids.sort_unstable();
        ids.dedup();
        ids.retain(|id| is_callable(self.ix.defs[*id].def.kind));
        match self.reachable(f, self.fitting(f, r, ids, 1)) {
            Ok(ids) if !ids.is_empty() => (
                relation_for(r.use_kind, self.ix.defs[ids[0]].def.kind),
                self.resolved_or_candidates(&ids, EvidenceKind::ImportedName),
            ),
            _ if unseen => (found.0, unresolved(UnresolvedReason::NoBinding)),
            _ => found,
        }
    }

    fn member_use(&self, f: usize, scope: &[Name], r: &Ref) -> (Relation, Resolution) {
        let plain = plain_relation(r.use_kind);
        let call = matches!(r.use_kind, UseKind::Call | UseKind::PassAsValue);
        let (entity, how) = match self.chain_head(f, scope, r) {
            Head::Resolved(entity, how) => (entity, how),
            Head::Bare => return self.bare(f, scope, r, plain),
        };
        self.apply(f, scope, r, entity, how, plain, call)
    }

    /// What a reference lands on given what its chain stands for before the name.
    fn apply(
        &self,
        f: usize,
        scope: &[Name],
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
            Ent::Foreign(held) => (
                plain,
                self.foreign_holder(f, scope, held, r)
                    .unwrap_or_else(|| unresolved(UnresolvedReason::External)),
            ),
            Ent::External => (plain, unresolved(UnresolvedReason::External)),
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
            ids = self.fitting(f, r, ids, 0);
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

    /// The methods of the member's name and arity that may run in place of it when
    /// the call goes through `class`: those of a type below it, and those a type
    /// below it inherits from a class that is no subtype of it (a class method wins
    /// over an interface default). A `private`, `static` or `final` member is never
    /// replaced.
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
        let arity = rec.sig.params.map(|p| p.0);
        let fits = |id: usize| {
            let other = &self.ix.defs[id];
            other.def.kind == DefKind::Method
                && !other.sig.has("static")
                && other.sig.params.map(|p| p.0) == arity
        };
        let mut found: Vec<usize> = named
            .iter()
            .copied()
            .filter(|id| *id != member)
            .filter(|id| {
                let Some(owner) = self.holder_of(*id) else {
                    return false;
                };
                fits(*id)
                    && owner != class
                    && self.is_subtype(owner, class, 0)
                    && holder.is_none_or(|h| h != owner)
            })
            .collect();
        for sub in self.descendants(class) {
            let Some((inherited, true)) = self.members_of(sub, &rec.def.local.name, false, 0)
            else {
                continue;
            };
            for id in inherited {
                let from_outside = self
                    .holder_of(id)
                    .is_some_and(|owner| !self.is_subtype(owner, class, 0));
                if fits(id) && from_outside && !found.contains(&id) {
                    found.push(id);
                }
            }
        }
        found
    }

    /// The interface of the repository through which a subclass the repository
    /// does not hold could reach each public instance method of a class: it
    /// implements an interface that declares the method's name, in a package that
    /// sees both, and inherits the method.
    fn open_routes(&self) -> HashMap<DefKey, String> {
        let mut routes = HashMap::new();
        for (declared, rec) in self.ix.defs.iter().enumerate() {
            let interface = self
                .holder_of(declared)
                .filter(|h| self.ix.defs[*h].def.kind == DefKind::Interface);
            let Some(interface) = interface.filter(|_| rec.def.kind == DefKind::Method) else {
                continue;
            };
            for id in self
                .ix
                .members
                .get(&rec.def.local.name)
                .into_iter()
                .flatten()
            {
                let method = &self.ix.defs[*id];
                let holder = self.holder_of(*id).filter(|h| {
                    self.ix.defs[*h].def.kind == DefKind::Class
                        && !self.is_subtype(*h, interface, 0)
                });
                let Some(holder) = holder.filter(|_| {
                    method.def.kind == DefKind::Method
                        && method.sig.has("public")
                        && !method.sig.has("static")
                        && !self.ix.files[method.file].foreign
                }) else {
                    continue;
                };
                let open = std::iter::once(holder)
                    .chain(self.descendants(holder))
                    .any(|class| self.extensible(class) && self.usable_together(class, interface));
                if open {
                    let name = self.ix.defs[interface].def.local.name.as_str();
                    routes
                        .entry(method.key.clone())
                        .or_insert_with(|| format!("`{name}`"));
                }
            }
        }
        routes
    }

    /// Whether code outside the repository can extend the type.
    fn extensible(&self, class: usize) -> bool {
        let rec = &self.ix.defs[class];
        rec.def.kind == DefKind::Class
            && !rec.def.local.name.as_str().starts_with("new ")
            && rec.sig.keyword.as_deref() != Some("record")
            && !(rec.sig.has("final") || rec.sig.has("sealed"))
    }

    /// Whether one class can implement the interface and extend the other: neither is
    /// private, and the package-private ones share a package.
    fn usable_together(&self, class: usize, interface: usize) -> bool {
        let held = [&self.ix.defs[class], &self.ix.defs[interface]];
        let mut packages = held
            .iter()
            .filter(|rec| rec.def.visibility == Visibility::Package)
            .map(|rec| &self.ix.files[rec.file].package);
        held.iter()
            .all(|rec| rec.def.visibility != Visibility::Private)
            && packages
                .next()
                .is_none_or(|first| packages.all(|other| other == first))
    }

    /// The types that inherit from `class`, directly or not.
    fn descendants(&self, class: usize) -> Vec<usize> {
        let known = self.direct_subtypes.borrow().clone();
        let direct = known.unwrap_or_else(|| {
            let mut subs: HashMap<usize, Vec<usize>> = HashMap::new();
            for (id, rec) in self.ix.defs.iter().enumerate() {
                if !is_type(rec.def.kind) {
                    continue;
                }
                for sup in self.supers_of(id).iter() {
                    if let Super::Repo(held) = sup {
                        subs.entry(*held).or_default().push(id);
                    }
                }
            }
            let built = Rc::new(subs);
            *self.direct_subtypes.borrow_mut() = Some(Rc::clone(&built));
            built
        });
        let mut found: Vec<usize> = Vec::new();
        let mut pending = vec![class];
        while let Some(next) = pending.pop() {
            for sub in direct.get(&next).into_iter().flatten() {
                if *sub != class && !found.contains(sub) {
                    found.push(*sub);
                    pending.push(*sub);
                }
            }
        }
        found
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
            return self.apply(f, scope, r, entity, EvidenceKind::Constructed, plain, call);
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
                        self.fitting(f, r, ids, 0)
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
                (plain, self.case_label_constants(f, r))
            }
            Found::Nothing => (plain, unresolved(UnresolvedReason::NoBinding)),
        }
    }

    /// A bare upper-case name no scope, import or type binds: Java writes an
    /// enum constant so only in a `case` label, whose selector's type is not
    /// followed here, so every constant of the name the file can reach may be it.
    fn case_label_constants(&self, f: usize, r: &Ref) -> Resolution {
        let pool: Vec<usize> = self
            .ix
            .members
            .get(&r.name)
            .into_iter()
            .flatten()
            .copied()
            .filter(|id| self.ix.defs[*id].def.kind == DefKind::EnumMember)
            .filter(|id| self.reach(f, *id).is_ok())
            .collect();
        match pool.len() {
            0 => unresolved(UnresolvedReason::External),
            n if n > MAX_CANDIDATES => unresolved(UnresolvedReason::TooManyCandidates),
            _ => self.candidates(&pool, CandidateReason::UntypedReceiver),
        }
    }

    /// What the first name of a reference's chain stands for.
    fn chain_head(&self, f: usize, scope: &[Name], r: &Ref) -> Head {
        if r.qualifier.is_empty() {
            return Head::Bare;
        }
        let (heads, how) = match &r.receiver {
            Receiver::Typed(written) => (
                self.type_ents(f, scope, None, &Name::new(written.as_str()), 0),
                EvidenceKind::TypedReceiver,
            ),
            Receiver::CallResult(chain) => (
                vec![self.call_result(f, scope, chain, 0)],
                EvidenceKind::Constructed,
            ),
            Receiver::Unknown => (vec![Ent::Unknown], EvidenceKind::TypedReceiver),
            Receiver::SelfType => {
                let mut found = (Ent::Unknown, EvidenceKind::SelfReceiver);
                for (class, instance) in self.implicit(f, scope) {
                    if instance {
                        found = (Ent::Instance(class), EvidenceKind::SelfReceiver);
                        break;
                    }
                }
                (vec![found.0], found.1)
            }
            Receiver::None | Receiver::Module(_) | Receiver::New(_) => (
                vec![self.head_entity(f, scope, &r.qualifier[0])],
                EvidenceKind::ModuleAttribute,
            ),
        };
        // A typed or constructed head is the first name of the chain itself; the
        // self receiver stands before it.
        let skip = usize::from(!matches!(r.receiver, Receiver::SelfType));
        let walked: Vec<Ent> = heads
            .into_iter()
            .map(|mut entity| {
                for name in r.qualifier.iter().skip(skip) {
                    entity = self.step(f, &entity, name);
                }
                entity
            })
            .collect();
        Head::Resolved(self.settle(walked, &r.name), how)
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
