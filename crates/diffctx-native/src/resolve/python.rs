//! Python: every reference and import of the facts layer bound to the
//! definitions it lands on.
//!
//! The facts state names, never meanings: `a.b.f()` is the name `f` with the
//! chain `[a, b]` written before it, and `a` may be a module, a class, a local
//! or a parameter. The resolver walks that chain through the file's own scopes,
//! its imports (import roots of the project, the directories the file puts on
//! `sys.path`, relative imports, what an `__init__` re-exports), the classes
//! and their bases, and the types the code states: an annotation, an instance
//! it builds (`x = Cls()`), a function annotated `-> Cls`.
//!
//! The facts carry no initializer of an assignment, so the one place that reads
//! the file's text is the check that `name = [await] callee(...)` is the whole
//! statement: a binding adjacent to a call, with nothing but `=` between them.

use std::cell::{OnceCell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;

use super::index::{ImportRec, Index, Path, Spec, join, scope_path};
use super::modules::ModuleRes;
use super::solved::{Gathered, before_deadline, gather, reader_of};
use super::twins;
use super::{
    Binding, CandidateReason, Definition, Evidence, EvidenceKind, Relation, Resolution, Solved,
    UnresolvedReason, plain_relation,
};
use crate::facts::lang::python::is_dotted_name;
use crate::facts::{
    DefKey, DefKind, Fact, FactsCache, Import, ImportKind, Language, Name, Receiver, Ref, Scope,
    UseKind,
};
use crate::snapshot::{
    AnalysisContext, Bytes, Loss, RepoPath, Result, SideTag, SnapshotReader, WorkspaceModel,
};

/// A call through an untyped receiver is a candidate for at most this many
/// methods; past it the list says nothing a reader could use.
const MAX_CANDIDATES: usize = 24;
/// How far a chain of inferred types may feed one another.
const MAX_DEPTH: usize = 6;
/// How many re-exporting modules a name may pass through.
const MAX_HOPS: usize = 8;

/// The bindings of one snapshot's Python files.
pub struct PythonResolver {
    solved: Solved,
}

impl PythonResolver {
    /// Resolves every Python file of `side`, found by extension or shebang.
    pub fn open(ctx: &AnalysisContext, side: SideTag, cache: &FactsCache) -> Result<Self> {
        let reader = reader_of(ctx, side);
        let Gathered {
            mut losses,
            files,
            read,
            ..
        } = gather(
            ctx,
            side,
            cache,
            |entry, _| {
                let language = Language::from_path(&entry.path);
                language == Some(Language::Python)
                    || (language.is_none() && Language::needs_shebang(&entry.path))
            },
            |file| file.language() == Language::Python,
        )?;
        let engine = Engine::new(
            Index::build(files, &ctx.workspace),
            &ctx.workspace,
            reader,
            side,
        );
        let (definitions, bindings) = engine.run(ctx);
        losses.extend(engine.losses.take());
        Ok(Self {
            solved: Solved::new(definitions, bindings, losses, read),
        })
    }

    /// The modules of the snapshot a dotted name, whole or by its tail, names:
    /// `pkg.tools` and `tools` are `pkg/tools.py`, `pkg` is `pkg/__init__.py`.
    pub fn modules_named(&self, dotted: &str) -> Vec<RepoPath> {
        let name: Vec<&str> = dotted.split('.').collect();
        self.read_paths()
            .iter()
            .filter(|path| {
                let (dir, file) = path
                    .as_str()
                    .rsplit_once('/')
                    .unwrap_or(("", path.as_str()));
                let stem = file.rsplit_once('.').map_or(file, |(stem, _)| stem);
                let mut module: Vec<&str> = dir.split('/').filter(|d| !d.is_empty()).collect();
                if stem != "__init__" {
                    module.push(stem);
                }
                module.ends_with(name.as_slice())
            })
            .cloned()
            .collect()
    }
}

solved_by!(PythonResolver);

#[derive(Clone, PartialEq, Eq, Debug)]
enum Entity {
    Module(usize),
    /// A directory of the repository without an `__init__.py`, by dotted name.
    Package(String),
    Class(usize),
    /// A value of the class: `self`, an annotated name, a built object.
    Instance(usize),
    /// `super()` inside the class: its bases, not itself.
    Super(usize),
    /// A function, method, variable or other definition.
    Def(usize),
    /// A module found only under a requirement that names no source.
    DoubtfulModule(usize),
    /// A definition reached through a `DoubtfulModule`: a possibility.
    Maybe(usize),
    /// Annotated `Any` or `object`: any class.
    Dynamic,
    External,
    Builtin,
    Outside,
    Unknown,
}

type Hit = twins::Hit<Entity>;

enum Lookup {
    Found {
        entity: Entity,
        through: Vec<usize>,
        inherited: bool,
    },
    Absent(UnresolvedReason),
    /// The receiver says nothing about what it is.
    Untyped,
    Any,
}

type Outcome = twins::EntityOutcome<Entity>;

type Seen = HashSet<(usize, Name)>;

/// What stands between a statement's keyword and the call it hands over.
#[derive(Clone, Copy)]
enum Lead {
    /// `= ` after a name that was just bound.
    Assign,
    /// After `return`.
    Return,
}

struct Engine<'a> {
    ix: Index,
    workspace: &'a WorkspaceModel,
    reader: &'a SnapshotReader,
    side: SideTag,
    texts: RefCell<HashMap<usize, Option<Bytes>>>,
    modules: RefCell<HashMap<(usize, u32, String), ModuleRes>>,
    instances: RefCell<HashMap<usize, Entity>>,
    returns: RefCell<HashMap<usize, Entity>>,
    /// The type a local is read under while its next assignment is.
    assumed: RefCell<HashMap<(usize, Path, Name), Entity>>,
    bases: RefCell<HashMap<usize, Rc<Vec<Entity>>>>,
    imported: RefCell<HashMap<usize, Rc<HashSet<usize>>>>,
    /// The classes of the repository that have a class among their ancestors,
    /// by that ancestor.
    subclasses: OnceCell<HashMap<usize, Vec<usize>>>,
    losses: RefCell<Vec<Loss>>,
}

impl<'a> Engine<'a> {
    fn new(
        ix: Index,
        workspace: &'a WorkspaceModel,
        reader: &'a SnapshotReader,
        side: SideTag,
    ) -> Self {
        Self {
            ix,
            workspace,
            reader,
            side,
            texts: RefCell::default(),
            modules: RefCell::default(),
            instances: RefCell::default(),
            returns: RefCell::default(),
            assumed: RefCell::default(),
            bases: RefCell::default(),
            imported: RefCell::default(),
            subclasses: OnceCell::new(),
            losses: RefCell::default(),
        }
    }

    fn run(&self, ctx: &AnalysisContext) -> (Vec<Definition>, Vec<Binding>) {
        let mut bindings = Vec::new();
        for (f, file) in self.ix.files.iter().enumerate() {
            for fact in before_deadline(ctx, &self.losses, &file.facts.blob.facts) {
                match fact {
                    Fact::Reference(r) => self.reference(f, r, &mut bindings),
                    Fact::Import(import) => self.import(f, import, &mut bindings),
                    _ => {}
                }
            }
        }
        self.overrides(&mut bindings);
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
        (definitions, bindings)
    }

    fn path_of(&self, f: usize) -> &RepoPath {
        &self.ix.files[f].facts.path
    }

    fn paths(&self, files: &[usize]) -> Vec<RepoPath> {
        files.iter().map(|f| self.path_of(*f).clone()).collect()
    }

    reachable_by_project!();

    fn module(&self, f: usize, relative: u32, dotted: &str) -> ModuleRes {
        let key = (f, relative, dotted.to_string());
        if let Some(known) = self.modules.borrow().get(&key) {
            return known.clone();
        }
        let found = self.ix.resolve_module(self.workspace, f, relative, dotted);
        self.modules.borrow_mut().insert(key, found.clone());
        found
    }

    file_text!();

    // ---- references

