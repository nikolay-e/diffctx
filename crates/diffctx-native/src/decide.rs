//! What an impact answer may claim about the uses of a definition of a language
//! the resolvers read: Python, and TypeScript with JavaScript.
//!
//! The resolver says what each reference lands on; this module turns that into
//! the statements the answer prints. Every statement about a definition's
//! callers is one of four claims: `resolved` (a static chain leads to it),
//! `candidate` (the receiver is not typed, so the definition is one it may be),
//! `unresolved` (names like it that landed on nothing) or `partial` (the answer
//! is incomplete). An absence, "nothing calls it" or "untested", is printed only
//! when the definition's coverage is complete, and a caller that is itself
//! changed in the range is `also_changed`, never a caller outside the diff.
//!
//! The changed definitions are the facts layer's `semantic_changes`; the callers
//! are the resolver's bindings. The ranking graph and the text filters take no
//! part.

use std::cell::OnceCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use anyhow::Result;

use crate::facts::{
    DefChange, DefKey, DefKind, FactsCache, Language, ModulePath, Name, Origin, ProjectId, Scope,
    Visibility, facts_of, semantic_changes,
};
use crate::pipeline::ScoredState;
use crate::resolve::{
    Binding, CandidateReason, Definition, EvidenceKind, PythonResolver, Relation, Resolution,
    TypeScriptResolver, UnresolvedReason,
};
use crate::snapshot::{
    self, Admission, AdmissionPolicy, AnalysisContext, Loss, RangeSpec, RepoPath, RunBudget, Side,
    SideTag,
};

/// Uses of a name that left, or moved, listed one by one up to this many; past
/// it the name is too common to say which lines were meant.
const MAX_UNPLACED_LISTED: usize = 500;

/// The reasons a candidate row carries in the answer.
const UNTYPED_RECEIVER: &str = "receiver_unresolved";
const AMBIGUOUS_MODULE: &str = "ambiguous_module";
const IMPORT_UNRESOLVED: &str = "import_unresolved";
const IMPORT_GLOB: &str = "import_glob";
const GLOBAL: &str = "global";

/// The languages one resolver reads: a cell answers for one family.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Family {
    Python,
    TypeScript,
}

impl Family {
    const ALL: [Self; 2] = [Self::Python, Self::TypeScript];

    fn has(self, language: Language) -> bool {
        match self {
            Self::Python => language == Language::Python,
            Self::TypeScript => matches!(language, Language::JavaScript | Language::TypeScript),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Python => "Python",
            Self::TypeScript => "TypeScript/JavaScript",
        }
    }

    /// Whether a file of this extension is read by the family's resolver.
    fn has_path(self, path: &str) -> bool {
        RepoPath::new(path)
            .ok()
            .and_then(|p| Language::from_path(&p))
            .is_some_and(|language| self.has(language))
    }
}

/// The resolver of a family, behind the one set of questions a cell asks.
enum Resolver {
    Python(PythonResolver),
    TypeScript(TypeScriptResolver),
}

impl Resolver {
    fn open(
        family: Family,
        ctx: &AnalysisContext,
        side: SideTag,
        cache: &FactsCache,
    ) -> Result<Self> {
        Ok(match family {
            Family::Python => Self::Python(PythonResolver::open(ctx, side, cache)?),
            Family::TypeScript => Self::TypeScript(TypeScriptResolver::open(ctx, side, cache)?),
        })
    }

    fn bindings(&self) -> &[Binding] {
        match self {
            Self::Python(r) => r.bindings(),
            Self::TypeScript(r) => r.bindings(),
        }
    }

    fn definitions(&self) -> &[Definition] {
        match self {
            Self::Python(r) => r.definitions(),
            Self::TypeScript(r) => r.definitions(),
        }
    }

    fn references_to(&self, key: &DefKey) -> Vec<&Binding> {
        match self {
            Self::Python(r) => r.references_to(key),
            Self::TypeScript(r) => r.references_to(key),
        }
    }

    fn losses(&self) -> &[Loss] {
        match self {
            Self::Python(r) => r.losses(),
            Self::TypeScript(r) => r.losses(),
        }
    }
}

/// What a statement about a definition's uses may be.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Claim {
    Resolved,
    Candidate,
    Unresolved,
    Partial,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Standing {
    Resolved,
    Candidate(&'static str),
}

/// One use of a definition: the line, how it uses it, and how sure the
/// resolver is. `scope` names who owns the line.
#[derive(Clone, Debug)]
pub struct Use {
    pub file: String,
    pub line: u32,
    pub relation: Relation,
    pub standing: Standing,
    pub scope: Scope,
    /// The owner of the line is itself changed in the range.
    pub also_changed: bool,
}

/// Why a definition's answer is incomplete.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Gap {
    /// `uses` of the name that could be a use of the definition landed on
    /// nothing the model could name: an untyped read, a name no import binds.
    Unlanded { uses: usize },
    /// The range pairs the definition's two sides ambiguously.
    Unpaired,
}

