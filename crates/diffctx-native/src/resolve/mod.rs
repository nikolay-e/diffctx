//! What each reference and import of a file's facts lands on.
//!
//! The facts layer states what a file writes; this layer says what it means:
//! every `Ref` and `Import` becomes a [`Binding`] with a relation (calls, reads,
//! writes, instantiates, passes as value, imports, re-exports, overrides) and a
//! resolution with its evidence. `Resolved` names the one definition a static
//! chain of the file and its imports leads to; `Candidate` names the definitions
//! a receiver the code does not type may be; `Unresolved` says why nothing is
//! claimed, and only some of those reasons are an unknown (see
//! [`UnresolvedReason::is_unknown`]).
//!
//! Two rules hold for every language here: a name nothing resolved never crosses
//! a project root, and a link between two projects exists only through a declared
//! dependency (`WorkspaceModel::may_reference` decides, in one place).
//!
//! The layer reads a repository only through `crate::snapshot` and `crate::facts`
//! (`tests/resolve_isolation.rs` holds it).
//!
//! An empty value is never how a failed read or a lost fact looks here: the
//! methods that make one (`Result::ok`, `unwrap_or_default`) and a direct read of
//! the disk or of a child process are denied in this module. A justified use
//! carries `#[allow(clippy::disallowed_methods, reason = "...")]`.

#![deny(clippy::disallowed_methods)]
#![allow(clippy::cognitive_complexity, clippy::significant_drop_tightening)]

#[macro_use]
mod solved;
#[macro_use]
mod twins;

mod c;
mod chart;
mod config;
mod dockerfile;
mod go;
mod index;
mod java;
mod jvm;
mod kotlin;
mod modules;
mod php;
mod python;
mod rust;
mod shell;
mod sql;
mod swift;
mod terraform;
mod typescript;

pub use c::CResolver;
pub use config::ConfigResolver;
pub use dockerfile::DockerfileResolver;
pub use go::GoResolver;
pub use java::JavaResolver;
pub use kotlin::KotlinResolver;
pub use php::{DYNAMIC, PhpResolver};
pub use python::PythonResolver;
pub use rust::RustResolver;
pub use shell::ShellResolver;
pub use solved::Solved;
pub use sql::SqlResolver;
pub use swift::SwiftResolver;
pub use terraform::TerraformResolver;
pub use typescript::TypeScriptResolver;

use crate::facts::{DefKey, DefKind, Name, Scope, Span, UseKind};
use crate::snapshot::RepoPath;

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub enum Relation {
    Calls,
    Reads,
    Writes,
    Instantiates,
    PassesAsValue,
    /// A name written where a type is expected: an annotation, a base class.
    TypeUses,
    Imports,
    /// A name a module takes from another and offers on as its own (`__init__`).
    Reexports,
    /// A method that replaces one of a base class.
    Overrides,
}

impl Relation {
    pub const ALL: [Self; 9] = [
        Self::Calls,
        Self::Reads,
        Self::Writes,
        Self::Instantiates,
        Self::PassesAsValue,
        Self::TypeUses,
        Self::Imports,
        Self::Reexports,
        Self::Overrides,
    ];

    /// The one spelling of a relation: `lang::RelationKind` and the case schema
    /// carry the same names (`lang::kind_of_use` maps one to the other).
    pub const fn name(self) -> &'static str {
        match self {
            Self::Calls => "calls",
            Self::Reads => "reads",
            Self::Writes => "writes",
            Self::Instantiates => "instantiates",
            Self::PassesAsValue => "passes_as_value",
            Self::TypeUses => "type_use",
            Self::Imports => "imports",
            Self::Reexports => "reexports",
            Self::Overrides => "overrides",
        }
    }

    /// The relations a change to the target reaches the reference through: the
    /// ones that use the definition, as opposed to naming it in a type or moving
    /// it between modules.
    pub const fn uses(self) -> bool {
        matches!(
            self,
            Self::Calls
                | Self::Reads
                | Self::Writes
                | Self::Instantiates
                | Self::PassesAsValue
                | Self::Overrides
        )
    }
}