    fn reference(&self, f: usize, r: &Ref, out: &mut Vec<Binding>) {
        let file = &self.ix.files[f];
        let outcome = self.outcome(f, r, 0);
        let (relation, resolution) = self.finish(f, r.use_kind, outcome);
        let guarded = file
            .guards
            .iter()
            .any(|g| g.start <= r.site.bytes.start && r.site.bytes.end <= g.end);
        let binding = |name: &Name, relation: Relation, resolution: Resolution| Binding {
            file: file.facts.path.clone(),
            site: r.site.clone(),
            from: r.enclosing.clone(),
            relation,
            name: name.clone(),
            resolution,
            entry_guard: guarded,
        };
        out.push(binding(&r.name, relation, resolution));
        // A read or a call through `self` inside a class body is found on the
        // class or its bases, and may as well run in any subclass that replaces
        // the member: the subclass definition has a possible reader.
        let overriding = self.overriding(f, r);
        if !overriding.is_empty() {
            out.push(binding(
                &r.name,
                relation,
                Resolution::Candidate {
                    targets: overriding
                        .iter()
                        .map(|id| self.ix.defs[*id].key.clone())
                        .collect(),
                    reason: CandidateReason::Dispatch,
                },
            ));
        }
        // The facts keep the head of a chain out of the references: `limits.max`
        // is the name `max` with `limits` written before it, and reading `limits`
        // is part of it. The head's own binding is made here, at the chain's line.
        // Each is read where it is written, not where the chain ends.
        let sites = twins::qualifier_sites(self.text(f).as_deref(), r);
        let at = |index: usize, binding: Binding| Binding {
            site: sites[index].clone(),
            ..binding
        };
        if let Some((head, resolution)) = self.head_read(f, r) {
            out.push(at(0, binding(&head, Relation::Reads, resolution)));
        }
        // The same for the members in the middle of the chain: `self.decoder`
        // in `self.decoder.decode()` is read.
        for (index, member, resolution) in self.chain_reads(f, r) {
            out.push(at(index, binding(&member, Relation::Reads, resolution)));
        }
    }

    /// The members of subclasses that replace the member a method of a class
    /// reads or calls through its own instance (`self.x`, `cls.x`): one per
    /// subclass of the class, in any depth, that defines `x` itself. A write
    /// makes an attribute of the instance and reaches none.
    fn overriding(&self, f: usize, r: &Ref) -> Vec<usize> {
        if r.use_kind == UseKind::Write || r.qualifier.len() != 1 {
            return Vec::new();
        }
        let scope = scope_path(&r.enclosing);
        let Some((receiver, class)) = self.receiver_name(f, &scope) else {
            return Vec::new();
        };
        if r.qualifier[0] != receiver {
            return Vec::new();
        }
        let mut found: Vec<usize> = self
            .subclasses_of(class)
            .into_iter()
            .flat_map(|sub| self.own_members(sub, &r.name))
            .filter(|id| self.reachable(f, *id))
            .collect();
        found.sort_unstable();
        found.dedup();
        found
    }

    /// Every class of the repository that has `class` among its ancestors.
    fn subclasses_of(&self, class: usize) -> Vec<usize> {
        let all = self.subclasses.get_or_init(|| {
            let mut map: HashMap<usize, Vec<usize>> = HashMap::new();
            for (id, rec) in self.ix.defs.iter().enumerate() {
                if rec.def.kind != DefKind::Class {
                    continue;
                }
                let mut queue: VecDeque<usize> = VecDeque::new();
                let mut seen: HashSet<usize> = HashSet::new();
                let first = self.bases(id);
                queue.extend(first.iter().filter_map(|b| match b {
                    Entity::Class(c) => Some(*c),
                    _ => None,
                }));
                while let Some(ancestor) = queue.pop_front() {
                    if !seen.insert(ancestor) {
                        continue;
                    }
                    map.entry(ancestor).or_default().push(id);
                    let next = self.bases(ancestor);
                    queue.extend(next.iter().filter_map(|b| match b {
                        Entity::Class(c) => Some(*c),
                        _ => None,
                    }));
                }
            }
            map
        });
        match all.get(&class) {
            Some(subs) => subs.clone(),
            None => Vec::new(),
        }
    }

    /// The fields, properties and variables a chain passes through between its
    /// head and its last name, as far as the receiver's type is known. Where
    /// nothing types the receiver, each member from there on is an untyped read
    /// like the chain's last name: `error.request.url` reads some object's
    /// `request`, which may be the property a change removed. A member the
    /// receiver's class lacks is bound to nothing, as the last name is: the line
    /// a removal strands.
    fn chain_reads(&self, f: usize, r: &Ref) -> Vec<(usize, Name, Resolution)> {
        let mut reads = Vec::new();
        if r.qualifier.len() < 2 {
            return reads;
        }
        let scope = scope_path(&r.enclosing);
        let (mut entity, kind) = self.head_entity(f, &scope, &r.qualifier[0], 0);
        let mut members = r.qualifier.iter().enumerate().skip(1);
        while let Some((index, member)) = members.next() {
            let (next, through, inherited) = match self.member(f, &entity, member) {
                Lookup::Found {
                    entity,
                    through,
                    inherited,
                } => (entity, through, inherited),
                Lookup::Untyped | Lookup::Any => {
                    reads.extend(std::iter::once((index, member)).chain(members).map(
                        |(index, name)| {
                            (
                                index,
                                name.clone(),
                                Resolution::Unresolved {
                                    reason: UnresolvedReason::UntypedRead,
                                },
                            )
                        },
                    ));
                    break;
                }
                Lookup::Absent(reason) => {
                    if reason.is_ours() {
                        reads.push((index, member.clone(), Resolution::Unresolved { reason }));
                    }
                    break;
                }
            };
            if let Entity::Def(id) = next {
                let held = matches!(
                    self.ix.defs[id].def.kind,
                    DefKind::Field | DefKind::Variable | DefKind::Constant | DefKind::Property
                );
                if held && self.reachable(f, id) {
                    let how = if inherited {
                        EvidenceKind::Inherited
                    } else {
                        kind
                    };
                    reads.push((
                        index,
                        member.clone(),
                        Resolution::Resolved {
                            target: self.ix.defs[id].key.clone(),
                            evidence: Evidence::through(how, self.paths(&through)),
                        },
                    ));
                }
            }
            entity = next;
        }
        reads
    }

    /// The variable, constant, field or class a chain starts from, when it is one
    /// the repository defines and not the receiver, a local or a module.
    fn head_read(&self, f: usize, r: &Ref) -> Option<(Name, Resolution)> {
        let head = r.qualifier.first()?;
        if matches!(r.receiver, Receiver::SelfType | Receiver::Typed(_)) {
            return None;
        }
        let scope = scope_path(&r.enclosing);
        if self.is_local(f, &scope, head)
            || self
                .receiver_name(f, &scope)
                .is_some_and(|(name, _)| name == *head)
        {
            return None;
        }
        let hit = self.lookup_name(f, &scope, head)?;
        let (Entity::Class(id) | Entity::Def(id)) = hit.entity else {
            return None;
        };
        let kind = self.ix.defs[id].def.kind;
        let reads = matches!(
            kind,
            DefKind::Class
                | DefKind::Variable
                | DefKind::Constant
                | DefKind::Field
                | DefKind::Function
        );
        if !reads || !self.reachable(f, id) {
            return None;
        }
        Some((
            head.clone(),
            Resolution::Resolved {
                target: self.ix.defs[id].key.clone(),
                evidence: Evidence::through(hit.kind, self.paths(&hit.through)),
            },
        ))
    }

    fn outcome(&self, f: usize, r: &Ref, depth: usize) -> Outcome {
        let scope = scope_path(&r.enclosing);
        if r.qualifier.is_empty() && matches!(r.receiver, Receiver::None) {
            if self.is_local(f, &scope, &r.name) {
                return Outcome::Unresolved(UnresolvedReason::Local);
            }
            return match self.lookup_name(f, &scope, &r.name) {
                Some(hit) => Outcome::Found {
                    entity: hit.entity,
                    evidence: Evidence::through(hit.kind, self.paths(&hit.through)),
                },
                None if is_builtin(r.name.as_str()) => {
                    Outcome::Unresolved(UnresolvedReason::Builtin)
                }
                None => Outcome::Unresolved(UnresolvedReason::NoBinding),
            };
        }
        if r.qualifier.is_empty() && matches!(r.receiver, Receiver::Unknown) {
            return Outcome::Unresolved(UnresolvedReason::Opaque);
        }
        let (base, kind) = self.base_entity(f, &scope, r, depth);
        let via_module = matches!(base, Entity::Module(_) | Entity::Package(_));
        match self.member(f, &base, &r.name) {
            Lookup::Found {
                entity,
                through,
                inherited,
            } => {
                let kind = if inherited {
                    EvidenceKind::Inherited
                } else if via_module {
                    EvidenceKind::ModuleAttribute
                } else {
                    kind
                };
                Outcome::Found {
                    entity,
                    evidence: Evidence::through(kind, self.paths(&through)),
                }
            }
            Lookup::Absent(reason) => Outcome::Unresolved(reason),
            Lookup::Untyped => self.untyped(f, r, false),
            Lookup::Any => self.untyped(f, r, true),
        }
    }

