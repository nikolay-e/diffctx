//! `diffctx.impact.v2`: the impact answer's wire form, and the map from v1.
//!
//! Every list is a [`Listing`] (a total and the items shown; what is omitted is
//! their difference, never stored), every count of uses a [`Count`] (exact only
//! under a complete search), every unknown a closed [`Gap`], every test fact one
//! [`Guard`], every shed row a [`ShedRows`] ledger entry. No field holds prose
//! other than the repository's own text (paths, symbols, commit-less). The shape
//! is the one DESIGN.md §2 of the W6-A note gives; nothing here is wired into the
//! doors yet: [`ImpactV2::from_v1`] builds it from the v1 answer, field by field,
//! until the builders produce it natively.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::decide;
use crate::impact::{
    self, Caller, ChangedSymbol, Contract, DeletedModule, ImpactOutput, KeySearch, NamedFile,
    PathReference, StaleReference, UnanalysedFile, Unknown,
};
use crate::resource::LimitReason;

use super::Tally;

pub const IMPACT_SCHEMA_V2: &str = "diffctx.impact.v2";

/// A number of uses, and whether it is the whole: `exact` only under the witness
/// of a complete search, otherwise `n` is the least there are.
#[derive(Serialize, JsonSchema, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Count {
    pub n: usize,
    pub exact: bool,
}

impl From<decide::Count> for Count {
    fn from(count: decide::Count) -> Self {
        Self {
            n: count.found(),
            exact: matches!(count, decide::Count::Exact(..)),
        }
    }
}

/// A list and its total, which travel together: the items shown are the first of
/// `total`.
#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug)]
pub struct Listing<T> {
    pub total: usize,
    pub items: Vec<T>,
}

impl<T> Default for Listing<T> {
    fn default() -> Self {
        Self {
            total: 0,
            items: Vec::new(),
        }
    }
}

impl<T> Listing<T> {
    /// Nothing listed and nothing omitted: the document leaves such a list out.
    pub fn is_empty(&self) -> bool {
        self.total == 0
    }

    /// `items`, and `omitted` more the list lost elsewhere.
    pub fn of(items: Vec<T>, omitted: usize) -> Self {
        Self {
            total: items.len() + omitted,
            items,
        }
    }

    /// What the list holds and does not show.
    pub fn omitted(&self) -> usize {
        self.total - self.items.len()
    }
}

/// A definition, named where a v1 string said `path::symbol (lines)`.
#[derive(Serialize, JsonSchema, Clone, Debug, PartialEq, Eq)]
pub struct DefRef {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    pub lines: String,
}

impl DefRef {
    /// Reads `path::symbol (lines)`, `path::symbol` and `path:lines`, the three
    /// spellings v1 wrote a definition in.
    pub fn parse(text: &str) -> Self {
        if let Some((path, rest)) = text.split_once("::") {
            let (symbol, lines) = match rest.rsplit_once(" (") {
                Some((symbol, lines)) => (symbol, lines.trim_end_matches(')')),
                None => (rest, ""),
            };
            return Self {
                path: path.to_string(),
                symbol: Some(symbol.to_string()),
                lines: lines.to_string(),
            };
        }
        match text.rsplit_once(':') {
            Some((path, lines)) if lines.chars().all(|c| c.is_ascii_digit() || c == '-') => Self {
                path: path.to_string(),
                symbol: None,
                lines: lines.to_string(),
            },
            _ => Self {
                path: text.to_string(),
                symbol: None,
                lines: String::new(),
            },
        }
    }
}

/// Why a search of a definition's uses is not complete: the closed vocabulary that
/// replaces v1's sentence in `unresolved`.
#[derive(Serialize, JsonSchema, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Gap {
    /// Uses of the name that could be of the definition landed on nothing the model
    /// could name: an untyped read, a name no import binds.
    Unlanded {
        /// The name that landed on nothing.
        name: String,
        uses: usize,
    },
    /// The two sides of the range pair the definition in more than one way.
    Unpaired {
        name: String,
    },
    BaseUnresolved {
        reason: String,
    },
    /// The run stopped before the search finished.
    Stopped {
        reasons: Vec<LimitReason>,
    },
    OutsideScope {
        files: usize,
    },
    /// `files` of the definition's interaction group the run could neither read as
    /// source nor rule out by the names they spell: why, in `reason`.
    Unaccounted {
        reason: decide::Reason,
        files: usize,
    },
    Withheld {
        files: usize,
    },
    Untracked {
        files: usize,
        /// The block-scoped binding no tracked definition stands for, when one does.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// A member of a trait, or the implementation of one: calls arrive through
    /// `dyn Trait` and generic bounds.
    Dispatched {
        via: String,
    },
    /// `files` that could name it declare items through a macro (`what`) that is
    /// not expanded.
    Macros {
        files: usize,
        what: String,
    },
    Capped,
    NotTraced,
    /// A name defined in many files of a language without a binding model.
    Ambiguous {
        name: String,
        definitions: usize,
        language: String,
    },
    /// Code in a language no binding model reads.
    NotModelled {
        language: String,
    },
    /// No direct caller was found, and a supertype, an interface or the runtime can
    /// call it through `via`, which nothing traces.
    Overridable {
        via: String,
    },
    /// `files` hold a template that was read around, not expanded.
    Templated {
        files: usize,
    },
    /// `files` of a chart or release whose values or context are not determined.
    ContextUnbound {
        files: usize,
    },
    /// `files` generate objects whose names are not written in any file.
    RenderedElsewhere {
        files: usize,
    },
    /// `pointers` name a source outside the repository, which was not followed.
    ExternalSource {
        pointers: usize,
    },
    /// `files` run or read a path the shell computes at run time.
    SourceFlag {
        files: usize,
    },
    /// Untracked directories the read of the checkout did not enter: `total` of
    /// them, `directories` the first named. A use of the name in them is unknown.
    UnreadDirectories {
        directories: Vec<String>,
        total: usize,
    },
}