pub struct Verdict {
    pub uses: Vec<Use>,
    pub gap: Option<Gap>,
}

impl Verdict {
    /// Callers outside the diff with a static chain to the definition.
    pub fn resolved(&self) -> impl Iterator<Item = &Use> {
        self.uses
            .iter()
            .filter(|u| !u.also_changed && u.standing == Standing::Resolved)
    }

    pub fn candidates(&self) -> impl Iterator<Item = &Use> {
        self.uses
            .iter()
            .filter(|u| !u.also_changed && matches!(u.standing, Standing::Candidate(_)))
    }

    pub fn also_changed(&self) -> impl Iterator<Item = &Use> {
        self.uses.iter().filter(|u| u.also_changed)
    }

    /// The strongest thing the evidence carries. `None` is an absence: nothing
    /// calls the definition and nothing that could was left unread, which is the
    /// only case an answer may say so.
    pub fn claim(&self) -> Option<Claim> {
        if self.resolved().next().is_some() {
            Some(Claim::Resolved)
        } else if self.candidates().next().is_some() {
            Some(Claim::Candidate)
        } else {
            self.gap.as_ref().map(|gap| match gap {
                Gap::Unlanded { .. } => Claim::Unresolved,
                Gap::Unpaired => Claim::Partial,
            })
        }
    }

    pub fn complete(&self) -> bool {
        self.gap.is_none()
    }
}

/// Which relations of a subject reach the definition.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Reach {
    Every,
    /// Building the class: what a constructor's callers do.
    Construction,
    /// Only the base identities in `stale` reach it: a class reported for a
    /// field it lost.
    Stale,
}

impl Reach {
    fn admits(self, relation: Relation) -> bool {
        match self {
            Self::Every => relation.uses(),
            Self::Construction => matches!(
                relation,
                Relation::Instantiates | Relation::Calls | Relation::PassesAsValue
            ),
            Self::Stale => false,
        }
    }
}