    /// A member of a receiver the code does not type. A call may be any method
    /// of that name a file of the caller's reach holds; a read, a write or a pass
    /// may be any object's attribute, so it names nothing.
    fn untyped(&self, f: usize, r: &Ref, any: bool) -> Outcome {
        if r.use_kind != UseKind::Call {
            return Outcome::Unresolved(UnresolvedReason::UntypedRead);
        }
        let Some(pool) = self.ix.members.get(&r.name) else {
            return Outcome::Unresolved(UnresolvedReason::NoCandidates);
        };
        let linked = self.imported_files(f);
        let ids: Vec<usize> = pool
            .iter()
            .copied()
            .filter(|id| {
                let g = self.ix.defs[*id].file;
                (any || g == f || self.same_package(f, g) || linked.contains(&g))
                    && self.reachable(f, *id)
            })
            .collect();
        if ids.is_empty() {
            Outcome::Unresolved(UnresolvedReason::NoCandidates)
        } else if ids.len() > MAX_CANDIDATES {
            Outcome::Unresolved(UnresolvedReason::TooManyCandidates)
        } else {
            Outcome::Candidates {
                ids,
                reason: if any {
                    CandidateReason::AnyReceiver
                } else {
                    CandidateReason::UntypedReceiver
                },
            }
        }
    }

    fn same_package(&self, a: usize, b: usize) -> bool {
        let dir = &self.ix.files[a].dir;
        *dir == self.ix.files[b].dir && self.ix.has_init(dir)
    }

    fn finish(&self, f: usize, use_kind: UseKind, outcome: Outcome) -> (Relation, Resolution) {
        let plain = plain_relation(use_kind);
        match outcome {
            Outcome::Found { entity, evidence } => match entity {
                Entity::Class(id) | Entity::Def(id) => {
                    if !self.reachable(f, id) {
                        return unresolved(plain, UnresolvedReason::OutsideProject);
                    }
                    let relation = relation_for(use_kind, self.ix.defs[id].def.kind);
                    (
                        relation,
                        Resolution::Resolved {
                            target: self.ix.defs[id].key.clone(),
                            evidence,
                        },
                    )
                }
                Entity::Module(g) => (
                    plain,
                    Resolution::Module {
                        file: self.path_of(g).clone(),
                        evidence,
                    },
                ),
                Entity::DoubtfulModule(g) => (
                    plain,
                    Resolution::Module {
                        file: self.path_of(g).clone(),
                        evidence: Evidence::through(
                            EvidenceKind::AmbiguousModule,
                            evidence.through,
                        ),
                    },
                ),
                Entity::Maybe(id) => (
                    relation_for(use_kind, self.ix.defs[id].def.kind),
                    Resolution::Candidate {
                        targets: vec![self.ix.defs[id].key.clone()],
                        reason: CandidateReason::AmbiguousModule,
                    },
                ),
                Entity::External => unresolved(plain, UnresolvedReason::External),
                Entity::Builtin => unresolved(plain, UnresolvedReason::Builtin),
                Entity::Outside => unresolved(plain, UnresolvedReason::OutsideProject),
                Entity::Unknown => unresolved(plain, UnresolvedReason::NoBinding),
                _ => unresolved(plain, UnresolvedReason::Opaque),
            },
            Outcome::Candidates { ids, reason } => {
                let keys: Vec<DefKey> = ids
                    .iter()
                    .filter(|id| self.reachable(f, **id))
                    .map(|id| self.ix.defs[*id].key.clone())
                    .collect();
                if keys.is_empty() {
                    unresolved(plain, UnresolvedReason::NoCandidates)
                } else {
                    (
                        plain,
                        Resolution::Candidate {
                            targets: keys,
                            reason,
                        },
                    )
                }
            }
            Outcome::Unresolved(reason) => unresolved(plain, reason),
        }
    }

    // ---- names

    /// Whether `name` is bound as a parameter or a variable of a function around
    /// the scope, or of the module's own top-level code.
    fn is_local(&self, f: usize, scope: &[Name], name: &Name) -> bool {
        let file = &self.ix.files[f];
        for depth in (0..=scope.len()).rev() {
            let prefix = &scope[..depth];
            if depth > 0 && !self.is_function(f, prefix) {
                continue;
            }
            if file
                .locals
                .get(prefix)
                .is_some_and(|set| set.contains(name))
            {
                return true;
            }
        }
        false
    }

    fn is_function(&self, f: usize, path: &[Name]) -> bool {
        self.ix.owner(f, path).is_some_and(|id| {
            matches!(
                self.ix.defs[id].def.kind,
                DefKind::Function | DefKind::Method | DefKind::Constructor | DefKind::Property
            )
        })
    }

    fn entity_of_def(&self, id: usize) -> Entity {
        if self.ix.defs[id].def.kind == DefKind::Class {
            Entity::Class(id)
        } else {
            Entity::Def(id)
        }
    }

    /// What a bare name means in a scope: the definitions of the scopes around it
    /// (a class's names are seen by its body, not by its methods), then the
    /// imports of those scopes, then the module's star imports.
    fn lookup_name(&self, f: usize, scope: &[Name], name: &Name) -> Option<Hit> {
        let file = &self.ix.files[f];
        for depth in (0..=scope.len()).rev() {
            let prefix = &scope[..depth];
            if depth > 0 && depth < scope.len() && !self.is_function(f, prefix) {
                continue;
            }
            if let Some(ids) = file.children.get(prefix) {
                let named: Vec<usize> = ids
                    .iter()
                    .copied()
                    .filter(|id| self.ix.defs[*id].def.local.name == *name)
                    .collect();
                if let Some(id) = self.ix.primary(&named) {
                    return Some(Hit {
                        entity: self.entity_of_def(id),
                        kind: EvidenceKind::SameScope,
                        through: Vec::new(),
                    });
                }
            }
            if let Some(hit) = self.lookup_import(f, prefix, name) {
                return Some(hit);
            }
        }
        self.lookup_star(f, name)
    }

    fn lookup_import(&self, f: usize, scope: &[Name], name: &Name) -> Option<Hit> {
        let rec = self.ix.files[f]
            .imports
            .iter()
            .rev()
            .find(|r| r.scope.as_slice() == scope && r.local.as_ref() == Some(name))?;
        let mut through = Vec::new();
        let mut seen = Seen::new();
        let entity = self.import_entity(f, rec, 0, &mut through, &mut seen);
        Some(Hit {
            entity,
            kind: EvidenceKind::ImportedName,
            through,
        })
    }

    fn lookup_star(&self, f: usize, name: &Name) -> Option<Hit> {
        if name.as_str().starts_with('_') {
            return None;
        }
        for rec in &self.ix.files[f].imports {
            let Spec::Star { relative, module } = &rec.spec else {
                continue;
            };
            if !rec.scope.is_empty() {
                continue;
            }
            if let ModuleRes::File(g) = self.module(f, *relative, module) {
                let mut through = Vec::new();
                let mut seen = Seen::new();
                if let Some(entity) = self.member_of_module(g, name, 0, &mut through, &mut seen) {
                    return Some(Hit {
                        entity,
                        kind: EvidenceKind::StarImport,
                        through,
                    });
                }
            }
        }
        None
    }

    fn module_entity(&self, f: usize, relative: u32, dotted: &str) -> Entity {
        match self.module(f, relative, dotted) {
            ModuleRes::File(g) => Entity::Module(g),
            ModuleRes::Package(p) => Entity::Package(p),
            ModuleRes::External => Entity::External,
            ModuleRes::Outside => Entity::Outside,
            ModuleRes::Ambiguous(g) => Entity::DoubtfulModule(g),
            ModuleRes::Missing => Entity::Unknown,
        }
    }

    /// The same entity as reached through a module that may not be the one meant.
    fn doubt(entity: Entity) -> Entity {
        match entity {
            Entity::Class(id) | Entity::Def(id) => Entity::Maybe(id),
            Entity::Module(g) => Entity::DoubtfulModule(g),
            other => other,
        }
    }

