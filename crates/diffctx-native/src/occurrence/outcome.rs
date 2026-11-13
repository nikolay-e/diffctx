//! The outcome of each occurrence of a definition's names in one file of one
//! side: what a binding, a declaration or a fact of that side says the spelling
//! is, and when it says nothing, a candidate.
//!
//! A claim binds an occurrence only by equal (file, byte range) and equal name,
//! never by line or containment. A `not_ours` is given only where the
//! accounting checks its basis in the facts of the side itself; a resolver's
//! word alone leaves a candidate. For a removed or moved definition the caller
//! passes the head's [`View`] with the base's [`Definition`]: a head occurrence
//! bound to another head definition is `not_ours`, one bound to nothing is a
//! candidate.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use super::{Basis, Case, Class, Entry, Occurrence, Outcome, Reason, scan};
use crate::facts::{
    DefKey, DefKind, Export, Fact, FileFacts, Import, Local, LocalKey, Name, Scope, Star,
};
use crate::resolve::{Binding, Definition, Relation, Resolution, UnresolvedReason};
use crate::snapshot::RepoPath;

#[derive(Default)]
struct FileBindings<'a> {
    /// By start and end byte of the binding's site.
    exact: HashMap<(u32, u32), Vec<&'a Binding>>,
    all: Vec<&'a Binding>,
}

/// One side's resolved snapshot, indexed for the questions an occurrence asks.
pub struct View<'a> {
    files: HashMap<RepoPath, &'a FileFacts>,
    bindings: HashMap<RepoPath, FileBindings<'a>>,
    definitions: HashSet<DefKey>,
    /// What each definition overrides, and what overrides each.
    bases: HashMap<DefKey, Vec<DefKey>>,
    overriders: HashMap<DefKey, Vec<DefKey>>,
    manifests_whole: bool,
}

impl<'a> View<'a> {
    /// `manifests_whole`: the workspace manifests of the side were read without
    /// a loss, which is what a name outside the project or on an external base
    /// rests on.
    pub fn new(
        files: &'a [FileFacts],
        bindings: &'a [Binding],
        definitions: &'a [Definition],
        manifests_whole: bool,
    ) -> Self {
        let by_path: HashMap<RepoPath, &'a FileFacts> =
            files.iter().map(|file| (file.path.clone(), file)).collect();
        let mut held: HashMap<RepoPath, FileBindings<'a>> = HashMap::new();
        let mut bases: HashMap<DefKey, Vec<DefKey>> = HashMap::new();
        let mut overriders: HashMap<DefKey, Vec<DefKey>> = HashMap::new();
        for binding in bindings {
            let at = held.entry(binding.file.clone()).or_default();
            at.exact
                .entry((binding.site.bytes.start, binding.site.bytes.end))
                .or_default()
                .push(binding);
            at.all.push(binding);
            if binding.relation != Relation::Overrides {
                continue;
            }
            let (Scope::Def(local), Resolution::Resolved { target, .. }) =
                (&binding.from, &binding.resolution)
            else {
                continue;
            };
            let Some(file) = by_path.get(&binding.file) else {
                continue;
            };
            let overrider = file.key(local);
            bases
                .entry(overrider.clone())
                .or_default()
                .push(target.clone());
            overriders
                .entry(target.clone())
                .or_default()
                .push(overrider);
        }
        Self {
            files: by_path,
            bindings: held,
            definitions: definitions.iter().map(|def| def.key.clone()).collect(),
            bases,
            overriders,
            manifests_whole,
        }
    }