#[derive(Clone, Debug)]
struct Subject {
    key: DefKey,
    reach: Reach,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum How {
    Edited { layout_only: bool },
    Added,
    Removed,
}

/// A definition the range changed, as the answer reports it. A constructor is
/// reported as its class (its callers build the class), and a class-level field
/// also touches the class that holds it.
#[derive(Clone, Debug)]
pub struct Changed {
    pub name: String,
    pub kind: DefKind,
    /// The head file; the base file of a removed definition.
    pub file: String,
    /// The head span; the base span of a removed definition.
    pub lines: (u32, u32),
    pub how: How,
    subjects: Vec<Subject>,
    /// Base identities the head's callers may still name: the key a removed
    /// definition had, the one a moved or renamed one left.
    stale: Vec<DefKey>,
    unpaired: bool,
    /// Defined inside a function: named only there, so a file elsewhere that
    /// spells the name is no hedge on it (#546).
    pub local: bool,
    /// Part of the module's public surface: a removed one is a contract that
    /// moved.
    pub public: bool,
}

type Hunks = HashMap<String, Vec<(u32, u32)>>;

pub struct Cell {
    family: Family,
    ctx: AnalysisContext,
    cache: FactsCache,
    head: Resolver,
    base: OnceCell<Option<Resolver>>,
    changed: Vec<Changed>,
    /// Every key the range changed outside layout, by its head key.
    changed_keys: HashSet<DefKey>,
    hunks: Hunks,
    /// The project, module and language of each file that holds a definition.
    homes: HashMap<String, (ProjectId, ModulePath, Language)>,
    /// Definitions by file: indices into the head's definitions.
    defs_by_file: HashMap<String, Vec<usize>>,
    /// Definitions by key: indices into the head's definitions.
    defs_by_key: HashMap<DefKey, usize>,
    /// Head bindings by file.
    bindings_by_file: HashMap<String, Vec<usize>>,
    /// Files the resolver read.
    files: HashSet<String>,
    /// Names with uses that landed on nothing, by name: the files.
    /// The file, line, owner and whether it reads a property through a receiver
    /// nothing types.
    unlanded: HashMap<String, Vec<(String, u32, Scope, bool)>>,
    /// Resolved uses by target: the file and line of each.
    landed: HashMap<DefKey, Vec<(String, u32)>>,
    /// (user file, used file) pairs a binding links.
    reaches: HashSet<(String, String)>,
    losses: Vec<Loss>,
    /// The run's pathspecs: the cell reads the files inside them alone, as the
    /// fragments do. Empty for the whole repository.
    scope: Vec<String>,
}

/// The context the cell reads: the range the run analysed, or one snapshot of
/// the checkout for a symbol.
fn context(state: &ScoredState, range: Option<&str>, symbol: bool) -> Result<AnalysisContext> {
    let repo = snapshot::open(&state.root_dir)?;
    let spec = if symbol {
        RangeSpec::At(Side::WorkingTree)
    } else {
        match snapshot::resolve(&repo, range.map_or("HEAD", str::trim))? {
            RangeSpec::Staged { base } => RangeSpec::Review {
                base,
                head: snapshot::index_tree(&repo)?,
            },
            other => other,
        }
    };
    let (deadline, cancel) = state.run.io_limits();
    let budget = RunBudget {
        deadline,
        cancel: Some(cancel),
        read_fault: None,
    };
    // The cap the engine reads a changed file under: a file past it is listed,
    // not read, here as in the fragments.
    let admission = Admission::new(AdmissionPolicy {
        size_cap: crate::config::limits::LIMITS.max_changed_file_size as u64,
        ..AdmissionPolicy::default()
    });
    Ok(AnalysisContext::with_admission(
        repo,
        spec,
        budget,
        Arc::new(admission),
    )?)
}

/// An import that happens when the code runs, `lazy(() => import('./Card'))`:
/// the module is loaded for what it offers, so the line is a use of that.
fn loads_lazily(b: &Binding) -> bool {
    b.relation == Relation::Imports
        && matches!(&b.resolution, Resolution::Resolved { evidence, .. }
            if evidence.kind == EvidenceKind::DynamicImport)
}

/// Whether `file` is inside the run's pathspecs (all of the repository when
/// there are none).
fn within_scope(scope: &[String], file: &str) -> bool {
    scope.is_empty()
        || scope.iter().any(|p| {
            file == p
                || file
                    .strip_prefix(p.as_str())
                    .is_some_and(|r| r.starts_with('/'))
        })
}

fn within(line: u32, (start, end): (u32, u32)) -> bool {
    start <= line && line <= end
}

fn root_scope(scope: &Scope) -> &Scope {
    match scope {
        Scope::Callback { parent, .. } => root_scope(parent),
        other => other,
    }
}

fn standing_of(resolution: &Resolution) -> Option<Standing> {
    match resolution {
        Resolution::Resolved { .. } => Some(Standing::Resolved),
        Resolution::Candidate { reason, .. } => Some(Standing::Candidate(match reason {
            CandidateReason::UntypedReceiver | CandidateReason::AnyReceiver => UNTYPED_RECEIVER,
            CandidateReason::AmbiguousModule => AMBIGUOUS_MODULE,
            CandidateReason::ImportUnresolved => IMPORT_UNRESOLVED,
            CandidateReason::ImportGlob => IMPORT_GLOB,
            CandidateReason::Global => GLOBAL,
        })),
        Resolution::Module { .. } | Resolution::Unresolved { .. } => None,
    }
}

impl Cell {
    /// Whether any file the answer is about is of the family: a changed file
    /// with its extension, or none and its shebang, or a queried name in one.
    fn concerns(family: Family, state: &ScoredState, symbol: bool) -> bool {
        let source = state.run.source();
        let python = |path: &str| {
            let Ok(p) = RepoPath::new(path) else {
                return false;
            };
            match Language::from_path(&p) {
                Some(language) => family.has(language),
                None => {
                    Language::needs_shebang(&p)
                        && source
                            .read_bytes(&state.root_dir.join(path))
                            .is_some_and(|bytes| {
                                Language::from_shebang(&bytes).is_some_and(|l| family.has(l))
                            })
                }
            }
        };
        let rel = |p: &std::path::Path| crate::paths::display_rel_or_abs(&state.root_dir, p);
        if symbol {
            return state
                .all_fragments
                .iter()
                .any(|f| python(&rel(std::path::Path::new(f.path()))));
        }
        state
            .changed_files
            .iter()
            .map(|p| rel(p))
            .chain(state.deleted_files.iter().cloned())
            .chain(state.renamed_files.iter().map(|(old, _)| old.clone()))
            .any(|p| python(&p))
    }

    /// Resolves the family's files of the snapshot the run reads and, for a
    /// range, reads what it changed.
    fn open(
        family: Family,
        state: &ScoredState,
        range: Option<&str>,
        symbol: bool,
    ) -> Result<Self> {
        let ctx = context(state, range, symbol)?;
        let cache = FactsCache::user();
        let head = Resolver::open(family, &ctx, SideTag::Head, &cache)?;
        let mut cell = Self {
            family,
            ctx,
            cache,
            head,
            base: OnceCell::new(),
            changed: Vec::new(),
            changed_keys: HashSet::new(),
            hunks: HashMap::new(),
            homes: HashMap::new(),
            defs_by_file: HashMap::new(),
            defs_by_key: HashMap::new(),
            bindings_by_file: HashMap::new(),
            files: HashSet::new(),
            unlanded: HashMap::new(),
            landed: HashMap::new(),
            reaches: HashSet::new(),
            losses: Vec::new(),
            scope: state.scope.clone(),
        };
        for h in &state.hunk_texts {
            cell.hunks
                .entry(h.path.clone())
                .or_default()
                .push(h.new_range());
        }
        cell.index();
        if !symbol {
            cell.read_changes()?;
        }
        Ok(cell)
    }

