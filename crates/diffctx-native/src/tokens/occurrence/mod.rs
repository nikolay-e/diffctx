//! Absence accounted for by occurrences: a definition's relation is `Complete`
//! when every occurrence of a name of it, in the code of the files of its group,
//! has one outcome and none is a candidate. A reader finds the occurrences, never
//! a resolver; a resolver only explains them.
//!
//! Nothing here is read on the answer path yet. Who fills what, by segment of
//! phase A step 2:
//!
//! - A2.1 (this file and `scanner`): the types, the verdict of [`Account`], and
//!   [`scan`], which finds the occurrences of names in one file.
//! - A2.2 (`facts::lexical`): one pass over each parsed tree fills `BlobFacts::lexical`.
//! - A2.3 (`outcome`): a `Binding`, a declaration or an import name becomes an
//!   [`Outcome`] with its [`Basis`]; a binding that gives none is a candidate.
//! - A2.4 (`alias`, `implicit`): the alias closure within [`MAX_ALIAS_HOPS`] and
//!   [`MAX_ALIAS_NAMES`], value leaks, and the [`Implicit`] kinds of a definition.
//! - A2.5 (`debug`): the [`Account`] of each (definition, relation) beside today's
//!   verdict, and the `DIFFCTX_DEBUG_OCCURRENCES` writer.
//!
//! Every `match` over these enums names each variant, so a new one does not compile
//! until it is decided.

pub mod debug;
mod implicit;
mod outcome;
mod scanner;

use std::fmt;
use std::ops::Range;

use crate::facts::{DefKey, ModulePath, Name, Scope};
use crate::resolve::{CandidateReason, Relation, UnresolvedReason};
use crate::snapshot::{Loss, RepoPath, SideTag};

pub use crate::facts::Lexical;
pub use implicit::{Known, implicit_kinds};
pub use outcome::{FileOutcomes, Index, View, admitted, counts_against, outcomes};
pub use scanner::{Case, Scan, scan};

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
    Return,
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
}

/// The row of the contract that gave an outcome: the debug line prints it, and
/// each basis of a `not_ours` is one the accounting checks itself.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Basis {
    /// `ours`: the binding resolved to the definition.
    Resolved,
    /// `ours`: the module is the definition's own, or re-exports it.
    OwnModule,
    /// `ours`: an import name of the definition's module.
    ImportedName,
    /// `ours`: an export name of the definition.
    ExportedName,
    /// `ours`: a removed member looked up on its owner.
    OwnerReceiver,
    /// `declaration`: the definition's `name_span`.
    DefinitionName,
    /// `declaration`: the site of a `LocalBinding`.
    LocalSite,
    /// `not_ours`: resolved to another definition that exists in the side's facts.
    OtherDefinition,
    /// `not_ours`: a module that is not the definition's and does not re-export it.
    OtherModule,
    /// `not_ours`: a local of that name in a scope enclosing the occurrence.
    Local,
    /// `not_ours`: no import binds the name, and the definition is not the file's own.
    Builtin,
    /// `not_ours`: an import binds the name to a module outside the repository.
    ExternalModule,
    /// `not_ours`: outside the project or on an external base, the manifests read whole.
    Manifests,
    /// `not_ours`: the receiver's class was found and is not the owner's hierarchy.
    ReceiverClass,
    /// `alias`: `import f as g`, `use m::f as g`.
    RenamedImport,
    /// `alias`: `export { f as g }`, `pub use m::f as g`.
    RenamedExport,
    /// `alias`: `g = f`.
    Assignment,
    /// `alias`: `{ f: g } = mod`.
    Destructuring,
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
            Self::ImportedName => "imported_name",
            Self::ExportedName => "exported_name",
            Self::OwnerReceiver => "owner_receiver",
            Self::DefinitionName => "definition_name",
            Self::LocalSite => "local_site",
            Self::OtherDefinition => "other_definition",
            Self::OtherModule => "other_module",
            Self::Local => "local",
            Self::Builtin => "builtin",
            Self::ExternalModule => "external_module",
            Self::Manifests => "manifests",
            Self::ReceiverClass => "receiver_class",
            Self::RenamedImport => "renamed_import",
            Self::RenamedExport => "renamed_export",
            Self::Assignment => "assignment",
            Self::Destructuring => "destructuring",
            Self::TypeAlias => "type_alias",
            Self::ValuePosition => "value_position",
            Self::Unproven => "unproven",
        }
    }

    /// Whether the basis is one a `not_ours` may rest on.
    pub const fn proves_not_ours(self) -> bool {
        match self {
            Self::OtherDefinition
            | Self::OtherModule
            | Self::Local
            | Self::Builtin
            | Self::ExternalModule
            | Self::Manifests
            | Self::ReceiverClass => true,
            Self::Resolved
            | Self::OwnModule
            | Self::ImportedName
            | Self::ExportedName
            | Self::OwnerReceiver
            | Self::DefinitionName
            | Self::LocalSite
            | Self::RenamedImport
            | Self::RenamedExport
            | Self::Assignment
            | Self::Destructuring
            | Self::TypeAlias
            | Self::ValuePosition
            | Self::Unproven => false,
        }
    }
}

