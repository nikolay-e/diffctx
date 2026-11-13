//! The outcome of each occurrence of a definition's names in one file of one
//! side. A binding claims an occurrence only at its exact span and name; a
//! `not_ours` stands only on a basis the facts of the side confirm; the rest is a
//! candidate. For a removed definition the caller passes the head's [`View`] with
//! the base's [`Definition`].

use std::collections::{HashMap, HashSet};

use super::{Basis, Case, Class, Entry, Occurrence, Outcome, Reason, scan};
use crate::facts::{
    DefKey, DefKind, Export, Fact, FileFacts, Import, Local, LocalKind, Name, Scope, Star,
};
use crate::resolve::{Binding, Definition, Relation, Resolution, UnresolvedReason};
use crate::snapshot::RepoPath;

/// One side's resolved snapshot, indexed for the questions an occurrence asks.
pub struct View<'a> {
    files: HashMap<RepoPath, &'a FileFacts>,
    by_file: HashMap<RepoPath, Vec<&'a Binding>>,
    exact: HashMap<(RepoPath, u32, u32), Vec<&'a Binding>>,
    definitions: HashSet<DefKey>,
    bases: HashMap<DefKey, Vec<DefKey>>,
    overriders: HashMap<DefKey, Vec<DefKey>>,
    manifests_whole: bool,
    family_unproven: bool,
    /// Where the base resolved a use to a removed definition.
    stale: HashSet<Place>,
}

/// Where a use is written, free of byte offsets: a changed file's differ by side.
type Place = (RepoPath, Vec<Name>, Name);

fn place(binding: &Binding) -> Place {
    let owners = path_of(&binding.from).into_iter().cloned().collect();
    (binding.file.clone(), owners, binding.name.clone())
}

impl<'a> View<'a> {
    /// `manifests_whole`: the workspace manifests were read without a loss.
    pub fn new(
        files: &'a [FileFacts],
        bindings: &'a [Binding],
        definitions: &'a [Definition],
        manifests_whole: bool,
    ) -> Self {
        let mut view = Self {
            files: files.iter().map(|file| (file.path.clone(), file)).collect(),
            by_file: HashMap::new(),
            exact: HashMap::new(),
            definitions: definitions.iter().map(|def| def.key.clone()).collect(),
            bases: HashMap::new(),
            overriders: HashMap::new(),
            manifests_whole,
            family_unproven: false,
            stale: HashSet::new(),
        };
        for binding in bindings {
            let bytes = &binding.site.bytes;
            let file = &binding.file;
            view.by_file.entry(file.clone()).or_default().push(binding);
            view.exact
                .entry((file.clone(), bytes.start, bytes.end))
                .or_default()
                .push(binding);
            let (Scope::Def(local), Resolution::Resolved { target, .. }) =
                (&binding.from, &binding.resolution)
            else {
                continue;
            };
            let Some(&facts) = view.files.get(file) else {
                continue;
            };
            if binding.relation == Relation::Overrides {
                view.overrides(facts.key(local), target.clone());
            }
        }
        view
    }

    fn overrides(&mut self, overrider: DefKey, target: DefKey) {
        self.bases
            .entry(overrider.clone())
            .or_default()
            .push(target.clone());
        self.overriders.entry(target).or_default().push(overrider);
    }

    /// A removed member's family is what the base side bound at the places it resolved
    /// a use to it (no `@Override` needed); with no base read, a call resolved to a
    /// same-named member is a candidate.
    pub fn removed_sites(mut self, base: Option<&[Binding]>, removed: &[DefKey]) -> Self {
        match base {
            Some(bindings) => {
                let named = |binding: &&Binding| {
                    (binding.targets().into_iter()).any(|target| removed.contains(target))
                };
                self.stale = bindings.iter().filter(named).map(place).collect();
            }
            None => self.family_unproven = true,
        }
        self
    }

