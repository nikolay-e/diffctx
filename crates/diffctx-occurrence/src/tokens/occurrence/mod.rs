//! Absence accounted for by occurrences: a definition's relation is `Complete`
//! when every occurrence of a name of it, in the code of the files of its group,
//! has one outcome and none is a candidate. A reader finds the occurrences, never
//! a resolver; a resolver only explains them.
//!
//! Every `match` over these enums names each variant, so a new one does not compile
//! until it is decided.

pub mod debug;
#[cfg(debug_assertions)]
pub mod identity;
mod implicit;
mod outcome;
mod rows;
mod scanner;
mod seal;
#[cfg(test)]
mod tests;

use std::fmt;
use std::ops::Range;

use diffctx_facts::facts::{DefKey, ModulePath, Name, Scope, Span};
use diffctx_resolve::resolve::{CandidateReason, Relation, UnresolvedReason};
use diffctx_snapshot::snapshot::{Loss, RepoPath};

pub use diffctx_facts::facts::Lexical;
pub use implicit::{Known, entered, implicit_kinds};
pub use outcome::{FileOutcomes, Index, View, admitted, counts_against, outcomes};
pub use scanner::{Case, Scan, scan};
pub use seal::{Accounted, Sealed, Settled, Settlement};

/// The longest chain of aliases of aliases the closure follows (*estimate*,
/// measured in step 2).
pub const MAX_ALIAS_HOPS: usize = 3;

/// The most alias names one definition may gather (*estimate*, measured in step 2).
pub const MAX_ALIAS_NAMES: usize = 16;

/// What an occurrence is, by where its bytes lie. A comment is not an occurrence:
/// the scanner drops it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Class {
    Code,
    /// In a string literal: counted, never blocking, unless a resolver explains
    /// it (a reflective string), when it becomes code.
    Mention,
}

impl Class {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Code => "code",
            Self::Mention => "mention",
        }
    }
}

/// One spelling of a name in one file, keyed by (file, byte range) of the blob
/// read for its side: an outcome binds it only when its name span equals `bytes`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Occurrence {
    pub file: RepoPath,
    pub bytes: Range<u32>,
    /// 1-based.
    pub line: u32,
    pub class: Class,
}

/// Where a value is passed on, so that a call may come from anywhere.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ValuePlace {
    Argument,
    /// An element of a collection.
    Element,
    /// The right side of an assignment to a field or an index.
    Store,
}

/// Where a new name of the definition is in force.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum AliasScope {
    /// The importing file.
    File(RepoPath),
    /// Every importer of the re-exporting module.
    Importers(ModulePath),
    /// The scope of the assignment or the declaration (module level is exported).
    Declared(Scope),
}

/// A new name of the definition: the occurrences of it are accounted next.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Alias {
    pub name: Name,
    pub scope: AliasScope,
    /// An alias of the definition is 1, an alias of that alias 2.
    pub hops: usize,
}

/// The alias closure went past its bound: the pair is partial, never complete.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AliasBound {
    pub hops: usize,
    pub names: usize,
}

impl AliasBound {
    /// The bound, when a closure of `hops` and `names` is past it.
    pub const fn exceeded(hops: usize, names: usize) -> Option<Self> {
        if hops > MAX_ALIAS_HOPS || names > MAX_ALIAS_NAMES {
            Some(Self { hops, names })
        } else {
            None
        }
    }
}

/// Why an occurrence is a candidate: the accounting could not say it is not a use
/// of the definition.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reason {
    /// No binding names the occurrence.
    Unexplained,
    /// The binding at the occurrence is wider than the name: its family does not
    /// report the name's span yet.
    WideBinding,
    /// Two outcomes claim one span: a defect of the resolver.
    Conflict,
    /// A target in the definition's override family, or a head binding at a place the
    /// base resolved to a removed definition: a call through a supertype.
    Dispatch,
    /// A removed member's call through a same-named member, its base family unread.
    OverrideUnproven,
    /// A `Candidate` resolution, with its reason.
    Resolver(CandidateReason),
    /// An `Unresolved` resolution whose reason gives no basis for `not_ours`.
    Unresolved(UnresolvedReason),
    /// A `not_ours` whose basis the accounting could not verify itself.
    BasisUnverified,
    /// The file was read with a loss: every occurrence in it is a candidate.
    FileLost,
    /// In code a script hands to the definition's runtime: nothing reads it.
    Embedded,
}

