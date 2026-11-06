//! What an impact answer may claim about the uses of a definition of a language
//! the resolvers read: Python, and TypeScript with JavaScript.
//!
//! The resolver says what each reference lands on; this module turns that into
//! the statements the answer prints. Every statement about a definition's
//! callers is one of five claims: `resolved` (a static chain leads to it),
//! `candidate` (the receiver is not typed, so the definition is one it may be),
//! `unresolved` (names like it that landed on nothing), `partial` (the answer
//! is incomplete) or `absent` (nothing calls it, and the search was complete).
//! An absence, "nothing calls it" or "untested", is printed only from that last
//! one, and a caller that is itself changed in the range is `also_changed`,
//! never a caller outside the diff.
//!
//! The changed definitions are the facts layer's `semantic_changes`; the callers
//! are the resolver's bindings. The ranking graph and the text filters take no
//! part.
//!
//! An absence is never a default. [`Complete`] is the witness that the search for
//! a definition's uses left nothing unread; only this module makes one (the cell's
//! own search, [`Coverage::searched`] and [`Coverage::scope`]), from the gaps the
//! search found, and an absence claim ([`Claim::Absent`]) or an exact
//! count ([`Count::Exact`]) holds it. Every other outcome names its [`Gap`]. A
//! failed read is an `Err` or a gap here, never an empty value: the lint that
//! says so is denied in this module.

#![deny(clippy::disallowed_methods)]

use std::cell::OnceCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use anyhow::Result;

use crate::facts::{
    Def, DefChange, DefKey, DefKind, FactsCache, Language, ModulePath, Name, Origin, ProjectId,
    Scope, SemanticDef, Visibility, facts_of, semantic_changes,
};
use crate::pipeline::ScoredState;
use crate::resolve::{
    Binding, CandidateReason, Definition, EvidenceKind, PythonResolver, Relation, Resolution,
    TypeScriptResolver, UnresolvedReason,
};
use crate::resource::LimitReason;
use crate::snapshot::{
    self, Admission, AdmissionPolicy, AnalysisContext, ChangeKind, Loss, RangeSpec, RepoPath,
    RunBudget, Side, SideTag,
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
            .is_ok_and(|p| Language::from_path(&p).is_some_and(|language| self.has(language)))
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

/// The witness that the search for a definition's uses covered everything that
/// could name it. It has no public field and no `Default`, and `Coverage::of`,
/// which makes it, is private to this module: the cell's search, `searched` and
/// `scope` are the only call sites, so an absence or an exact count cannot be
/// written without the coverage computation having found no gap.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Complete(());

/// Why a definition's answer is incomplete.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Gap {
    /// `uses` of the name that could be a use of the definition landed on
    /// nothing the model could name: an untyped read, a name no import binds.
    Unlanded { uses: usize },
    /// The range pairs the definition's two sides ambiguously.
    Unpaired,
    /// The base side of the range could not be resolved: what still names a
    /// removed or moved definition is the base's question, and it went unasked.
    BaseUnresolved { reason: String },
    /// The run stopped (a deadline, a failed read) before the search finished.
    Stopped { reasons: Vec<LimitReason> },
    /// The run is limited to pathspecs, and `files` outside them could name the
    /// definition: their uses were left out.
    OutsideScope { files: usize },
    /// Files that spell the name were not analysed whole.
    Unread { files: usize },
    /// Files the run withheld by ignore rules were not read.
    Withheld { files: usize },
    /// Changed code in `files` lies outside every tracked definition (a
    /// module-level statement, a `__main__` block): what it changes is not traced.
    Untracked { files: usize },
    /// The answer cap shed the definition: its callers were found, not listed.
    Capped,
    /// A route no model traces can reach it: dispatch, an untyped receiver, a
    /// language no binding model reads.
    NotTraced,
}