    fn index(&mut self) {
        let mut file_of: HashMap<&DefKey, &str> = HashMap::new();
        // Uses that landed on nothing: (name, file, owner, an untyped read).
        let mut unlanded: Vec<(String, String, u32, Scope, bool)> = Vec::new();
        // The names a scope calls or reads through an untyped receiver and the
        // resolver placed as candidates: a read of `w.switch` beside the call
        // `w.switch()` is the call's question. A use a typed receiver resolved
        // says nothing of another receiver's read (`c.get()` beside `x.get`).
        let mut placed: HashSet<(String, Scope, String)> = HashSet::new();
        for (at, def) in self.head.definitions().iter().enumerate() {
            let file = def.file.as_str();
            self.files.insert(file.to_string());
            self.homes.entry(file.to_string()).or_insert_with(|| {
                (
                    def.key.project.clone(),
                    def.key.module.clone(),
                    def.key.language,
                )
            });
            self.defs_by_file
                .entry(file.to_string())
                .or_default()
                .push(at);
            file_of.insert(&def.key, file);
            self.defs_by_key.entry(def.key.clone()).or_insert(at);
        }
        for (at, b) in self.head.bindings().iter().enumerate() {
            let file = b.file.as_str();
            if !within_scope(&self.scope, file) {
                continue;
            }
            self.files.insert(file.to_string());
            self.bindings_by_file
                .entry(file.to_string())
                .or_default()
                .push(at);
            let line = b.site.lines.0;
            match &b.resolution {
                Resolution::Resolved { target, .. } => {
                    if b.relation.uses() {
                        self.landed
                            .entry(target.clone())
                            .or_default()
                            .push((file.to_string(), line));
                    }
                    if let Some(used) = file_of.get(target) {
                        self.reaches.insert((file.to_string(), (*used).to_string()));
                    }
                }
                Resolution::Module { file: used, .. } => {
                    self.reaches
                        .insert((file.to_string(), used.as_str().to_string()));
                }
                Resolution::Candidate { .. } => {}
                Resolution::Unresolved { reason } => {
                    if b.relation.uses() && reason.is_unknown() {
                        unlanded.push((
                            b.name.as_str().to_string(),
                            file.to_string(),
                            line,
                            b.from.clone(),
                            *reason == UnresolvedReason::UntypedRead,
                        ));
                    }
                }
            }
            if b.relation.uses() && matches!(b.resolution, Resolution::Candidate { .. }) {
                placed.insert((
                    file.to_string(),
                    b.from.clone(),
                    b.name.as_str().to_string(),
                ));
            }
        }
        for (name, file, line, from, read) in unlanded {
            if read && placed.contains(&(file.clone(), from.clone(), name.clone())) {
                continue;
            }
            self.unlanded
                .entry(name)
                .or_default()
                .push((file, line, from, read));
        }
        self.losses = self.head.losses().to_vec();
    }

    /// What the range did to the family's definitions: the cell's changed list.
    fn read_changes(&mut self) -> Result<()> {
        let set = semantic_changes(&self.ctx, &self.cache)?;
        self.losses.extend(set.losses.iter().cloned());
        let python: Vec<_> = set
            .defs
            .iter()
            .filter(|d| self.family.has(d.key.language))
            .filter(|d| {
                [&d.head, &d.base]
                    .into_iter()
                    .flatten()
                    .any(|file| within_scope(&self.scope, file.as_str()))
            })
            .collect();
        let layout_only = |change: &DefChange| {
            matches!(change, DefChange::Changed { origin: Origin::Same, delta }
                if !(delta.signature || delta.body || delta.visibility || delta.decorators))
        };
        for d in &python {
            if !layout_only(&d.change) {
                self.changed_keys.insert(d.key.clone());
            }
        }
        let mut by_key: BTreeMap<DefKey, Changed> = BTreeMap::new();
        let mut removed: Vec<Changed> = Vec::new();
        for d in &python {
            match &d.change {
                // A definition the range removed, or one whose pairing is ambiguous and
                // that only the base has: reported either way, the second as not
                // determined (`Gap::Unpaired`), never dropped.
                DefChange::Removed | DefChange::Unknown { .. } if d.head.is_none() => {
                    let unpaired = matches!(d.change, DefChange::Unknown { .. });
                    let Some(file) = d.base.as_ref() else {
                        continue;
                    };
                    let Some((kind, lines, public)) = self.base_definition(file, &d.key)? else {
                        continue;
                    };
                    let gone = Changed {
                        name: d.key.local.name.as_str().to_string(),
                        kind,
                        file: file.as_str().to_string(),
                        lines,
                        how: How::Removed,
                        subjects: Vec::new(),
                        stale: vec![d.key.clone()],
                        unpaired,
                        local: false,
                        public,
                    };
                    // What reads a field the class lost is the class's to
                    // report: the readers are listed under it, not under a
                    // name that no longer exists.
                    let class =
                        matches!(kind, DefKind::Field | DefKind::Variable | DefKind::Constant)
                            .then(|| self.class_holding(&gone.file, &d.key))
                            .flatten();
                    match class {
                        Some(class) if !self.stale_uses(&gone).is_empty() => {
                            let mut held = Changed {
                                name: class.key.local.name.as_str().to_string(),
                                kind: class.kind,
                                file: class.file.as_str().to_string(),
                                lines: class.span.lines,
                                how: How::Edited { layout_only: false },
                                subjects: vec![Subject {
                                    key: class.key.clone(),
                                    reach: Reach::Stale,
                                }],
                                stale: vec![d.key.clone()],
                                unpaired,
                                local: false,
                                public: false,
                            };
                            held.subjects.truncate(1);
                            merge(&mut by_key, held);
                        }
                        _ => removed.push(gone),
                    }
                }
                change => {
                    let Some(def) = self.definition(&d.key) else {
                        continue;
                    };
                    let how = match change {
                        DefChange::Added => How::Added,
                        other => How::Edited {
                            layout_only: layout_only(other),
                        },
                    };
                    let stale = match change {
                        DefChange::Changed {
                            origin: Origin::Moved { from } | Origin::Renamed { from },
                            ..
                        } => vec![from.clone()],
                        _ => Vec::new(),
                    };
                    let unpaired = matches!(change, DefChange::Unknown { .. });
                    for changed in self.report_as(def, how, stale, unpaired, true) {
                        merge(&mut by_key, changed);
                    }
                }
            }
        }
        self.changed = by_key.into_values().chain(removed).collect();
        self.changed.sort_by(|a, b| {
            (a.file.as_str(), a.lines, a.name.as_str()).cmp(&(
                b.file.as_str(),
                b.lines,
                b.name.as_str(),
            ))
        });
        Ok(())
    }