/// The row of the contract that gave an outcome: the debug line prints it. A
/// `not_ours` carries its row as its [`Witness`], so none can be made without one.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Basis {
    /// `ours`: the binding resolved to the definition.
    Resolved,
    /// `ours`: the module is the definition's own, or re-exports it.
    OwnModule,
    /// `declaration`: the definition's `name_span`.
    DefinitionName,
    /// `declaration`: the site of a `LocalBinding`.
    LocalSite,
    /// `not_ours`: the basis is the outcome's [`Witness`].
    Witnessed,
    /// `alias`: `import f as g`, `use m::f as g`; `declaration`: the new name `g`.
    RenamedImport,
    /// `alias`: `g = f`.
    Assignment,
    /// `alias`: `typealias G = F`, `typedef F G`.
    TypeAlias,
    /// `leak`: an `ours` occurrence in a value position.
    ValuePosition,
    /// `candidate`: nothing proves it is not ours.
    Unproven,
}

impl Basis {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Resolved => "resolved",
            Self::OwnModule => "own_module",
            Self::DefinitionName => "definition_name",
            Self::LocalSite => "local_site",
            Self::Witnessed => "witnessed",
            Self::RenamedImport => "renamed_import",
            Self::Assignment => "assignment",
            Self::TypeAlias => "type_alias",
            Self::ValuePosition => "value_position",
            Self::Unproven => "unproven",
        }
    }
}

/// What a `not_ours` stands on, and the one way to make it.
///
/// The identity of what the occurrence belongs to, or of the search that found no
/// member of that name. A false "nobody calls" needs a witness, so a wrong one names itself.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Witness {
    /// `other_definition`: resolved to another definition that exists in the side's facts.
    OtherDefinition(DefKey),
    /// `other_module`: a module that is not the definition's and does not re-export it.
    OtherModule(RepoPath),
    /// `other_module`: the module path an import or a package clause names.
    ModulePath(ModulePath),
    /// `other_module`: the namespace a path qualifier names, for a subject that is none.
    Namespace(Name),
    /// `local`: the site of a local of that name in a scope enclosing the occurrence.
    Local(Span),
    /// `builtin`, an absence: no import of the file binds the name and the definition is not
    /// the file's own.
    Builtin(RepoPath),
    /// `builtin`: the word is part of a longer one (`lint.sh`, `--lint`), which it names.
    Compound(Name),
    /// `builtin`: the word follows a command (its name) that runs nothing.
    CommandWord(Name),
    /// `external_module`: an import binds the name to a module outside the repository.
    ExternalModule(ModulePath),
    /// `external_name`, an absence: an external call no import binds, and no definition
    /// of the side is a member of that name.
    ExternalName(Name),
    /// `manifests`: the manifests read whole put the target outside the project or on an
    /// external base.
    Manifests(UnresolvedReason, Name),
    /// `bare_name`, an absence: a bare name is no member of the class it is written outside
    /// of (the class's path).
    BareName(Vec<Name>),
    /// `unreached`, an absence: no analysed file imports the file, nothing it imports leads to the
    /// definition's module or shares a module with one that does, and it runs no code it loads
    /// or enters: no object of the definition's class can be the receiver of its member reads.
    Unreached(RepoPath),
    /// `other_project`: the file's project cannot reference the definition's file, the
    /// manifests read whole.
    OtherProject(RepoPath),
    /// `statement`: the name follows a keyword that declares it or names an object of another
    /// namespace (`DROP FUNCTION f`, `INSERT INTO f` for a routine `f`), or is a word of an
    /// instruction that holds another language's text (`RUN go build`): the keyword; the manifest
    /// text of a Helm template outside its actions, which no values key is read in: `manifest`.
    Statement(Name),
    /// `argument_name`: the name of a keyword argument (`f(name=value)`), a parameter or a field
    /// of the callee.
    Keyword(Name),
}