fn plain_relation(use_kind: UseKind) -> Relation {
    match use_kind {
        UseKind::Call | UseKind::Decorate => Relation::Calls,
        UseKind::Read => Relation::Reads,
        UseKind::Write => Relation::Writes,
        UseKind::TypeUse => Relation::TypeUses,
        UseKind::PassAsValue => Relation::PassesAsValue,
        UseKind::Instantiate => Relation::Instantiates,
        UseKind::Override => Relation::Overrides,
    }
}

fn unresolved(reason: UnresolvedReason) -> Resolution {
    Resolution::Unresolved { reason }
}

fn is_value(kind: DefKind) -> bool {
    matches!(
        kind,
        DefKind::Property
            | DefKind::Variable
            | DefKind::Constant
            | DefKind::Field
            | DefKind::EnumMember
    )
}

fn is_callable(kind: DefKind) -> bool {
    matches!(
        kind,
        DefKind::Function | DefKind::Method | DefKind::Constructor
    )
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum EvidenceKind {
    /// The name is defined in the scope or a scope around it.
    SameScope,
    /// `from m import name`.
    ImportedName,
    /// `module.name`, the module bound by an import.
    ModuleAttribute,
    /// `from m import *`.
    StarImport,
    /// `self.name`, found on the enclosing class.
    SelfReceiver,
    /// Found on a base class of the receiver's class.
    Inherited,
    /// The receiver is annotated with its class.
    TypedReceiver,
    /// The receiver is an instance the code builds: `x = Cls()`, `Cls().m()`.
    Constructed,
    /// The receiver is what a function annotated `-> Cls` returns.
    FactoryReturn,
    /// `super().name`.
    SuperCall,
    /// A method that replaces the base class's.
    Overridden,
    /// A name bound to another definition's value or member by a static
    /// chain: `internals = options.internals`, `const {f} = Class`.
    Aliased,
    /// `importlib.import_module("m")` with a literal.
    DynamicImport,
    /// A module found only under a project the importer names as a requirement
    /// without a source.
    AmbiguousModule,
}

/// How a binding was reached: the kind, and the modules a name passed through
/// on the way (an `__init__` that re-exports it).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Evidence {
    pub kind: EvidenceKind,
    pub through: Vec<RepoPath>,
}

impl Evidence {
    pub(crate) fn new(kind: EvidenceKind) -> Self {
        Self {
            kind,
            through: Vec::new(),
        }
    }