    fn definition(&self, key: &DefKey) -> Option<&crate::resolve::Definition> {
        self.defs_by_key
            .get(key)
            .map(|at| &self.head.definitions()[*at])
    }

    /// The enclosing class of a member: the definition its container names.
    fn owner(&self, def: &crate::resolve::Definition) -> Option<&crate::resolve::Definition> {
        let (name, outer) = def.key.local.container.split_last()?;
        self.defs_by_file
            .get(def.file.as_str())?
            .iter()
            .map(|at| &self.head.definitions()[*at])
            .find(|d| {
                d.kind == DefKind::Class
                    && d.key.local.container.as_slice() == outer
                    && &d.key.local.name == name
            })
    }

    /// Whether a function is what holds the definition.
    fn inside_function(&self, def: &crate::resolve::Definition) -> bool {
        let Some((name, outer)) = def.key.local.container.split_last() else {
            return false;
        };
        self.defs_by_file
            .get(def.file.as_str())
            .into_iter()
            .flatten()
            .map(|at| &self.head.definitions()[*at])
            .any(|d| {
                matches!(d.kind, DefKind::Function | DefKind::Method)
                    && d.key.local.container.as_slice() == outer
                    && &d.key.local.name == name
            })
    }

    /// The class at the head that held a member the range removed.
    fn class_holding(&self, file: &str, key: &DefKey) -> Option<&crate::resolve::Definition> {
        let (name, outer) = key.local.container.split_last()?;
        self.defs_by_file
            .get(file)?
            .iter()
            .map(|at| &self.head.definitions()[*at])
            .find(|d| {
                d.kind == DefKind::Class
                    && d.key.local.container.as_slice() == outer
                    && &d.key.local.name == name
            })
    }

    fn report_as(
        &self,
        def: &crate::resolve::Definition,
        how: How,
        stale: Vec<DefKey>,
        unpaired: bool,
        holder: bool,
    ) -> Vec<Changed> {
        let mut out = Vec::new();
        let own = |def: &crate::resolve::Definition, reach| Changed {
            name: def.key.local.name.as_str().to_string(),
            kind: def.kind,
            file: def.file.as_str().to_string(),
            lines: def.span.lines,
            how,
            subjects: vec![Subject {
                key: def.key.clone(),
                reach,
            }],
            stale: stale.clone(),
            unpaired,
            local: self.inside_function(def),
            public: false,
        };
        let edited = !matches!(how, How::Edited { layout_only: true });
        match def.kind {
            DefKind::Constructor => match self.owner(def) {
                Some(class) => {
                    let mut changed = own(class, Reach::Every);
                    changed.subjects.push(Subject {
                        key: def.key.clone(),
                        reach: Reach::Every,
                    });
                    out.push(changed);
                }
                None => out.push(own(def, Reach::Every)),
            },
            DefKind::Variable | DefKind::Constant | DefKind::Field => {
                out.push(own(def, Reach::Every));
                if edited && holder && self.family == Family::Python {
                    if let Some(class) = self.owner(def) {
                        let mut changed = own(class, Reach::Construction);
                        changed.stale = Vec::new();
                        changed.unpaired = false;
                        changed.how = How::Edited { layout_only: false };
                        out.push(changed);
                    }
                }
            }
            _ => out.push(own(def, Reach::Every)),
        }
        out
    }