    fn file(&self, path: &RepoPath) -> Option<&'a FileFacts> {
        self.files.get(path).copied()
    }

    /// The bindings whose site is exactly `bytes`.
    fn bound_at(&self, path: &RepoPath, bytes: &Range<u32>) -> Vec<&'a Binding> {
        self.bindings
            .get(path)
            .and_then(|held| held.exact.get(&(bytes.start, bytes.end)))
            .into_iter()
            .flatten()
            .copied()
            .collect()
    }

    /// The bindings whose site holds `bytes` and is wider than it.
    fn wider_than(&self, path: &RepoPath, bytes: &Range<u32>) -> Vec<&'a Binding> {
        self.bindings
            .get(path)
            .into_iter()
            .flat_map(|held| held.all.iter().copied())
            .filter(|binding| {
                let site = &binding.site.bytes;
                site.start <= bytes.start && bytes.end <= site.end && site != bytes
            })
            .collect()
    }

    /// The definitions `key` overrides or is overridden by, transitively, either
    /// way; not `key` itself.
    fn family(&self, key: &DefKey) -> HashSet<DefKey> {
        let mut found: HashSet<DefKey> = HashSet::new();
        for step in [&self.bases, &self.overriders] {
            let mut queue = vec![key.clone()];
            while let Some(at) = queue.pop() {
                for next in step.get(&at).into_iter().flatten() {
                    if found.insert(next.clone()) {
                        queue.push(next.clone());
                    }
                }
            }
        }
        found.remove(key);
        found
    }

    /// Whether the module file offers the definition on: it exports the name or
    /// a whole module, or a binding of it re-exports the definition.
    fn reexports(&self, module: &FileFacts, subject: &Definition) -> bool {
        let exported = module.blob.facts.iter().any(|fact| match fact {
            Fact::Export(Export::Named { name, .. }) => *name == subject.key.local.name,
            Fact::Export(Export::Star { .. }) => true,
            _ => false,
        });
        exported
            || self.bindings.get(&module.path).is_some_and(|held| {
                held.all.iter().any(|binding| {
                    binding.relation == Relation::Reexports
                        && matches!(&binding.resolution,
                            Resolution::Resolved { target, .. } if *target == subject.key)
                })
            })
    }

    /// Whether some file of the side is the module `module` names, holds it, or
    /// lies under it.
    fn in_repository(&self, module: &str) -> bool {
        let related = |outer: &str, inner: &str| {
            outer == inner
                || outer
                    .strip_prefix(inner)
                    .is_some_and(|rest| rest.starts_with(['.', '/', ':']))
        };
        self.files.values().any(|file| {
            let own = file.module.as_str();
            related(own, module) || related(module, own)
        })
    }
}

/// What the accounting found for the spellings of one definition's names in one
/// file.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct FileOutcomes {
    /// Every code occurrence, and every mention a binding explained, by start
    /// byte.
    pub entries: Vec<Entry>,
    /// Mentions nobody explained: counted, never listed.
    pub mentions: usize,
    /// The scan could not read the file whole; with no entry to say so, the
    /// caller lists the file as unread.
    pub lost: bool,
}

/// The relations an occurrence no binding names may be a use through, by the
/// kind of the definition.
pub const fn admitted(kind: DefKind) -> &'static [Relation] {
    match kind {
        DefKind::Function | DefKind::Method | DefKind::Constructor => {
            &[Relation::Calls, Relation::PassesAsValue]
        }
        DefKind::Class | DefKind::Struct => &[
            Relation::Instantiates,
            Relation::TypeUses,
            Relation::PassesAsValue,
        ],
        DefKind::Enum | DefKind::Trait | DefKind::Interface => &[Relation::TypeUses],
        DefKind::TypeAlias => &[Relation::TypeUses, Relation::Reads],
        DefKind::Object => &[Relation::Reads, Relation::TypeUses],
        DefKind::Property
        | DefKind::Constant
        | DefKind::Field
        | DefKind::EnumMember
        | DefKind::Variable => &[Relation::Reads, Relation::Writes],
        DefKind::Macro => &[Relation::Calls],
        DefKind::Module => &[Relation::Imports, Relation::Reexports],
    }
}

/// Whether the entry stands in the way of `relation` for a definition of `kind`:
/// its binding's own relation, else every relation the kind admits.
pub fn counts_against(entry: &Entry, kind: DefKind, relation: Relation) -> bool {
    match entry.relation {
        Some(own) => own == relation,
        None => admitted(kind).contains(&relation),
    }
}

/// An outcome with the basis it rests on and the relation it counts against.
struct Claim {
    relation: Option<Relation>,
    outcome: Outcome,
    basis: Basis,
}

impl Claim {
    fn candidate(relation: Option<Relation>, reason: Reason) -> Self {
        Self {
            relation,
            outcome: Outcome::Candidate(reason),
            basis: Basis::Unproven,
        }
    }

    fn of(relation: Option<Relation>, outcome: Outcome, basis: Basis) -> Self {
        Self {
            relation,
            outcome,
            basis,
        }
    }

    fn conflict() -> Self {
        Self::candidate(None, Reason::Conflict)
    }

    fn is_ours(&self) -> bool {
        matches!(self.outcome, Outcome::Ours)
    }

    fn is_candidate(&self) -> bool {
        matches!(self.outcome, Outcome::Candidate(_))
    }
}