impl From<crate::facts::Delta> for Delta {
    fn from(delta: crate::facts::Delta) -> Self {
        Self {
            signature: delta.signature,
            body: delta.body,
            doc: delta.doc,
            visibility: delta.visibility,
            decorators: delta.decorators,
        }
    }
}

impl Change {
    /// What the range did to a definition, as the cell read it.
    pub fn of(def: &decide::Changed) -> Self {
        Self {
            kind: match def.how {
                decide::How::Added => ChangeKind::Added,
                decide::How::Removed => ChangeKind::Removed,
                decide::How::Edited { layout_only: true } => ChangeKind::LayoutOnly,
                decide::How::Edited { layout_only: false } => ChangeKind::Edited,
            },
            delta: def.delta.map(Delta::from),
            origin: def.origin.map(str::to_string),
        }
    }
}

impl std::fmt::Display for Gap {
    /// A gap in words, as a sentence about what was not covered.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unlanded { uses, .. } => write!(f, "{uses} use(s) that landed on nothing"),
            Self::Unpaired { .. } => f.write_str("its two sides of the range pair ambiguously"),
            Self::BaseUnresolved { reason } => {
                write!(f, "the base side of the range was not resolved ({reason})")
            }
            Self::Stopped { reasons } => {
                write!(f, "the run was limited ({})", impact::reason_names(reasons))
            }
            Self::OutsideScope { files } => write!(
                f,
                "{files} file(s) outside the run's pathspecs were left out"
            ),
            Self::Unaccounted { reason, files: 0 } => {
                write!(f, "no file of its interaction group was read: {reason}")
            }
            Self::Unaccounted { reason, files } => write!(
                f,
                "{files} file(s) of its interaction group not read: {reason}"
            ),
            Self::Withheld { files } => write!(f, "{files} file(s) withheld by ignore rules"),
            Self::Untracked { files, .. } => write!(
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
            Self::Ambiguous {
                name,
                definitions,
                language,
            } => write!(
                f,
                "`{name}` is ambiguous: {definitions} {language} definitions and no binding model to tell them apart"
            ),
            Self::NotModelled { language } => write!(f, "no binding model reads {language}"),
            Self::Overridable { via } => write!(
                f,
                "calls through {via} are not traced, and no direct caller was found"
            ),
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
            Self::UnreadDirectories { directories, total } => {
                write!(f, "{total} untracked director(y/ies) not read: ")?;
                f.write_str(&directories.join(", "))?;
                match total.saturating_sub(directories.len()) {
                    0 => Ok(()),
                    more => write!(f, " and {more} more"),
                }
            }
        }
    }
}

impl Gap {
    /// How much of the search the gap takes away; the lowest limits most. A limit
    /// of the whole search comes first, then what the model could not resolve, then
    /// what lay outside the scope asked for. The order is the answer's: a text
    /// that names one gap of a definition names the lowest, the same on every run.
    pub fn severity(&self) -> u8 {
        match self {
            Self::Stopped { .. } => 0,
            // A file that was lost or not read is a limit of the whole search; a file
            // only of a language no cell binds is a generic "no model", and any more
            // specific reason the answer has for the definition (an ambiguous name, a
            // name too common to trace) comes before it.
            Self::Unaccounted { reason, .. } if reason.lists_the_file() => 1,
            Self::Unaccounted { .. } => 20,
            Self::Withheld { .. } => 2,
            Self::Capped => 3,
            Self::Unpaired { .. } => 4,
            Self::Unlanded { .. } => 5,
            Self::Dispatched { .. } => 6,
            Self::Macros { .. } => 7,
            Self::NotModelled { .. } => 8,
            Self::Overridable { .. } => 9,
            Self::NotTraced => 10,
            Self::Ambiguous { .. } => 11,
            Self::BaseUnresolved { .. } => 12,
            Self::OutsideScope { .. } => 13,
            Self::Untracked { .. } => 14,
            Self::Templated { .. } => 15,
            Self::ContextUnbound { .. } => 16,
            Self::RenderedElsewhere { .. } => 17,
            Self::ExternalSource { .. } => 18,
            Self::SourceFlag { .. } => 19,
            Self::UnreadDirectories { .. } => 21,
        }
    }