impl Witness {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::OtherDefinition(_) => "other_definition",
            Self::OtherModule(_) | Self::ModulePath(_) | Self::Namespace(_) => "other_module",
            Self::Local(_) => "local",
            Self::Builtin(_) | Self::Compound(_) | Self::CommandWord(_) => "builtin",
            Self::ExternalModule(_) => "external_module",
            Self::ExternalName(_) => "external_name",
            Self::Manifests(..) => "manifests",
            Self::BareName(_) => "bare_name",
            Self::Unreached(_) => "unreached",
            Self::OtherProject(_) => "other_project",
            Self::Statement(_) => "statement",
            Self::Keyword(_) => "argument_name",
        }
    }
}

/// The one outcome of an occurrence.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Outcome {
    /// A use of the definition, by the relation of its entry.
    Ours,
    Declaration,
    /// Not a use of the definition, and the witness says what it is instead. Another crate can
    /// match it and cannot make one.
    #[non_exhaustive]
    NotOurs {
        witness: Witness,
    },
    /// A new name of the definition: accounted next.
    Alias(Alias),
    /// The definition passed on as a value: `calls` is partial.
    Leak(ValuePlace),
    Candidate(Reason),
}

impl Outcome {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Ours => "ours",
            Self::Declaration => "declaration",
            Self::NotOurs { .. } => "not_ours",
            Self::Alias(_) => "alias",
            Self::Leak(_) => "leak",
            Self::Candidate(_) => "candidate",
        }
    }
}

/// How sure the resolver is of a use: bound to the definition, or a candidate with the
/// reason it is only one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Standing {
    Resolved,
    Candidate(&'static str),
}

/// A code occurrence with its outcome. A mention a resolver explained is code and
/// an entry; one nobody explained stays a mention and is counted, never listed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Entry {
    pub occurrence: Occurrence,
    /// The binding's relation; `None` counts against every relation the
    /// definition's kind admits.
    pub relation: Option<Relation>,
    pub outcome: Outcome,
    pub basis: Basis,
    /// A leak into an argument list: the callee the value is handed to, when the text names it.
    pub passed_to: Option<Name>,
}

impl Entry {
    /// The row that gave the outcome, as the debug line names it.
    pub const fn basis_str(&self) -> &'static str {
        match &self.outcome {
            Outcome::NotOurs { witness } => witness.as_str(),
            _ => self.basis.as_str(),
        }
    }

    pub fn counts_for(&self, relation: Relation) -> bool {
        self.relation.is_none_or(|own| own == relation)
    }
}

/// How a definition is called without its name written: the answer names an
/// unknown of this kind, never "nobody", for `calls`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Implicit {
    /// Invoked by syntax: operators, item and attribute access, magic methods.
    Operator,
    /// Called by a language construct: iteration, formatting, destruction,
    /// conversion, comparison, context managers, serialisation hooks.
    Protocol,
    /// Overrides or implements a member of a base the group does not hold in full.
    Override,
    /// A `Fact::EntryPoint` of kind `Main`, `ScriptGuard`, `Command`, `Route`,
    /// `Scheduled`, `Tool` or `Fixture`, and a C constructor attribute.
    Entry,
    /// An `EntryKind::Test` outside a test file and not Rust's: inside one, and for a
    /// `#[test]`, the answer names the test runner as the entry.
    Test,
    /// Consumed outside the repository: configuration, a Dockerfile, Terraform
    /// root variables and outputs, an executable script, an exported symbol.
    Outside,
}