    fn import_entity(
        &self,
        f: usize,
        rec: &ImportRec,
        depth: usize,
        through: &mut Vec<usize>,
        seen: &mut Seen,
    ) -> Entity {
        match &rec.spec {
            Spec::Module { relative, dotted } => self.module_entity(f, *relative, dotted),
            Spec::Member {
                relative,
                module,
                name,
            } => match self.module(f, *relative, module) {
                ModuleRes::File(g) => self
                    .member_of_module(g, name, depth + 1, through, seen)
                    .unwrap_or(Entity::Unknown),
                ModuleRes::Package(p) => match self.module(f, 0, &format!("{p}.{name}")) {
                    ModuleRes::File(g) => Entity::Module(g),
                    ModuleRes::Package(q) => Entity::Package(q),
                    _ => Entity::Unknown,
                },
                ModuleRes::Ambiguous(g) => self
                    .member_of_module(g, name, depth + 1, through, seen)
                    .map_or(Entity::Unknown, Self::doubt),
                ModuleRes::External => Entity::External,
                ModuleRes::Outside => Entity::Outside,
                ModuleRes::Missing => Entity::Unknown,
            },
            Spec::Star { .. } => Entity::Unknown,
        }
    }

    /// What module `g` offers under `name`: its own definition, a name it takes
    /// from another module (re-exported), a star import's, or a submodule of the
    /// package. `through` collects the modules the name passed through.
    fn member_of_module(
        &self,
        g: usize,
        name: &Name,
        depth: usize,
        through: &mut Vec<usize>,
        seen: &mut Seen,
    ) -> Option<Entity> {
        if depth > MAX_HOPS || !seen.insert((g, name.clone())) {
            return None;
        }
        let file = &self.ix.files[g];
        let top: Path = Vec::new();
        let mut fallback = None;
        if let Some(ids) = file.children.get(&top) {
            let named: Vec<usize> = ids
                .iter()
                .copied()
                .filter(|id| self.ix.defs[*id].def.local.name == *name)
                .collect();
            if let Some(id) = self.ix.primary(&named) {
                if !self.ix.is_fallback(id) {
                    return Some(self.entity_of_def(id));
                }
                fallback = Some(id);
            }
        }
        for rec in file
            .imports
            .iter()
            .rev()
            .filter(|r| r.scope.is_empty() && r.local.as_ref() == Some(name))
        {
            let mut hops = Vec::new();
            let entity = self.import_entity(g, rec, depth + 1, &mut hops, seen);
            let outside = matches!(entity, Entity::External | Entity::Outside | Entity::Builtin);
            if entity != Entity::Unknown && !(fallback.is_some() && outside) {
                through.push(g);
                through.extend(hops);
                return Some(entity);
            }
        }
        if !name.as_str().starts_with('_') {
            for rec in file.imports.iter().filter(|r| r.scope.is_empty()) {
                let Spec::Star { relative, module } = &rec.spec else {
                    continue;
                };
                if let ModuleRes::File(h) = self.module(g, *relative, module) {
                    let mut hops = Vec::new();
                    if let Some(entity) = self.member_of_module(h, name, depth + 1, &mut hops, seen)
                    {
                        through.push(g);
                        through.extend(hops);
                        return Some(entity);
                    }
                }
            }
        }
        if let Some(id) = fallback {
            return Some(self.entity_of_def(id));
        }
        if file.is_init {
            if let Some(h) = self.ix.find_module("", &join(&file.dir, name.as_str())) {
                return Some(Entity::Module(h));
            }
        }
        None
    }

    // ---- receivers and chains

    /// The name a method calls its instance by, and the class it belongs to.
    fn receiver_name(&self, f: usize, scope: &[Name]) -> Option<(Name, usize)> {
        for depth in (1..=scope.len()).rev() {
            let Some(owner) = self.ix.owner(f, &scope[..depth]) else {
                continue;
            };
            let rec = &self.ix.defs[owner];
            if matches!(
                rec.def.kind,
                DefKind::Method | DefKind::Constructor | DefKind::Property
            ) {
                if rec
                    .def
                    .decorators
                    .iter()
                    .any(|d| d.as_str() == "staticmethod")
                {
                    return None;
                }
                let class = self.ix.owner(f, &scope[..depth - 1])?;
                if self.ix.defs[class].def.kind != DefKind::Class {
                    return None;
                }
                let name = self.ix.files[f].first_param.get(&scope[..depth])?.clone();
                return Some((name, class));
            }
        }
        None
    }

    fn self_entity(&self, f: usize, scope: &[Name]) -> Entity {
        self.receiver_name(f, scope)
            .map_or(Entity::Unknown, |(_, class)| Entity::Instance(class))
    }

    /// What the first name of a chain stands for.
    fn head_entity(
        &self,
        f: usize,
        scope: &[Name],
        head: &Name,
        depth: usize,
    ) -> (Entity, EvidenceKind) {
        if self
            .receiver_name(f, scope)
            .is_some_and(|(name, _)| name == *head)
        {
            return (self.self_entity(f, scope), EvidenceKind::SelfReceiver);
        }
        if self.is_local(f, scope, head) {
            return self.local_entity(f, scope, head, depth);
        }
        match self.lookup_name(f, scope, head) {
            Some(hit) => (hit.entity, hit.kind),
            None if is_builtin(head.as_str()) => (Entity::Builtin, EvidenceKind::SameScope),
            None => (Entity::Unknown, EvidenceKind::SameScope),
        }
    }

    /// The entity before the last name of a reference.
    fn base_entity(
        &self,
        f: usize,
        scope: &[Name],
        r: &Ref,
        depth: usize,
    ) -> (Entity, EvidenceKind) {
        if r.qualifier.is_empty() {
            return match &r.receiver {
                Receiver::CallResult(chain) => self.call_result(f, scope, chain, depth),
                _ => (Entity::Unknown, EvidenceKind::Constructed),
            };
        }
        match &r.receiver {
            Receiver::SelfType => return (self.self_entity(f, scope), EvidenceKind::SelfReceiver),
            Receiver::Typed(t) if r.qualifier.len() == 1 => {
                // An annotation that names no class (an alias, `URLTypes`) says
                // nothing of the object the name holds after `x = Cls(x)`.
                let typed = self.type_entity(f, scope, t.as_str());
                if typed != Entity::Unknown {
                    return (typed, EvidenceKind::TypedReceiver);
                }
            }
            _ => {}
        }
        let (mut entity, kind) = self.head_entity(f, scope, &r.qualifier[0], depth);
        for name in &r.qualifier[1..] {
            entity = self.step(f, &entity, name);
        }
        (entity, kind)
    }

    /// The entity a member is, for the next name of a chain to start from.
    fn step(&self, f: usize, entity: &Entity, name: &Name) -> Entity {
        match self.member(f, entity, name) {
            Lookup::Found { entity, .. } => entity,
            Lookup::Absent(UnresolvedReason::External) => Entity::External,
            Lookup::Absent(UnresolvedReason::Builtin) => Entity::Builtin,
            Lookup::Absent(UnresolvedReason::OutsideProject) => Entity::Outside,
            Lookup::Any => Entity::Dynamic,
            Lookup::Absent(_) | Lookup::Untyped => Entity::Unknown,
        }
    }

    /// What `path` names, as a chain from the scope: `Model.from_config`,
    /// `self.make`, `helpers.build`.
    fn path_entity(&self, f: usize, scope: &[Name], path: &[Name], depth: usize) -> Entity {
        let Some((head, rest)) = path.split_first() else {
            return Entity::Unknown;
        };
        let (mut entity, _) = self.head_entity(f, scope, head, depth);
        for name in rest {
            entity = self.step(f, &entity, name);
        }
        entity
    }

    /// The object a call makes: an instance of the class it constructs, or of
    /// the class a function annotated `-> Cls` returns.
    fn call_result(
        &self,
        f: usize,
        scope: &[Name],
        chain: &[Name],
        depth: usize,
    ) -> (Entity, EvidenceKind) {
        if let [only] = chain {
            if only.as_str() == "super" {
                let class = self.receiver_name(f, scope).map(|(_, class)| class);
                return (
                    class.map_or(Entity::Unknown, Entity::Super),
                    EvidenceKind::SuperCall,
                );
            }
        }
        if depth >= MAX_DEPTH {
            return (Entity::Unknown, EvidenceKind::Constructed);
        }
        match self.path_entity(f, scope, chain, depth + 1) {
            Entity::Class(c) => (Entity::Instance(c), EvidenceKind::Constructed),
            Entity::Def(id) => (self.return_instance(id), EvidenceKind::FactoryReturn),
            _ => (Entity::Unknown, EvidenceKind::Constructed),
        }
    }

