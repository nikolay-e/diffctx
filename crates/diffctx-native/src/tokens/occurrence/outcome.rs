//! The outcome of each occurrence of a definition's names in one file of one
//! side. A binding claims an occurrence only at its exact span and name; a
//! `not_ours` stands only on a basis the facts of the side confirm; the rest is a
//! candidate. For a removed definition the caller passes the head's [`View`] with
//! the base's [`Definition`].

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use super::rows::{
    BARE_HEADS, CLASS_CANDIDATES, COMMAND_ARGUMENTS, DECLARING_LOCALS, IMPL_HEADS,
    KEYWORD_ARGUMENTS, MODULE_PATHS, NAMED_LOCALS, OFFERING_MODULES, PACKAGE_HEADS,
    PAIRED_PROTOTYPES, PATH_SEPARATORS, SCOPED_LOCALS,
};
use super::scanner::dollar_joins;
use super::{Alias, AliasScope, Basis, Case, Class, Entry, Occurrence, Outcome, Reason, scan};
use crate::decide::Standing;
use crate::facts::{
    DefKey, DefKind, Export, Fact, FileFacts, Import, Local, LocalKind, Name, Scope, Star, UseKind,
};
use crate::resolve::{
    Binding, CandidateReason, DYNAMIC, Definition, Relation, Resolution, UnresolvedReason,
};
use crate::snapshot::RepoPath;

/// What one side's resolved snapshot is indexed by, as positions in its files and
/// bindings: built once for a run, so that every definition's [`View`] borrows it.
pub struct Index {
    files: HashMap<RepoPath, usize>,
    by_file: HashMap<RepoPath, Vec<usize>>,
    exact: HashMap<(RepoPath, u32, u32), Vec<usize>>,
    definitions: HashSet<DefKey>,
    /// The lower-cased names of the definitions that are members of a class, and
    /// how many are so named.
    members: HashMap<String, usize>,
    /// The definitions that state a function without its body.
    prototypes: HashSet<DefKey>,
    bases: HashMap<DefKey, Vec<DefKey>>,
    overriders: HashMap<DefKey, Vec<DefKey>>,
    /// The bindings of a use that spells no name (`$o->$m()`).
    unnamed: Vec<usize>,
    manifests_whole: bool,
}

impl Index {
    /// `manifests_whole`: the workspace manifests were read without a loss.
    pub fn new(
        files: &[FileFacts],
        bindings: &[Binding],
        definitions: &[Definition],
        manifests_whole: bool,
    ) -> Self {
        let mut index = Self {
            files: (files.iter().enumerate())
                .map(|(at, file)| (file.path.clone(), at))
                .collect(),
            by_file: HashMap::new(),
            exact: HashMap::new(),
            definitions: definitions.iter().map(|def| def.key.clone()).collect(),
            members: HashMap::new(),
            prototypes: (files.iter())
                .flat_map(|file| {
                    file.defs()
                        .filter(|def| def.body_hash == 0)
                        .map(|def| file.key(&def.local))
                })
                .collect(),
            bases: HashMap::new(),
            overriders: HashMap::new(),
            unnamed: Vec::new(),
            manifests_whole,
        };
        for def in definitions
            .iter()
            .filter(|def| !def.key.local.container.is_empty())
        {
            *(index.members)
                .entry(def.key.local.name.as_str().to_ascii_lowercase())
                .or_default() += 1;
        }
        for (at, binding) in bindings.iter().enumerate() {
            let bytes = &binding.site.bytes;
            let file = &binding.file;
            index.by_file.entry(file.clone()).or_default().push(at);
            if binding.name.as_str() == DYNAMIC {
                index.unnamed.push(at);
            }
            index
                .exact
                .entry((file.clone(), bytes.start, bytes.end))
                .or_default()
                .push(at);
            let (Scope::Def(local), Resolution::Resolved { target, .. }) =
                (&binding.from, &binding.resolution)
            else {
                continue;
            };
            let Some(&home) = index.files.get(file) else {
                continue;
            };
            if binding.relation == Relation::Overrides {
                index.overrides(files[home].key(local), target.clone());
            }
        }
        index
    }

    fn overrides(&mut self, overrider: DefKey, target: DefKey) {
        self.bases
            .entry(overrider.clone())
            .or_default()
            .push(target.clone());
        self.overriders.entry(target).or_default().push(overrider);
    }
}

/// One side's resolved snapshot, as the questions an occurrence asks see it.
pub struct View<'a> {
    index: Rc<Index>,
    files: &'a [FileFacts],
    bindings: &'a [Binding],
    family_unproven: bool,
    /// Where the base resolved a use to a removed definition, and how sure it was.
    stale: HashMap<Place, Standing>,
}

/// Where a use is written, free of byte offsets: a changed file's differ by side.
type Place = (RepoPath, Vec<Name>, Name);

fn place(binding: &Binding) -> Place {
    let owners = path_of(&binding.from).into_iter().cloned().collect();
    (binding.file.clone(), owners, binding.name.clone())
}