impl Implicit {
    /// Every kind: the class test seeds one case per element.
    pub const ALL: [Self; 6] = [
        Self::Operator,
        Self::Protocol,
        Self::Override,
        Self::Entry,
        Self::Test,
        Self::Outside,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Operator => "operator",
            Self::Protocol => "protocol",
            Self::Override => "override",
            Self::Entry => "entry",
            Self::Test => "test",
            Self::Outside => "outside",
        }
    }
}

/// A file of the group no position was read for (not read, or lost before it
/// could be scanned): one candidate row naming its loss.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Unread {
    pub(crate) file: RepoPath,
    pub(crate) loss: Loss,
}

/// Everything the accounting found for one relation of a definition at one side.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Account {
    pub(crate) relation: Relation,
    /// The code occurrences, each with its outcome.
    pub(crate) entries: Vec<Entry>,
    pub(crate) unread: Vec<Unread>,
    pub(crate) implicit: Vec<Implicit>,
    /// The definition's kind admits `calls`: only then a leak stands against it
    /// (a property is read, never called).
    pub(crate) admits_calls: bool,
    /// A tool outside the repository consumes the definition by reading it (a
    /// configuration object, not a Dockerfile stage, which is built by name):
    /// `Outside` then stands against its `reads`.
    pub(crate) consumed_by_reading: bool,
    pub(crate) alias_bound: Option<AliasBound>,
    /// A name too common to list: its occurrences, folded into one row.
    pub(crate) too_common: Option<usize>,
}

impl Account {
    pub(crate) fn new(relation: Relation) -> Self {
        Self {
            relation,
            entries: Vec::new(),
            unread: Vec::new(),
            implicit: Vec::new(),
            admits_calls: true,
            consumed_by_reading: false,
            alias_bound: None,
            too_common: None,
        }
    }

    /// `Complete` when nothing stands in the way: no candidate of the relation,
    /// no leak and no implicit kind for `calls`, no file left unread, the alias
    /// closure within its bound, the name not too common. The run's own gaps
    /// (a deadline, withheld files, the scope) are the caller's.
    pub(crate) fn verdict(&self) -> Verdict {
        let mut hedges = Vec::new();
        for entry in &self.entries {
            let at = &entry.occurrence;
            match &entry.outcome {
                Outcome::Ours
                | Outcome::Declaration
                | Outcome::NotOurs { .. }
                | Outcome::Alias(_) => {}
                Outcome::Leak(place) => {
                    if self.relation == Relation::Calls && self.admits_calls {
                        hedges.push(Hedge::Leak {
                            file: at.file.clone(),
                            line: at.line,
                            place: *place,
                            callee: entry.passed_to.clone(),
                        });
                    }
                }
                Outcome::Candidate(reason) => {
                    if entry.counts_for(self.relation) {
                        hedges.push(Hedge::Candidate {
                            file: at.file.clone(),
                            line: at.line,
                            bytes: at.bytes.clone(),
                            reason: *reason,
                        });
                    }
                }
            }
        }
        for unread in &self.unread {
            hedges.push(Hedge::Unread {
                file: unread.file.clone(),
                loss: unread.loss.clone(),
            });
        }
        // An implicit kind stands where the definition is consumed: against `calls` for a
        // kind that admits them; against `reads` for an object a tool outside reads
        // (`Outside`); against any other read-only kind's `calls` only while nothing in
        // the group uses it, so that the answer never says nobody does.
        let used = self.entries.iter().any(|e| e.outcome == Outcome::Ours);
        let standing = |kind: Implicit| {
            self.admits_calls || !(used || self.consumed_by_reading && kind == Implicit::Outside)
        };
        let kinds = self.implicit.iter().copied();
        match self.relation {
            Relation::Calls => hedges.extend(kinds.filter(|k| standing(*k)).map(Hedge::Implicit)),
            Relation::Reads if self.consumed_by_reading => hedges.extend(
                kinds
                    .filter(|kind| *kind == Implicit::Outside)
                    .map(Hedge::Implicit),
            ),
            _ => {}
        }
        hedges.extend(self.alias_bound.map(Hedge::AliasBound));
        hedges.extend(
            self.too_common
                .map(|occurrences| Hedge::TooCommon { occurrences }),
        );
        Verdict::of(hedges, 1)
    }
}