impl std::fmt::Display for Gap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unlanded { uses } => write!(f, "{uses} use(s) that landed on nothing"),
            Self::Unpaired => f.write_str("its two sides of the range pair ambiguously"),
            Self::BaseUnresolved { reason } => {
                write!(f, "the base side of the range was not resolved ({reason})")
            }
            Self::Stopped { reasons } => {
                write!(
                    f,
                    "the run was limited ({})",
                    crate::impact::reason_names(reasons)
                )
            }
            Self::OutsideScope { files } => {
                write!(
                    f,
                    "{files} file(s) outside the run's pathspecs were left out"
                )
            }
            Self::Unread { files } => {
                write!(f, "{files} file(s) naming it were not analysed whole")
            }
            Self::Withheld { files } => write!(f, "{files} file(s) withheld by ignore rules"),
            Self::Untracked { files } => write!(
                f,
                "changed code outside any tracked definition in {files} file(s)"
            ),
            Self::Capped => f.write_str("the answer cap shed it"),
            Self::NotTraced => f.write_str("a route no model traces can reach it"),
        }
    }
}

/// What the search for a definition's uses covered.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Coverage {
    Complete(Complete),
    Partial(Vec<Gap>),
}

impl Coverage {
    /// The coverage computation: complete exactly when the search found no gap.
    fn of(gaps: Vec<Gap>) -> Self {
        if gaps.is_empty() {
            Self::Complete(Complete(()))
        } else {
            Self::Partial(gaps)
        }
    }

    /// A search that is incomplete for one reason it already knows.
    pub fn partial(gap: Gap) -> Self {
        Self::Partial(vec![gap])
    }

    /// The search for one definition's callers outside the cell: the cell's own
    /// coverage when the cell answered for it, the stop that cut the search short
    /// or a route nothing traces when the answer names one, and the files that
    /// spell the name but were not analysed whole.
    pub fn searched(
        cell: Option<&Verdict>,
        stopped: Option<&[LimitReason]>,
        not_traced: bool,
        unread_files: usize,
    ) -> Self {
        let mut gaps: Vec<Gap> = cell.map_or_else(Vec::new, |v| v.coverage().gaps().to_vec());
        if gaps.is_empty() {
            if let Some(reasons) = stopped {
                gaps.push(Gap::Stopped {
                    reasons: reasons.to_vec(),
                });
            } else if not_traced {
                gaps.push(Gap::NotTraced);
            }
        }
        if unread_files > 0 {
            gaps.push(Gap::Unread {
                files: unread_files,
            });
        }
        Self::of(gaps)
    }

    /// What the run as a whole covered: a limit that can have lost a caller,
    /// files withheld by ignore rules, and files with changed code no tracked
    /// definition holds.
    pub fn scope(limits: &[LimitReason], withheld: usize, untracked: usize) -> Self {
        let mut gaps = Vec::new();
        if !limits.is_empty() {
            gaps.push(Gap::Stopped {
                reasons: limits.to_vec(),
            });
        }
        if withheld > 0 {
            gaps.push(Gap::Withheld { files: withheld });
        }
        if untracked > 0 {
            gaps.push(Gap::Untracked { files: untracked });
        }
        Self::of(gaps)
    }

    /// The same coverage with one more gap, when there is one. A complete
    /// coverage with none stays the witness it was; this never makes one.
    pub fn with(self, gap: Option<Gap>) -> Self {
        match (self, gap) {
            (coverage, None) => coverage,
            (Self::Complete(_), Some(gap)) => Self::Partial(vec![gap]),
            (Self::Partial(mut gaps), Some(gap)) => {
                gaps.push(gap);
                Self::Partial(gaps)
            }
        }
    }

    pub fn witness(&self) -> Option<Complete> {
        match self {
            Self::Complete(witness) => Some(*witness),
            Self::Partial(_) => None,
        }
    }

    pub fn gaps(&self) -> &[Gap] {
        match self {
            Self::Complete(_) => &[],
            Self::Partial(gaps) => gaps,
        }
    }

    /// `found` uses as the answer may count them: exactly only when nothing was
    /// left unread, otherwise as the least there are.
    pub fn count(&self, found: usize) -> Count {
        self.witness().map_or(Count::AtLeast(found), |witness| {
            Count::Exact(found, witness)
        })
    }
}