    fn base_definition(
        &self,
        file: &RepoPath,
        key: &DefKey,
    ) -> Result<Option<(DefKind, (u32, u32), bool)>> {
        let Some(facts) = facts_of(&self.ctx, SideTag::Base, &self.cache, file)? else {
            return Ok(None);
        };
        let Some(def) = facts.defs().find(|d| d.local == key.local) else {
            return Ok(None);
        };
        // A member is public when the definition holding it is too.
        let held = key
            .local
            .container
            .split_last()
            .is_none_or(|(name, outer)| {
                facts.defs().any(|c| {
                    c.local.name == *name
                        && c.local.container.as_slice() == outer
                        && c.visibility == Visibility::Public
                })
            });
        Ok(Some((
            def.kind,
            def.span.lines,
            held && def.visibility == Visibility::Public,
        )))
    }

    /// The resolver of the base side, built when a removed or moved definition
    /// asks who still names it.
    fn base(&self) -> Option<&Resolver> {
        self.base
            .get_or_init(|| {
                Resolver::open(self.family, &self.ctx, SideTag::Base, &self.cache)
                    .map_err(|e| tracing::warn!("decide: the base side was not resolved ({e})"))
                    .ok()
            })
            .as_ref()
    }

    pub fn changed(&self) -> &[Changed] {
        &self.changed
    }

    /// What the cell could not read, for the run's limits: a read that failed or
    /// a deadline. Files it read in part are said by the answer's own
    /// unanalysed list.
    pub fn stops(&self) -> Vec<crate::resource::LimitReason> {
        let mut out = Vec::new();
        for loss in &self.losses {
            let reason = match loss {
                Loss::Deadline { .. } => crate::resource::LimitReason::Deadline,
                Loss::ReadFailed { .. } => crate::resource::LimitReason::ReadFailed,
                _ => continue,
            };
            if !out.contains(&reason) {
                out.push(reason);
            }
        }
        out
    }

    /// Whether the cell answers for `rel`: a file of its family in the snapshot.
    pub fn owns(&self, rel: &str) -> bool {
        self.files.contains(rel) || self.family.has_path(rel)
    }

    /// Whether the resolver read the file whole: its definitions and uses are
    /// the cell's, whatever the fragments made of it. A file the grammar read
    /// only in part has uses the cell never saw, so it stays a name's unread
    /// namer.
    pub fn read(&self, rel: &str) -> bool {
        self.files.contains(rel)
            && !self
                .losses
                .iter()
                .any(|loss| matches!(loss, Loss::ParseError { path, .. } if path.as_str() == rel))
    }

    /// The definition of a queried name or a fragment at these lines, as the
    /// cell would report it.
    pub fn define(&self, file: &str, name: &str, lines: (u32, u32)) -> Vec<Changed> {
        let Some(def) = self.definition_at(file, name, lines) else {
            return Vec::new();
        };
        let mut by_key: BTreeMap<DefKey, Changed> = BTreeMap::new();
        for changed in self.report_as(
            def,
            How::Edited { layout_only: false },
            Vec::new(),
            false,
            false,
        ) {
            merge(&mut by_key, changed);
        }
        by_key.into_values().collect()
    }

    /// The definition called `name` whose span is nearest `lines` in `file`.
    fn definition_at(
        &self,
        file: &str,
        name: &str,
        lines: (u32, u32),
    ) -> Option<&crate::resolve::Definition> {
        self.defs_by_file
            .get(file)?
            .iter()
            .map(|at| &self.head.definitions()[*at])
            .filter(|d| d.key.local.name.as_str() == name)
            .filter(|d| d.span.lines.0 <= lines.1 && lines.0 <= d.span.lines.1)
            .min_by_key(|d| d.span.lines.0.abs_diff(lines.0) + d.span.lines.1.abs_diff(lines.1))
    }

    fn key_in(&self, file: &str, scope: &Scope) -> Option<DefKey> {
        let Scope::Def(local) = root_scope(scope) else {
            return None;
        };
        let (project, module, language) = self.homes.get(file)?;
        Some(DefKey {
            project: project.clone(),
            language: *language,
            module: module.clone(),
            local: local.clone(),
        })
    }

    fn in_diff(&self, file: &str, line: u32, scope: &Scope) -> bool {
        match root_scope(scope) {
            Scope::Def(_) => self
                .key_in(file, scope)
                .is_some_and(|key| self.changed_keys.contains(&key)),
            _ => self
                .hunks
                .get(file)
                .is_some_and(|ranges| ranges.iter().any(|r| within(line, *r))),
        }
    }