/// The one outcome of an occurrence.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Outcome {
    /// A use of the definition, by the relation of its entry.
    Ours,
    Declaration,
    NotOurs,
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
            Self::NotOurs => "not_ours",
            Self::Alias(_) => "alias",
            Self::Leak(_) => "leak",
            Self::Candidate(_) => "candidate",
        }
    }
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
}

impl Entry {
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
    /// An `EntryKind::Test`.
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
pub struct Unread {
    pub file: RepoPath,
    pub loss: Loss,
}

/// Everything the accounting found for one (definition, relation) of one side.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Account {
    pub definition: DefKey,
    pub relation: Relation,
    pub side: SideTag,
    /// The files of the group whose occurrences were accounted.
    pub files: usize,
    /// The code occurrences, each with its outcome.
    pub entries: Vec<Entry>,
    pub unread: Vec<Unread>,
    /// Occurrences in strings nobody explained: counted, never blocking.
    pub mentions: usize,
    pub implicit: Vec<Implicit>,
    pub alias_bound: Option<AliasBound>,
    /// A name too common to list: its occurrences, folded into one row.
    pub too_common: Option<usize>,
}

impl Account {
    pub fn new(definition: DefKey, relation: Relation, side: SideTag) -> Self {
        Self {
            definition,
            relation,
            side,
            files: 0,
            entries: Vec::new(),
            unread: Vec::new(),
            mentions: 0,
            implicit: Vec::new(),
            alias_bound: None,
            too_common: None,
        }
    }

    /// `Complete` when nothing stands in the way: no candidate of the relation,
    /// no leak and no implicit kind for `calls`, no file left unread, the alias
    /// closure within its bound, the name not too common. The run's own gaps
    /// (a deadline, withheld files, the scope) are the caller's.
    pub fn verdict(&self) -> Verdict {
        let mut hedges = Vec::new();
        for entry in &self.entries {
            let at = &entry.occurrence;
            match &entry.outcome {
                Outcome::Ours | Outcome::Declaration | Outcome::NotOurs | Outcome::Alias(_) => {}
                Outcome::Leak(place) => {
                    if self.relation == Relation::Calls {
                        hedges.push(Hedge::Leak {
                            file: at.file.clone(),
                            line: at.line,
                            place: *place,
                        });
                    }
                }
                Outcome::Candidate(reason) => {
                    if entry.counts_for(self.relation) {
                        hedges.push(Hedge::Candidate {
                            file: at.file.clone(),
                            line: at.line,
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
        if self.relation == Relation::Calls {
            hedges.extend(self.implicit.iter().copied().map(Hedge::Implicit));
        }
        hedges.extend(self.alias_bound.map(Hedge::AliasBound));
        hedges.extend(
            self.too_common
                .map(|occurrences| Hedge::TooCommon { occurrences }),
        );
        if hedges.is_empty() {
            Verdict::Complete
        } else {
            Verdict::Partial(hedges)
        }
    }
}

/// What stands in the way of `Complete`: an occurrence (file and line) or a
/// resource of the account.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Hedge {
    Candidate {
        file: RepoPath,
        line: u32,
        reason: Reason,
    },
    Leak {
        file: RepoPath,
        line: u32,
        place: ValuePlace,
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

/// The shadow verdict of one account.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Verdict {
    Complete,
    Partial(Vec<Hedge>),
}

impl Verdict {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Partial(_) => "partial",
        }
    }

    pub const fn is_complete(&self) -> bool {
        matches!(self, Self::Complete)
    }
}

mod alias;

pub use alias::{close_aliases, value_use};