    pub(crate) fn through(kind: EvidenceKind, through: Vec<RepoPath>) -> Self {
        Self { kind, through }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum CandidateReason {
    /// A method call on a receiver the code does not type: any class of a file
    /// the caller can reach that has the method may be it.
    UntypedReceiver,
    /// The receiver is annotated `Any` or `object`: any class of a project the
    /// caller may reference.
    AnyReceiver,
    /// The module is found only under a project the caller's project names as a
    /// requirement without a source: the name may be that project or an index's.
    AmbiguousModule,
    /// The specifier is an alias or subpath import no configuration places, or
    /// the project's configuration could not be read whole: the name may be a
    /// definition of that name in any module.
    ImportUnresolved,
    /// A module a glob (`import.meta.glob`, `require.context`) loads: the
    /// member asked of it is any definition of that name in the modules the
    /// pattern matches.
    ImportGlob,
    /// A global of a classic script or a worker that loads no script naming it:
    /// any script of the repository that sets it.
    Global,
    /// Several declarations of the name in view (overloads): which one a call
    /// reaches is for the compiler to say.
    Overloaded,
    /// A call through a supertype of a type that overrides the member: it may
    /// run in the override.
    Dispatch,
    /// A name a macro of the project defines: what the call or the use is, once
    /// the preprocessor has run, is not read.
    Macro,
    /// A function found by argument-dependent lookup: any function of the name
    /// in a namespace the argument types may bring in.
    Adl,
    /// Several objects of the name and kind stand in view, in namespaces the
    /// referring file does not tell apart: it may be any of them.
    AnyNamespace,
    /// A name found only with another case than the one written: the language
    /// does not say names are case-insensitive, and its tools disagree.
    CaseFold,
    /// A field whose key ends in `Name` or `Ref` and no row of the table names:
    /// it may name an object of that name, of any kind.
    NameSuffix,
    /// A label selector: every definition of labels in its namespace that holds
    /// all of its pairs. A selector is a candidate by nature: it may carry
    /// expressions, and the labels may be set elsewhere.
    LabelSelector,
    /// A chart's own `values.yaml`, read by every release of it: the repository
    /// names no release, so it may be overridden or not.
    NoRelease,
    /// An unqualified name and a definition of another schema: the schemas it
    /// resolves through are the session's search path, which no file states.
    SearchPath,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum UnresolvedReason {
    /// A parameter or a local variable: no definition is meant.
    Local,
    /// A name the language provides.
    Builtin,
    /// A module the repository does not hold: the standard library, a package.
    External,
    /// A name nothing binds in the file.
    NoBinding,
    /// The class and every base class are known, and none has the member.
    NoMember,
    /// The class has a base class outside the repository, which most likely
    /// holds the member (`self.assertEqual`); not an unknown.
    ExternalBase,
    /// An attribute read, written or passed through a receiver nothing types
    /// (`response.headers`): it is any object's.
    UntypedRead,
    /// A call through an untyped receiver, and no reachable class has a method
    /// of that name.
    NoCandidates,
    /// A call through an untyped receiver, and too many classes have a method of
    /// that name for the list to say anything.
    TooManyCandidates,
    /// The target is in a project the reference's project does not declare a
    /// dependency on.
    OutsideProject,
    /// A relative import that lands on no file.
    MissingModule,
    /// A receiver chain the facts do not let the resolver follow.
    Opaque,
    /// A selector no object's labels satisfy: it selects nothing now, and may
    /// have selected what a change took the labels from.
    NoMatch,
}

impl UnresolvedReason {
    /// Whether a name that was not found is for the project to answer: what lies
    /// in outside code is not.
    pub const fn is_ours(self) -> bool {
        !matches!(self, Self::External | Self::Builtin | Self::OutsideProject)
    }

    /// Whether the reference may be a use of something in the repository that
    /// was not found: what a report must disclose rather than count as nothing.
    pub const fn is_unknown(self) -> bool {
        matches!(
            self,
            Self::NoBinding
                | Self::UntypedRead
                | Self::NoCandidates
                | Self::TooManyCandidates
                | Self::MissingModule
                | Self::Opaque
        )
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Resolution {
    Resolved {
        target: DefKey,
        evidence: Evidence,
    },
    /// An import of a whole module: the file it lands on.
    Module {
        file: RepoPath,
        evidence: Evidence,
    },
    Candidate {
        targets: Vec<DefKey>,
        reason: CandidateReason,
    },
    Unresolved {
        reason: UnresolvedReason,
    },
}

/// One reference or import of one file, and what it lands on.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Binding {
    pub file: RepoPath,
    pub site: Span,
    /// Who owns the site.
    pub from: Scope,
    pub relation: Relation,
    /// The name as written (the last attribute of a chain).
    pub name: Name,
    pub resolution: Resolution,
    /// The site is in the body of an `if __name__ == "__main__":` guard: the run
    /// of the script, not a caller another module could have.
    pub entry_guard: bool,
}

impl Binding {
    /// The definitions the binding may land on, resolved or candidate.
    pub fn targets(&self) -> Vec<&DefKey> {
        match &self.resolution {
            Resolution::Resolved { target, .. } => vec![target],
            Resolution::Candidate { targets, .. } => targets.iter().collect(),
            Resolution::Module { .. } | Resolution::Unresolved { .. } => Vec::new(),
        }
    }
}

/// A definition of the resolved snapshot.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Definition {
    pub file: RepoPath,
    pub key: DefKey,
    pub kind: DefKind,
    pub span: Span,
    pub name_span: Span,
}