    fn use_of(&self, b: &Binding) -> Option<Use> {
        let standing = standing_of(&b.resolution)?;
        let file = b.file.as_str().to_string();
        let line = b.site.lines.0;
        Some(Use {
            also_changed: self.in_diff(&file, line, &b.from),
            file,
            line,
            relation: b.relation,
            standing,
            scope: b.from.clone(),
        })
    }

    /// Whether a use under the `__main__` guard of the definition's own file is
    /// the script running its entry function: the definition is a function of the
    /// module itself, called by name.
    fn runs_as_entry(&self, changed: &Changed, subject: &Subject, b: &Binding) -> bool {
        b.file.as_str() == changed.file
            && self
                .definition(&subject.key)
                .is_some_and(|d| d.key.local.container.is_empty() && d.kind == DefKind::Function)
    }

    /// `export { a } from './m'` offers a changed definition on: a module that
    /// re-exports it is where it reaches the package's users.
    fn reexports(&self, b: &Binding) -> bool {
        self.family == Family::TypeScript && b.relation == Relation::Reexports
    }

    /// An annotation or a base class: a changed type is read by every signature,
    /// declaration and subclass that states it, which is how the language's own
    /// indexers count it (`reads`).
    fn names_type(&self, b: &Binding) -> bool {
        b.relation == Relation::TypeUses
    }

    /// Who uses the definition, and what the answer cannot say about it.
    pub fn verdict(&self, changed: &Changed) -> Verdict {
        let mut uses: Vec<Use> = Vec::new();
        for subject in &changed.subjects {
            for b in self.head.references_to(&subject.key) {
                if !within_scope(&self.scope, b.file.as_str()) {
                    continue;
                }
                let reaches = subject.reach.admits(b.relation)
                    || (subject.reach == Reach::Every
                        && (loads_lazily(b) || self.names_type(b) || self.reexports(b)));
                // The guard of a script runs its entry function; a use in its own
                // body is not a caller of it. The rest of the file's definitions
                // are used there like anywhere else, and so is anything the guard
                // of another file runs.
                if !reaches
                    || (b.entry_guard && self.runs_as_entry(changed, subject, b))
                    || (b.file.as_str() == changed.file && within(b.site.lines.0, changed.lines))
                {
                    continue;
                }
                uses.extend(self.use_of(b));
            }
        }
        let stale = self.stale_uses(changed);
        let covered: HashSet<(String, u32)> =
            stale.iter().map(|u| (u.file.clone(), u.line)).collect();
        uses.extend(stale);
        let unplaced = self.unplaced(changed, &covered);
        let gap = if changed.unpaired {
            Some(Gap::Unpaired)
        } else if unplaced.is_empty() {
            None
        } else if !changed.stale.is_empty() && unplaced.len() <= MAX_UNPLACED_LISTED {
            // A name that left, or moved, may still be spelled where nothing
            // types the receiver: each such line may be a use that broke.
            uses.extend(unplaced.into_iter().map(|(file, line, scope)| Use {
                also_changed: self.in_diff(&file, line, &scope),
                file,
                line,
                relation: Relation::Reads,
                standing: Standing::Candidate(UNTYPED_RECEIVER),
                scope,
            }));
            None
        } else {
            Some(Gap::Unlanded {
                uses: unplaced.len(),
            })
        };
        uses.sort_by(|a, b| {
            (a.file.as_str(), a.line, a.relation.name()).cmp(&(
                b.file.as_str(),
                b.line,
                b.relation.name(),
            ))
        });
        uses.dedup_by(|a, b| a.file == b.file && a.line == b.line && a.standing == b.standing);
        // A candidate that is itself changed was never a caller of record.
        uses.retain(|u| !u.also_changed || u.standing == Standing::Resolved);
        Verdict { uses, gap }
    }

    /// Uses at the head of a definition the range removed or moved away: lines
    /// the base resolved to it, which the head still spells and no longer lands.
    fn stale_uses(&self, changed: &Changed) -> Vec<Use> {
        if changed.stale.is_empty() {
            return Vec::new();
        }
        let Some(base) = self.base() else {
            return Vec::new();
        };
        // How sure the base was of each: a call through an interface-typed
        // receiver reached the definition as a candidate, and the line that
        // still spells it is one.
        let mut named: HashMap<(String, Name), Standing> = HashMap::new();
        for key in &changed.stale {
            for b in base.references_to(key) {
                if b.relation.uses() && !b.entry_guard && within_scope(&self.scope, b.file.as_str())
                {
                    let standing = match (self.family, standing_of(&b.resolution)) {
                        (Family::TypeScript, Some(standing)) => standing,
                        _ => Standing::Resolved,
                    };
                    named
                        .entry((b.file.as_str().to_string(), b.name.clone()))
                        .and_modify(|known| {
                            if standing == Standing::Resolved {
                                *known = standing;
                            }
                        })
                        .or_insert(standing);
                }
            }
        }
        let mut out = Vec::new();
        for ((file, name), standing) in named {
            for at in self.bindings_by_file.get(&file).into_iter().flatten() {
                let h = &self.head.bindings()[*at];
                if h.name != name
                    || !h.relation.uses()
                    || !matches!(h.resolution, Resolution::Unresolved { .. })
                {
                    continue;
                }
                let line = h.site.lines.0;
                out.push(Use {
                    also_changed: self.in_diff(&file, line, &h.from),
                    file: file.clone(),
                    line,
                    relation: h.relation,
                    standing,
                    scope: h.from.clone(),
                });
            }
        }
        out
    }