    /// A local variable assigned from a call: `model = await Model.from_config()`.
    /// Every assignment of the name in the function must agree.
    fn local_entity(
        &self,
        f: usize,
        scope: &[Name],
        name: &Name,
        depth: usize,
    ) -> (Entity, EvidenceKind) {
        let file = &self.ix.files[f];
        for level in (0..=scope.len()).rev() {
            let key = (scope[..level].to_vec(), name.clone());
            let Some(assignments) = file.variables.get(&key) else {
                continue;
            };
            let slot = (f, key.0.clone(), name.clone());
            if let Some(known) = self.assumed.borrow().get(&slot) {
                return (known.clone(), EvidenceKind::Constructed);
            }
            let mut found: Option<(Entity, EvidenceKind)> = None;
            for at in assignments {
                // `x = x.copy_with(...)`: what the name held before is the type
                // its next assignment is read under.
                if let Some((entity, _)) = &found {
                    self.assumed
                        .borrow_mut()
                        .insert(slot.clone(), entity.clone());
                }
                let next = self.assigned_call(f, *at, depth);
                self.assumed.borrow_mut().remove(&slot);
                let Some(next) = next else {
                    return (Entity::Unknown, EvidenceKind::Constructed);
                };
                match &found {
                    Some((entity, _)) if *entity != next.0 => {
                        return (Entity::Unknown, EvidenceKind::Constructed);
                    }
                    _ => found = Some(next),
                }
            }
            return found.unwrap_or((Entity::Unknown, EvidenceKind::Constructed));
        }
        (Entity::Unknown, EvidenceKind::Constructed)
    }

    /// The object the variable bound by the fact at `binding` is assigned.
    fn assigned_call(
        &self,
        f: usize,
        binding: usize,
        depth: usize,
    ) -> Option<(Entity, EvidenceKind)> {
        let facts = &self.ix.files[f].facts.blob.facts;
        let Fact::LocalBinding(local) = facts.get(binding)? else {
            return None;
        };
        if self.literal_after(f, local.site.bytes.end) {
            return Some((Entity::Builtin, EvidenceKind::Constructed));
        }
        self.call_assigned_after(f, binding + 1, local.site.bytes.end, depth)
            .or_else(|| self.entered_call(f, local.site.bytes.start, depth))
            .or_else(|| self.alias_after(f, binding + 1, local.site.bytes.end, depth))
    }

    /// The object made by the call a `with` item binds the variable to:
    /// `with Cls(...) as v` / `async with Cls(...) as v`, the call read back from
    /// the text before `as` and found among the facts by where its name ends.
    fn entered_call(&self, f: usize, binding: u32, depth: usize) -> Option<(Entity, EvidenceKind)> {
        if depth >= MAX_DEPTH {
            return None;
        }
        let text = self.text(f)?;
        let blank = |b: &u8| b.is_ascii_whitespace();
        let mut end = binding as usize;
        let skip_back = |mut at: usize| {
            while at > 0 && blank(&text[at - 1]) {
                at -= 1;
            }
            at
        };
        end = skip_back(end);
        if end < 3
            || &text[end - 2..end] != b"as"
            || !blank(&text[end - 3]) && text[end - 3] != b')'
        {
            return None;
        }
        end = skip_back(end - 2);
        if end == 0 || text[end - 1] != b')' {
            return None;
        }
        let mut open = end - 1;
        let mut depth_parens = 0usize;
        loop {
            match text[open] {
                b')' => depth_parens += 1,
                b'(' => {
                    depth_parens -= 1;
                    if depth_parens == 0 {
                        break;
                    }
                }
                _ => {}
            }
            if open == 0 {
                return None;
            }
            open -= 1;
        }
        let name_end = skip_back(open);
        let facts = &self.ix.files[f].facts.blob.facts;
        let at = facts.partition_point(|fact| (fact.site().bytes.start as usize) < name_end);
        let Fact::Reference(callee) = facts.get(at.checked_sub(1)?)? else {
            return None;
        };
        if callee.use_kind != UseKind::Call || callee.site.bytes.end as usize != name_end {
            return None;
        }
        // The chain written before the name is the one the facts qualify it with.
        let chain: String = callee.qualifier.iter().map(|q| format!("{q}.")).collect();
        let mut from = callee.site.bytes.start as usize;
        for want in chain.bytes().rev() {
            from = skip_back(from);
            if from == 0 || text[from - 1] != want {
                return None;
            }
            from -= 1;
        }
        self.callee_result(f, callee, depth)
    }

    /// The object made by the call that is the fact `next`, when the text from
    /// `after` to the call is `= [await] chain.` and the call ends the statement.
    fn call_assigned_after(
        &self,
        f: usize,
        next: usize,
        after: u32,
        depth: usize,
    ) -> Option<(Entity, EvidenceKind)> {
        self.call_after(f, next, after, Lead::Assign, depth)
    }

    /// The object made by the call that is the fact `next`, when the text from
    /// `after` to it is `lead [await] chain.` and the call ends the statement.
    fn call_after(
        &self,
        f: usize,
        next: usize,
        after: u32,
        lead: Lead,
        depth: usize,
    ) -> Option<(Entity, EvidenceKind)> {
        if depth >= MAX_DEPTH {
            return None;
        }
        let facts = &self.ix.files[f].facts.blob.facts;
        let Fact::Reference(callee) = facts.get(next)? else {
            return None;
        };
        if callee.use_kind != UseKind::Call {
            return None;
        }
        self.chain_follows(f, after, lead, callee, true)?;
        self.callee_result(f, callee, depth)
    }