    /// A gap of the cell's search, with the name of the definition it is about.
    pub fn of(gap: &decide::Gap, name: &str) -> Self {
        match gap {
            decide::Gap::Unlanded { uses } => Self::Unlanded {
                name: name.to_string(),
                uses: *uses,
            },
            decide::Gap::Unpaired => Self::Unpaired {
                name: name.to_string(),
            },
            decide::Gap::BaseUnresolved { reason } => Self::BaseUnresolved {
                reason: reason.clone(),
            },
            decide::Gap::Stopped { reasons } => Self::Stopped {
                reasons: reasons.clone(),
            },
            decide::Gap::OutsideScope { files } => Self::OutsideScope { files: *files },
            decide::Gap::Unaccounted { reason, files } => Self::Unaccounted {
                reason: *reason,
                files: *files,
            },
            decide::Gap::Withheld { files } => Self::Withheld { files: *files },
            decide::Gap::Untracked { files } => Self::Untracked {
                files: *files,
                name: None,
            },
            decide::Gap::Dispatched { via } => Self::Dispatched { via: via.clone() },
            decide::Gap::Macros { files, what } => Self::Macros {
                files: *files,
                what: (*what).to_string(),
            },
            decide::Gap::Capped => Self::Capped,
            decide::Gap::NotTraced => Self::NotTraced,
            decide::Gap::Templated { files } => Self::Templated { files: *files },
            decide::Gap::ContextUnbound { files } => Self::ContextUnbound { files: *files },
            decide::Gap::RenderedElsewhere { files } => Self::RenderedElsewhere { files: *files },
            decide::Gap::ExternalSource { pointers } => Self::ExternalSource {
                pointers: *pointers,
            },
            decide::Gap::SourceFlag { files } => Self::SourceFlag { files: *files },
            decide::Gap::UnreadDirectories { directories, total } => Self::UnreadDirectories {
                directories: directories.clone(),
                total: *total,
            },
        }
    }
}

/// What the search of one group of relations covered. `complete` is the witness: no
/// row of the group in the answer then means no such use exists in the analysed
/// scope, and a count over it is exact.
#[derive(Serialize, JsonSchema, Clone, Debug)]
pub struct Search {
    pub complete: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gaps: Vec<Gap>,
}

impl Search {
    /// What a search covered, its gaps about the definition `name`.
    pub fn of(coverage: &decide::Coverage, name: &str) -> Self {
        Self {
            complete: coverage.witness().is_some(),
            gaps: coverage.gaps().iter().map(|g| Gap::of(g, name)).collect(),
        }
    }
}

/// Calls (calls, constructions, overrides) and reads (reads, writes, a value passed
/// on) are searched, and can be known, apart.
#[derive(Serialize, JsonSchema, Clone, Debug)]
pub struct Searched {
    pub calls: Search,
    pub reads: Search,
}

/// Whether a test reaches a caller: v1's `tested_by`, `tested_via`, `file_tested`,
/// `registered_by` and `unread_test`, as one state and what names it.
#[derive(Serialize, JsonSchema, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GuardState {
    /// A test reaches it by a static link.
    Linked,
    /// A test reaches its file but none of the paths to it.
    FileOnly,
    /// Tests were in view and none reaches it.
    None,
    /// A decorator registers it: whatever reads the registry reaches it.
    Registered,
    /// A framework enters it (a route, a command, a job): `registrar` states how.
    EntryPoint,
    /// A test file the size cap kept out names it.
    Unread,
    /// No test file was in view.
    NoTests,
    /// The search for a test stopped at its hop bound with uses left above it: a
    /// test may be there, so no absence is claimed.
    NotEstablished,
}

/// Tests a guard names before the rest are counted.
pub const TESTS_NAMED: usize = 3;

/// A test that reaches a caller, beyond the first the guard names.
#[derive(Serialize, JsonSchema, Clone, Debug, PartialEq, Eq)]
pub struct GuardTest {
    /// The test file.
    pub test: String,
    /// The test function, where the answer resolved it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The function between the caller and the test.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
}

#[derive(Serialize, JsonSchema, Clone, Debug)]
pub struct Guard {
    pub state: GuardState,
    /// The first test file (`linked`) in path order, or the unread one (`unread`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test: Option<String>,
    /// The function of that test, where the answer resolved it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The function between the caller and the test.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    /// The other tests that reach the caller at the same fewest hops: the first
    /// of each test file, then the rest, in path then name order, three tests in all.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub also: Vec<GuardTest>,
    /// How many more reach it beyond those named.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub more: u32,
    /// The decorator (`registered`), or the framework's own sentence (`entry_point`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registrar: Option<String>,
}

impl Guard {
    pub fn bare(state: GuardState) -> Self {
        Self {
            state,
            test: None,
            name: None,
            via: None,
            also: Vec::new(),
            more: 0,
            registrar: None,
        }
    }

    pub fn of(caller: &Caller, tests_known: bool) -> Self {
        let bare = Self::bare;
        let named = crate::impact::named_tests(&caller.tests);
        if let Some((first, rest)) = named.split_first() {
            return Self {
                test: Some(first.path.clone()),
                name: first.name.clone(),
                via: first.via.clone(),
                also: rest
                    .iter()
                    .map(|t| GuardTest {
                        test: t.path.clone(),
                        name: t.name.clone(),
                        via: t.via.clone(),
                    })
                    .collect(),
                more: u32::try_from(caller.tests.len() - named.len()).unwrap_or(u32::MAX),
                ..bare(GuardState::Linked)
            };
        }
        if let Some(registrar) = &caller.registered_by {
            let state = if registrar.starts_with(crate::facts::framework::ENTRY_PREFIX) {
                GuardState::EntryPoint
            } else {
                GuardState::Registered
            };
            return Self {
                registrar: Some(registrar.clone()),
                ..bare(state)
            };
        }
        if let Some(test) = &caller.unread_test {
            return Self {
                test: Some(test.clone()),
                ..bare(GuardState::Unread)
            };
        }
        bare(if !tests_known {
            GuardState::NoTests
        } else if caller.tests_cut {
            GuardState::NotEstablished
        } else if caller.file_tested {
            GuardState::FileOnly
        } else {
            GuardState::None
        })
    }
}

/// How a row uses the definition, coarsely and always.
#[derive(Serialize, JsonSchema, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Class {
    Call,
    Reference,
    Implementation,
    Reexports,
}