    /// The uses of the name that landed on nothing, in files that could reach
    /// the definition, apart from those already given as stale.
    fn unplaced(
        &self,
        changed: &Changed,
        covered: &HashSet<(String, u32)>,
    ) -> Vec<(String, u32, Scope)> {
        // A property read through an object nothing types cannot be a use of a
        // top-level function or constant of a module: `c.env` is no use of the
        // exported `env`, which a use reaches through its import. Only a member
        // is read that way.
        let member = changed
            .subjects
            .first()
            .is_some_and(|s| !s.key.local.container.is_empty());
        let top_level_only = self.family == Family::TypeScript && !member;
        self.unlanded
            .get(&changed.name)
            .into_iter()
            .flatten()
            .filter(|(file, line, _, read)| {
                !(top_level_only && *read)
                    && self.ctx.workspace.may_reference(file, &changed.file)
                    && !covered.contains(&(file.clone(), *line))
            })
            .map(|(file, line, scope, _)| (file.clone(), *line, scope.clone()))
            .collect()
    }

    /// Whether a line range of `user_file` holds a resolved use of the
    /// definition called `name` at `lines` of `used_file`: the link a test has to
    /// a function it calls.
    pub fn links(
        &self,
        user_file: &str,
        user_lines: (u32, u32),
        used_file: &str,
        name: &str,
        used_lines: (u32, u32),
    ) -> bool {
        let Some(def) = self.definition_at(used_file, name, used_lines) else {
            return false;
        };
        self.landed.get(&def.key).is_some_and(|uses| {
            uses.iter()
                .any(|(file, line)| file == user_file && within(*line, user_lines))
        })
    }

    /// Whether anything in `user_file` is bound to something in `used_file`.
    pub fn reaches(&self, user_file: &str, used_file: &str) -> bool {
        self.reaches
            .contains(&(user_file.to_string(), used_file.to_string()))
    }
}

/// The cells of one run: one per family of languages the answer is about.
pub struct Cells {
    cells: Vec<Cell>,
    /// Families whose files could not be resolved: their definitions are
    /// undetermined, never answered by the paths the cells replaced.
    lost: Vec<Family>,
}

impl Cells {
    pub fn open(state: &ScoredState, range: Option<&str>, symbol: bool) -> Self {
        let mut cells = Vec::new();
        let mut lost = Vec::new();
        for family in Family::ALL
            .into_iter()
            .filter(|f| Cell::concerns(*f, state, symbol))
        {
            match Cell::open(family, state, range, symbol) {
                Ok(cell) => {
                    for stop in cell.stops() {
                        state.run.note(stop);
                    }
                    cells.push(cell);
                }
                Err(e) => {
                    tracing::warn!(
                        "impact: the {} cell was not resolved ({e}); its definitions are undetermined",
                        family.name()
                    );
                    state.run.note(crate::resource::LimitReason::ReadFailed);
                    lost.push(family);
                }
            }
        }
        Self { cells, lost }
    }

    pub fn iter(&self) -> impl Iterator<Item = &Cell> {
        self.cells.iter()
    }

    /// The cell that answers for `rel`.
    pub fn of(&self, rel: &str) -> Option<&Cell> {
        self.cells.iter().find(|cell| cell.owns(rel))
    }

    /// Whether `rel` is a file of a family that could not be resolved.
    pub fn lost(&self, rel: &str) -> bool {
        self.lost.iter().any(|family| family.has_path(rel))
    }
}

/// Reports one definition once, with the subjects of every reason it is
/// reported for.
fn merge(by_key: &mut BTreeMap<DefKey, Changed>, changed: Changed) {
    let Some(first) = changed.subjects.first() else {
        return;
    };
    match by_key.get_mut(&first.key) {
        Some(known) => {
            for subject in changed.subjects {
                match known.subjects.iter_mut().find(|s| s.key == subject.key) {
                    Some(seen) if subject.reach == Reach::Every => seen.reach = Reach::Every,
                    Some(_) => {}
                    None => known.subjects.push(subject),
                }
            }
            known.stale.extend(changed.stale);
            known.unpaired |= changed.unpaired;
            known.how = match (known.how, changed.how) {
                (How::Edited { layout_only: a }, How::Edited { layout_only: b }) => How::Edited {
                    layout_only: a && b,
                },
                (How::Added, _) | (_, How::Added) => How::Added,
                (other, _) => other,
            };
        }
        None => {
            by_key.insert(first.key.clone(), changed);
        }
    }
}