/// A number of uses, and whether it is the whole.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(
    dead_code,
    reason = "the witness is held so that an exact count needs one, never read"
)]
pub enum Count {
    Exact(usize, Complete),
    AtLeast(usize),
}

impl Count {
    pub const fn found(self) -> usize {
        match self {
            Self::Exact(n, _) | Self::AtLeast(n) => n,
        }
    }
}

/// What a statement about a definition's uses may be.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(
    dead_code,
    reason = "the witness is held so that an absence needs one, never read"
)]
pub enum Claim {
    Resolved,
    Candidate,
    Unresolved,
    Partial,
    /// Nothing calls the definition and nothing that could was left unread: the
    /// only claim an answer may print as an absence.
    Absent(Complete),
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

/// The uses of a definition and what covered the search for them: the one
/// coverage of the relations that use it (`Relation::uses`), which is every
/// relation an absence is claimed for.
pub struct Verdict {
    pub uses: Vec<Use>,
    coverage: Coverage,
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

    pub fn coverage(&self) -> &Coverage {
        &self.coverage
    }

    /// The gap the answer names first.
    pub fn gap(&self) -> Option<&Gap> {
        self.coverage.gaps().first()
    }

    /// The strongest thing the evidence carries. An absence is the one claim only
    /// a complete search makes.
    pub fn claim(&self) -> Claim {
        if self.resolved().next().is_some() {
            Claim::Resolved
        } else if self.candidates().next().is_some() {
            Claim::Candidate
        } else {
            match &self.coverage {
                Coverage::Complete(witness) => Claim::Absent(*witness),
                Coverage::Partial(gaps) => match gaps.first() {
                    Some(Gap::Unlanded { .. }) => Claim::Unresolved,
                    _ => Claim::Partial,
                },
            }
        }
    }

    pub fn complete(&self) -> bool {
        self.coverage.witness().is_some()
    }
}

/// Which relations of a subject reach the definition.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Reach {
    Every,
    /// Building the class: what a constructor's callers do.
    Construction,
    /// What a changed or removed constructor reaches (R11): the class's
    /// instantiations and the explicit and `super()` calls of the constructor.
    /// Annotations, `isinstance`, `raises` and the class passed as a value are
    /// uses of the class a constructor change leaves alone.
    Constructor,
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
            Self::Constructor => matches!(
                relation,
                Relation::Instantiates | Relation::Calls | Relation::Overrides
            ),
            Self::Stale => false,
        }
    }
}

#[derive(Clone, Debug)]
struct Subject {
    key: DefKey,
    reach: Reach,
    /// The span of the definition whose change this is, when it has no members:
    /// a use inside it is the definition's own (a function calling itself). A
    /// class has none, since the members around the change are other
    /// definitions: the unchanged ones are callers, the changed ones
    /// `also_changed`.
    within: Option<(u32, u32)>,
}