/// How sure the resolver is of a row.
#[derive(Serialize, JsonSchema, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Standing {
    Resolved,
    Candidate,
    /// A word match: listed as text, never counted as a caller.
    Name,
}

/// A use of a changed definition (v1's `Caller`).
#[derive(Serialize, JsonSchema, Clone, Debug)]
pub struct Row {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    pub lines: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sites: Vec<u32>,
    pub class: Class,
    /// What the row does (`calls`, `reads`, ...), stated only where the (language,
    /// relation) cell is precise; absent elsewhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub standing: Standing,
    /// Why a candidate is one (`receiver_unresolved`, `import_unresolved`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub guard: Guard,
    /// The rule of the folding table that would fold the row; only under `unfold`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folded_by: Option<String>,
}

impl Row {
    /// A row that counts as a caller: resolved, and no re-export (counted apart).
    pub fn is_caller(&self) -> bool {
        self.standing == Standing::Resolved && self.class != Class::Reexports
    }

    /// A caller no static test link reaches although one could have: the one
    /// predicate behind the header count, the folded count and the line (#386).
    pub fn without_link(&self) -> bool {
        self.is_caller()
            && !matches!(
                self.guard.state,
                GuardState::Linked
                    | GuardState::Registered
                    | GuardState::EntryPoint
                    | GuardState::NotEstablished
            )
    }

    pub fn of(caller: &Caller, tests_known: bool) -> Self {
        Self {
            path: caller.path.clone(),
            symbol: caller.symbol.clone(),
            lines: caller.lines.clone(),
            sites: caller.sites.clone(),
            class: match caller.relation {
                "reference" => Class::Reference,
                "implementation" => Class::Implementation,
                "reexports" => Class::Reexports,
                _ => Class::Call,
            },
            kind: caller.kind.map(str::to_string),
            standing: match caller.evidence {
                "candidate" => Standing::Candidate,
                "name" => Standing::Name,
                _ => Standing::Resolved,
            },
            reason: caller.reason.map(str::to_string),
            guard: Guard::of(caller, tests_known),
            folded_by: caller.folded_by.map(str::to_string),
        }
    }
}

/// Rows the answer cap shed, by what they were: the ledger every total of shed rows
/// is a sum of, filled where the rows are shed.
#[derive(Serialize, JsonSchema, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ShedRows {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub resolved: usize,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub candidate: usize,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub name: usize,
    /// Of those, the rows no static test link reaches.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub untested: usize,
}

impl ShedRows {
    /// Every row shed: `untested` is a part of them, not more.
    pub fn total(&self) -> usize {
        self.resolved + self.candidate + self.name
    }

    /// Adds another ledger.
    pub fn merge(&mut self, other: Self) {
        self.resolved += other.resolved;
        self.candidate += other.candidate;
        self.name += other.name;
        self.untested += other.untested;
    }

    /// Records a row the cap shed.
    pub fn add(&mut self, row: &Caller) {
        match row.evidence {
            "candidate" => self.candidate += 1,
            "name" => self.name += 1,
            _ => self.resolved += 1,
        }
        if impact::without_test_link(row) {
            self.untested += 1;
        }
    }
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero(n: &usize) -> bool {
    *n == 0
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero_u32(n: &u32) -> bool {
    *n == 0
}

#[derive(Serialize, JsonSchema, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Removed,
    Edited,
    LayoutOnly,
}

/// What differs between the two sides of an edited definition: the folding
/// table's own reading.
#[derive(Serialize, JsonSchema, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Delta {
    pub signature: bool,
    pub body: bool,
    pub doc: bool,
    pub visibility: bool,
    pub decorators: bool,
}

#[derive(Serialize, JsonSchema, Clone, Debug)]
pub struct Change {
    pub kind: ChangeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta: Option<Delta>,
    /// `moved` or `renamed`, when the pairing says so.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
}

/// A definition that is a local of another, not one of its own: its uses are the
/// ones inside the enclosing one.
#[derive(Serialize, JsonSchema, Clone, Debug)]
pub struct LocalOf {
    pub enclosing: String,
}

/// Rows of one definition the folding table says the change cannot have affected,
/// counted in one line.
#[derive(Serialize, JsonSchema, Clone, Debug)]
pub struct Fold {
    pub rule: String,
    pub kind: String,
    pub count: Count,
    /// Tests first, then by path and line.
    pub examples: Vec<DefRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parameters: Vec<String>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub sharing: usize,
}

#[derive(Serialize, JsonSchema, Clone, Debug)]
pub struct Definition {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    /// Head-side lines; base-side when the change is a removal.
    pub lines: String,
    /// The definition's kind; absent when v1 said only that it was removed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub change: Change,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_of: Option<LocalOf>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub commits: usize,
    /// The role that gives it an implicit caller: a runner, a registry, `main`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
    /// Whether a test reaches the definition itself; stated only when none does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard: Option<Guard>,
    pub rows: Vec<Row>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub folded: Vec<Fold>,
    /// Users the range changes too, resolved: the diff's own lines.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub also_changed: Vec<DefRef>,
    /// Users the range changes too whose binding is not resolved.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub possibly_also_changed: Vec<DefRef>,
    pub coverage: Searched,
    /// Files in view that spell the name but were not analysed whole: a use there is
    /// not seen, so an empty `rows` says nothing about them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missed_in: Vec<UnanalysedFile>,
    /// Why the search is incomplete, in words of the closed vocabulary.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gaps: Vec<Gap>,
    pub shed: ShedRows,
}