    /// What `key` overrides or is overridden by, transitively; not `key`.
    fn family(&self, key: &DefKey) -> HashSet<DefKey> {
        let mut found = HashSet::new();
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

    fn reexports(&self, module: &FileFacts, subject: &Definition) -> bool {
        module.blob.facts.iter().any(|fact| match fact {
            Fact::Export(Export::Named { name, .. }) => *name == subject.key.local.name,
            Fact::Export(Export::Star { .. }) => true,
            _ => false,
        }) || self
            .by_file
            .get(&module.path)
            .into_iter()
            .flatten()
            .any(|binding| {
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
/// that may run), a `not_ours` beside it is a candidate. Definite outcomes that
/// differ are a defect of the resolver and read as a candidate.
fn settle(mut claims: Vec<Claim>) -> Claim {
    let ours = claims
        .iter()
        .position(|c| matches!(c.outcome, Outcome::Ours));
    let hedge = claims
        .iter()
        .position(|c| matches!(c.outcome, Outcome::Candidate(_)));
    let other = claims
        .iter()
        .any(|c| !matches!(c.outcome, Outcome::Ours | Outcome::Candidate(_)));
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
                    // Only a parameter's span is surely no use: a pattern's binding
                    // may be a field read too (`let Retry { attempt } = r;`).
                    if local.kind == LocalKind::Parameter {
                        let site = &local.site.bytes;
                        declared
                            .entry((site.start, site.end))
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
            .exact
            .get(&at)
            .into_iter()
            .flatten()
            .filter(|binding| same(binding.name.as_str(), spelled, self.case))
            .map(|binding| self.claim(binding, spelled))
            .collect()
    }

    fn claim(&self, binding: &Binding, spelled: &[u8]) -> Claim {
        let relation = Some(binding.relation);
        match &binding.resolution {
            Resolution::Resolved { target, .. } => self.resolved(target, binding),
            Resolution::Module { file, .. } => self.module(file, relation),
            Resolution::Candidate { reason, .. } => {
                Claim::candidate(relation, Reason::Resolver(*reason))
            }
            Resolution::Unresolved { reason } => self.unresolved(*reason, binding, spelled),
        }
    }

    fn resolved(&self, target: &DefKey, binding: &Binding) -> Claim {
        let relation = Some(binding.relation);
        if *target == self.subject.key {
            Claim::new(relation, Outcome::Ours, Basis::Resolved)
        } else if self.family.contains(target) || self.view.stale.contains(&place(binding)) {
            Claim::candidate(relation, Reason::Dispatch)
        } else if self.view.family_unproven
            && target.local.name == self.subject.key.local.name
            && !self.view.definitions.contains(&self.subject.key)
        {
            Claim::candidate(relation, Reason::OverrideUnproven)
        } else if self.view.definitions.contains(target) {
            Claim::new(relation, Outcome::NotOurs, Basis::OtherDefinition)
        } else {
            Claim::candidate(relation, Reason::BasisUnverified)
        }
    }

    fn module(&self, module: &RepoPath, relation: Option<Relation>) -> Claim {
        let found = self.view.files.get(module).copied();
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
            UnresolvedReason::External => verified(
                Basis::ExternalModule,
                self.imports.iter().any(|import| {
                    import.relative == 0
                        && self.binds(import, spelled)
                        && !self
                            .view
                            .files
                            .values()
                            .any(|file| file.module == import.module)
                }),
            ),
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
            _ => settle(claims),
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

    /// No binding at the span: a wider one of the name holds it, or none does.
    fn unbound(&self, occurrence: &Occurrence) -> Claim {
        let spelled = self.spelled(occurrence);
        let bytes = &occurrence.bytes;
        let wide = self
            .view
            .by_file
            .get(&occurrence.file)
            .into_iter()
            .flatten()
            .find(|binding| {
                let site = &binding.site.bytes;
                site.start <= bytes.start
                    && bytes.end <= site.end
                    && same(binding.name.as_str(), spelled, self.case)
            });
        wide.map_or_else(
            || Claim::candidate(None, Reason::Unexplained),
            |binding| Claim::candidate(Some(binding.relation), Reason::WideBinding),
        )
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