impl<'a> View<'a> {
    pub fn new(
        files: &'a [FileFacts],
        bindings: &'a [Binding],
        definitions: &'a [Definition],
        manifests_whole: bool,
    ) -> Self {
        let index = Index::new(files, bindings, definitions, manifests_whole);
        Self::over(Rc::new(index), files, bindings)
    }

    /// The view of what `index` was built from.
    pub fn over(index: Rc<Index>, files: &'a [FileFacts], bindings: &'a [Binding]) -> Self {
        Self {
            index,
            files,
            bindings,
            family_unproven: false,
            stale: HashMap::new(),
        }
    }

    /// The uses that spell no name and whose relation reaches a definition of `kind`:
    /// each may be a use of any such definition.
    pub fn unnamed(&self, kind: DefKind) -> impl Iterator<Item = &'a Binding> + '_ {
        let reaches = admitted(kind);
        self.picked(Some(&self.index.unnamed))
            .filter(move |binding| reaches.contains(&binding.relation))
    }

    /// Whether the workspace manifests were read without a loss.
    pub fn manifests_whole(&self) -> bool {
        self.index.manifests_whole
    }

    fn file(&self, path: &RepoPath) -> Option<&'a FileFacts> {
        let files = self.files;
        self.index.files.get(path).map(|&at| &files[at])
    }

    /// The bindings written in `path`.
    fn in_file<'s>(&'s self, path: &RepoPath) -> impl Iterator<Item = &'a Binding> + 's {
        self.picked(self.index.by_file.get(path))
    }

    /// The bindings whose site is exactly this span.
    fn at<'s>(&'s self, span: &(RepoPath, u32, u32)) -> impl Iterator<Item = &'a Binding> + 's {
        self.picked(self.index.exact.get(span))
    }

    fn picked<'s>(&'s self, at: Option<&'s Vec<usize>>) -> impl Iterator<Item = &'a Binding> + 's {
        let bindings = self.bindings;
        at.into_iter().flatten().map(move |&at| &bindings[at])
    }

    /// A removed member's family is what the base side bound at the places it resolved
    /// a use to it (no `@Override` needed); with no base read, a call resolved to a
    /// same-named member is a candidate.
    pub fn removed_sites(
        mut self,
        base: Option<&[Binding]>,
        removed: &[DefKey],
        standing: impl Fn(&Binding) -> Standing,
    ) -> Self {
        match base {
            Some(bindings) => {
                let named = |binding: &&Binding| {
                    (binding.targets().into_iter()).any(|target| removed.contains(target))
                };
                for binding in bindings.iter().filter(named) {
                    let standing = standing(binding);
                    (self.stale.entry(place(binding)))
                        .and_modify(|known| {
                            if standing == Standing::Resolved {
                                *known = standing;
                            }
                        })
                        .or_insert(standing);
                }
            }
            None => self.family_unproven = true,
        }
        self
    }

    /// The head's bindings at the places the base resolved a use to a removed
    /// definition, with how sure the base was: whatever the head lands them on, they
    /// are the uses it had.
    pub fn stranded(&self) -> impl Iterator<Item = (&'a Binding, Standing)> + '_ {
        let files: HashSet<&RepoPath> = self.stale.keys().map(|(file, ..)| file).collect();
        (files.into_iter().flat_map(|file| self.in_file(file)))
            .filter_map(|binding| Some((binding, *self.stale.get(&place(binding))?)))
    }

    /// What `key` overrides or is overridden by, transitively; not `key`.
    fn family(&self, key: &DefKey) -> HashSet<DefKey> {
        let mut found = HashSet::new();
        for step in [&self.index.bases, &self.index.overriders] {
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

    fn reexports(&self, module: &FileFacts, subject: &Definition) -> bool {
        module.blob.facts.iter().any(|fact| match fact {
            Fact::Export(Export::Named { name, .. }) => *name == subject.key.local.name,
            Fact::Export(Export::Star { .. }) => true,
            _ => false,
        }) || self.in_file(&module.path).any(|binding| {
            binding.relation == Relation::Reexports
                && matches!(&binding.resolution,
                    Resolution::Resolved { target, .. } if *target == subject.key)
        })
    }
}

/// What the accounting found for one definition's names in one file.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FileOutcomes {
    /// Code occurrences, and mentions a binding explained, by start byte.
    pub entries: Vec<Entry>,
    /// Mentions nobody explained: counted, never listed.
    pub mentions: usize,
    /// The scan could not read the file whole; with no entry to say so, the
    /// caller lists the file as unread.
    pub lost: bool,
}

/// The relations an occurrence no binding names may be a use through.
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

/// Whether a name may be written through to the subject (`global`): its kind admits writes.
fn writable(subject: &Definition) -> bool {
    admitted(subject.kind).contains(&Relation::Writes)
}

/// Whether the entry stands in the way of `relation`: its binding's own relation,
/// else every relation the kind admits.
pub fn counts_against(entry: &Entry, kind: DefKind, relation: Relation) -> bool {
    entry
        .relation
        .map_or_else(|| admitted(kind).contains(&relation), |own| own == relation)
}

struct Claim {
    relation: Option<Relation>,
    outcome: Outcome,
    basis: Basis,
}

impl Claim {
    fn new(relation: Option<Relation>, outcome: Outcome, basis: Basis) -> Self {
        Self {
            relation,
            outcome,
            basis,
        }
    }

    fn candidate(relation: Option<Relation>, reason: Reason) -> Self {
        Self::new(relation, Outcome::Candidate(reason), Basis::Unproven)
    }

    fn at(self, occurrence: Occurrence) -> Entry {
        Entry {
            occurrence,
            relation: self.relation,
            outcome: self.outcome,
            basis: self.basis,
        }
    }
}

/// The one claim of several on one span. A candidate is a hedge: an `ours` stays
/// `ours` beside it (the resolver pairs a call through `self` with the overriders
/// that may run), a `not_ours` beside it is a candidate. A reference bound to
/// another definition under another relation says nothing against one that
/// resolves to the subject (`S { a }` writes the field `a` and reads the binding
/// `a`); the same relation bound to two definitions is a disagreement. Other
/// definite outcomes that differ are a defect of the resolver and read as a
/// candidate.
fn settle(mut claims: Vec<Claim>) -> Claim {
    let ours = claims
        .iter()
        .position(|c| matches!(c.outcome, Outcome::Ours));
    let hedge = claims
        .iter()
        .position(|c| matches!(c.outcome, Outcome::Candidate(_)));
    let elsewhere = |c: &Claim| {
        matches!(c.outcome, Outcome::NotOurs)
            && c.basis == Basis::OtherDefinition
            && ours.is_some_and(|at| claims[at].relation != c.relation)
    };
    let other = claims
        .iter()
        .any(|c| !matches!(c.outcome, Outcome::Ours | Outcome::Candidate(_)) && !elsewhere(c));
    let pick = match (ours, hedge, other) {
        (Some(_), _, true) => None,
        (Some(at), _, false) | (None, Some(at), _) => Some(at),
        (None, None, _) => claims
            .windows(2)
            .all(|pair| pair[0].outcome.as_str() == pair[1].outcome.as_str())
            .then_some(0),
    };
    pick.map_or_else(
        || Claim::candidate(None, Reason::Conflict),
        |at| claims.swap_remove(at),
    )
}

fn split_words(text: &[u8]) -> Vec<&[u8]> {
    (text.split(|b| !(b.is_ascii_alphanumeric() || *b == b'_')))
        .filter(|word| !word.is_empty())
        .collect()
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

/// Whether names bound in `outer` are in force in `inner`. A closure's names
/// reach only the closure itself.
fn encloses(outer: &Scope, inner: &Scope) -> bool {
    match outer {
        Scope::Module => true,
        Scope::Def(_) => path_of(inner).starts_with(&path_of(outer)),
        Scope::Callback { .. } => inner == outer,
    }
}

struct Reading<'a> {
    view: &'a View<'a>,
    file: &'a FileFacts,
    text: &'a [u8],
    subject: &'a Definition,
    case: Case,
    family: HashSet<DefKey>,
    /// The span of each name the file declares: how, and whether it is the
    /// subject's own.
    declared: HashMap<(u32, u32), (Basis, bool)>,
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
        let mut declared = HashMap::new();
        let mut locals = Vec::new();
        let mut imports = Vec::new();
        let named = NAMED_LOCALS
            .iter()
            .any(|(family, _)| family.has(file.language()));
        for fact in &file.blob.facts {
            match fact {
                Fact::Definition(def) => {
                    let name = &def.name_span.bytes;
                    let at = declared
                        .entry((name.start, name.end))
                        .or_insert((Basis::DefinitionName, false));
                    at.1 |= file.key(&def.local) == subject.key;
                }
                Fact::LocalBinding(local) => {
                    // A parameter's span is surely no use, and a declarator's where its
                    // language's row says so. A value a name may be written through
                    // (`global`) keeps its hedge at a local of its own name.
                    let declares = local.kind == LocalKind::Parameter
                        || (!writable(subject)
                            && (DECLARING_LOCALS.iter().any(|(family, kind)| {
                                *kind == local.kind && family.has(file.language())
                            }) || (local.kind == LocalKind::Variable && named)));
                    if declares {
                        let site = &local.site.bytes;
                        let sigil = !dollar_joins(file.language())
                            && text.get(site.start as usize) == Some(&b'$');
                        declared
                            .entry((site.start + u32::from(sigil), site.end))
                            .or_insert((Basis::LocalSite, false));
                    }
                    locals.push(local);
                }
                Fact::Import(import) => imports.push(import),
                _ => {}
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

    fn claims(&self, occurrence: &Occurrence) -> Vec<Claim> {
        let spelled = self.spelled(occurrence);
        let at = (
            occurrence.file.clone(),
            occurrence.bytes.start,
            occurrence.bytes.end,
        );
        self.view
            .at(&at)
            .filter(|binding| same(binding.name.as_str(), spelled, self.case))
            .map(|binding| self.claim(binding, spelled))
            .collect()
    }

    fn claim(&self, binding: &Binding, spelled: &[u8]) -> Claim {
        let relation = Some(binding.relation);
        match &binding.resolution {
            Resolution::Resolved { target, .. } => self.resolved(target, binding),
            Resolution::Module { file, .. } => self.module(file, relation),
            Resolution::Candidate { targets, reason } => self.candidate(targets, *reason, relation),
            Resolution::Unresolved { reason } => self.unresolved(*reason, binding, spelled),
        }
    }

    /// A head definition of the class the candidates were read from, left out of them and
    /// no override of one, is not called; any other subject may be.
    fn candidate(
        &self,
        targets: &[DefKey],
        reason: CandidateReason,
        relation: Option<Relation>,
    ) -> Claim {
        let subject = &self.subject.key;
        let class = |a: &DefKey, b: &DefKey| {
            a.project == b.project && a.module == b.module && a.local.container == b.local.container
        };
        let left_out = CLASS_CANDIDATES
            .iter()
            .any(|(family, kind)| *kind == reason && family.has(self.file.language()))
            && self.view.index.definitions.contains(subject)
            && targets.iter().any(|key| class(key, subject))
            && targets.iter().all(|key| {
                key != subject
                    && !self.family.contains(key)
                    && self.view.index.definitions.contains(key)
            });
        if left_out {
            Claim::new(relation, Outcome::NotOurs, Basis::OtherDefinition)
        } else {
            Claim::candidate(relation, Reason::Resolver(reason))
        }
    }

    fn resolved(&self, target: &DefKey, binding: &Binding) -> Claim {
        let relation = Some(binding.relation);
        if *target == self.subject.key || self.paired(target) {
            Claim::new(relation, Outcome::Ours, Basis::Resolved)
        } else if binding.relation != Relation::Overrides
            && (self.family.contains(target) || self.view.stale.contains_key(&place(binding)))
        {
            Claim::candidate(relation, Reason::Dispatch)
        } else if self.view.family_unproven
            && target.local.name == self.subject.key.local.name
            && !self.view.index.definitions.contains(&self.subject.key)
        {
            Claim::candidate(relation, Reason::OverrideUnproven)
        } else if self.view.index.definitions.contains(target) {
            Claim::new(relation, Outcome::NotOurs, Basis::OtherDefinition)
        } else {
            Claim::candidate(relation, Reason::BasisUnverified)
        }
    }

    /// Whether `target` and the subject are a prototype and its definition.
    fn paired(&self, target: &DefKey) -> bool {
        let (subject, prototypes) = (&self.subject.key, &self.view.index.prototypes);
        PAIRED_PROTOTYPES
            .iter()
            .any(|f| f.has(self.file.language()))
            && target.project == subject.project
            && target.language == subject.language
            && target.local == subject.local
            && (prototypes.contains(target) || prototypes.contains(subject))
    }

    fn module(&self, module: &RepoPath, relation: Option<Relation>) -> Claim {
        let found = self.view.file(module);
        let ours = *module == self.subject.file
            || found.is_some_and(|file| {
                self.is_own_module(file) || self.view.reexports(file, self.subject)
            });
        match found {
            _ if ours => Claim::new(relation, Outcome::Ours, Basis::OwnModule),
            Some(file) if file.blob.losses.is_empty() => {
                Claim::new(relation, Outcome::NotOurs, Basis::OtherModule)
            }
            _ => Claim::candidate(relation, Reason::BasisUnverified),
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
                Claim::new(relation, Outcome::NotOurs, basis)
            } else {
                Claim::candidate(relation, Reason::BasisUnverified)
            }
        };
        match reason {
            UnresolvedReason::Local => verified(
                Basis::Local,
                self.locals.iter().any(|local| {
                    same(local.name.as_str(), spelled, self.case)
                        && encloses(&local.enclosing, &binding.from)
                }),
            ),
            UnresolvedReason::Builtin => verified(
                Basis::Builtin,
                !self
                    .imports
                    .iter()
                    .any(|import| self.binds(import, spelled))
                    && !(self.subject.key.local.container.is_empty()
                        && self.is_own_module(self.file)),
            ),
            UnresolvedReason::External => {
                let named: Vec<&Import> = (self.imports.iter())
                    .filter(|import| self.binds(import, spelled))
                    .copied()
                    .collect();
                let imported = named.iter().any(|import| {
                    import.relative == 0
                        && !(self.view.files.iter()).any(|file| file.module == import.module)
                });
                // With no import of the name, an external call is no member of any
                // class of the side: a same-named member anywhere but the subject
                // keeps the candidate.
                let lower = String::from_utf8_lossy(spelled).to_ascii_lowercase();
                let own = usize::from(
                    !self.subject.key.local.container.is_empty()
                        && self
                            .subject
                            .key
                            .local
                            .name
                            .as_str()
                            .eq_ignore_ascii_case(&lower),
                );
                let others =
                    (self.view.index.members.get(&lower)).map_or(0, |n| n.saturating_sub(own));
                let bare = named.is_empty() && others == 0;
                let basis = if imported {
                    Basis::ExternalModule
                } else {
                    Basis::ExternalName
                };
                // An override names its base by its declaration, whatever else shares the name.
                verified(
                    basis,
                    imported || bare || binding.relation == Relation::Overrides,
                )
            }
            UnresolvedReason::OutsideProject | UnresolvedReason::ExternalBase => {
                verified(Basis::Manifests, self.view.index.manifests_whole)
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

    /// Whether the import may bind the name; a glob may bind any.
    fn binds(&self, import: &Import, spelled: &[u8]) -> bool {
        let named = |name: &Name| same(name.as_str(), spelled, self.case);
        import
            .names
            .iter()
            .any(|(name, alias)| named(alias.as_ref().unwrap_or(name)))
            || import.default.as_ref().is_some_and(|name| named(name))
            || match &import.star {
                Star::Glob => true,
                Star::Alias(alias) => named(alias),
                Star::None => false,
            }
    }

    fn entry(&self, occurrence: Occurrence) -> Entry {
        let declared = self
            .declared
            .get(&(occurrence.bytes.start, occurrence.bytes.end))
            .copied();
        if declared.is_some_and(|(_, own)| own) {
            let own = Claim::new(None, Outcome::Declaration, Basis::DefinitionName);
            return own.at(occurrence);
        }
        let claims = self.claims(&occurrence);
        let claim = match declared {
            Some((basis, _)) if claims.is_empty() => Claim::new(None, Outcome::Declaration, basis),
            _ if claims.is_empty() => self.unbound(&occurrence),
            _ => self.settled(&occurrence, claims),
        };
        // A declaration needs no basis, so it is the better statement of a
        // `not_ours` at the place that declares.
        match declared {
            Some((basis, _)) if matches!(claim.outcome, Outcome::NotOurs) => {
                Claim::new(None, Outcome::Declaration, basis).at(occurrence)
            }
            _ => claim.at(occurrence),
        }
    }

    /// The claim of the bindings at a span. Bound only to other definitions, a span that is also
    /// a plain read of a local of its name is that read (`S { a }` after `let a`).
    fn settled(&self, occurrence: &Occurrence, claims: Vec<Claim>) -> Claim {
        let bound_elsewhere = claims
            .iter()
            .all(|c| matches!(c.outcome, Outcome::NotOurs) && c.basis == Basis::OtherDefinition);
        let local = bound_elsewhere
            .then(|| self.plain_read(occurrence, self.spelled(occurrence)))
            .flatten();
        local.map_or_else(
            || settle(claims),
            |basis| Claim::new(None, Outcome::NotOurs, basis),
        )
    }

    /// No binding at the span: a basis shows it is no use of the subject, a wider
    /// binding of the name holds it, or none does.
    fn unbound(&self, occurrence: &Occurrence) -> Claim {
        let spelled = self.spelled(occurrence);
        let bytes = &occurrence.bytes;
        if let Some(claim) = self
            .impl_type(occurrence)
            .or_else(|| self.keyword_field(occurrence))
            .or_else(|| self.scoped_site(occurrence, spelled))
        {
            return claim;
        }
        if let Some(basis) = self
            .bare_head(occurrence, spelled)
            .or_else(|| self.module_path(occurrence))
            .or_else(|| self.package_clause(occurrence))
            .or_else(|| self.path_qualifier(occurrence))
            .or_else(|| self.plain_read(occurrence, spelled))
            .or_else(|| self.command_argument(occurrence))
        {
            return Claim::new(None, Outcome::NotOurs, basis);
        }
        let wide = self.view.in_file(&occurrence.file).find(|binding| {
            let site = &binding.site.bytes;
            site.start <= bytes.start
                && bytes.end <= site.end
                && same(binding.name.as_str(), spelled, self.case)
        });
        self.renamed(occurrence, spelled, wide).unwrap_or_else(|| {
            wide.map_or_else(
                || Claim::candidate(None, Reason::Unexplained),
                |binding| Claim::candidate(Some(binding.relation), Reason::WideBinding),
            )
        })
    }

    /// In an import that renames (`import f as g`): `f`, when the import is the subject's, is
    /// named `g` from there on; `g` itself is the declaration of that name.
    fn renamed(
        &self,
        occurrence: &Occurrence,
        spelled: &[u8],
        wide: Option<&Binding>,
    ) -> Option<Claim> {
        let at = occurrence.bytes.start;
        let import = (self.imports.iter()).find(|import| import.site.bytes.contains(&at))?;
        let named = |name: &Name| same(name.as_str(), spelled, self.case);
        let alias = import
            .names
            .iter()
            .find_map(|(name, alias)| alias.as_ref().filter(|_| named(name)));
        let binding = wide.filter(|binding| {
            matches!(binding.relation, Relation::Imports | Relation::Reexports)
                && binding.site.bytes == import.site.bytes
        });
        if let (Some(binding), Some(alias)) = (binding, alias) {
            let ours = matches!(self.claim(binding, spelled).outcome, Outcome::Ours);
            let scope = if OFFERING_MODULES.iter().any(|f| f.has(self.file.language())) {
                AliasScope::Importers(self.file.module.clone())
            } else {
                AliasScope::File(self.file.path.clone())
            };
            let alias = Alias {
                name: alias.clone(),
                scope,
                hops: 1,
            };
            return ours.then(|| {
                Claim::new(
                    Some(binding.relation),
                    Outcome::Alias(alias),
                    Basis::RenamedImport,
                )
            });
        }
        (import.names.iter())
            .any(|(_, alias)| alias.as_ref().is_some_and(named))
            .then(|| Claim::new(None, Outcome::Declaration, Basis::RenamedImport))
    }

    /// A bare name heading a chain (`name.x`) is a local, an import or the file's own
    /// name, and never a member of a class it is written outside of.
    fn bare_head(&self, occurrence: &Occurrence, spelled: &[u8]) -> Option<Basis> {
        let (_, scoped, receiver) = BARE_HEADS
            .iter()
            .find(|row| row.0.has(self.file.language()))?;
        let (start, end) = (occurrence.bytes.start, occurrence.bytes.end);
        let spaced = |b: &&u8| !b.is_ascii_whitespace();
        let before = self.text[..start as usize].iter().rev().find(spaced);
        if before == Some(&b'.') || self.text[end as usize..].iter().find(spaced) != Some(&b'.') {
            return None;
        }
        let named = |name: &Name| same(name.as_str(), spelled, self.case);
        let local = *scoped && self.shadowed(start, spelled, u32::MAX);
        let imports = (self.view.in_file(&self.file.path))
            .filter(|binding| binding.relation == Relation::Imports && named(&binding.name));
        let claims: Vec<Claim> = imports
            .map(|binding| self.claim(binding, spelled))
            .collect();
        let defined =
            (self.file.defs()).any(|def| def.local.container.is_empty() && named(&def.local.name));
        if local {
            Some(Basis::Local)
        } else if !defined
            && !claims.is_empty()
            && claims.iter().all(|c| matches!(c.outcome, Outcome::NotOurs))
        {
            Some(claims[0].basis)
        } else {
            (*receiver && self.outside_class(start)).then_some(Basis::BareName)
        }
    }

    /// The innermost definition that holds `start`.
    fn scope_at(&self, start: u32) -> Scope {
        (self.file.defs())
            .filter(|def| def.span.bytes.contains(&start))
            .min_by_key(|def| def.span.bytes.len())
            .map_or(Scope::Module, |def| Scope::Def(def.local.clone()))
    }

    /// The declaring occurrence of a local whose site starts where it comes into force: the
    /// name of a declarator (left of any `=`) before that start, or a parameter in the header
    /// of its scope. A bare name only; a call or a member is another occurrence.
    fn scoped_site(&self, occurrence: &Occurrence, spelled: &[u8]) -> Option<Claim> {
        if writable(self.subject) || !SCOPED_LOCALS.iter().any(|f| f.has(self.file.language())) {
            return None;
        }
        let (start, end) = (occurrence.bytes.start, occurrence.bytes.end);
        let (prior, after) = (&self.text[..start as usize], &self.text[end as usize..]);
        let bare = !prior.last().is_some_and(|b| b".:>".contains(b))
            && !after.trim_ascii_start().starts_with(b"(");
        let statement = &prior[prior
            .iter()
            .rposition(|b| b"{};".contains(b))
            .map_or(0, |at| at + 1)..];
        let scope = self.scope_at(start);
        let declares = self.locals.iter().any(|local| {
            let site = &local.site.bytes;
            let header = |from: u32, to: u32| {
                self.text[from as usize..to as usize]
                    .iter()
                    .all(|b| !b"{;".contains(b))
            };
            let at_site = if local.kind == LocalKind::Parameter {
                site.start <= start && start < site.end && header(site.start, start)
            } else {
                start <= site.start
                    && !statement.contains(&b'=')
                    && header(end.min(site.start), site.start)
            };
            at_site
                && same(local.name.as_str(), spelled, self.case)
                && encloses(&local.enclosing, &scope)
        });
        (bare && declares).then(|| Claim::new(None, Outcome::Declaration, Basis::LocalSite))
    }

    /// Whether a local of that name, declared by `until`, is in a function enclosing `start`.
    fn shadowed(&self, start: u32, spelled: &[u8], until: u32) -> bool {
        let scope = self.scope_at(start);
        let joins = dollar_joins(self.file.language());
        !writable(self.subject)
            && self.locals.iter().any(|local| {
                let name = local.name.as_str();
                same(
                    if joins {
                        name
                    } else {
                        name.trim_start_matches('$')
                    },
                    spelled,
                    self.case,
                ) && local.site.bytes.end <= until
                    && matches!(local.enclosing, Scope::Def(_))
                    && encloses(&local.enclosing, &scope)
            })
    }

    /// A read of a local without a binding: no member (`x.name`), the sigil or no call, after it.
    fn plain_read(&self, occurrence: &Occurrence, spelled: &[u8]) -> Option<Basis> {
        let (_, sigil) = (NAMED_LOCALS.iter()).find(|row| row.0.has(self.file.language()))?;
        let at = occurrence.bytes.start;
        let prior = &self.text[..at as usize];
        let sign = prior.strip_suffix(b"{").unwrap_or(prior).strip_suffix(b"$");
        let mark = sign.filter(|_| *sigil);
        let called = self.text[occurrence.bytes.end as usize..]
            .trim_ascii_start()
            .starts_with(b"(");
        let after_path = mark
            .unwrap_or(prior)
            .last()
            .is_some_and(|b| b".:>".contains(b));
        // A sigil language writes a target and computes bare (`n=1`, `$((n + 1))`).
        let line = prior.rsplit(|b| *b == b'\n').next().unwrap_or_default();
        let opens = |pair: &[u8]| line.windows(2).filter(|w| *w == pair).count();
        let rest = self.text[occurrence.bytes.end as usize..].trim_ascii_start();
        let bare =
            (rest.starts_with(b"=") && !rest.starts_with(b"==")) || opens(b"((") > opens(b"))");
        let read = mark.is_some() || !(*sigil || called) || (*sigil && bare);
        (read && !after_path && self.shadowed(at, spelled, at)).then_some(Basis::Local)
    }

    /// A segment of the module path an import names is a module, never a definition.
    fn module_path(&self, occurrence: &Occurrence) -> Option<Basis> {
        let (_, opens, closes) = MODULE_PATHS
            .iter()
            .find(|row| row.0.has(self.file.language()))?;
        let at = occurrence.bytes.start;
        let import = (self.imports.iter()).find(|import| import.site.bytes.contains(&at))?;
        let site = &import.site.bytes;
        let words = split_words(&self.text[site.start as usize..at as usize]);
        (words.first() == Some(&opens.as_bytes()) && !words.contains(&closes.as_bytes()))
            .then_some(Basis::OtherModule)
    }

    /// The name a package clause gives the file's package, and each segment of a dotted one, names
    /// a module, never a definition.
    fn package_clause(&self, occurrence: &Occurrence) -> Option<Basis> {
        let (_, keyword) = PACKAGE_HEADS
            .iter()
            .find(|row| row.0.has(self.file.language()))?;
        let before = &self.text[..occurrence.bytes.start as usize];
        let line = before.rsplit(|b| *b == b'\n').next()?;
        let dotted = line
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || b"_. \t".contains(b));
        (dotted && split_words(line).first() == Some(&keyword.as_bytes()))
            .then_some(Basis::OtherModule)
    }

    /// A segment before the `::` of a path names a namespace: a subject of another kind is not it.
    fn path_qualifier(&self, occurrence: &Occurrence) -> Option<Basis> {
        let (_, separator) = PATH_SEPARATORS
            .iter()
            .find(|row| row.0.has(self.file.language()))?;
        let after = self.text[occurrence.bytes.end as usize..].trim_ascii_start();
        let namespace = matches!(
            self.subject.kind,
            DefKind::Class
                | DefKind::Struct
                | DefKind::Enum
                | DefKind::Trait
                | DefKind::Interface
                | DefKind::Object
                | DefKind::TypeAlias
                | DefKind::Module
        );
        (after.starts_with(separator.as_bytes()) && !namespace).then_some(Basis::OtherModule)
    }

    /// `Reply(url=...)` writes the field `url` of the class it builds: the keyword is a use
    /// of that field when the callee, found at the call's opening parenthesis, is its class.
    fn keyword_field(&self, occurrence: &Occurrence) -> Option<Claim> {
        if !KEYWORD_ARGUMENTS
            .iter()
            .any(|f| f.has(self.file.language()))
        {
            return None;
        }
        let (start, end) = (
            occurrence.bytes.start as usize,
            occurrence.bytes.end as usize,
        );
        let before = self.text[..start].trim_ascii_end();
        let after = self.text[end..].trim_ascii_start();
        if !matches!(before.last(), Some(b'(' | b','))
            || !after.starts_with(b"=")
            || after.starts_with(b"==")
        {
            return None;
        }
        let mut depth = 0;
        let open = before.iter().rposition(|b| match b {
            b')' => {
                depth += 1;
                false
            }
            b'(' if depth > 0 => {
                depth -= 1;
                false
            }
            _ => *b == b'(',
        })?;
        let (name, owner) = self.subject.key.local.container.split_last()?;
        let callee = self.view.in_file(&self.file.path).find(|binding| {
            binding.site.bytes.end as usize == open
                && matches!(&binding.resolution, Resolution::Resolved { target, .. }
                    if target.module == self.subject.key.module
                        && target.local.name == *name
                        && target.local.container == owner)
        });
        callee.map(|_| Claim::new(Some(Relation::Writes), Outcome::Ours, Basis::Resolved))
    }

    /// The type an `impl` header names is declared there, unlike a return `impl Trait` or a bound.
    fn impl_type(&self, occurrence: &Occurrence) -> Option<Claim> {
        let (_, keyword) = IMPL_HEADS
            .iter()
            .find(|row| row.0.has(self.file.language()))?;
        let before = &self.text[..occurrence.bytes.start as usize];
        let at = before.iter().rposition(|b| b"\n;{}".contains(b));
        let header = &before[at.map_or(0, |at| at + 1)..];
        let words = split_words(header);
        let opens = keyword.as_bytes();
        let named = [opens, b"for".as_slice()].contains(words.last()?)
            || header.trim_ascii_end().ends_with(b">");
        let ahead = self.text[occurrence.bytes.end as usize..].split(|b| b"{;".contains(b));
        let traited = (split_words(ahead.into_iter().next()?).into_iter())
            .take_while(|word| *word != b"where")
            .any(|word| word == b"for");
        let first = words.iter().position(|word| *word == opens);
        (named
            && !traited
            && first.is_some_and(|at| at < 2)
            && !words.contains(&b"where".as_slice()))
        .then(|| Claim::new(None, Outcome::Declaration, Basis::DefinitionName))
    }

    /// A word after a listed command names no function (an unlisted command may run it); a script, only after a
    /// listed command that runs nothing (`cat check`, not `bash check`). Part of a longer word
    /// (`lint.sh`, `--lint`, `lint-all`) it is no function's name at all.
    fn command_argument(&self, occurrence: &Occurrence) -> Option<Basis> {
        let language = self.file.language();
        let family = COMMAND_ARGUMENTS.iter().find(|row| row.0.has(language))?.0;
        let at = occurrence.bytes.start;
        let joined = |b: Option<&u8>| b.is_some_and(|b| b".-/".contains(b));
        let (before, after) = (
            self.text[..at as usize].last(),
            self.text[occurrence.bytes.end as usize..].first(),
        );
        if self.subject.kind != DefKind::Module && (joined(before) || joined(after)) {
            return Some(Basis::Builtin);
        }
        let call = (self.file.blob.facts.iter())
            .filter_map(|fact| match fact {
                Fact::Reference(call)
                    if call.use_kind == UseKind::Call && call.site.bytes.end <= at =>
                {
                    Some(call)
                }
                _ => None,
            })
            .max_by_key(|call| call.site.bytes.end)?;
        let between = &self.text[call.site.bytes.end as usize..at as usize];
        let row = COMMAND_ARGUMENTS.iter().find(|(of, commands, _)| {
            of.has(language) && commands.split(' ').any(|word| word == call.name.as_str())
        });
        let plain = if self.subject.kind == DefKind::Module {
            family.has(self.subject.key.language) && row.is_some_and(|row| !row.2)
        } else {
            row.is_some() && !writable(self.subject)
        };
        (plain && !between.iter().any(|b| b"\n;&|(){}`".contains(b))).then_some(Basis::Builtin)
    }

    /// Whether the subject is a member of a class, and `at` is not in the class body.
    fn outside_class(&self, at: u32) -> bool {
        let home = self.view.file(&self.subject.file);
        let owner = &self.subject.key.local.container;
        let class = (home.into_iter().flat_map(|home| home.defs())).find(|def| {
            def.kind != DefKind::Module
                && (def.local.container.iter().chain([&def.local.name])).eq(owner)
        });
        class
            .is_some_and(|def| self.file.path != self.subject.file || !def.span.bytes.contains(&at))
    }

    /// A mention is an entry only when a binding at its span explains it; it is
    /// then code.
    fn explained(&self, mention: Occurrence) -> Option<Entry> {
        let claims = self.claims(&mention);
        (!claims.is_empty()).then(|| {
            let code = Occurrence {
                class: Class::Code,
                ..mention
            };
            settle(claims).at(code)
        })
    }
}

/// The outcome of each spelling of `names` in `file` (read from `text`, the bytes
/// the facts came from) for `subject`, in the side `view` holds. A file the scan
/// could not trust gives every spelling as a candidate.
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
            .map(|occurrence| Claim::candidate(None, Reason::FileLost).at(occurrence))
            .collect();
        return done;
    }
    let reading = Reading::new(view, file, text, subject);
    done.entries = found
        .code
        .into_iter()
        .map(|occurrence| reading.entry(occurrence))
        .collect();
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