/// The span a use must lie in to be the definition's own use of itself.
fn own_span(kind: DefKind, lines: (u32, u32)) -> Option<(u32, u32)> {
    (!matches!(
        kind,
        DefKind::Class
            | DefKind::Struct
            | DefKind::Enum
            | DefKind::Trait
            | DefKind::Interface
            | DefKind::Object
            | DefKind::Module
    ))
    .then_some(lines)
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

/// The simple names in the parentheses of a class signature: `class A(B, m.C[int],
/// metaclass=M)` lists `B` and `C`.
fn bases_in(signature: Option<&str>) -> Vec<String> {
    let Some(text) = signature else {
        return Vec::new();
    };
    let (Some(open), Some(close)) = (text.find('('), text.rfind(')')) else {
        return Vec::new();
    };
    if close <= open {
        return Vec::new();
    }
    text[open + 1..close]
        .split(',')
        .map(str::trim)
        .filter(|base| !base.contains('='))
        .filter_map(|base| {
            let last = base.rsplit('.').next()?;
            let word: String = last
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            (!word.is_empty()).then_some(word)
        })
        .collect()
}

impl Changed {
    /// The name the answer gives it: a member field is `Class.field`, so two
    /// classes' fields of one name are two definitions.
    pub fn display_name(&self) -> String {
        let key = self
            .subjects
            .first()
            .map(|s| &s.key)
            .or_else(|| self.stale.first());
        match (self.kind, key.and_then(|k| k.local.container.last())) {
            (DefKind::Field | DefKind::Variable | DefKind::Constant, Some(class)) => {
                format!("{}.{}", class.as_str(), self.name)
            }
            _ => self.name.clone(),
        }
    }
}

type Hunks = HashMap<String, Vec<(u32, u32)>>;

pub struct Cell {
    family: Family,
    ctx: AnalysisContext,
    cache: FactsCache,
    head: Resolver,
    base: OnceCell<Result<Resolver, String>>,
    changed: Vec<Changed>,
    /// Every key the range changed outside layout, by its head key.
    changed_keys: HashSet<DefKey>,
    hunks: Hunks,
    /// The lines each file gained or rewrote, by number, outside formatter-only
    /// hunks.
    added: HashMap<String, Vec<(u32, String)>>,
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
    /// Files outside the run's pathspecs that hold uses the cell left out.
    outside: HashSet<String>,
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
        read_fault: crate::resource::test_read_fault().map(snapshot::ReadFault::at),
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

/// The losses that leave a search a lower bound: a read that failed or a deadline.
fn stops_in(losses: &[Loss]) -> Vec<LimitReason> {
    let mut out = Vec::new();
    for loss in losses {
        let reason = match loss {
            Loss::Deadline { .. } => LimitReason::Deadline,
            Loss::ReadFailed { .. } => LimitReason::ReadFailed,
            _ => continue,
        };
        if !out.contains(&reason) {
            out.push(reason);
        }
    }
    out
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
            added: HashMap::new(),
            homes: HashMap::new(),
            defs_by_file: HashMap::new(),
            defs_by_key: HashMap::new(),
            bindings_by_file: HashMap::new(),
            files: HashSet::new(),
            outside: HashSet::new(),
            unlanded: HashMap::new(),
            landed: HashMap::new(),
            reaches: HashSet::new(),
            losses: Vec::new(),
            scope: state.scope.clone(),
        };
        // A queried name has no diff: the lines a symbol run anchors its
        // definitions on are no hunks, and a use inside one is a caller.
        for h in state.hunk_texts.iter().filter(|_| !symbol) {
            cell.hunks
                .entry(h.path.clone())
                .or_default()
                .push(h.new_range());
            if !h.layout {
                cell.added
                    .entry(h.path.clone())
                    .or_default()
                    .extend((h.new_start..).zip(h.added.iter().cloned()));
            }
        }
        cell.index();
        if !symbol {
            cell.read_changes()?;
        }
        Ok(cell)
    }

    fn index(&mut self) {
        let mut file_of: HashMap<&DefKey, &str> = HashMap::new();
        // Uses that landed on nothing: (name, file, owner, a use through a receiver
        // nothing types: a property read, and in TypeScript a method call).
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
                self.outside.insert(file.to_string());
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
                            *reason == UnresolvedReason::UntypedRead
                                || (self.family == Family::TypeScript
                                    && matches!(
                                        reason,
                                        UnresolvedReason::NoCandidates
                                            | UnresolvedReason::TooManyCandidates
                                    )),
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
        // A field that changed class and nothing else (into the subclass that
        // inherited it, out to a new base) is the same field: the classes it
        // moved between are what changed.
        let moved_as_it_was = |d: &SemanticDef| {
            self.family == Family::Python
                && matches!(&d.change, DefChange::Changed { origin: Origin::Moved { .. }, delta }
                    if !(delta.signature || delta.body || delta.visibility || delta.decorators))
                && self.definition(&d.key).is_some_and(|def| {
                    matches!(
                        def.kind,
                        DefKind::Field | DefKind::Variable | DefKind::Constant
                    ) && !def.key.local.container.is_empty()
                })
        };
        let python: Vec<_> = python
            .into_iter()
            .filter(|&d| !moved_as_it_was(d))
            .collect();
        let moved = self.moved_declarations(&python)?;
        let python: Vec<_> = python
            .into_iter()
            .filter(|d| !moved.contains(&d.key))
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
        let class_removed = |key: &DefKey| {
            let Some((name, outer)) = key.local.container.split_last() else {
                return false;
            };
            python.iter().any(|c| {
                c.head.is_none()
                    && matches!(c.change, DefChange::Removed)
                    && c.key.project == key.project
                    && c.key.module == key.module
                    && c.key.language == key.language
                    && c.key.local.name == *name
                    && c.key.local.container.as_slice() == outer
            })
        };
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
                    // The members of a class that left go with its line.
                    if matches!(kind, DefKind::Field | DefKind::Variable | DefKind::Constant)
                        && class_removed(&d.key)
                    {
                        continue;
                    }
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
                    // A class that lost its constructor is changed like one whose
                    // constructor was edited: what builds, extends or names it is
                    // affected, and nothing says so under the removed `__init__`.
                    if kind == DefKind::Constructor && self.family == Family::Python {
                        if let Some(class) = self.class_holding(&gone.file, &d.key) {
                            merge(
                                &mut by_key,
                                Changed {
                                    name: class.key.local.name.as_str().to_string(),
                                    kind: class.kind,
                                    file: class.file.as_str().to_string(),
                                    lines: class.span.lines,
                                    how: How::Edited { layout_only: false },
                                    subjects: vec![Subject {
                                        key: class.key.clone(),
                                        reach: Reach::Constructor,
                                        within: own_span(class.kind, class.span.lines),
                                    }],
                                    stale: Vec::new(),
                                    unpaired: false,
                                    local: false,
                                    public: false,
                                },
                            );
                        }
                    }
                    // What reads a field the class lost is the class's to
                    // report: the readers are listed under it, not under a
                    // name that no longer exists.
                    let class =
                        matches!(kind, DefKind::Field | DefKind::Variable | DefKind::Constant)
                            .then(|| self.class_holding(&gone.file, &d.key))
                            .flatten();
                    match class {
                        Some(class) if !self.stale_uses(&gone).0.is_empty() => {
                            let mut held = Changed {
                                name: class.key.local.name.as_str().to_string(),
                                kind: class.kind,
                                file: class.file.as_str().to_string(),
                                lines: class.span.lines,
                                how: How::Edited { layout_only: false },
                                subjects: vec![Subject {
                                    key: class.key.clone(),
                                    reach: Reach::Stale,
                                    within: own_span(class.kind, class.span.lines),
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
                        if self.folds_into_its_class(&changed, def) {
                            continue;
                        }
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

    /// A field declaration only one side has, with an equal one on the other
    /// side in a class of the same lineage: it moved between a base and its
    /// subclass (merged into the subclass, lifted into a new base), and the
    /// field is the same. Annotation-only fields have no body to pair them by.
    fn moved_declarations(&self, entries: &[&SemanticDef]) -> Result<HashSet<DefKey>> {
        if self.family != Family::Python {
            return Ok(HashSet::new());
        }
        struct Field<'a> {
            entry: &'a SemanticDef,
            file: &'a RepoPath,
            class: Name,
            declaration: Def,
        }
        let mut gone: Vec<Field<'_>> = Vec::new();
        let mut added: Vec<Field<'_>> = Vec::new();
        for &entry in entries {
            let Some(class) = entry.key.local.container.last().cloned() else {
                continue;
            };
            let (side, file, into) = match (&entry.change, &entry.base, &entry.head) {
                (DefChange::Removed, Some(file), None) => (SideTag::Base, file, &mut gone),
                (DefChange::Added, None, Some(file)) => (SideTag::Head, file, &mut added),
                _ => continue,
            };
            let Some(declaration) = self.declared(side, file, &entry.key)? else {
                continue;
            };
            if matches!(
                declaration.kind,
                DefKind::Field | DefKind::Variable | DefKind::Constant
            ) {
                into.push(Field {
                    entry,
                    file,
                    class,
                    declaration,
                });
            }
        }
        let mut moved = HashSet::new();
        for from in &gone {
            for to in added.iter().filter(|to| {
                to.file == from.file
                    && to.entry.key.local.name == from.entry.key.local.name
                    && to.declaration.signature == from.declaration.signature
                    && to.declaration.body_hash == from.declaration.body_hash
                    && to.declaration.visibility == from.declaration.visibility
                    && to.declaration.decorators == from.declaration.decorators
            }) {
                let mut related = false;
                for side in [SideTag::Base, SideTag::Head] {
                    related = related
                        || self
                            .lineage(side, to.file, &to.class)?
                            .iter()
                            .any(|n| n == from.class.as_str())
                        || self
                            .lineage(side, from.file, &from.class)?
                            .iter()
                            .any(|n| n == to.class.as_str());
                }
                if related {
                    moved.insert(from.entry.key.clone());
                    moved.insert(to.entry.key.clone());
                }
            }
        }
        Ok(moved)
    }

    fn declared(&self, side: SideTag, file: &RepoPath, key: &DefKey) -> Result<Option<Def>> {
        Ok(facts_of(&self.ctx, side, &self.cache, file)?
            .and_then(|facts| facts.defs().find(|d| d.local == key.local).cloned()))
    }

    /// The names a class lists as bases on one side, and theirs in turn, within
    /// its file.
    fn lineage(&self, side: SideTag, file: &RepoPath, class: &Name) -> Result<Vec<String>> {
        let Some(facts) = facts_of(&self.ctx, side, &self.cache, file)? else {
            return Ok(Vec::new());
        };
        let mut seen: Vec<String> = Vec::new();
        let mut queue = vec![class.as_str().to_string()];
        while let Some(name) = queue.pop() {
            for def in facts
                .defs()
                .filter(|d| d.kind == DefKind::Class && d.local.name.as_str() == name)
            {
                for base in bases_in(def.signature.as_deref()) {
                    if !seen.contains(&base) {
                        seen.push(base.clone());
                        queue.push(base);
                    }
                }
            }
        }
        Ok(seen)
    }

    /// A field of a class is the class's line, edited, unless something outside
    /// the diff reads it: a new or edited class prints no line per field.
    fn folds_into_its_class(&self, changed: &Changed, def: &crate::resolve::Definition) -> bool {
        self.family == Family::Python
            && matches!(
                changed.kind,
                DefKind::Field | DefKind::Variable | DefKind::Constant
            )
            && !def.key.local.container.is_empty()
            && !matches!(changed.how, How::Edited { layout_only: true })
            && self.owner(def).is_some()
            && {
                let verdict = self.verdict(changed);
                verdict.complete() && verdict.uses.iter().all(|u| u.also_changed)
            }
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
                within: own_span(def.kind, def.span.lines),
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
                    let mut changed = own(class, Reach::Constructor);
                    changed.subjects.push(Subject {
                        key: def.key.clone(),
                        reach: Reach::Every,
                        within: own_span(def.kind, def.span.lines),
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
    /// asks who still names it. A side that could not be resolved is the reason
    /// the question went unasked, not an empty answer to it.
    fn base(&self) -> Result<&Resolver, Gap> {
        self.base
            .get_or_init(|| {
                Resolver::open(self.family, &self.ctx, SideTag::Base, &self.cache)
                    .map_err(|e| format!("{e:#}"))
            })
            .as_ref()
            .map_err(|reason| Gap::BaseUnresolved {
                reason: reason.clone(),
            })
    }

    pub fn changed(&self) -> &[Changed] {
        &self.changed
    }

    /// What the cell could not read, for the run's limits: a read that failed or
    /// a deadline. Files it read in part are said by the answer's own
    /// unanalysed list.
    pub fn stops(&self) -> Vec<LimitReason> {
        stops_in(&self.losses)
    }

    /// The runs of changed lines in the family's files that no tracked definition
    /// holds and that are code, not a blank line, a comment or an import: a
    /// module-level statement, the body of an `if __name__ == "__main__":`. The
    /// cell traces definitions, so what these lines change is not traced, and the
    /// answer says so. A file the range adds, and a test file, have no callers to
    /// lose.
    pub fn outside_definitions(&self) -> Vec<(String, Vec<(u32, u32)>)> {
        let added_files: HashSet<&str> = self
            .ctx
            .changes
            .changes
            .iter()
            .filter(|c| c.kind == ChangeKind::Added)
            .map(|c| c.path.as_str())
            .collect();
        let mut files: Vec<&String> = self.added.keys().collect();
        files.sort();
        let mut out = Vec::new();
        for file in files {
            if !self.family.has_path(file)
                || !within_scope(&self.scope, file)
                || added_files.contains(file.as_str())
                || crate::testfiles::is_test_path(std::path::Path::new(file))
            {
                continue;
            }
            let held: Vec<(u32, u32)> = self
                .defs_by_file
                .get(file)
                .into_iter()
                .flatten()
                .map(|at| self.head.definitions()[*at].span.lines)
                .collect();
            let imported: Vec<(u32, u32)> = self
                .bindings_by_file
                .get(file)
                .into_iter()
                .flatten()
                .map(|at| &self.head.bindings()[*at])
                .filter(|b| matches!(b.relation, Relation::Imports | Relation::Reexports))
                .map(|b| b.site.lines)
                .collect();
            let mut spans: Vec<(u32, u32)> = Vec::new();
            for (line, text) in &self.added[file] {
                let text = text.trim_start();
                let code = !text.is_empty()
                    && !["#", "//", "/*", "*"].iter().any(|c| text.starts_with(c))
                    && !held.iter().any(|s| within(*line, *s))
                    && !imported.iter().any(|s| within(*line, *s));
                if !code {
                    continue;
                }
                match spans.last_mut() {
                    Some(last) if last.1 + 1 == *line => last.1 = *line,
                    _ => spans.push((*line, *line)),
                }
            }
            if !spans.is_empty() {
                out.push((file.clone(), spans));
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
                    || subject.within.is_some_and(|own| {
                        b.file.as_str() == changed.file && within(b.site.lines.0, own)
                    })
                {
                    continue;
                }
                uses.extend(self.use_of(b));
            }
        }
        let (stale, base_gap) = self.stale_uses(changed);
        let covered: HashSet<(String, u32)> =
            stale.iter().map(|u| (u.file.clone(), u.line)).collect();
        uses.extend(stale);
        let unplaced = self.unplaced(changed, &covered);
        let mut gaps: Vec<Gap> = Vec::new();
        if changed.unpaired {
            gaps.push(Gap::Unpaired);
        } else if !unplaced.is_empty() {
            if !changed.stale.is_empty() && unplaced.len() <= MAX_UNPLACED_LISTED {
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
            } else {
                gaps.push(Gap::Unlanded {
                    uses: unplaced.len(),
                });
            }
        }
        gaps.extend(base_gap);
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
        Verdict {
            uses,
            coverage: self.coverage(changed, gaps),
        }
    }

    /// What covered the search: the gaps its definition found, the files outside
    /// the run's pathspecs that could name it, and a read the cell lost.
    fn coverage(&self, changed: &Changed, mut gaps: Vec<Gap>) -> Coverage {
        let outside = self
            .outside
            .iter()
            .filter(|file| self.ctx.workspace.may_reference(file, &changed.file))
            .count();
        if outside > 0 {
            gaps.push(Gap::OutsideScope { files: outside });
        }
        let reasons = self.stops();
        if !reasons.is_empty() {
            gaps.push(Gap::Stopped { reasons });
        }
        Coverage::of(gaps)
    }

    /// Uses at the head of a definition the range removed or moved away: lines
    /// the base resolved to it, which the head still spells and no longer lands.
    /// With them, the gap that kept the base side from saying all of it: it was
    /// not resolved, or what it read was cut short.
    fn stale_uses(&self, changed: &Changed) -> (Vec<Use>, Option<Gap>) {
        if changed.stale.is_empty() {
            return (Vec::new(), None);
        }
        let base = match self.base() {
            Ok(base) => base,
            Err(gap) => return (Vec::new(), Some(gap)),
        };
        let reasons = stops_in(base.losses());
        let gap = (!reasons.is_empty()).then_some(Gap::Stopped { reasons });
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
        (out, gap)
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
                    state.run.note(LimitReason::ReadFailed);
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