impl Definition {
    /// The name a word match is said to name: the definition's own, or its file.
    pub fn word(&self) -> &str {
        self.symbol
            .as_deref()
            .map_or(self.path.as_str(), crate::impact::base_symbol)
    }

    /// Both groups of its uses were searched whole: no row listed means no such use.
    pub fn complete(&self) -> bool {
        self.coverage.calls.complete && self.coverage.reads.complete
    }

    pub fn removed(&self) -> bool {
        self.change.kind == ChangeKind::Removed
    }

    /// The gap that limits its search most (see [`Gap::severity`]); the first of
    /// equals.
    pub fn limit(&self) -> Option<&Gap> {
        self.gaps.iter().min_by_key(|g| g.severity())
    }

    /// Nothing outside the diff uses it, and its search was whole: the definitions
    /// the range's own changes use, or removed with no reference left.
    pub fn only_in_the_diff(&self) -> bool {
        (self.removed() || !(self.also_changed.is_empty() && self.possibly_also_changed.is_empty()))
            && self.rows.is_empty()
            && self.complete()
            && self.folded.is_empty()
            && self.commits < 2
    }
}

/// A definition the cap shed, and what it had.
#[derive(Serialize, JsonSchema, Clone, Debug)]
pub struct OmittedDefinition {
    pub def: DefRef,
    pub shed: ShedRows,
    /// Its search for calls was not complete when it was shed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub incomplete: bool,
}

/// What the answer cap shed whole: the one count of definitions, the names that
/// fit, and the budget that shows them.
#[derive(Serialize, JsonSchema, Clone, Debug)]
pub struct Omitted {
    pub definitions: Listing<OmittedDefinition>,
    /// The rows of every shed definition, named or not: the names are capped, this is
    /// not.
    pub shed: ShedRows,
    pub needs: u32,
    /// Of the shed definitions, named or not, those whose search for calls was not
    /// complete: the counts that include their rows are the least there are.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub incomplete: usize,
}

#[derive(Serialize, JsonSchema, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum How {
    Deleted,
    Renamed,
}

/// A line that still names a path the change deleted or renamed.
#[derive(Serialize, JsonSchema, Clone, Debug)]
pub struct StaleRef {
    pub path: String,
    pub line: u32,
    pub target: String,
    pub how: How,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// The kind of place the line sits in: a script names the path as a command, a
    /// document only talks about it.
    pub class: crate::impact::MentionClass,
}

impl From<&StaleReference> for StaleRef {
    fn from(r: &StaleReference) -> Self {
        let renamed = r.kind.strip_prefix("renamed to ");
        Self {
            path: r.path.clone(),
            line: r.line,
            target: r.target.clone(),
            how: if renamed.is_some() {
                How::Renamed
            } else {
                How::Deleted
            },
            to: renamed.map(str::to_string),
            class: r.class,
        }
    }
}

/// Lines outside the diff that name a changed file by its path, or by a key it
/// changed.
#[derive(Serialize, JsonSchema, Clone, Debug, Default)]
pub struct Named {
    #[serde(default, skip_serializing_if = "Listing::is_empty")]
    pub by_path: Listing<PathReference>,
    #[serde(default, skip_serializing_if = "Listing::is_empty")]
    pub by_key: Listing<PathReference>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub key_searches: Vec<KeySearch>,
    #[serde(default, skip_serializing_if = "Listing::is_empty")]
    pub collections: Listing<crate::impact::CollectionRead>,
}

impl Named {
    pub fn is_empty(&self) -> bool {
        self.by_path.is_empty()
            && self.by_key.is_empty()
            && self.key_searches.is_empty()
            && self.collections.is_empty()
    }
}

/// A name no file defines, spelled in files.
#[derive(Serialize, JsonSchema, Clone, Debug)]
pub struct Spelled {
    pub files: usize,
    pub rows: Vec<NamedFile>,
    /// What this build read as text and could not tell a definition in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gap: Option<Gap>,
}

#[derive(Serialize, JsonSchema, Clone, Debug)]
pub struct Tests {
    /// Whether any test file was in view: without one, an absent link says nothing.
    pub known: bool,
    /// How many call hops a static test link reaches.
    pub hops: usize,
    pub definitions_without_link: Count,
}

#[derive(Serialize, JsonSchema, Clone, Debug)]
pub struct Entered {
    pub def: DefRef,
    pub role: String,
}