/// The one claim several claims on one span come to. A candidate is a hedge, not
/// a different answer: an `ours` stays `ours` beside it (the resolver pairs a
/// call through `self` with the overriders that may run), and a `not_ours`
/// beside it is a candidate. Two definite outcomes that differ are a defect of
/// the resolver and read as a candidate.
fn settle(mut claims: Vec<Claim>) -> Claim {
    let ours = claims.iter().position(Claim::is_ours);
    let candidate = claims.iter().position(Claim::is_candidate);
    let others = claims
        .iter()
        .any(|claim| !claim.is_ours() && !claim.is_candidate());
    if let Some(at) = ours {
        return if others {
            Claim::conflict()
        } else {
            claims.swap_remove(at)
        };
    }
    if let Some(at) = candidate {
        return claims.swap_remove(at);
    }
    let alike = claims
        .windows(2)
        .all(|pair| pair[0].outcome.as_str() == pair[1].outcome.as_str());
    if alike && !claims.is_empty() {
        claims.swap_remove(0)
    } else {
        Claim::conflict()
    }
}

/// A place that declares something with the name.
struct Declared {
    basis: Basis,
    /// The place declares the definition itself.
    subject: bool,
}

fn same(name: &str, spelled: &[u8], case: Case) -> bool {
    match case {
        Case::Sensitive => name.as_bytes() == spelled,
        Case::Insensitive => name.as_bytes().eq_ignore_ascii_case(spelled),
    }
}

/// The definitions a scope can name, outermost first.
fn path_of(scope: &Scope) -> Vec<&Name> {
    match scope {
        Scope::Module => Vec::new(),
        Scope::Def(key) => key
            .container
            .iter()
            .chain(std::iter::once(&key.name))
            .collect(),
        Scope::Callback { parent, .. } => path_of(parent),
    }
}

/// Whether `inner` is `outer` or a closure that sits inside it.
fn within_callback(outer: &Scope, inner: &Scope) -> bool {
    let mut at = inner;
    loop {
        if at == outer {
            return true;
        }
        match at {
            Scope::Callback { parent, .. } => at = parent,
            Scope::Def(_) | Scope::Module => return false,
        }
    }
}

/// What reads one file: the side's view, the definition accounted, and the
/// file's own declarations, locals and imports.
struct Reading<'a> {
    view: &'a View<'a>,
    file: &'a FileFacts,
    text: &'a [u8],
    subject: &'a Definition,
    case: Case,
    family: HashSet<DefKey>,
    declared: HashMap<(u32, u32), Declared>,
    locals: Vec<&'a Local>,
    imports: Vec<&'a Import>,
}

impl<'a> Reading<'a> {
    fn new(
        view: &'a View<'a>,
        file: &'a FileFacts,
        text: &'a [u8],
        subject: &'a Definition,
    ) -> Self {
        let mut declared: HashMap<(u32, u32), Declared> = HashMap::new();
        let mut locals = Vec::new();
        let mut imports = Vec::new();
        for fact in &file.blob.facts {
            if let Fact::Definition(def) = fact {
                let at = declared
                    .entry((def.name_span.bytes.start, def.name_span.bytes.end))
                    .or_insert(Declared {
                        basis: Basis::DefinitionName,
                        subject: false,
                    });
                at.subject |= file.key(&def.local) == subject.key;
            } else if let Fact::LocalBinding(local) = fact {
                declared
                    .entry((local.site.bytes.start, local.site.bytes.end))
                    .or_insert(Declared {
                        basis: Basis::LocalSite,
                        subject: false,
                    });
                locals.push(local);
            } else if let Fact::Import(import) = fact {
                imports.push(import);
            }
        }
        Self {
            view,
            file,
            text,
            subject,
            case: Case::of(file.language()),
            family: view.family(&subject.key),
            declared,
            locals,
            imports,
        }
    }