/// What stands in the way of `Complete`: an occurrence (file and line) or a
/// resource of the account.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Hedge {
    Candidate {
        file: RepoPath,
        line: u32,
        bytes: Range<u32>,
        reason: Reason,
    },
    /// A candidate at a line a resolved use is listed on, in a span of its own:
    /// counted, never listed, so that no line stands twice in the answer.
    Unlisted {
        occurrences: usize,
    },
    Leak {
        file: RepoPath,
        line: u32,
        place: ValuePlace,
        callee: Option<Name>,
    },
    Unread {
        file: RepoPath,
        loss: Loss,
    },
    Implicit(Implicit),
    AliasBound(AliasBound),
    TooCommon {
        occurrences: usize,
    },
}

impl fmt::Display for Hedge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Candidate { file, line, .. } => {
                write!(f, "candidate at {}:{line}", file.as_str())
            }
            Self::Unlisted { occurrences } => {
                write!(
                    f,
                    "{occurrences} candidate(s) beside a use the answer lists"
                )
            }
            Self::Leak { file, line, .. } => write!(f, "leak at {}:{line}", file.as_str()),
            Self::Unread { file, .. } => write!(f, "{} was not read", file.as_str()),
            Self::Implicit(kind) => write!(f, "called implicitly ({})", kind.as_str()),
            Self::AliasBound(bound) => write!(
                f,
                "alias closure past its bound ({} hops, {} names)",
                bound.hops, bound.names
            ),
            Self::TooCommon { occurrences } => {
                write!(f, "name too common ({occurrences} occurrences)")
            }
        }
    }
}

/// What a `Complete` verdict carries. `Verdict::of` makes it, because the field is private to this
/// module tree: from a settled account of at least one definition, which is what a [`Sealed`]
/// carries, or from one definition's own account, which never leaves this crate. Another crate holds
/// a [`Sealed`] verdict and asks it for the proof.
///
/// ```compile_fail,E0423
/// use diffctx_occurrence::tokens::occurrence::Proof;
/// let _ = Proof(());
/// ```
///
/// ```compile_fail,E0599
/// use diffctx_occurrence::tokens::occurrence::Proof;
/// let _ = Proof::default();
/// ```
///
/// ```compile_fail,E0639
/// use diffctx_facts::facts::Name;
/// use diffctx_occurrence::tokens::occurrence::{Outcome, Witness};
/// let _ = Outcome::NotOurs { witness: Witness::Statement(Name::new("drop")) };
/// ```
///
/// ```compile_fail,E0423
/// use diffctx_occurrence::tokens::occurrence::Sealed;
/// let _ = Sealed(());
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Proof(());

/// The verdict of one account: complete, or what stands in the way (no hedge at all when no
/// definition was accounted).
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Verdict {
    Complete(Proof),
    Partial(Vec<Hedge>),
}

impl Verdict {
    /// The one place a `Proof` is made: no hedge stands in the way of what `definitions` accounted,
    /// and something was.
    fn of(hedges: Vec<Hedge>, definitions: usize) -> Self {
        if hedges.is_empty() && definitions > 0 {
            Self::Complete(Proof(()))
        } else {
            Self::Partial(hedges)
        }
    }

    pub(crate) const fn as_str(&self) -> &'static str {
        match self {
            Self::Complete(_) => "complete",
            Self::Partial(_) => "partial",
        }
    }

    pub(crate) const fn is_complete(&self) -> bool {
        matches!(self, Self::Complete(_))
    }
}

mod alias;

pub use alias::{callee_of, close_aliases, value_use};
