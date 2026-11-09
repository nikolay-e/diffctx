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
//! own search, [`Coverages::searched`] and [`Coverage::scope`]), from the gaps the
//! search found, and an absence claim ([`Claim::Absent`]) or an exact
//! count ([`Count::Exact`]) holds it. Every other outcome names its [`Gap`]. A
//! failed read is an `Err` or a gap here, never an empty value: the lint that
//! says so is denied in this module.

#![deny(clippy::disallowed_methods)]

use std::cell::{OnceCell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use anyhow::Result;

use crate::facts::{
    Def, DefChange, DefKey, DefKind, Delta, Disambig, EntryKind, Export, Fact, FactsCache,
    Framework, Language, ModulePath, Name, Origin, ProjectId, Scope, SemanticDef, Visibility,
    facts_of, semantic_changes,
};
use crate::fold::{self, Basis, Rule};
use crate::pipeline::ScoredState;
use crate::resolve::{
    Binding, CResolver, CandidateReason, ConfigResolver, Definition, DockerfileResolver,
    EvidenceKind, GoResolver, JavaResolver, KotlinResolver, PhpResolver, PythonResolver, Relation,
    Resolution, RustResolver, ShellResolver, TerraformResolver, TypeScriptResolver,
    UnresolvedReason,
};
use crate::resource::LimitReason;
use crate::snapshot::{
    self, Admission, AdmissionPolicy, AnalysisContext, ChangeKind, Loss, RangeSpec, RepoPath,
    RunBudget, Side, SideTag, WorkspaceModel,
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
const OVERLOADED: &str = "overloaded";
const DISPATCH: &str = "dispatch";
const MACRO: &str = "macro";
const ADL: &str = "adl";
const ANY_NAMESPACE: &str = "any_namespace";
const NAME_SUFFIX: &str = "name_suffix";
const LABEL_SELECTOR: &str = "label_selector";
const NO_RELEASE: &str = "no_release";
const CASE_FOLD: &str = "case_fold";

/// The languages one resolver reads: a cell answers for one family.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Family {
    Python,
    TypeScript,
    Rust,
    Kotlin,
    Java,
    Go,
    Php,
    C,
    /// YAML files read as configuration objects: what an object's fields name,
    /// by the table of the facts layer.
    Config,
    /// Terraform files: a module is a directory, and a name is a traversal's
    /// root and its attributes.
    Terraform,
    /// A Dockerfile: its stages, the variables and places its instructions name.
    Dockerfile,
    /// Shell scripts: functions and variables, the files a script sources.
    Shell,
}

impl Family {
    const ALL: [Self; 12] = [
        Self::Python,
        Self::TypeScript,
        Self::Rust,
        Self::Kotlin,
        Self::Java,
        Self::Go,
        Self::Php,
        Self::C,
        Self::Config,
        Self::Terraform,
        Self::Dockerfile,
        Self::Shell,
    ];

    fn has(self, language: Language) -> bool {
        match self {
            Self::Python => language == Language::Python,
            Self::TypeScript => matches!(language, Language::JavaScript | Language::TypeScript),
            Self::Rust => language == Language::Rust,
            Self::Kotlin => language == Language::Kotlin,
            Self::Java => language == Language::Java,
            Self::Go => language == Language::Go,
            Self::Php => language == Language::Php,
            Self::C => matches!(language, Language::C | Language::Cpp),
            Self::Config => language.is_config_family(),
            Self::Terraform => language == Language::Terraform,
            Self::Dockerfile => language == Language::Dockerfile,
            Self::Shell => matches!(language, Language::Shell | Language::ShellUnread),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Python => "Python",
            Self::TypeScript => "TypeScript/JavaScript",
            Self::Rust => "Rust",
            Self::Kotlin => "Kotlin",
            Self::Java => "Java",
            Self::Go => "Go",
            Self::Php => "PHP",
            Self::C => "C",
            Self::Config => "configuration",
            Self::Terraform => "Terraform",
            Self::Dockerfile => "Dockerfile",
            Self::Shell => "shell",
        }
    }

    /// Whether a file of this extension is read by the family's resolver.
    fn has_path(self, path: &str) -> bool {
        // A YAML file is the configuration cell's only where it holds objects
        // (`Cell::owns` reads that off the resolver): the rest of the YAML of a
        // repository (workflows, compose files, values) is read by other paths.
        if self == Self::Config {
            return false;
        }
        RepoPath::new(path)
            .is_ok_and(|p| Language::from_path(&p).is_some_and(|language| self.has(language)))
    }
}

/// The resolver of a family, behind the one set of questions a cell asks.
enum Resolver {
    Python(PythonResolver),
    TypeScript(TypeScriptResolver),
    Rust(RustResolver),
    Kotlin(KotlinResolver),
    Java(JavaResolver),
    Go(GoResolver),
    Php(PhpResolver),
    C(CResolver),
    Config(ConfigResolver),
    Terraform(TerraformResolver),
    Dockerfile(DockerfileResolver),
    Shell(ShellResolver),
}

impl Resolver {
    fn open(
        family: Family,
        ctx: &AnalysisContext,
        side: SideTag,
        cache: &FactsCache,
    ) -> Result<Self> {
        let _open = crate::profile::detail(
            match side {
                SideTag::Head => "resolver.open.head",
                SideTag::Base => "resolver.open.base",
            },
            family.name(),
        );
        Ok(match family {
            Family::Python => Self::Python(PythonResolver::open(ctx, side, cache)?),
            Family::TypeScript => Self::TypeScript(TypeScriptResolver::open(ctx, side, cache)?),
            Family::Rust => Self::Rust(RustResolver::open(ctx, side, cache)?),
            Family::Kotlin => Self::Kotlin(KotlinResolver::open(ctx, side, cache)?),
            Family::Java => Self::Java(JavaResolver::open(ctx, side, cache)?),
            Family::Go => Self::Go(GoResolver::open(ctx, side, cache)?),
            Family::Php => Self::Php(PhpResolver::open(ctx, side, cache)?),
            Family::C => Self::C(CResolver::open(ctx, side, cache)?),
            Family::Config => Self::Config(ConfigResolver::open(ctx, side, cache)?),
            Family::Terraform => Self::Terraform(TerraformResolver::open(ctx, side, cache)?),
            Family::Dockerfile => Self::Dockerfile(DockerfileResolver::open(ctx, side, cache)?),
            Family::Shell => Self::Shell(ShellResolver::open(ctx, side, cache)?),
        })
    }

    fn bindings(&self) -> &[Binding] {
        match self {
            Self::Python(r) => r.bindings(),
            Self::TypeScript(r) => r.bindings(),
            Self::Rust(r) => r.bindings(),
            Self::Kotlin(r) => r.bindings(),
            Self::Java(r) => r.bindings(),
            Self::Go(r) => r.bindings(),
            Self::Php(r) => r.bindings(),
            Self::C(r) => r.bindings(),
            Self::Config(r) => r.bindings(),
            Self::Terraform(r) => r.bindings(),
            Self::Dockerfile(r) => r.bindings(),
            Self::Shell(r) => r.bindings(),
        }
    }

    fn definitions(&self) -> &[Definition] {
        match self {
            Self::Python(r) => r.definitions(),
            Self::TypeScript(r) => r.definitions(),
            Self::Rust(r) => r.definitions(),
            Self::Kotlin(r) => r.definitions(),
            Self::Java(r) => r.definitions(),
            Self::Go(r) => r.definitions(),
            Self::Php(r) => r.definitions(),
            Self::C(r) => r.definitions(),
            Self::Config(r) => r.definitions(),
            Self::Terraform(r) => r.definitions(),
            Self::Dockerfile(r) => r.definitions(),
            Self::Shell(r) => r.definitions(),
        }
    }

    fn references_to(&self, key: &DefKey) -> Vec<&Binding> {
        match self {
            Self::Python(r) => r.references_to(key),
            Self::TypeScript(r) => r.references_to(key),
            Self::Rust(r) => r.references_to(key),
            Self::Kotlin(r) => r.references_to(key),
            Self::Java(r) => r.references_to(key),
            Self::Go(r) => r.references_to(key),
            Self::Php(r) => r.references_to(key),
            Self::C(r) => r.references_to(key),
            Self::Config(r) => r.references_to(key),
            Self::Terraform(r) => r.references_to(key),
            Self::Dockerfile(r) => r.references_to(key),
            Self::Shell(r) => r.references_to(key),
        }
    }

    /// Whether a removal in `file` is a change of what the project exports. Rust
    /// exports only through modules declared `pub` from a library root, Go through
    /// packages other than `main` and those under `internal/`; a file
    /// outside every crate (no manifest names it) and one the head no longer
    /// holds are taken to have exported.
    fn exports(&self, file: &str, workspace: &WorkspaceModel) -> bool {
        match self {
            Self::Rust(r) => {
                r.is_public_module(file)
                    || workspace.rust_module(file).is_none()
                    || !r.definitions().iter().any(|d| d.file.as_str() == file)
            }
            Self::Go(r) => {
                r.is_public_package(file)
                    || !r.definitions().iter().any(|d| d.file.as_str() == file)
            }
            _ => true,
        }
    }

    fn losses(&self) -> &[Loss] {
        match self {
            Self::Python(r) => r.losses(),
            Self::TypeScript(r) => r.losses(),
            Self::Rust(r) => r.losses(),
            Self::Kotlin(r) => r.losses(),
            Self::Java(r) => r.losses(),
            Self::Go(r) => r.losses(),
            Self::Php(r) => r.losses(),
            Self::C(r) => r.losses(),
            Self::Config(r) => r.losses(),
            Self::Terraform(r) => r.losses(),
            Self::Dockerfile(r) => r.losses(),
            Self::Shell(r) => r.losses(),
        }
    }

    /// The module calls that name a source outside the repository.
    fn external_sources(&self) -> usize {
        match self {
            Self::Terraform(r) => r.external_sources(),
            _ => 0,
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
    /// A trait's member, or the implementation of one: calls arrive through
    /// `dyn Trait` and generic bounds, which only candidates follow.
    Dispatched { via: String },
    /// `files` that could name the definition reach it through macros or define it
    /// under conditional compilation (Rust: `cfg_if!`, which defines items by
    /// expansion): what the preprocessor makes of it is not read.
    Macros { files: usize, what: &'static str },
    /// The answer cap shed the definition: its callers were found, not listed.
    Capped,
    /// A route no model traces can reach it: dispatch, an untyped receiver, a
    /// language no binding model reads.
    NotTraced,
    /// `files` hold a template (Helm, Go template files) that was read around,
    /// not expanded: what it renders is not known.
    Templated { files: usize },
    /// `files` of a chart or release whose values or context are not determined:
    /// a key that no release is known to read is not an unread key.
    ContextUnbound { files: usize },
    /// The object is generated (a Kustomize generator, `count` or `for_each`,
    /// a rendered chart): its name is not written in any file.
    #[allow(dead_code, reason = "the config cells of wave 7 build it")]
    RenderedElsewhere { files: usize },
    /// `pointers` name a source outside the repository (a remote repository,
    /// a registry module, an image): what they reach is not held.
    ExternalSource { pointers: usize },
    /// `files` run or read a path the shell computes at run time (`source
    /// "$x"`, `eval`): what they reach is not named in the text.
    SourceFlag { files: usize },
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
            Self::Dispatched { via } => write!(
                f,
                "it is a member of `{via}`, whose calls through `dyn {via}` or a bound are candidates"
            ),
            Self::Macros { files, what } => {
                write!(f, "{files} file(s) that could name it {what}")
            }
            Self::Capped => f.write_str("the answer cap shed it"),
            Self::NotTraced => f.write_str("a route no model traces can reach it"),
            Self::Templated { files } => {
                write!(f, "{files} template file(s) were read around, not expanded")
            }
            Self::ContextUnbound { files } => write!(
                f,
                "{files} file(s) of a chart or release whose context is not determined"
            ),
            Self::RenderedElsewhere { files } => write!(
                f,
                "{files} file(s) generate objects whose names are not written in any file"
            ),
            Self::ExternalSource { pointers } => write!(
                f,
                "{pointers} source(s) outside the repository were not followed"
            ),
            Self::SourceFlag { files } => {
                write!(f, "{files} file(s) read a path computed at run time")
            }
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

    /// The search for one relation's uses outside the cell: the cell's own
    /// coverage of it when the cell answered, the stop that cut the search short
    /// or a route nothing traces when the answer names one, and the files that
    /// spell the name but were not analysed whole.
    fn searching(
        cell: Option<Self>,
        stopped: Option<&[LimitReason]>,
        not_traced: bool,
        unread_files: usize,
    ) -> Self {
        let mut gaps: Vec<Gap> = cell.map_or_else(Vec::new, |c| c.gaps().to_vec());
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

    /// Both coverages at once: complete only when each is, and then the witness
    /// of the first. It makes no witness.
    pub fn both(self, other: Self) -> Self {
        match (self, other) {
            (Self::Complete(witness), Self::Complete(_)) => Self::Complete(witness),
            (first, second) => {
                let mut gaps = first.gaps().to_vec();
                for gap in second.gaps() {
                    if !gaps.contains(gap) {
                        gaps.push(gap.clone());
                    }
                }
                Self::Partial(gaps)
            }
        }
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

/// What the search covered of a definition's uses, by the two things an answer
/// says about them: its calls (calls, constructions, overrides) and its reads
/// (reads, writes, a value passed on). One can be known while the other is not:
/// an untyped read of a name leaves its reads undetermined and its calls found.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Coverages {
    calls: Coverage,
    reads: Coverage,
}

impl Coverages {
    /// A search incomplete for one reason it already knows, of both.
    pub fn partial(gap: Gap) -> Self {
        Self {
            calls: Coverage::partial(gap.clone()),
            reads: Coverage::partial(gap),
        }
    }

    /// What the cell covered of a definition.
    pub fn of_verdict(verdict: &Verdict) -> Self {
        Self {
            calls: verdict.calls(),
            reads: verdict.reads(),
        }
    }

    /// The search for one definition's uses outside the cell, each relation
    /// group starting from the cell's coverage of it (see `Coverage::searching`).
    pub fn searched(
        cell: Option<&Verdict>,
        stopped: Option<&[LimitReason]>,
        not_traced: bool,
        unread_files: usize,
    ) -> Self {
        Self {
            calls: Coverage::searching(cell.map(Verdict::calls), stopped, not_traced, unread_files),
            reads: Coverage::searching(cell.map(Verdict::reads), stopped, not_traced, unread_files),
        }
    }

    pub const fn calls(&self) -> &Coverage {
        &self.calls
    }

    pub const fn reads(&self) -> &Coverage {
        &self.reads
    }

    /// Both groups, as one coverage: what a claim about every use needs.
    pub fn all(&self) -> Coverage {
        self.calls.clone().both(self.reads.clone())
    }
}

/// The relations of a use: the ones a change to a definition reaches its readers
/// and callers through (`Relation::uses`).
const USES: [Relation; 6] = [
    Relation::Calls,
    Relation::Instantiates,
    Relation::Overrides,
    Relation::Reads,
    Relation::Writes,
    Relation::PassesAsValue,
];

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
    /// The rule of the folding table that says the change cannot have affected
    /// this row; the answer folds it unless asked to list every row.
    pub folded: Option<Rule>,
    /// For a candidate: how many definitions share the name it may be.
    pub sharing: usize,
}

/// The uses of a definition and what covered the search for them: the one
/// coverage of the relations that use it (`Relation::uses`), which is every
/// relation an absence is claimed for.
pub struct Verdict {
    pub uses: Vec<Use>,
    by_relation: [(Relation, Coverage); 6],
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

    /// What covered the search of one relation of use.
    pub fn relation(&self, relation: Relation) -> &Coverage {
        self.by_relation
            .iter()
            .find(|(r, _)| *r == relation)
            .map_or(&self.by_relation[0].1, |(_, coverage)| coverage)
    }

    fn merged<const N: usize>(&self, relations: [Relation; N]) -> Coverage {
        relations
            .into_iter()
            .map(|r| self.relation(r).clone())
            .reduce(Coverage::both)
            .unwrap_or_else(|| self.by_relation[0].1.clone())
    }

    /// The calls of the definition: calls, constructions and overrides.
    pub fn calls(&self) -> Coverage {
        self.merged([Relation::Calls, Relation::Instantiates, Relation::Overrides])
    }

    /// The reads of the definition: reads, writes and a value passed on.
    pub fn reads(&self) -> Coverage {
        self.merged([Relation::Reads, Relation::Writes, Relation::PassesAsValue])
    }

    /// Every relation of use at once.
    pub fn coverage(&self) -> Coverage {
        self.calls().both(self.reads())
    }

    /// The gap the answer names first.
    pub fn gap(&self) -> Option<Gap> {
        self.coverage().gaps().first().cloned()
    }

    /// The strongest thing the evidence carries of one relation. An absence is
    /// the one claim only a complete search of that relation makes.
    pub fn claim(&self, relation: Relation) -> Claim {
        if self.resolved().any(|u| u.relation == relation) {
            Claim::Resolved
        } else if self.candidates().any(|u| u.relation == relation) {
            Claim::Candidate
        } else {
            match self.relation(relation) {
                Coverage::Complete(witness) => Claim::Absent(*witness),
                Coverage::Partial(gaps) => match gaps.first() {
                    Some(Gap::Unlanded { .. }) => Claim::Unresolved,
                    _ => Claim::Partial,
                },
            }
        }
    }

    pub fn complete(&self) -> bool {
        self.coverage().witness().is_some()
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
/// The labels of a configuration object: a definition of its own, under it.
fn is_label_definition(key: &DefKey) -> bool {
    key.local.container.len() == 1 && matches!(key.local.name.as_str(), "labels" | "pod-labels")
}

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
    /// What the range did to the definition, for the folding table; none when
    /// it was removed, renamed, moved, re-scoped, ambiguously paired or added.
    basis: Option<Basis>,
    /// A function, method or class with decorators: a decorator may read what a
    /// doc or layout edit touched, so such an edit is never set aside.
    pub decorated: bool,
    /// What the range did moves the definition's place in the module's surface: it
    /// was added, renamed or moved, or its signature (which carries its kind) or
    /// its visibility changed. A body, doc or decorator edit leaves it. In Kotlin
    /// a definition not public at the head has no place in the surface to move.
    pub api: bool,
}

impl Changed {
    /// The parameters a compatible signature change added, when it was one.
    pub fn new_parameters(&self) -> Vec<String> {
        match &self.basis {
            Some(Basis {
                compatible: Some(names),
                ..
            }) => names.clone(),
            _ => Vec::new(),
        }
    }
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
    /// The name the answer prints beside the file. A shell script is the definition
    /// of its own file: the file names it, and the path a second time would make it
    /// two names.
    pub fn symbol(&self) -> Option<String> {
        (!(self.kind == DefKind::Module && self.name == self.file)).then(|| self.display_name())
    }

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
    /// Rust files that invoke `cfg_if!`.
    macro_files: HashSet<String>,
    /// Python files that import a library which executes annotations at run time.
    runtime_files: RefCell<HashMap<String, bool>>,
    /// Names with uses that landed on nothing, by name: the files.
    /// The file, line, owner and whether it reads a property through a receiver
    /// nothing types.
    unlanded: HashMap<String, Vec<(String, u32, Scope, bool, Relation)>>,
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
        read_fault: crate::resource::test_read_fault(),
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
            CandidateReason::Overloaded => OVERLOADED,
            CandidateReason::Dispatch => DISPATCH,
            CandidateReason::Macro => MACRO,
            CandidateReason::Adl => ADL,
            CandidateReason::AnyNamespace => ANY_NAMESPACE,
            CandidateReason::NameSuffix => NAME_SUFFIX,
            CandidateReason::LabelSelector => LABEL_SELECTOR,
            CandidateReason::NoRelease => NO_RELEASE,
            CandidateReason::CaseFold => CASE_FOLD,
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
            macro_files: HashSet::new(),
            runtime_files: RefCell::new(HashMap::new()),
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
        let mut unlanded: Vec<(String, String, u32, Scope, bool, Relation)> = Vec::new();
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
            // A configuration file that only names places (a Kustomize file) is
            // not the cell's: it holds no object a use could land on.
            if !(self.family == Family::Config && b.relation == Relation::Imports) {
                self.files.insert(file.to_string());
            }
            if (self.family == Family::Rust
                && b.relation == Relation::Calls
                && b.name.as_str() == "cfg_if")
                || (self.family == Family::C
                    && matches!(
                        b.resolution,
                        Resolution::Candidate {
                            reason: CandidateReason::Macro,
                            ..
                        }
                    ))
            {
                self.macro_files.insert(file.to_string());
            }
            self.bindings_by_file
                .entry(file.to_string())
                .or_default()
                .push(at);
            let line = b.site.lines.0;
            match &b.resolution {
                Resolution::Resolved { target, .. } => {
                    // A Rust test reaches a type through the function that names it
                    // in a signature or a type argument (`size_of::<T>()`).
                    if b.relation.uses()
                        || (self.family == Family::Rust && b.relation == Relation::TypeUses)
                    {
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
                            b.relation,
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
        for (name, file, line, from, read, relation) in unlanded {
            if read && placed.contains(&(file.clone(), from.clone(), name.clone())) {
                continue;
            }
            self.unlanded
                .entry(name)
                .or_default()
                .push((file, line, from, read, relation));
        }
        self.losses = self.head.losses().to_vec();
    }

    /// What the range did to the family's definitions: the cell's changed list.
    fn read_changes(&mut self) -> Result<()> {
        let set = semantic_changes(&self.ctx, &self.cache)?;
        self.losses.extend(set.losses.iter().cloned());
        let (reshaped, away) = self.reshaped_defaults(&set.defs)?;
        let python: Vec<_> = set
            .defs
            .iter()
            .filter(|d| !away.contains(&d.key))
            .chain(reshaped.iter())
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
        // A decorated function, method or class is never set aside as a doc or
        // layout edit: a decorator may read the docstring (CLI help, routes). A
        // docstring edit lists its rows; a comment-only one is traced and its rows
        // fold as no code change (`basis_of`).
        let mut decorated: HashSet<DefKey> = HashSet::new();
        for d in &python {
            let only_doc = matches!(&d.change, DefChange::Changed { origin: Origin::Same, delta }
                if !(delta.signature || delta.body || delta.visibility || delta.decorators));
            if only_doc && self.decorated_at_head(d) {
                decorated.insert(d.key.clone());
            }
        }
        let layout_only = |d: &SemanticDef| {
            matches!(&d.change, DefChange::Changed { origin: Origin::Same, delta }
                if !(delta.signature || delta.body || delta.visibility || delta.decorators))
                && !decorated.contains(&d.key)
        };
        for d in &python {
            if !layout_only(d) {
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
        // The members of a Rust type that moved (an enum's variants, a struct's
        // fields) went with it: they are no removals of their own.
        let family = self.family;
        let holder_moved = |key: &DefKey| {
            let Some((name, outer)) = key.local.container.split_last() else {
                return false;
            };
            family == Family::Rust
                && python.iter().any(|c| {
                    matches!(&c.change, DefChange::Changed { origin: Origin::Moved { from }, .. }
                        if from.project == key.project
                            && from.module == key.module
                            && from.language == key.language
                            && from.local.name == *name
                            && from.local.container.as_slice() == outer)
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
                    if holder_moved(&d.key) || self.moved_within_type(&d.key, kind) {
                        continue;
                    }
                    let gone = Changed {
                        name: self.shown_name(kind, d.key.local.name.as_str(), file.as_str()),
                        kind,
                        file: file.as_str().to_string(),
                        lines,
                        how: How::Removed,
                        subjects: Vec::new(),
                        stale: vec![d.key.clone()],
                        unpaired,
                        local: false,
                        public,
                        basis: None,
                        decorated: false,
                        api: false,
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
                                    basis: None,
                                    decorated: false,
                                    api: false,
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
                                basis: None,
                                decorated: false,
                                api: false,
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
                        _ => How::Edited {
                            layout_only: layout_only(d),
                        },
                    };
                    let basis = match change {
                        DefChange::Changed {
                            origin: Origin::Same,
                            delta,
                        } => self.basis_of(d, def, *delta),
                        _ => None,
                    };
                    let mut stale = match change {
                        DefChange::Changed {
                            origin: Origin::Moved { from } | Origin::Renamed { from },
                            ..
                        } => vec![from.clone()],
                        _ => Vec::new(),
                    };
                    // A Rust definition that moved while a file still names the old
                    // path is a removal as well: what names it broke, and says so
                    // under the old path, not as a caller of the new one.
                    if let (
                        Family::Rust,
                        DefChange::Changed {
                            origin: Origin::Moved { from },
                            ..
                        },
                        Some(base_file),
                    ) = (self.family, change, d.base.as_ref())
                    {
                        if let Some((kind, lines, public)) =
                            self.base_definition(base_file, from)?
                        {
                            let gone = Changed {
                                name: from.local.name.as_str().to_string(),
                                kind,
                                file: base_file.as_str().to_string(),
                                lines,
                                how: How::Removed,
                                subjects: Vec::new(),
                                stale: vec![from.clone()],
                                unpaired: false,
                                local: false,
                                public,
                                basis: None,
                                decorated: false,
                                api: false,
                            };
                            if !self.stale_uses(&gone).0.is_empty() {
                                removed.push(gone);
                                stale = Vec::new();
                            }
                        }
                    }
                    // The selectors that selected labels at the base, and select
                    // nothing after the edit, are the readers the edit broke: the
                    // key is its own stale name.
                    if self.family == Family::Config
                        && is_label_definition(&d.key)
                        && matches!(change, DefChange::Changed { origin: Origin::Same, delta }
                            if delta.body)
                    {
                        stale = vec![d.key.clone()];
                    }
                    let unpaired = matches!(change, DefChange::Unknown { .. });
                    // A default export that only changed its form is one export: its
                    // place in the surface moves with its signature, not its name.
                    let reshaped_here = reshaped.iter().any(|r| r.key == d.key);
                    let api = self.surface_moved(d, change, reshaped_here);
                    for mut changed in self.report_as(def, how, stale, unpaired, true) {
                        changed.basis.clone_from(&basis);
                        changed.decorated = decorated.contains(&d.key);
                        changed.api =
                            api && changed.subjects.first().is_some_and(|s| s.key == d.key);
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

    /// Whether a Rust member the head's `impl` no longer has is still a member of
    /// its type: another `impl` of that type, in any module, has one of its name
    /// and kind. An inherent member is matched by an inherent one and a trait
    /// impl's by the impl of the same trait: a removed inherent method is a
    /// removal whatever a trait impl of the type has under the same name.
    fn moved_within_type(&self, key: &DefKey, kind: DefKind) -> bool {
        let Resolver::Rust(head) = &self.head else {
            return false;
        };
        if key.local.container.is_empty() {
            return false;
        }
        let Ok(Resolver::Rust(base)) = self.base() else {
            return false;
        };
        let Some(owner) = base.owner_of(key) else {
            return false;
        };
        head.definitions().iter().any(|d| {
            d.kind == kind
                && d.key.local.name == key.local.name
                && d.key.local.disambig == key.local.disambig
                && head.owner_of(&d.key) == Some(owner)
        })
    }

    /// Whether the range moved the definition's place in the module's surface: it
    /// is new, moved to another file or container, renamed, or has another
    /// signature or visibility than at the base. A body, doc or layout edit is
    /// none, and a default export that only changed its form (`reshaped`) moves
    /// with its signature and visibility alone. In Kotlin (JVM callers and the
    /// modules that depend on them name it)
    /// it also has to be public at the head.
    fn surface_moved(&self, d: &SemanticDef, change: &DefChange, reshaped: bool) -> bool {
        let moved = match change {
            DefChange::Changed { delta, .. } if reshaped => delta.signature || delta.visibility,
            DefChange::Changed { origin, delta } => {
                !matches!(origin, Origin::Same) || delta.signature || delta.visibility
            }
            _ => true,
        };
        moved
            && (self.family != Family::Kotlin
                || d.head.as_ref().is_some_and(|file| {
                    self.declared(SideTag::Head, file, &d.key)
                        .unwrap_or(None)
                        .is_some_and(|def| def.visibility == Visibility::Public)
                }))
    }

    /// The annotations a Kotlin definition states at the head, by where it is:
    /// the JVM entry points a framework calls are read off them. `None` for
    /// another family or a definition the facts do not hold.
    pub fn annotations_at(&self, file: &str, name: &str, lines: (u32, u32)) -> Option<Vec<String>> {
        if self.family != Family::Kotlin {
            return None;
        }
        let def = self.head.definitions().iter().find(|d| {
            d.file.as_str() == file
                && d.key.local.name.as_str() == name
                && d.span.lines.0 <= lines.1
                && lines.0 <= d.span.lines.1
        })?;
        let found = self
            .declared(SideTag::Head, &def.file, &def.key)
            .unwrap_or(None)?;
        Some(
            found
                .decorators
                .iter()
                .map(|n| n.as_str().to_string())
                .collect(),
        )
    }

    /// The entries a framework makes of the definition at the head, by where it
    /// is: what the facts state of who calls it (a route, a scheduled job, a
    /// command, a tool, a fixture). Empty for a definition no rule matches, and
    /// for one the facts do not hold.
    pub fn framework_entries_at(
        &self,
        file: &str,
        name: &str,
        lines: (u32, u32),
    ) -> Vec<(EntryKind, Framework)> {
        let Some(def) = self.head.definitions().iter().find(|d| {
            d.file.as_str() == file
                && d.key.local.name.as_str() == name
                && d.span.lines.0 <= lines.1
                && lines.0 <= d.span.lines.1
        }) else {
            return Vec::new();
        };
        let Ok(Some(facts)) = facts_of(&self.ctx, SideTag::Head, &self.cache, &def.file) else {
            return Vec::new();
        };
        facts
            .blob
            .facts
            .iter()
            .filter_map(|fact| match fact {
                Fact::EntryPoint(entry) if entry.target == Scope::Def(def.key.local.clone()) => {
                    Some((entry.kind, entry.framework.clone()?))
                }
                _ => None,
            })
            .collect()
    }

    /// Whether the definition at the head is a function, method or class with
    /// decorators.
    fn decorated_at_head(&self, d: &SemanticDef) -> bool {
        let Some(file) = d.head.as_ref() else {
            return false;
        };
        // A definition whose facts cannot be read is not known to be undecorated:
        // `true` keeps it listed.
        let Ok(found) = self.declared(SideTag::Head, file, &d.key) else {
            return true;
        };
        found.is_some_and(|def| {
            !def.decorators.is_empty()
                && matches!(
                    def.kind,
                    DefKind::Function | DefKind::Method | DefKind::Constructor | DefKind::Class
                )
        })
    }

    /// What an edit in place did to a definition, as the folding table reads it.
    /// None when the facts of either side are unread, or when the edit moved the
    /// definition's visibility or only its docstring (a decorator may read it): then
    /// nothing folds. A comment-only edit has a basis with no code delta.
    fn basis_of(&self, d: &SemanticDef, def: &Definition, delta: Delta) -> Option<Basis> {
        // The folding table has no row for a configuration definition: one changed
        // key reaching one reader is the whole answer, whatever the kind says
        // (a Helm named template and a shell function are `Function`s).
        if matches!(
            self.family,
            Family::Config | Family::Terraform | Family::Dockerfile | Family::Shell
        ) {
            return None;
        }
        let code = delta.signature || delta.body || delta.decorators;
        if delta.visibility || (!code && delta.doc) {
            return None;
        }
        let (head, base) = (d.head.as_ref()?, d.base.as_ref()?);
        let at_head = self.declared(SideTag::Head, head, &d.key).unwrap_or(None)?;
        let compatible = if delta.signature && !delta.decorators {
            let at_base = self.declared(SideTag::Base, base, &d.key).unwrap_or(None)?;
            fold::compatible(
                at_base.signature.as_deref()?,
                at_head.signature.as_deref()?,
                d.key.local.name.as_str(),
                d.key.language,
            )
        } else {
            None
        };
        Some(Basis {
            kind: def.kind,
            signature: delta.signature,
            body: delta.body,
            decorators: delta.decorators,
            compatible,
            typed: fold::declared_types(
                def.kind,
                d.key.language,
                at_head.signature.as_deref(),
                d.key.local.name.as_str(),
            ),
        })
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

    /// The module's default export when it changed its form and nothing else: `export
    /// default (a) => …` became `export default function name(a) { … }`, or `export
    /// default f` over a constant. The facts see the removal of `default` and a new
    /// named definition; the export is one, so the two are one rename of it (with the
    /// signature compared, since the name was never part of it), and neither is an
    /// export removed or added. The second list holds the keys the pair replaces.
    fn reshaped_defaults(&self, defs: &[SemanticDef]) -> Result<(Vec<SemanticDef>, Vec<DefKey>)> {
        let mut pairs = Vec::new();
        let mut away = Vec::new();
        if self.family != Family::TypeScript {
            return Ok((pairs, away));
        }
        for gone in defs.iter().filter(|d| {
            matches!(d.change, DefChange::Removed)
                && d.head.is_none()
                && d.key.local.name.as_str() == "default"
                && d.key.local.container.is_empty()
        }) {
            let Some(file) = gone.base.as_ref() else {
                continue;
            };
            let Some(head) = facts_of(&self.ctx, SideTag::Head, &self.cache, file)? else {
                continue;
            };
            let Some(local) = head.blob.facts.iter().find_map(|fact| match fact {
                Fact::Export(Export::Default { local: Some(l), .. }) => Some(l.clone()),
                _ => None,
            }) else {
                continue;
            };
            let Some(added) = defs.iter().find(|d| {
                matches!(d.change, DefChange::Added)
                    && d.head.as_ref() == Some(file)
                    && d.key.local.name == local
                    && d.key.local.container.is_empty()
                    && d.key.project == gone.key.project
                    && d.key.module == gone.key.module
            }) else {
                continue;
            };
            let before = self.declared(SideTag::Base, file, &gone.key)?;
            let after = self.declared(SideTag::Head, file, &added.key)?;
            let (Some(before), Some(after)) = (before, after) else {
                continue;
            };
            pairs.push(SemanticDef {
                key: added.key.clone(),
                change: DefChange::Changed {
                    origin: Origin::Renamed {
                        from: gone.key.clone(),
                    },
                    delta: Delta {
                        signature: before.kind != after.kind || before.signature != after.signature,
                        body: true,
                        ..Delta::default()
                    },
                },
                base: Some(file.clone()),
                head: Some(file.clone()),
            });
            away.push(gone.key.clone());
            away.push(added.key.clone());
        }
        Ok((pairs, away))
    }

    /// The name the answer gives a definition: a script is named by its path, its
    /// definition being the program of the file.
    fn shown_name(&self, kind: DefKind, name: &str, file: &str) -> String {
        if self.family == Family::Shell && kind == DefKind::Module {
            file.to_string()
        } else {
            name.to_string()
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
            name: self.shown_name(def.kind, def.key.local.name.as_str(), def.file.as_str()),
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
            basis: None,
            decorated: false,
            api: false,
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
            held && def.visibility == Visibility::Public
                && self.head.exports(file.as_str(), &self.ctx.workspace),
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
                let code = text.chars().any(char::is_alphanumeric)
                    && !["#", "//", "/*", "*", "@"]
                        .iter()
                        .any(|c| text.starts_with(c))
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

    /// Whether what consumes the family's files lies outside the code the cell
    /// reads: a CI workflow, a compose file or a script runs a Dockerfile, and
    /// a tool applies a configuration. The cell traces the relations inside and
    /// between the files; their consumers are reached only by a mention.
    pub fn consumed_outside(&self) -> bool {
        matches!(self.family, Family::Config | Family::Dockerfile)
    }

    /// Whether the resolver read the file whole: its definitions and uses are
    /// the cell's, whatever the fragments made of it. A file the grammar read
    /// only in part has uses the cell never saw, so it stays a name's unread
    /// namer.
    pub fn read(&self, rel: &str) -> bool {
        self.files.contains(rel)
            && !self.losses.iter().any(|loss| {
                matches!(
                    loss,
                    Loss::ParseError { path, .. } | Loss::GrammarOutdated { path, .. }
                        if path.as_str() == rel
                )
            })
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
            folded: None,
            sharing: b.targets().len(),
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

    /// `export { a } from './m'` and `from .m import a as a` in a package's
    /// `__init__` offer a changed definition on: a module that re-exports it is
    /// where it reaches the package's users.
    fn reexports(&self, b: &Binding) -> bool {
        matches!(self.family, Family::TypeScript | Family::Python)
            && b.relation == Relation::Reexports
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
        // What the search could not tell of every relation, then what it could not
        // tell of one: a use that landed on nothing is a gap of the relation it
        // would have been, a read of a function leaves its calls known.
        let mut lead: Vec<Gap> = Vec::new();
        let mut per: Vec<(Relation, Gap)> = Vec::new();
        if changed.unpaired {
            lead.push(Gap::Unpaired);
        } else if !unplaced.is_empty() {
            if !changed.stale.is_empty() && unplaced.len() <= MAX_UNPLACED_LISTED {
                // A name that left, or moved, may still be spelled where nothing
                // types the receiver: each such line may be a use that broke.
                uses.extend(unplaced.into_iter().map(|(file, line, scope, _)| Use {
                    // Still named at the head, so possibly broken, whoever changed it.
                    also_changed: false,
                    file,
                    line,
                    relation: Relation::Reads,
                    standing: Standing::Candidate(UNTYPED_RECEIVER),
                    scope,
                    folded: None,
                    sharing: 0,
                }));
            } else {
                for relation in USES {
                    let landed_on_nothing = unplaced.iter().filter(|u| u.3 == relation).count();
                    if landed_on_nothing > 0 {
                        per.push((
                            relation,
                            Gap::Unlanded {
                                uses: landed_on_nothing,
                            },
                        ));
                    }
                }
            }
        }
        uses.sort_by(|a, b| {
            (a.file.as_str(), a.line, a.relation.name()).cmp(&(
                b.file.as_str(),
                b.line,
                b.relation.name(),
            ))
        });
        uses.dedup_by(|a, b| a.file == b.file && a.line == b.line && a.standing == b.standing);
        if let Some(basis) = &changed.basis {
            for u in uses.iter_mut().filter(|u| !u.also_changed) {
                let candidate = matches!(u.standing, Standing::Candidate(_));
                let runtime = self.runtime_annotation(u);
                u.folded = fold::rule(basis, u.relation, candidate, runtime);
            }
        }
        Verdict {
            uses,
            by_relation: self.coverage(changed, lead, per, base_gap.into_iter().collect()),
        }
    }

    /// Whether a head file of the family holds an import whose target the code
    /// computes. A file whose facts cannot be read is not known to hold none.
    fn computes_an_import(&self) -> bool {
        self.files.iter().any(|file| {
            RepoPath::new(file).map_or(true, |path| {
                facts_of(&self.ctx, SideTag::Head, &self.cache, &path).map_or(true, |facts| {
                    facts.is_some_and(|f| {
                        f.blob
                            .facts
                            .iter()
                            .any(|fact| matches!(fact, Fact::ComputedImport(_)))
                    })
                })
            })
        })
    }

    /// Whether a type use is an annotation a framework may execute at run time
    /// (Python): a class body's field annotation, one in the signature of a
    /// decorated function, or any in a file that imports pydantic, fastapi,
    /// dataclasses, attrs, marshmallow, typer or click. Unsure counts as yes.
    fn runtime_annotation(&self, u: &Use) -> bool {
        if self.family != Family::Python || u.relation != Relation::TypeUses {
            return false;
        }
        if self.imports_runtime_library(&u.file) {
            return true;
        }
        let Scope::Def(_) = root_scope(&u.scope) else {
            return false;
        };
        let Some(key) = self.key_in(&u.file, &u.scope) else {
            return true;
        };
        let Some(def) = self.definition(&key) else {
            return true;
        };
        match def.kind {
            DefKind::Class => true,
            DefKind::Field | DefKind::Variable | DefKind::Constant => {
                !key.local.container.is_empty()
            }
            DefKind::Function | DefKind::Method | DefKind::Constructor => RepoPath::new(&u.file)
                .map_or(true, |file| {
                    self.declared(SideTag::Head, &file, &key)
                        .map_or(true, |found| {
                            found.is_some_and(|d| !d.decorators.is_empty())
                        })
                }),
            _ => false,
        }
    }

    fn imports_runtime_library(&self, file: &str) -> bool {
        const LIBRARIES: [&str; 7] = [
            "pydantic",
            "fastapi",
            "dataclasses",
            "attrs",
            "attr",
            "marshmallow",
            "typer",
        ];
        if let Some(known) = self.runtime_files.borrow().get(file) {
            return *known;
        }
        let found = RepoPath::new(file).map_or(true, |path| {
            facts_of(&self.ctx, SideTag::Head, &self.cache, &path).map_or(true, |facts| {
                facts.is_some_and(|f| {
                    f.blob.facts.iter().any(|fact| match fact {
                        Fact::Import(i) => {
                            let root = i.module.as_str().split('.').next().unwrap_or("");
                            LIBRARIES.contains(&root) || root == "click"
                        }
                        _ => false,
                    })
                })
            })
        });
        self.runtime_files
            .borrow_mut()
            .insert(file.to_string(), found);
        found
    }

    /// What covered the search of each relation of use: the gaps of every
    /// relation (`lead`, `tail`), the one a relation has of its own (`per`), the
    /// files outside the run's pathspecs that could name the definition, and a read
    /// the cell lost. The one place the cell makes a `Complete`.
    fn coverage(
        &self,
        changed: &Changed,
        lead: Vec<Gap>,
        per: Vec<(Relation, Gap)>,
        tail: Vec<Gap>,
    ) -> [(Relation, Coverage); 6] {
        let mut tail = tail;
        let outside = self
            .outside
            .iter()
            .filter(|file| self.ctx.workspace.may_reference(file, &changed.file))
            .count();
        if outside > 0 {
            tail.push(Gap::OutsideScope { files: outside });
        }
        // A module's variables and outputs are what a call reaches: a call whose
        // source is outside the repository may be a caller of any of them.
        let external = self.head.external_sources();
        let interface = changed.subjects.iter().any(|s| {
            matches!(
                s.key.local.container.first().map(Name::as_str),
                Some("var" | "output")
            )
        });
        if external > 0 && interface {
            tail.push(Gap::ExternalSource { pointers: external });
        }
        if self.family == Family::Rust {
            let macros = self
                .macro_files
                .iter()
                .filter(|file| self.ctx.workspace.may_reference(file, &changed.file))
                .count();
            if macros > 0 {
                tail.push(Gap::Macros {
                    files: macros,
                    what: "declare items through `cfg_if!`, which is not expanded",
                });
            }
            let dispatched = changed
                .subjects
                .iter()
                .filter_map(|s| self.definition(&s.key))
                .find_map(|def| self.dispatched_via(def));
            if let Some(via) = dispatched {
                tail.push(Gap::Dispatched { via });
            }
        }
        if self.family == Family::C {
            let macros = self
                .macro_files
                .iter()
                .filter(|file| self.ctx.workspace.may_reference(file, &changed.file))
                .count();
            if macros > 0 {
                tail.push(Gap::Macros {
                    files: macros,
                    what: "call or use it through macros, whose expansion is not read",
                });
            }
            // What a build configures may not exist: the definition is under `#if`.
            let conditional = changed.subjects.iter().any(|s| {
                matches!(&s.key.local.disambig, Some(Disambig::Cfg(cond)) if cond != "declaration")
            });
            if conditional {
                tail.push(Gap::Macros {
                    files: 1,
                    what: "define it under conditional compilation, which is not evaluated",
                });
            }
        }
        // What a configuration object's readers could be, outside what the cell
        // reads: a place a file names outside the repository, a name a template
        // writes.
        if let Resolver::Config(config) = &self.head {
            if config.external_sources() > 0 {
                tail.push(Gap::ExternalSource {
                    pointers: config.external_sources(),
                });
            }
            if config.templated_files() > 0 {
                tail.push(Gap::Templated {
                    files: config.templated_files(),
                });
            }
            if config.unbound_values() > 0 {
                tail.push(Gap::ContextUnbound {
                    files: config.unbound_values(),
                });
            }
        }
        // A script that sources a path nothing settles may source any name; one
        // the bash grammar does not read may call or define any.
        if let Resolver::Shell(shell) = &self.head {
            if shell.unknown_sources() > 0 {
                tail.push(Gap::SourceFlag {
                    files: shell.unknown_sources(),
                });
            }
            if shell.unread_scripts() > 0 {
                tail.push(Gap::Unread {
                    files: shell.unread_scripts(),
                });
            }
        }
        let reasons = self.stops();
        if !reasons.is_empty() {
            tail.push(Gap::Stopped { reasons });
        }
        USES.map(|relation| {
            let mut gaps = lead.clone();
            gaps.extend(
                per.iter()
                    .filter(|(r, _)| *r == relation)
                    .map(|(_, gap)| gap.clone()),
            );
            gaps.extend(tail.iter().cloned());
            (relation, Coverage::of(gaps))
        })
    }

    /// The trait a Rust definition belongs to: the one it implements, or the one
    /// that declares it.
    fn dispatched_via(&self, def: &crate::resolve::Definition) -> Option<String> {
        let last = |name: &str| name.rsplit("::").next().unwrap_or(name).to_string();
        if let Some(Disambig::ImplOf(tr)) = &def.key.local.disambig {
            return Some(last(tr.as_str()));
        }
        let (name, outer) = def.key.local.container.split_last()?;
        self.defs_by_file
            .get(def.file.as_str())?
            .iter()
            .map(|at| &self.head.definitions()[*at])
            .find(|d| {
                d.kind == DefKind::Trait
                    && d.key.local.container.as_slice() == outer
                    && &d.key.local.name == name
            })
            .map(|tr| tr.key.local.name.as_str().to_string())
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
                if strands(self.family, b.relation)
                    && !b.entry_guard
                    && within_scope(&self.scope, b.file.as_str())
                {
                    let standing = match (self.family, standing_of(&b.resolution)) {
                        (Family::Python, _) | (_, None) => Standing::Resolved,
                        (_, Some(standing)) => standing,
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
            let stranded: Vec<&Binding> = self
                .bindings_by_file
                .get(&file)
                .into_iter()
                .flatten()
                .map(|at| &self.head.bindings()[*at])
                .filter(|h| {
                    h.name == name
                        && strands(self.family, h.relation)
                        && matches!(h.resolution, Resolution::Unresolved { .. })
                })
                .collect();
            // An import is a broken line too, but a file that still uses the name
            // has said so on the use: the import names it only when nothing else does.
            let used = stranded.iter().any(|h| h.relation != Relation::Imports);
            for h in stranded
                .into_iter()
                .filter(|h| !used || h.relation != Relation::Imports)
            {
                let line = h.site.lines.0;
                out.push(Use {
                    // A line that still names what left is a broken reference
                    // whoever else the range changed: never the diff's own line.
                    also_changed: false,
                    file: file.clone(),
                    line,
                    relation: h.relation,
                    standing,
                    scope: h.from.clone(),
                    folded: None,
                    sharing: 0,
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
    ) -> Vec<(String, u32, Scope, Relation)> {
        // A property read through an object nothing types cannot be a use of a
        // top-level function or constant of a module: `c.env` is no use of the
        // exported `env`, which a use reaches through its import. Only a member
        // is read that way.
        let member = changed
            .subjects
            .first()
            .is_some_and(|s| !s.key.local.container.is_empty());
        let top_level_only = self.family == Family::TypeScript && !member;
        // A use whose name is not written (`$o->$m()`, `new $c`) may be of any
        // definition of a kind its relation reaches.
        let dynamic = self
            .unlanded
            .get(crate::resolve::DYNAMIC)
            .into_iter()
            .flatten()
            .filter(|(_, _, _, _, relation)| dynamic_reaches(*relation, changed.kind));
        self.unlanded
            .get(&changed.name)
            .into_iter()
            .flatten()
            .chain(dynamic)
            .filter(|(file, line, _, read, _)| {
                !(top_level_only && *read)
                    && self.ctx.workspace.may_reference(file, &changed.file)
                    && !covered.contains(&(file.clone(), *line))
            })
            .map(|(file, line, scope, _, relation)| (file.clone(), *line, scope.clone(), *relation))
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

    /// The file and line of each resolved use of the definition called `name`
    /// at `lines` of `file`: who a test has to be, or to reach, to reach it.
    pub fn users(&self, file: &str, name: &str, lines: (u32, u32)) -> Vec<(String, u32)> {
        let Some(def) = self.definition_at(file, name, lines) else {
            return Vec::new();
        };
        self.landed
            .get(&def.key)
            .map_or_else(Vec::new, |uses| uses.clone())
    }

    /// Whether the family's files name other files by a path they write (a
    /// `COPY`, a `source`, a kustomization's `resources`, a module's `source`),
    /// which its resolver settles from the file's own directory.
    fn points_by_path(&self) -> bool {
        matches!(
            self.family,
            Family::Config | Family::Terraform | Family::Dockerfile | Family::Shell
        )
    }

    /// The file and line of each pointer the family settled on `target`.
    pub fn pointers_to(&self, target: &str) -> Vec<(String, u32)> {
        if !self.points_by_path() {
            return Vec::new();
        }
        self.head
            .bindings()
            .iter()
            .filter(|b| {
                matches!(&b.resolution, Resolution::Module { file, .. } if file.as_str() == target)
                    && within_scope(&self.scope, b.file.as_str())
            })
            .map(|b| (b.file.as_str().to_string(), b.site.lines.0))
            .collect()
    }

    /// Whether the family settled a pointer written at `file:line` on a file
    /// other than `target`.
    pub fn points_elsewhere(&self, file: &str, line: u32, target: &str) -> bool {
        self.points_by_path()
            && self.bindings_by_file.get(file).is_some_and(|at| {
                at.iter().map(|i| &self.head.bindings()[*i]).any(|b| {
                    b.site.lines.0 <= line
                        && line <= b.site.lines.1
                        && matches!(&b.resolution, Resolution::Module { file: found, .. }
                                if found.as_str() != target)
                })
            })
    }

    /// The files whose bindings reach something in `used_file`.
    pub fn reached_from<'a>(&'a self, used_file: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.reaches
            .iter()
            .filter(move |(_, used)| used == used_file)
            .map(|(user, _)| user.as_str())
    }
}

/// Whether a use that names nothing it could be a use of a definition of this
/// kind: a call reaches what is called, an instantiation a class, an access a
/// value.
fn dynamic_reaches(relation: Relation, kind: DefKind) -> bool {
    match relation {
        Relation::Calls | Relation::PassesAsValue => matches!(
            kind,
            DefKind::Function | DefKind::Method | DefKind::Constructor
        ),
        Relation::Instantiates => matches!(kind, DefKind::Class),
        Relation::Reads | Relation::Writes => matches!(
            kind,
            DefKind::Property
                | DefKind::Constant
                | DefKind::Field
                | DefKind::EnumMember
                | DefKind::Variable
        ),
        _ => false,
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

    /// Whether a head file of a cell language may import a module by a name it
    /// computes (`importlib.import_module(name)`, `import(path)`, `require(path)`),
    /// which no importer search can see: some file holds a `ComputedImport` fact, or
    /// a family could not be read at all, so none is known to hold none.
    pub fn computed_imports_possible(&self) -> bool {
        !self.lost.is_empty() || self.cells.iter().any(Cell::computes_an_import)
    }

    /// Whether `rel` is a file of a family that could not be resolved.
    pub fn lost(&self, rel: &str) -> bool {
        self.lost.iter().any(|family| family.has_path(rel))
    }
}

/// Whether a line that names a definition the range removed is stranded by it: a
/// use that now finds nothing, or an export that still offers the missing name. In
/// the code families a name written where a type is expected, and an import of it,
/// are the only uses a removed interface, class or alias may have: a file that still
/// imports a removed type and annotates with it is broken, and without them the
/// search of a removed type finds nothing and the answer says nothing references it.
fn strands(family: Family, relation: Relation) -> bool {
    relation.uses()
        || relation == Relation::Reexports
        || (matches!(relation, Relation::TypeUses | Relation::Imports)
            && !matches!(
                family,
                Family::Config | Family::Terraform | Family::Dockerfile | Family::Shell
            ))
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
            // Two reasons for one entry fold by neither: the table has no row
            // for the two together.
            if known.basis != changed.basis {
                known.basis = None;
            }
            known.decorated |= changed.decorated;
            known.api |= changed.api;
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