    fn spelled(&self, occurrence: &Occurrence) -> &'a [u8] {
        &self.text[occurrence.bytes.start as usize..occurrence.bytes.end as usize]
    }

    /// The claims of the bindings at exactly the occurrence's span that name it.
    fn claims(&self, occurrence: &Occurrence) -> Vec<Claim> {
        let spelled = self.spelled(occurrence);
        self.view
            .bound_at(&occurrence.file, &occurrence.bytes)
            .into_iter()
            .filter(|binding| same(binding.name.as_str(), spelled, self.case))
            .map(|binding| self.claim(binding, spelled))
            .collect()
    }

    fn claim(&self, binding: &Binding, spelled: &[u8]) -> Claim {
        let relation = Some(binding.relation);
        match &binding.resolution {
            Resolution::Resolved { target, .. } => self.resolved(target, relation),
            Resolution::Module { file, .. } => self.module(file, relation),
            Resolution::Candidate { reason, .. } => {
                Claim::candidate(relation, Reason::Resolver(*reason))
            }
            Resolution::Unresolved { reason } => self.unresolved(*reason, binding, spelled),
        }
    }

    fn resolved(&self, target: &DefKey, relation: Option<Relation>) -> Claim {
        if *target == self.subject.key {
            return Claim::of(relation, Outcome::Ours, Basis::Resolved);
        }
        if self.family.contains(target) {
            return Claim::candidate(relation, Reason::Dispatch);
        }
        if self.view.definitions.contains(target) {
            return Claim::of(relation, Outcome::NotOurs, Basis::OtherDefinition);
        }
        Claim::candidate(relation, Reason::BasisUnverified)
    }

    fn module(&self, module: &RepoPath, relation: Option<Relation>) -> Claim {
        let found = self.view.file(module);
        let own = *module == self.subject.file
            || found.is_some_and(|file| self.is_own_module(file))
            || found.is_some_and(|file| self.view.reexports(file, self.subject));
        if own {
            return Claim::of(relation, Outcome::Ours, Basis::OwnModule);
        }
        match found {
            Some(file) if file.blob.losses.is_empty() => {
                Claim::of(relation, Outcome::NotOurs, Basis::OtherModule)
            }
            Some(_) | None => Claim::candidate(relation, Reason::BasisUnverified),
        }
    }

    fn is_own_module(&self, file: &FileFacts) -> bool {
        let key = &self.subject.key;
        file.module == key.module && file.language() == key.language && file.project == key.project
    }

    fn unresolved(&self, reason: UnresolvedReason, binding: &Binding, spelled: &[u8]) -> Claim {
        let relation = Some(binding.relation);
        let verified = |basis: Basis, held: bool| {
            if held {
                Claim::of(relation, Outcome::NotOurs, basis)
            } else {
                Claim::candidate(relation, Reason::BasisUnverified)
            }
        };
        match reason {
            UnresolvedReason::Local => {
                verified(Basis::Local, self.shadowed(spelled, &binding.from))
            }
            UnresolvedReason::Builtin => verified(
                Basis::Builtin,
                !self.imported(spelled) && !self.own_top_level(),
            ),
            UnresolvedReason::External => {
                verified(Basis::ExternalModule, self.imported_from_outside(spelled))
            }
            UnresolvedReason::OutsideProject | UnresolvedReason::ExternalBase => {
                verified(Basis::Manifests, self.view.manifests_whole)
            }
            UnresolvedReason::NoMember
            | UnresolvedReason::NoBinding
            | UnresolvedReason::UntypedRead
            | UnresolvedReason::NoCandidates
            | UnresolvedReason::TooManyCandidates
            | UnresolvedReason::MissingModule
            | UnresolvedReason::Opaque
            | UnresolvedReason::NoMatch => Claim::candidate(relation, Reason::Unresolved(reason)),
        }
    }

    /// A local or parameter of the name in a scope around the occurrence's.
    fn shadowed(&self, spelled: &[u8], from: &Scope) -> bool {
        self.locals.iter().any(|local| {
            same(local.name.as_str(), spelled, self.case) && self.encloses(&local.enclosing, from)
        })
    }

    /// Whether names bound in `outer` are in force in `inner`. A type's own
    /// scope holds its members, which its methods do not see unqualified.
    fn encloses(&self, outer: &Scope, inner: &Scope) -> bool {
        match outer {
            Scope::Module => true,
            Scope::Def(key) => {
                let own = path_of(outer);
                let at = path_of(inner);
                if self.is_type_scope(key) {
                    own == at
                } else {
                    at.starts_with(&own)
                }
            }
            Scope::Callback { .. } => within_callback(outer, inner),
        }
    }

    fn is_type_scope(&self, key: &LocalKey) -> bool {
        self.file.defs().any(|def| {
            def.local == *key
                && matches!(
                    def.kind,
                    DefKind::Class
                        | DefKind::Struct
                        | DefKind::Enum
                        | DefKind::Trait
                        | DefKind::Interface
                        | DefKind::Object
                )
        })
    }

    /// Whether an import of the file may bind the name; a glob may bind any.
    fn imported(&self, spelled: &[u8]) -> bool {
        self.imports
            .iter()
            .any(|import| self.binds(import, spelled, true))
    }

    fn binds(&self, import: &Import, spelled: &[u8], glob: bool) -> bool {
        let named = |name: &Name| same(name.as_str(), spelled, self.case);
        let listed = import
            .names
            .iter()
            .any(|(name, alias)| named(alias.as_ref().unwrap_or(name)));
        let default = import.default.as_ref().is_some_and(|name| named(name));
        let star = match &import.star {
            Star::Glob => glob,
            Star::Alias(alias) => named(alias),
            Star::None => false,
        };
        let head = import.names.is_empty()
            && import.default.is_none()
            && !import.star.is_star()
            && !import.module.as_str().starts_with(['.', '/'])
            && import
                .module
                .as_str()
                .split(['.', '/', ':'])
                .next()
                .is_some_and(|head| same(head, spelled, self.case));
        listed || default || star || head
    }

    /// Whether an import of the file binds the name to a module the repository
    /// does not hold.
    fn imported_from_outside(&self, spelled: &[u8]) -> bool {
        self.imports.iter().any(|import| {
            let module = import.module.as_str();
            import.relative == 0
                && !module.is_empty()
                && !module.starts_with(['.', '/'])
                && self.binds(import, spelled, false)
                && !self.view.in_repository(module)
        })
    }

    /// Whether the definition is a top-level definition of the file's own module.
    fn own_top_level(&self) -> bool {
        self.subject.key.local.container.is_empty() && self.is_own_module(self.file)
    }

    fn entry(&self, occurrence: Occurrence) -> Entry {
        let at = (occurrence.bytes.start, occurrence.bytes.end);
        let declared = self.declared.get(&at);
        if declared.is_some_and(|place| place.subject) {
            return finish(
                occurrence,
                Claim::of(None, Outcome::Declaration, Basis::DefinitionName),
            );
        }
        let claims = self.claims(&occurrence);
        if claims.is_empty() {
            if let Some(place) = declared {
                return finish(
                    occurrence,
                    Claim::of(None, Outcome::Declaration, place.basis),
                );
            }
            let wide = self.view.wider_than(&occurrence.file, &occurrence.bytes);
            let claim = self.unbound(&occurrence, &wide);
            return finish(occurrence, claim);
        }
        let mut claim = settle(claims);
        if let Some(place) = declared {
            if matches!(claim.outcome, Outcome::NotOurs) {
                claim = Claim::of(None, Outcome::Declaration, place.basis);
            }
        }
        finish(occurrence, claim)
    }

    /// A spelling no binding is at: wider bindings of its name hold it, or none.
    fn unbound(&self, occurrence: &Occurrence, wide: &[&Binding]) -> Claim {
        let spelled = self.spelled(occurrence);
        let named: Vec<&&Binding> = wide
            .iter()
            .filter(|binding| same(binding.name.as_str(), spelled, self.case))
            .collect();
        match named.split_first() {
            None => Claim::candidate(None, Reason::Unexplained),
            Some((first, rest)) => {
                let relation = first.relation;
                let alike = rest.iter().all(|binding| binding.relation == relation);
                Claim::candidate(alike.then_some(relation), Reason::WideBinding)
            }
        }
    }

    /// A mention is an entry only when a binding at its span explains it: it is
    /// then code.
    fn explained(&self, mention: Occurrence) -> Option<Entry> {
        let claims = self.claims(&mention);
        if claims.is_empty() {
            return None;
        }
        let occurrence = Occurrence {
            class: Class::Code,
            ..mention
        };
        Some(finish(occurrence, settle(claims)))
    }
}