    /// The text from `after` to `source` is `lead [await] chain.`, the chain
    /// written before the name as the facts qualify it, and the expression ends
    /// its statement: what a variable is assigned is that one reference. With
    /// `call`, the reference is a callee and the statement ends with its call.
    fn chain_follows(
        &self,
        f: usize,
        after: u32,
        lead: Lead,
        source: &Ref,
        call: bool,
    ) -> Option<()> {
        let text = self.text(f)?;
        #[allow(
            clippy::disallowed_methods,
            reason = "the slice is read for an assignment shape; bytes that are not UTF-8 show none, so nothing is inferred from them"
        )]
        let between =
            std::str::from_utf8(text.get(after as usize..source.site.bytes.start as usize)?)
                .ok()?;
        let mut rest = match lead {
            Lead::Assign => {
                let equals = between.find('=')?;
                let before = between[..equals].trim();
                let after_equals = &between[equals + 1..];
                if !(before.is_empty() || before.starts_with(':') && !before.contains('\n'))
                    || after_equals.starts_with('=')
                {
                    return None;
                }
                after_equals.trim_start()
            }
            Lead::Return => between.trim_start(),
        };
        if let Some(awaited) = rest.strip_prefix("await") {
            if awaited.starts_with(char::is_whitespace) {
                rest = awaited.trim_start();
            }
        }
        let written: String = rest.split_whitespace().collect();
        let expected: String = source.qualifier.iter().map(|q| format!("{q}.")).collect();
        let end = source.site.bytes.end as usize;
        let ends = if call {
            ends_statement(&text, end)
        } else {
            name_ends_statement(&text, end)
        };
        (written == expected && ends).then_some(())
    }

    /// What a variable assigned a name or an attribute chain holds, when the
    /// chain is static: `internals = options.internals`,
    /// `retry = self.options.retry`. The fact at `next` is the chain's last name,
    /// read; what it resolves to is what the variable holds.
    fn alias_after(
        &self,
        f: usize,
        next: usize,
        after: u32,
        depth: usize,
    ) -> Option<(Entity, EvidenceKind)> {
        if depth >= MAX_DEPTH {
            return None;
        }
        let facts = &self.ix.files[f].facts.blob.facts;
        let Fact::Reference(source) = facts.get(next)? else {
            return None;
        };
        if source.use_kind != UseKind::Read {
            return None;
        }
        self.chain_follows(f, after, Lead::Assign, source, false)?;
        match self.outcome(f, source, depth + 1) {
            Outcome::Found {
                entity:
                    held @ (Entity::Class(_)
                    | Entity::Instance(_)
                    | Entity::Def(_)
                    | Entity::Module(_)
                    | Entity::Package(_)
                    | Entity::Dynamic),
                ..
            } => Some((held, EvidenceKind::Aliased)),
            _ => None,
        }
    }

    /// What calling `callee` makes: the language's own object, an object of a
    /// class of a package outside the repository (a capitalised name: the type is
    /// known and not ours), an instance of a class of the repository, or what a
    /// function returns. What a function of outside code returns is unknown: it
    /// may hand back an object of ours (`pytest.raises(E).value`).
    fn callee_result(
        &self,
        f: usize,
        callee: &Ref,
        depth: usize,
    ) -> Option<(Entity, EvidenceKind)> {
        match self.outcome(f, callee, depth + 1) {
            Outcome::Unresolved(UnresolvedReason::Builtin) => {
                Some((Entity::Builtin, EvidenceKind::Constructed))
            }
            Outcome::Unresolved(UnresolvedReason::External)
            | Outcome::Found {
                entity: Entity::External,
                ..
            } if callee.name.as_str().starts_with(char::is_uppercase) => {
                Some((Entity::External, EvidenceKind::Constructed))
            }
            Outcome::Found {
                entity: Entity::Class(c),
                ..
            } => Some((Entity::Instance(c), EvidenceKind::Constructed)),
            Outcome::Found {
                entity: Entity::Def(id),
                ..
            } => {
                // A variable that holds a class makes an instance of it when called.
                let made = match self.ix.defs[id].def.kind {
                    DefKind::Variable | DefKind::Constant | DefKind::Field => {
                        match self.var_instance(id) {
                            Entity::Class(c) => Entity::Instance(c),
                            _ => Entity::Unknown,
                        }
                    }
                    _ => self.return_instance(id),
                };
                match made {
                    Entity::Unknown => None,
                    entity => Some((entity, EvidenceKind::FactoryReturn)),
                }
            }
            _ => None,
        }
    }

    /// Whether the text after `after` is `= <literal>`: a list, a dict, a string,
    /// a number. What it binds is the language's own object, never a class of the
    /// repository.
    fn literal_after(&self, f: usize, after: u32) -> bool {
        let Some(text) = self.text(f) else {
            return false;
        };
        let Some(tail) = text.get(after as usize..) else {
            return false;
        };
        let tail = String::from_utf8_lossy(&tail[..tail.len().min(96)]).into_owned();
        let line = tail.split('\n').next().unwrap_or("");
        let Some((before, rest)) = line.split_once('=') else {
            return false;
        };
        let before = before.trim();
        if !(before.is_empty() || before.starts_with(':')) || rest.starts_with('=') {
            return false;
        }
        let mut chars = rest.trim_start().chars();
        match chars.next() {
            Some('[' | '{' | '"' | '\'') => true,
            Some(c) if c.is_ascii_digit() => true,
            Some('f' | 'b' | 'r' | 'u' | 'F' | 'B' | 'R' | 'U') => {
                matches!(chars.next(), Some('"' | '\''))
            }
            _ => false,
        }
    }

    /// The instance of the class a function's `-> Cls` annotation names; without
    /// an annotation, the class of its one `return Cls(...)`.
    fn return_instance(&self, id: usize) -> Entity {
        let rec = &self.ix.defs[id];
        let annotation = rec
            .def
            .signature
            .as_deref()
            .and_then(|signature| signature.rfind("->").map(|at| &signature[at + 2..]));
        match annotation {
            Some(written) => self.type_entity(rec.file, &rec.def.local.container, written),
            None => self.sole_return(id),
        }
    }

    fn sole_return(&self, id: usize) -> Entity {
        if let Some(known) = self.returns.borrow().get(&id) {
            return known.clone();
        }
        self.returns.borrow_mut().insert(id, Entity::Unknown);
        let found = self.compute_sole_return(id);
        self.returns.borrow_mut().insert(id, found.clone());
        found
    }

    /// The function has one `return`, no `yield` and no function or class inside
    /// it, and the `return` hands back a call: what the call makes, the instance
    /// of a constructor it names.
    fn compute_sole_return(&self, id: usize) -> Entity {
        let rec = &self.ix.defs[id];
        if !matches!(rec.def.kind, DefKind::Function | DefKind::Method) {
            return Entity::Unknown;
        }
        let f = rec.file;
        let Some(text) = self.text(f) else {
            return Entity::Unknown;
        };
        let from = rec.def.span.bytes.start as usize;
        #[allow(
            clippy::disallowed_methods,
            reason = "a body that is not UTF-8 resolves as `Entity::Unknown`, which callers keep unresolved"
        )]
        let Some(body) = text
            .get(from..rec.def.span.bytes.end as usize)
            .and_then(|b| std::str::from_utf8(b).ok())
        else {
            return Entity::Unknown;
        };
        if body
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .any(|word| word == "yield")
        {
            return Entity::Unknown;
        }
        let (mut returns, mut defined, mut offset) = (Vec::new(), 0usize, from);
        for line in body.split_inclusive('\n') {
            let trimmed = line.trim_start();
            if ["def ", "async def ", "class "]
                .iter()
                .any(|keyword| trimmed.starts_with(keyword))
            {
                defined += 1;
            }
            if let Some(after) = trimmed.strip_prefix("return") {
                if after.is_empty() || after.starts_with(|c: char| c.is_whitespace() || c == '(') {
                    returns.push(offset + (line.len() - trimmed.len()) + "return".len());
                }
            }
            offset += line.len();
        }
        let ([after], 1) = (returns.as_slice(), defined) else {
            return Entity::Unknown;
        };
        let facts = &self.ix.files[f].facts.blob.facts;
        let at = facts.partition_point(|fact| (fact.site().bytes.start as usize) < *after);
        match self.call_after(f, at, *after as u32, Lead::Return, 1) {
            Some((entity, _)) => entity,
            None => Entity::Unknown,
        }
    }

    var_instance_memo!();

    fn compute_var_instance(&self, id: usize) -> Entity {
        let rec = &self.ix.defs[id];
        if let Some((_, annotation)) = rec.def.signature.as_deref().and_then(|s| s.split_once(':'))
        {
            let typed = self.type_entity(rec.file, &rec.def.local.container, annotation);
            if typed != Entity::Unknown {
                return typed;
            }
        }
        if self.literal_after(rec.file, rec.def.name_span.bytes.end) {
            return Entity::Builtin;
        }
        let after = rec.def.name_span.bytes.end;
        let value = self.value_fact(rec.file, rec.fact + 1, after);
        match self
            .call_assigned_after(rec.file, value, after, 0)
            .or_else(|| self.alias_after(rec.file, value, after, 0))
        {
            Some((entity, _)) => entity,
            None => Entity::Unknown,
        }
    }

    /// The fact that starts the value a declaration assigns: the facts the
    /// annotation between the name and the `=` holds are the type's, not the value's.
    fn value_fact(&self, f: usize, next: usize, after: u32) -> usize {
        let Some(text) = self.text(f) else {
            return next;
        };
        let line = text.get(after as usize..).map_or(&[][..], |tail| {
            tail.split(|b| *b == b'\n').next().unwrap_or(tail)
        });
        let Some(equals) = line.iter().position(|b| *b == b'=') else {
            return next;
        };
        let start = after + equals as u32 + 1;
        let facts = &self.ix.files[f].facts.blob.facts;
        next + facts[next.min(facts.len())..]
            .partition_point(|fact| fact.site().bytes.start < start)
    }

    /// The instance a type written in code stands for.
    fn type_entity(&self, f: usize, scope: &[Name], text: &str) -> Entity {
        let text = text.trim().trim_matches(|c| c == '"' || c == '\'');
        if matches!(
            text,
            "Any" | "typing.Any" | "t.Any" | "typing_extensions.Any" | "object"
        ) {
            return Entity::Dynamic;
        }
        if !is_dotted_name(text) {
            return Entity::Unknown;
        }
        match self.resolve_dotted(f, scope, text) {
            Entity::Class(c) => Entity::Instance(c),
            Entity::External => Entity::External,
            Entity::Builtin => Entity::Builtin,
            _ => Entity::Unknown,
        }
    }

    fn resolve_dotted(&self, f: usize, scope: &[Name], text: &str) -> Entity {
        let mut parts = text.split('.');
        let Some(head) = parts.next() else {
            return Entity::Unknown;
        };
        let head = Name::new(head);
        let mut entity = match self.lookup_name(f, scope, &head) {
            Some(hit) => hit.entity,
            None if is_builtin(head.as_str()) => Entity::Builtin,
            None => Entity::Unknown,
        };
        for part in parts {
            entity = self.step(f, &entity, &Name::new(part));
        }
        entity
    }

    // ---- members

    fn member(&self, f: usize, entity: &Entity, name: &Name) -> Lookup {
        match entity {
            Entity::Module(g) => {
                let mut through = Vec::new();
                let mut seen = Seen::new();
                match self.member_of_module(*g, name, 0, &mut through, &mut seen) {
                    Some(entity) => Lookup::Found {
                        entity,
                        through,
                        inherited: false,
                    },
                    None => Lookup::Absent(UnresolvedReason::NoBinding),
                }
            }
            Entity::Package(p) => match self.module(f, 0, &format!("{p}.{name}")) {
                ModuleRes::File(g) => Lookup::Found {
                    entity: Entity::Module(g),
                    through: Vec::new(),
                    inherited: false,
                },
                ModuleRes::Package(q) => Lookup::Found {
                    entity: Entity::Package(q),
                    through: Vec::new(),
                    inherited: false,
                },
                ModuleRes::Outside => Lookup::Absent(UnresolvedReason::OutsideProject),
                _ => Lookup::Absent(UnresolvedReason::NoBinding),
            },
            Entity::DoubtfulModule(g) => {
                let mut through = Vec::new();
                let mut seen = Seen::new();
                match self.member_of_module(*g, name, 0, &mut through, &mut seen) {
                    Some(entity) => Lookup::Found {
                        entity: Self::doubt(entity),
                        through,
                        inherited: false,
                    },
                    None => Lookup::Absent(UnresolvedReason::NoBinding),
                }
            }
            Entity::Maybe(_) => Lookup::Untyped,
            // A structural type is implemented by any class that has the member.
            Entity::Instance(c) if self.is_protocol(*c) => Lookup::Any,
            Entity::Class(c) | Entity::Instance(c) => self.class_member(*c, name, false),
            Entity::Super(c) => self.class_member(*c, name, true),
            Entity::Def(id) => match self.ix.defs[*id].def.kind {
                DefKind::Variable | DefKind::Constant | DefKind::Field => {
                    match self.var_instance(*id) {
                        Entity::Unknown => Lookup::Untyped,
                        held => self.member(f, &held, name),
                    }
                }
                DefKind::Property => match self.return_instance(*id) {
                    Entity::Unknown => Lookup::Untyped,
                    held => self.member(f, &held, name),
                },
                _ => Lookup::Untyped,
            },
            Entity::Dynamic => Lookup::Any,
            Entity::External => Lookup::Absent(UnresolvedReason::External),
            Entity::Builtin => Lookup::Absent(UnresolvedReason::Builtin),
            Entity::Outside => Lookup::Absent(UnresolvedReason::OutsideProject),
            Entity::Unknown => Lookup::Untyped,
        }
    }

    /// Whether the class lists `Protocol` among its bases.
    fn is_protocol(&self, class: usize) -> bool {
        (self.ix.defs[class].def.signature.as_deref())
            .map_or(Vec::new(), base_names)
            .iter()
            .any(|base| base.rsplit('.').next() == Some("Protocol"))
    }

    fn class_member(&self, class: usize, name: &Name, bases_only: bool) -> Lookup {
        let (defs, opaque) = self.class_members(class, name, bases_only);
        match self.ix.primary(&defs) {
            Some(id) => {
                let found = &self.ix.defs[id];
                let owner = &self.ix.defs[class];
                let inherited = !bases_only
                    && (found.file != owner.file
                        || found.path[..found.path.len() - 1] != owner.path[..]);
                Lookup::Found {
                    entity: self.entity_of_def(id),
                    through: Vec::new(),
                    inherited,
                }
            }
            None if opaque => Lookup::Absent(UnresolvedReason::ExternalBase),
            None => Lookup::Absent(UnresolvedReason::NoMember),
        }
    }

    /// The members called `name` of the first class, in breadth-first order of
    /// its bases, that has any; and whether a base the repository does not hold
    /// stood in the way.
    fn class_members(&self, class: usize, name: &Name, bases_only: bool) -> (Vec<usize>, bool) {
        let mut queue = VecDeque::from([class]);
        let mut seen = HashSet::new();
        let mut opaque = false;
        let mut first = true;
        while let Some(current) = queue.pop_front() {
            if !seen.insert(current) {
                continue;
            }
            if !(first && bases_only) {
                let own = self.own_members(current, name);
                if !own.is_empty() {
                    return (own, opaque);
                }
            }
            first = false;
            for base in self.bases(current).iter() {
                match base {
                    Entity::Class(b) => queue.push_back(*b),
                    Entity::Builtin => {}
                    _ => opaque = true,
                }
            }
        }
        (Vec::new(), opaque)
    }

    fn own_members(&self, class: usize, name: &Name) -> Vec<usize> {
        let rec = &self.ix.defs[class];
        let Some(ids) = self.ix.files[rec.file].children.get(&rec.path) else {
            return Vec::new();
        };
        let named: Vec<usize> = ids
            .iter()
            .copied()
            .filter(|id| self.ix.defs[*id].def.local.name == *name)
            .collect();
        self.ix.primary(&named).into_iter().collect()
    }

    /// The bases of a class as written in its header, each resolved from the
    /// scope the class is defined in; one that is no class of the repository is
    /// `External`, `Builtin` or `Unknown`.
    fn bases(&self, class: usize) -> Rc<Vec<Entity>> {
        if let Some(known) = self.bases.borrow().get(&class) {
            return Rc::clone(known);
        }
        self.bases.borrow_mut().insert(class, Rc::new(Vec::new()));
        let rec = &self.ix.defs[class];
        #[allow(
            clippy::disallowed_methods,
            reason = "a definition without a signature lists no bases"
        )]
        let entities: Vec<Entity> = rec
            .def
            .signature
            .as_deref()
            .map(base_names)
            .unwrap_or_default()
            .iter()
            .map(|text| self.resolve_dotted(rec.file, &rec.def.local.container, text))
            .collect();
        let entities = Rc::new(entities);
        self.bases.borrow_mut().insert(class, Rc::clone(&entities));
        entities
    }

    /// The files the file imports names or modules from, and the files those
    /// names passed through.
    fn imported_files(&self, f: usize) -> Rc<HashSet<usize>> {
        if let Some(known) = self.imported.borrow().get(&f) {
            return Rc::clone(known);
        }
        let mut files = HashSet::new();
        for rec in &self.ix.files[f].imports {
            match &rec.spec {
                Spec::Module { relative, dotted } => {
                    if let ModuleRes::File(g) | ModuleRes::Ambiguous(g) =
                        self.module(f, *relative, dotted)
                    {
                        files.insert(g);
                    }
                }
                Spec::Star { relative, module } => {
                    if let ModuleRes::File(g) = self.module(f, *relative, module) {
                        files.insert(g);
                    }
                }
                Spec::Member {
                    relative,
                    module,
                    name,
                } => {
                    if let ModuleRes::File(g) = self.module(f, *relative, module) {
                        files.insert(g);
                        let mut through = Vec::new();
                        let mut seen = Seen::new();
                        let found = self.member_of_module(g, name, 0, &mut through, &mut seen);
                        files.extend(through);
                        match found {
                            Some(Entity::Class(id) | Entity::Def(id)) => {
                                files.insert(self.ix.defs[id].file);
                            }
                            Some(Entity::Module(h)) => {
                                files.insert(h);
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        let files = Rc::new(files);
        self.imported.borrow_mut().insert(f, Rc::clone(&files));
        files
    }

    // ---- imports

    fn import(&self, f: usize, import: &Import, out: &mut Vec<Binding>) {
        let file = &self.ix.files[f];
        let kind = if import.kind == ImportKind::Dynamic {
            EvidenceKind::DynamicImport
        } else {
            EvidenceKind::ImportedName
        };
        let make = |relation: Relation, name: Name, resolution: Resolution| Binding {
            file: file.facts.path.clone(),
            site: import.site.clone(),
            from: import.enclosing.clone(),
            relation,
            name,
            resolution,
            entry_guard: false,
        };
        let found = self.module(f, import.relative, import.module.as_str());
        if import.names.is_empty() {
            let resolution = match &found {
                ModuleRes::File(g) => Resolution::Module {
                    file: self.path_of(*g).clone(),
                    evidence: Evidence::new(kind),
                },
                ModuleRes::Ambiguous(g) => Resolution::Module {
                    file: self.path_of(*g).clone(),
                    evidence: Evidence::new(EvidenceKind::AmbiguousModule),
                },
                other => module_failure(other),
            };
            out.push(make(
                Relation::Imports,
                Name::new(import.module.as_str()),
                resolution,
            ));
            return;
        }
        let offered = file.is_init && matches!(import.enclosing, Scope::Module);
        for (name, alias) in &import.names {
            let local = alias.as_ref().unwrap_or(name);
            let relation =
                if (offered && !local.as_str().starts_with('_')) || file.exports.contains(local) {
                    Relation::Reexports
                } else {
                    Relation::Imports
                };
            let resolution = match &found {
                ModuleRes::File(g) => {
                    let mut through = Vec::new();
                    let mut seen = Seen::new();
                    match self.member_of_module(*g, name, 0, &mut through, &mut seen) {
                        Some(entity) => self.imported_resolution(
                            f,
                            entity,
                            Evidence::through(kind, self.paths(&through)),
                        ),
                        None => Resolution::Unresolved {
                            reason: UnresolvedReason::NoBinding,
                        },
                    }
                }
                ModuleRes::Ambiguous(g) => {
                    let mut through = Vec::new();
                    let mut seen = Seen::new();
                    match self.member_of_module(*g, name, 0, &mut through, &mut seen) {
                        Some(entity) => self.imported_resolution(
                            f,
                            Self::doubt(entity),
                            Evidence::through(kind, self.paths(&through)),
                        ),
                        None => Resolution::Unresolved {
                            reason: UnresolvedReason::NoBinding,
                        },
                    }
                }
                ModuleRes::Package(p) => match self.module(f, 0, &format!("{p}.{name}")) {
                    ModuleRes::File(g) => Resolution::Module {
                        file: self.path_of(g).clone(),
                        evidence: Evidence::new(kind),
                    },
                    _ => Resolution::Unresolved {
                        reason: UnresolvedReason::Opaque,
                    },
                },
                other => module_failure(other),
            };
            out.push(make(relation, name.clone(), resolution));
        }
    }

    fn imported_resolution(&self, f: usize, entity: Entity, evidence: Evidence) -> Resolution {
        match entity {
            Entity::Class(id) | Entity::Def(id) => {
                if self.reachable(f, id) {
                    Resolution::Resolved {
                        target: self.ix.defs[id].key.clone(),
                        evidence,
                    }
                } else {
                    Resolution::Unresolved {
                        reason: UnresolvedReason::OutsideProject,
                    }
                }
            }
            Entity::Maybe(id) => Resolution::Candidate {
                targets: vec![self.ix.defs[id].key.clone()],
                reason: CandidateReason::AmbiguousModule,
            },
            Entity::Module(g) => Resolution::Module {
                file: self.path_of(g).clone(),
                evidence,
            },
            Entity::External => Resolution::Unresolved {
                reason: UnresolvedReason::External,
            },
            _ => Resolution::Unresolved {
                reason: UnresolvedReason::Opaque,
            },
        }
    }

    // ---- overrides

    /// A method that replaces one of a base class of the repository.
    fn overrides(&self, out: &mut Vec<Binding>) {
        for (id, rec) in self.ix.defs.iter().enumerate() {
            if !matches!(rec.def.kind, DefKind::Method | DefKind::Property) || rec.path.len() < 2 {
                continue;
            }
            if self.ix.owner(rec.file, &rec.path) != Some(id) {
                continue;
            }
            let Some(class) = self.ix.owner(rec.file, &rec.path[..rec.path.len() - 1]) else {
                continue;
            };
            if self.ix.defs[class].def.kind != DefKind::Class {
                continue;
            }
            let (found, _) = self.class_members(class, &rec.def.local.name, true);
            let Some(base) = self.ix.primary(&found) else {
                continue;
            };
            if !self.reachable(rec.file, base) {
                continue;
            }
            out.push(Binding {
                file: self.path_of(rec.file).clone(),
                site: rec.def.name_span.clone(),
                from: Scope::Def(rec.def.local.clone()),
                relation: Relation::Overrides,
                name: rec.def.local.name.clone(),
                resolution: Resolution::Resolved {
                    target: self.ix.defs[base].key.clone(),
                    evidence: Evidence::new(EvidenceKind::Overridden),
                },
                entry_guard: false,
            });
        }
    }
}

fn unresolved(relation: Relation, reason: UnresolvedReason) -> (Relation, Resolution) {
    (relation, Resolution::Unresolved { reason })
}

fn module_failure(found: &ModuleRes) -> Resolution {
    let reason = match found {
        ModuleRes::External => UnresolvedReason::External,
        ModuleRes::Outside => UnresolvedReason::OutsideProject,
        ModuleRes::Missing => UnresolvedReason::MissingModule,
        ModuleRes::File(_) | ModuleRes::Package(_) | ModuleRes::Ambiguous(_) => {
            UnresolvedReason::Opaque
        }
    };
    Resolution::Unresolved { reason }
}

/// A call of a class makes an object; a variable passed as an argument is read.
fn relation_for(use_kind: UseKind, target: DefKind) -> Relation {
    match (use_kind, target) {
        (UseKind::Call, DefKind::Class) => Relation::Instantiates,
        (
            UseKind::PassAsValue,
            DefKind::Variable | DefKind::Constant | DefKind::Field | DefKind::TypeAlias,
        ) => Relation::Reads,
        _ => plain_relation(use_kind),
    }
}

/// The base classes a `class Name(A, b.B[int], metaclass=M)` signature names.
fn base_names(signature: &str) -> Vec<String> {
    let Some(rest) = signature.strip_prefix("class ") else {
        return Vec::new();
    };
    let Some(open) = rest.find('(') else {
        return Vec::new();
    };
    let mut items: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut depth = 0usize;
    for c in rest[open + 1..].chars() {
        match c {
            '(' | '[' | '{' => {
                depth += 1;
                current.push(c);
            }
            ')' | ']' | '}' if depth > 0 => {
                depth -= 1;
                current.push(c);
            }
            ')' => break,
            ',' if depth == 0 => items.push(std::mem::take(&mut current)),
            _ => current.push(c),
        }
    }
    items.push(current);
    items
        .into_iter()
        .filter_map(|item| {
            let item = item.trim();
            let head = item.split('[').next().unwrap_or(item).trim();
            (!item.contains('=') && is_dotted_name(head)).then(|| head.to_string())
        })
        .collect()
}

/// Whether a name that ends at `at` is the whole of its statement: nothing but
/// blanks and a comment follows on the line.
fn name_ends_statement(text: &[u8], mut at: usize) -> bool {
    while text.get(at).is_some_and(|b| *b == b' ' || *b == b'\t') {
        at += 1;
    }
    text.get(at)
        .is_none_or(|b| matches!(*b, b'\n' | b'\r' | b'#' | b';'))
}

/// Whether the call whose callee ends at `at` is the end of its statement.
fn ends_statement(text: &[u8], mut at: usize) -> bool {
    let blank = |b: u8| b == b' ' || b == b'\t';
    while text.get(at).is_some_and(|b| blank(*b)) {
        at += 1;
    }
    if text.get(at) != Some(&b'(') {
        return false;
    }
    let mut depth = 0usize;
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
                b'"' | b'\'' => quote = Some(c),
                b'#' => {
                    while text.get(at).is_some_and(|b| *b != b'\n') {
                        at += 1;
                    }
                    continue;
                }
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        at += 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        at += 1;
    }
    if depth != 0 {
        return false;
    }
    while text.get(at).is_some_and(|b| blank(*b)) {
        at += 1;
    }
    text.get(at)
        .is_none_or(|b| matches!(*b, b'\n' | b'\r' | b'#' | b';'))
}

/// Names Python provides without an import.
fn is_builtin(name: &str) -> bool {
    const BUILTINS: &[&str] = &[
        "abs",
        "aiter",
        "all",
        "anext",
        "any",
        "ascii",
        "bin",
        "bool",
        "breakpoint",
        "bytearray",
        "bytes",
        "callable",
        "chr",
        "classmethod",
        "compile",
        "complex",
        "delattr",
        "dict",
        "dir",
        "divmod",
        "enumerate",
        "eval",
        "exec",
        "filter",
        "float",
        "format",
        "frozenset",
        "getattr",
        "globals",
        "hasattr",
        "hash",
        "help",
        "hex",
        "id",
        "input",
        "int",
        "isinstance",
        "issubclass",
        "iter",
        "len",
        "list",
        "locals",
        "map",
        "max",
        "memoryview",
        "min",
        "next",
        "object",
        "oct",
        "open",
        "ord",
        "pow",
        "print",
        "property",
        "range",
        "repr",
        "reversed",
        "round",
        "set",
        "setattr",
        "slice",
        "sorted",
        "staticmethod",
        "str",
        "sum",
        "super",
        "tuple",
        "type",
        "vars",
        "zip",
        "__import__",
        "__name__",
        "__file__",
        "__doc__",
        "Exception",
        "BaseException",
        "ValueError",
        "TypeError",
        "KeyError",
        "IndexError",
        "AttributeError",
        "RuntimeError",
        "NotImplementedError",
        "StopIteration",
        "OSError",
        "IOError",
        "ImportError",
        "AssertionError",
        "NameError",
        "LookupError",
        "ArithmeticError",
        "ZeroDivisionError",
        "KeyboardInterrupt",
        "SystemExit",
        "FileNotFoundError",
        "PermissionError",
        "TimeoutError",
        "UnicodeDecodeError",
        "UnicodeEncodeError",
        "Warning",
        "DeprecationWarning",
        "UserWarning",
        "NotImplemented",
        "Ellipsis",
        "True",
        "False",
        "None",
        "self",
        "cls",
    ];
    BUILTINS.contains(&name)
}