/// A definition listed apart from the entries, with the role that gives it an
/// implicit caller when it has one (a runner, a registry): the role is why a missing
/// test link is a finding there.
#[derive(Serialize, JsonSchema, Clone, Debug)]
pub struct Unreached {
    #[serde(flatten)]
    pub def: DefRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

/// v1 wrote a definition and its role in one string: `path::symbol (lines) — role`.
fn split_role(text: &str) -> (&str, Option<String>) {
    match text.split_once(" \u{2014} ") {
        Some((def, role)) => (def, Some(role.to_string())),
        None => (text, None),
    }
}

impl Unreached {
    fn parse(text: &str) -> Self {
        let (def, role) = split_role(text);
        Self {
            def: DefRef::parse(def),
            role,
        }
    }
}

/// Changed definitions the answer does not list as entries, each for a reason.
#[derive(Serialize, JsonSchema, Clone, Debug, Default)]
pub struct Unlisted {
    /// Complete searches found no caller: the absence claim.
    #[serde(default, skip_serializing_if = "Listing::is_empty")]
    pub no_caller: Listing<Unreached>,
    #[serde(default, skip_serializing_if = "Listing::is_empty")]
    pub called_in_diff: Listing<Unreached>,
    #[serde(default, skip_serializing_if = "Listing::is_empty")]
    pub entered_by: Listing<Entered>,
    #[serde(default, skip_serializing_if = "Listing::is_empty")]
    pub layout_only: Listing<DefRef>,
}

impl Unlisted {
    pub fn is_empty(&self) -> bool {
        self.no_caller.is_empty()
            && self.called_in_diff.is_empty()
            && self.entered_by.is_empty()
            && self.layout_only.is_empty()
    }
}

/// What the run did not cover as a whole.
#[derive(Serialize, JsonSchema, Clone, Debug)]
pub struct RunCoverage {
    pub scope: Search,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limits: Vec<LimitReason>,
    #[serde(default, skip_serializing_if = "Listing::is_empty")]
    pub unanalysed: Listing<UnanalysedFile>,
    /// Of the unanalysed files left out of the listing, those outside code that only
    /// their path was searched for.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub untraced_omitted: usize,
    /// Definitions the run stopped before it searched and the cap then dropped.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub not_searched: usize,
    /// Changed configs whose keys and values the capped text search left out.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub keys_unsearched: usize,
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RangeKind {
    Committed,
    Staged,
    Worktree,
    Window,
    Symbol,
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug)]
pub struct Range {
    pub kind: RangeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_tree: Option<String>,
    /// Commits the range holds.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub commits: usize,
    /// The newest of those commits' messages, subject and body: locate and pack
    /// carry them, impact does not.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<String>,
}

#[derive(Serialize, JsonSchema, Clone, Debug, Default)]
pub struct Inventory {
    #[serde(default, skip_serializing_if = "Listing::is_empty")]
    pub changed: Listing<String>,
    #[serde(default, skip_serializing_if = "Listing::is_empty")]
    pub untracked: Listing<String>,
    #[serde(default, skip_serializing_if = "Listing::is_empty")]
    pub deleted: Listing<String>,
    #[serde(default, skip_serializing_if = "Listing::is_empty")]
    pub lockfiles: Listing<String>,
    /// Files the run withheld (ignore rules, secret names): not read.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub withheld: usize,
}

impl Inventory {
    pub fn is_empty(&self) -> bool {
        self.changed.is_empty()
            && self.untracked.is_empty()
            && self.deleted.is_empty()
            && self.lockfiles.is_empty()
            && self.withheld == 0
    }
}

/// The totals the Markdown header prints, from the same leaves.
#[derive(Serialize, JsonSchema, Clone, Debug)]
pub struct Summary {
    pub definitions: usize,
    pub callers: Count,
    pub possible: usize,
    pub folded: usize,
    pub shed: usize,
    pub untested_callers: usize,
    pub files: usize,
    /// Changed definitions whose callers were traced; none with comments or
    /// formatting only is "callers not traced", not "no callers".
    pub traced: usize,
}

/// The cap the answer was fitted to.
#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug)]
pub struct Fit {
    /// The budget asked for; absent when the run sized the answer itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asked: Option<u32>,
    pub cap: u32,
    /// What showing everything would take, to the next hundred tokens; absent
    /// until a fit measured it.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub needs: u32,
    /// Set when the least the answer can say still outgrows the cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub least: Option<u32>,
    /// Tokens the cap does not charge: a raw diff, which is input verbatim.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub exempt: u32,
}

#[derive(Serialize, JsonSchema, Clone, Debug)]
pub struct Provenance {
    pub engine: crate::run_provenance::Engine,
}

#[derive(Serialize, JsonSchema, Clone, Debug)]
pub struct ImpactV2 {
    pub schema: &'static str,
    pub name: String,
    pub provenance: Provenance,
    pub range: Range,
    /// The name asked for, on a `--symbol` query.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spelled: Option<Spelled>,
    #[serde(default, skip_serializing_if = "Inventory::is_empty")]
    pub inventory: Inventory,
    pub definitions: Vec<Definition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omitted: Option<Omitted>,
    #[serde(default, skip_serializing_if = "Unlisted::is_empty")]
    pub unlisted: Unlisted,
    #[serde(default, skip_serializing_if = "Listing::is_empty")]
    pub contracts: Listing<Contract>,
    #[serde(default, skip_serializing_if = "Listing::is_empty")]
    pub stale: Listing<StaleRef>,
    #[serde(default, skip_serializing_if = "Named::is_empty")]
    pub named: Named,
    #[serde(default, skip_serializing_if = "Listing::is_empty")]
    pub deleted_modules: Listing<DeletedModule>,
    pub tests: Tests,
    pub coverage: RunCoverage,
    /// Nothing outside the diff depends on the change, and no search left a gap.
    pub empty: bool,
    pub summary: Summary,
    pub fit: Fit,
}

/// JSON Schema 2020-12 for `diffctx.impact.v2`.
pub fn impact_v2_schema() -> serde_json::Value {
    let mut generator = schemars::generate::SchemaSettings::draft2020_12().into_generator();
    let schema = generator.root_schema_for::<ImpactV2>();
    serde_json::to_value(schema).expect("schema serializes")
}

// ---- v1 -> v2, field by field

pub(crate) fn range_kind(spec: Option<&str>, index_tree: Option<&str>, symbol: bool) -> RangeKind {
    if symbol {
        return RangeKind::Symbol;
    }
    let digits_then_unit = |s: &str| {
        let unit = s.trim_start_matches(|c: char| c.is_ascii_digit());
        unit.len() < s.len() && !unit.is_empty() && unit.chars().all(|c| c.is_ascii_alphabetic())
    };
    match spec {
        _ if index_tree.is_some() => RangeKind::Staged,
        None | Some("HEAD") => RangeKind::Worktree,
        Some(s) if s.contains("..") => RangeKind::Committed,
        Some(s) if digits_then_unit(s) => RangeKind::Window,
        Some(_) => RangeKind::Committed,
    }
}

impl ImpactV2 {
    /// The v2 answer of a v1 one. `hops` is the call depth static test links reach
    /// (the engine's constant). Where v1 did not carry a fact (a change's delta, the
    /// split of shed rows) the field says so by its absence or by `unclassified`.
    pub fn from_v1(v1: &ImpactOutput, hops: usize) -> Self {
        let scope = impact::scope_coverage(v1);
        let listing = |items: &[String], omitted: usize| Listing::of(items.to_vec(), omitted);
        let (omitted_entry, listed): (Vec<&ChangedSymbol>, Vec<&ChangedSymbol>) = v1
            .changed
            .iter()
            .partition(|c| matches!(c.unresolved, Some(Unknown::Omitted { .. })));
        let omitted = omitted_entry.first().and_then(|entry| {
            let Some(Unknown::Omitted {
                needs,
                count,
                incomplete,
            }) = &entry.unresolved
            else {
                return None;
            };
            let named = entry
                .omitted
                .iter()
                .map(|d| OmittedDefinition {
                    def: DefRef {
                        lines: d.lines.clone(),
                        ..DefRef::parse(&d.definition)
                    },
                    shed: d.shed,
                    incomplete: d.incomplete,
                })
                .collect::<Vec<_>>();
            let total = *count;
            Some(Omitted {
                definitions: Listing {
                    total: total.max(named.len()),
                    items: named,
                },
                shed: entry.shed,
                needs: *needs,
                incomplete: *incomplete,
            })
        });
        let symbol_query = v1.symbol.is_some();
        let mut doc = Self {
            schema: IMPACT_SCHEMA_V2,
            name: v1.name.clone(),
            provenance: Provenance {
                engine: crate::run_provenance::Engine {
                    name: "diffctx",
                    version: env!("CARGO_PKG_VERSION"),
                    build: option_env!("DIFFCTX_BUILD_SHA"),
                },
            },
            range: Range {
                kind: range_kind(v1.range.as_deref(), v1.index_tree.as_deref(), symbol_query),
                spec: v1.range.clone(),
                index_tree: v1.index_tree.clone(),
                commits: v1.commit_count,
                messages: Vec::new(),
            },
            symbol: v1.symbol.clone(),
            spelled: v1.symbol_named.as_ref().map(|named| Spelled {
                files: named.files,
                rows: named.rows.clone(),
                gap: (!named.unread.is_empty()).then(|| Gap::Unaccounted {
                    reason: decide::Reason::NotRead,
                    files: named.unread.len(),
                }),
            }),
            inventory: Inventory {
                changed: listing(&v1.changed_files, v1.changed_files_omitted),
                untracked: listing(&v1.untracked_files, v1.untracked_files_omitted),
                deleted: listing(&v1.deleted_files, 0),
                lockfiles: listing(&v1.lockfile_changes, 0),
                withheld: v1.withheld_files,
            },
            definitions: listed
                .iter()
                .map(|c| definition(c, v1.tests_known))
                .collect(),
            omitted,
            unlisted: Unlisted {
                no_caller: Listing::of(unreached(&v1.untested_unlisted), 0),
                called_in_diff: Listing::of(unreached(&v1.untested_called_in_diff), 0),
                entered_by: Listing::of(
                    v1.entry_unlisted
                        .iter()
                        .map(|text| {
                            let (def, role) = split_role(text);
                            Entered {
                                def: DefRef::parse(def),
                                role: role.as_deref().unwrap_or("").to_string(),
                            }
                        })
                        .collect(),
                    0,
                ),
                layout_only: Listing::of(refs(&v1.layout_only), v1.layout_only_omitted),
            },
            contracts: Listing::of(v1.contracts.clone(), v1.contracts_omitted),
            stale: Listing::of(
                v1.stale_references.iter().map(StaleRef::from).collect(),
                v1.stale_references_omitted,
            ),
            named: Named {
                by_path: Listing::of(
                    v1.named_by
                        .iter()
                        .filter(|r| r.key.is_none())
                        .cloned()
                        .collect(),
                    v1.named_by_omitted,
                ),
                by_key: Listing::of(
                    v1.named_by
                        .iter()
                        .filter(|r| r.key.is_some())
                        .cloned()
                        .collect(),
                    0,
                ),
                key_searches: v1.key_searches.clone(),
                collections: Listing::of(v1.collection_reads.clone(), v1.collection_reads_omitted),
            },
            deleted_modules: Listing::of(v1.deleted_modules.clone(), v1.deleted_modules_omitted),
            tests: Tests {
                known: v1.tests_known,
                hops,
                definitions_without_link: Count {
                    n: v1.untested_definitions,
                    exact: scope.witness().is_some(),
                },
            },
            coverage: RunCoverage {
                scope: Search::of(&scope, ""),
                limits: v1.limits.clone(),
                unanalysed: Listing::of(v1.unanalysed_files.clone(), v1.unanalysed_files_omitted),
                untraced_omitted: v1.untraced_omitted,
                not_searched: v1.unfinished_omitted,
                keys_unsearched: v1.key_files_unsearched,
            },
            empty: v1.empty,
            // Filled below from the document's own leaves.
            summary: Summary {
                definitions: 0,
                callers: Count { n: 0, exact: false },
                possible: 0,
                folded: 0,
                shed: 0,
                untested_callers: 0,
                files: 0,
                traced: v1.traced_definitions,
            },
            fit: Fit {
                asked: None,
                cap: v1.cap,
                needs: v1.needs,
                least: v1.budget_needed,
                exempt: 0,
            },
        };
        super::one_row_per_fact(&mut doc.definitions);
        doc.summary = Tally::of(&doc).summary(v1.traced_definitions);
        doc
    }
}

fn refs(labels: &[String]) -> Vec<DefRef> {
    labels.iter().map(|l| DefRef::parse(l)).collect()
}

fn unreached(labels: &[String]) -> Vec<Unreached> {
    labels.iter().map(|l| Unreached::parse(l)).collect()
}

fn definition(c: &ChangedSymbol, tests_known: bool) -> Definition {
    let removed = c.kind == "removed";
    let local_of = match &c.unresolved {
        Some(Unknown::LocalOf { enclosing, .. }) => Some(LocalOf {
            enclosing: enclosing.clone(),
        }),
        _ => None,
    };
    // The name the gaps are about: the definition's own, without its owner.
    let name = c.symbol.as_deref().map_or(String::new(), |s| {
        s.rsplit(['.', ':']).next().unwrap_or(s).to_string()
    });
    let mut gaps: Vec<Gap> = c
        .coverage
        .all()
        .gaps()
        .iter()
        .map(|g| Gap::of(g, &name))
        .collect();
    // What the coverage could only call "a route nothing traces", the answer's own
    // reason names.
    let specific = match &c.unresolved {
        Some(Unknown::Ambiguous {
            name,
            definitions,
            language,
        }) => Some(Gap::Ambiguous {
            name: name.clone(),
            definitions: *definitions,
            language: (*language).to_string(),
        }),
        Some(Unknown::NotModelled { language }) => Some(Gap::NotModelled {
            language: (*language).to_string(),
        }),
        Some(Unknown::Overridable { via }) => Some(Gap::Overridable { via: via.clone() }),
        Some(Unknown::Untracked { name }) => Some(Gap::Untracked {
            files: 1,
            name: Some(name.clone()),
        }),
        // The answer's own reasons that no cell gap carries: a name on too many
        // lines to trace, reads through a receiver nothing types, a member of a
        // trait, a run stopped before the search.
        Some(Unknown::CommonWord { name, lines }) => Some(Gap::Unlanded {
            name: name.clone(),
            uses: *lines,
        }),
        Some(Unknown::UnresolvedReads { name, reads }) => Some(Gap::Unlanded {
            name: name.clone(),
            uses: *reads,
        }),
        Some(Unknown::Dispatched { trait_name }) => Some(Gap::Dispatched {
            via: trait_name.clone(),
        }),
        Some(Unknown::Unfinished { stops }) => Some(Gap::Stopped {
            reasons: stops.clone(),
        }),
        _ => None,
    };
    if let Some(gap) = specific {
        // One gap of a kind: the answer's own reason names what a cell gap of the
        // same kind could only count.
        gaps.retain(|g| {
            *g != Gap::NotTraced && std::mem::discriminant(g) != std::mem::discriminant(&gap)
        });
        gaps.push(gap);
    }
    let mut folded: Vec<Fold> = c
        .folded
        .iter()
        .map(|f| Fold {
            rule: f.rule.to_string(),
            kind: f.kind.to_string(),
            count: Count {
                n: f.count,
                exact: f.exact,
            },
            examples: refs(&f.examples),
            parameters: f.parameters.clone(),
            sharing: f.sharing,
        })
        .collect();
    // A reference to an interface or a type is never folded (condition (д)): what a
    // member added to one earns is no count of calls the change cannot have affected.
    let type_level = matches!(c.kind.as_str(), "interface" | "type" | "typealias");
    if c.compatible_callers > 0 && !type_level {
        // The text rule for the fields a class gained reports through the same fold.
        folded.push(Fold {
            rule: "check-the-default".to_string(),
            kind: "calls".to_string(),
            count: Count {
                n: c.compatible_callers,
                exact: c.coverage.calls().witness().is_some(),
            },
            examples: Vec::new(),
            parameters: Vec::new(),
            sharing: 0,
        });
    }
    Definition {
        path: c.path.clone(),
        symbol: c.symbol.clone(),
        lines: c.lines.clone(),
        kind: (!matches!(c.kind.as_str(), "removed" | "local" | "omitted")).then(|| c.kind.clone()),
        // The cell's own reading where there is one; a removal read off the text has
        // only its kind.
        change: c.change.clone().unwrap_or(Change {
            kind: if removed {
                ChangeKind::Removed
            } else {
                ChangeKind::Edited
            },
            delta: None,
            origin: None,
        }),
        local_of,
        commits: c.commits,
        entry: c.entry.clone(),
        guard: c.untested.map(Guard::bare),
        rows: c.callers.iter().map(|r| Row::of(r, tests_known)).collect(),
        folded,
        also_changed: refs(&c.also_changed),
        possibly_also_changed: refs(&c.possibly_also_changed),
        coverage: Searched {
            calls: Search::of(c.coverage.calls(), &name),
            reads: Search::of(c.coverage.reads(), &name),
        },
        missed_in: c.missed_in.clone(),
        gaps,
        shed: c.shed,
    }
}