fn finish(occurrence: Occurrence, claim: Claim) -> Entry {
    Entry {
        occurrence,
        relation: claim.relation,
        outcome: claim.outcome,
        basis: claim.basis,
    }
}

/// The outcome of each spelling of `names` in `file` (read from `text`, the
/// bytes of the blob the facts came from) for the definition `subject`, in the
/// side `view` holds. A file the scan could not trust gives every spelling as a
/// candidate.
pub fn outcomes(
    view: &View<'_>,
    file: &FileFacts,
    text: &[u8],
    subject: &Definition,
    names: &[&str],
) -> FileOutcomes {
    let found = scan(file, text, names, Case::of(file.language()));
    let mut done = FileOutcomes {
        entries: Vec::new(),
        mentions: 0,
        lost: found.lost,
    };
    if found.lost {
        done.entries = found
            .code
            .into_iter()
            .map(|occurrence| finish(occurrence, Claim::candidate(None, Reason::FileLost)))
            .collect();
        return done;
    }
    let reading = Reading::new(view, file, text, subject);
    for occurrence in found.code {
        done.entries.push(reading.entry(occurrence));
    }
    for mention in found.mentions {
        match reading.explained(mention) {
            Some(entry) => done.entries.push(entry),
            None => done.mentions += 1,
        }
    }
    done.entries
        .sort_by_key(|entry| (entry.occurrence.bytes.start, entry.occurrence.bytes.end));
    done
}
