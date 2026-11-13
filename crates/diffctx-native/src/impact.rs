//! A failed read is a named limit or an `Err` in this module, never an empty
//! value: the lint that says so is denied here and in its submodules. A lookup
//! that finds nothing is spelled as the absence it is.

#![deny(clippy::disallowed_methods)]

use std::path::Path;
use std::time::Instant;

use rustc_hash::{FxHashMap, FxHashSet};
use schemars::JsonSchema;
use serde::Serialize;

use crate::answer::v2::GuardState;
use crate::answer::{Answer, RenderCtx};
use crate::container::{innermost, named_container_of, within};
use crate::decide::{self, Cells, Effect};
use crate::facts::{EntryKind, framework};
use crate::pipeline::ScoredState;
use crate::tokens::Names;
use crate::tokens::occurrence::{Hedge, Reason};
use crate::types::{Fragment, FragmentId, FragmentKind};
use crate::vocabulary;

mod keys;
mod mentions;
mod rust;
mod tests_reach;
mod text;
pub use mentions::Class as MentionClass;
use mentions::{
    class_of, commented, has_data_extension, is_config_file, is_data, is_import_line,
    named_by_path, reads_imports, selects_paths, stale_references,
};
use tests_reach::TestsReach;
use text::render;
pub use text::render_markdown;

/// The whole serialized answer, not its fragment tokens: a hook injects this
/// into a context the reader did not ask to spend, and locate taught that a
/// budget on the parts leaves the envelope unbounded (#300).
pub const IMPACT_TOKEN_CAP: u32 = 2_000;

/// The line log is one git call per changed symbol; past this many the range
/// is a rewrite, and commit overlap on it says nothing a reader would act on.
const MAX_BLAMED_SYMBOLS: usize = 50;

/// Call sites named per file in the text form before the rest fold into a
/// count.
const MAX_CALLERS_SHOWN_PER_FILE: usize = 3;

/// Contracts are a list of facts the reader can grep for; past this many the
/// list is the diff's own `pub` lines read back.
const MAX_CONTRACTS: usize = 12;
/// A declaration longer than this many lines before its body opens is
/// compared on its first lines only.
const MAX_SIGNATURE_LINES: usize = 12;
/// Paths listed before the rest fold into a count: the reader has the diff.
const MAX_CHANGED_FILES_LISTED: usize = 24;
/// Lines a removed name is read from per file: one caller per function is
/// the answer, and a flood of one common word is not.
const MAX_REMOVED_HITS_PER_FILE: usize = 20;
/// Past this many lines a removed name is a common word: unresolved, not
/// traced.
pub(crate) const MAX_REMOVED_NAME_HITS: usize = 500;
/// Unanalysed files kept when the answer must shrink; the rest are counted.
const MAX_UNANALYSED_LISTED: usize = 6;
/// Unanalysed paths the text names per language; the reader holds the diff.
pub(crate) const UNANALYSED_EXAMPLES: usize = 2;

/// A symbol name with more definitions than this cannot be traced by name.
const MAX_DEFINITIONS: usize = 2;

const SCHEMA_DIRECTORIES: &[&str] = &[
    "migrations",
    "migration",
    "schema",
    "schemas",
    "openapi",
    "swagger",
];
/// Names a data or API description file takes: `schema.json`,
/// `openapi.yaml`, `user.schema.json`. A script named for a schema is code.
const SCHEMA_FILE_STEMS: &[&str] = &["schema", "openapi", "swagger"];
const SCHEMA_DATA_EXTENSIONS: &[&str] = &["json", "yaml", "yml", "graphql", "graphqls", "sql"];
const SCHEMA_FILE_EXTENSIONS: &[&str] = &[
    ".sql",
    ".proto",
    ".graphql",
    ".graphqls",
    ".avsc",
    ".prisma",
];

/// What a change reaches that its own diff does not show (#310): the
/// callers outside the diff of each changed symbol, whether a test guards
/// each caller, which symbols more than one commit of the range touched,
/// and the public contracts the range edits.
#[derive(Clone)]
pub struct ImpactOutput {
    pub name: String,
    pub range: Option<String>,
    /// The index captured as a tree, when the range is the staged changes:
    /// the snapshot this answer is about. A later `git add` makes it stale.
    pub index_tree: Option<String>,
    /// What the snapshot holds, when the caller knows it better than a tree
    /// id says — the hook's session edits, the index a commit records: the
    /// header names it (#549).
    pub range_name: Option<String>,
    /// Set on a `--symbol` query (#336): `changed` then lists the symbol's
    /// definitions, and `changed_files` the files that hold them.
    pub symbol: Option<String>,
    /// A `--symbol` name no file defines but files spell as a word: where,
    /// as text. `changed` is empty then (Forgejo #114).
    pub symbol_named: Option<SymbolNamed>,
    /// What git tracks and the change modifies; untracked work is listed
    /// apart, in `untracked_files`.
    pub changed_files: Vec<String>,
    /// New files git does not track yet, analysed with the change (#418).
    pub untracked_files: Vec<String>,
    /// Deleted by the change: no fragment, but what still names them is
    /// what breaks (#347).
    pub deleted_files: Vec<String>,
    /// Lines outside the change that still name a deleted or renamed path:
    /// a Dockerfile `COPY`, a workflow step, an import (#313).
    pub stale_references: Vec<StaleReference>,
    pub stale_references_omitted: usize,
    /// Modules the change deletes that no file at the head imports: one entry
    /// each, in place of a line per definition they held.
    pub deleted_modules: Vec<DeletedModule>,
    pub deleted_modules_omitted: usize,
    /// Lines outside the change that name a changed file by its path from
    /// another runtime or a config: a command line, a Dockerfile, a
    /// settings entry that runs it (#393, #523). Imports are callers.
    pub named_by: Vec<PathReference>,
    pub named_by_omitted: usize,
    /// The changed files outside code whose changed keys were searched as
    /// text; the lines naming them are in `named_by` with their `key`.
    pub key_searches: Vec<KeySearch>,
    /// Changed configs whose keys and values the capped search left out;
    /// they stay in `unanalysed_files`.
    pub key_files_unsearched: usize,
    /// Lines outside the change that read a changed entry's Astro content
    /// collection by name: `getCollection('blog')` reads every entry under
    /// `src/content/blog/`, and no import or path leads there (#432).
    pub collection_reads: Vec<CollectionRead>,
    pub collection_reads_omitted: usize,
    pub commit_count: usize,
    pub changed: Vec<ChangedSymbol>,
    pub contracts: Vec<Contract>,
    /// Nothing outside the diff depends on it, no contract moved, no symbol
    /// was edited twice: the diff already says everything. A hook stays
    /// silent on this, which is why it is a field and not an absence.
    pub empty: bool,
    /// Callers dropped to fit `IMPACT_TOKEN_CAP`, the last dealt first.
    pub truncated: usize,
    /// Of `truncated`, the possible callers: the header counts every caller
    /// found, shown or shed, and keeps the possible ones apart (#413).
    pub truncated_possible: usize,
    /// The tokens the answer took before shedding: the budget that shows a
    /// shed symbol, which its stub names.
    pub needs: u32,
    /// Of `truncated`, the callers no test reaches: a header that counts
    /// every caller found counts their test links the same way.
    pub truncated_untested: usize,
    /// Of `truncated`, the word matches: they are listed as text, so the
    /// header keeps them out of the caller count.
    pub truncated_textual: usize,
    /// Changed files left out of `changed_files` to fit the cap; the count
    /// is still whole. The reader holds the file list already.
    pub changed_files_omitted: usize,
    /// Untracked files left out of `untracked_files` to fit the same cap.
    pub untracked_files_omitted: usize,
    /// Contracts beyond `MAX_CONTRACTS`, schema changes never among them.
    pub contracts_omitted: usize,
    /// A limit that can have lost a caller, by the rule pack's coverage uses:
    /// a run-wide stop (the deadline, a truncated discovery, a failed git
    /// read) or a per-file limit on a changed file. `empty` is never claimed
    /// while this is set. The graph's per-node cap, which every real
    /// repository trips (#306), is not listed: a caller it cut an edge from
    /// is recovered by the same tests an edge faces.
    pub limits: Vec<crate::resource::LimitReason>,
    /// Whether any test file was in view. Without one, an absent `tests`
    /// says nothing about the caller, and the text form does not call it
    /// untested.
    pub tests_known: bool,
    /// Changed definitions no test reaches by a static link, listed or not:
    /// a change nobody calls can still be a change nothing tests.
    pub untested_definitions: usize,
    /// The changed definitions of that count with no caller found in the
    /// analysed scope, so they are not among `changed`: `path::symbol (lines)`
    /// each. Their searches were complete; `limits` and `withheld_files` say
    /// whether the run's scope was whole, which makes it "nothing calls them".
    pub untested_unlisted: Vec<String>,
    /// The changed definitions of that count whose only callers are inside
    /// the diff: live code, not code nothing calls (#441).
    pub untested_called_in_diff: Vec<String>,
    /// The changed definitions of that kind a runner or a framework enters
    /// without a caller (a script, a program's `main`, a registered or
    /// dispatched handler, a test), named by role: `path::symbol (lines) — role`.
    pub entry_unlisted: Vec<String>,
    /// Changed definitions whose hunks are comments, formatting or a
    /// verbatim move only, which no caller sees, so their callers were not
    /// traced (#370): `path::symbol (lines)` each. Unsaid, a comment edit
    /// read "no resolved static callers" while the function had three (#498).
    pub layout_only: Vec<String>,
    /// Lock files the range changes: dependency versions moved, and what that
    /// reaches is not traced. Dropped from every count, a dependency bump
    /// answered "No changes to analyse" (#495).
    pub lockfile_changes: Vec<String>,
    /// `layout_only` entries left out to fit the cap; the count is whole.
    pub layout_only_omitted: usize,
    /// Changed definitions whose callers were traced; zero with `layout_only`
    /// set means the answer has nothing but layout to report.
    pub traced_definitions: usize,
    /// Files the run withheld (`.diffctx/ignore`, gitignore, secret names):
    /// changed files of the range, or files naming the queried symbol. Their
    /// calls and tests were not read, so a zero says nothing about them
    /// (#392).
    pub withheld_files: usize,
    /// Untracked directories the read of the checkout did not enter (a working
    /// tree over its entry budget): callers in them are unknown, so the answer
    /// is partial and names them.
    pub unread_directories: Vec<String>,
    /// Changed code files whose definitions were not read — no grammar for
    /// the language in this build, or a parse that failed — so their callers
    /// and contracts are not counted, and `empty` is never claimed (#454).
    pub unanalysed_files: Vec<UnanalysedFile>,
    pub unanalysed_files_omitted: usize,
    /// Of `unanalysed_files_omitted`, the `content_untraced` ones.
    pub untraced_omitted: usize,
    /// Changed definitions shed whole from `changed` while their callers were
    /// still not determined (the run stopped first); the header counts them.
    pub unfinished_omitted: usize,
    /// The run that built it: its limits end the fit too.
    pub run: Option<crate::resource::RunContext>,
    /// The token cap the answer was fitted to.
    pub cap: u32,
    /// Set when the least the answer can say still outgrows the budget asked:
    /// the tokens that least takes, which a budget of that size delivers. The
    /// answer cuts nothing more than that least.
    pub budget_needed: Option<u32>,
}

impl ImpactOutput {
    /// Every changed definition the answer stands for: the listed ones, and
    /// the shed ones its one omitted entry counts.
    pub(crate) fn definitions(&self) -> usize {
        self.changed
            .iter()
            .map(|c| match &c.unresolved {
                Some(Unknown::Omitted { count, .. }) => *count,
                _ => 1,
            })
            .sum()
    }
}

/// A module the range deletes and no file at the head imports: its definitions
/// are named once, here, since nothing can reach any of them.
#[derive(Debug, Serialize, JsonSchema, Clone)]
pub struct DeletedModule {
    pub path: String,
    /// The definitions it held.
    pub definitions: usize,
    /// An import whose name is computed at run time could reach it, and the facts
    /// cannot rule one out: "no importers" is what was found, not a proof.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dynamic_import: bool,
}

/// The files outside documents and data that spell a name nothing defines:
/// a textual match, not a resolved binding.
#[derive(Debug, Clone)]
pub struct SymbolNamed {
    /// Every file that spells the name, listed or not.
    pub files: usize,
    /// Code and config first, `MAX_TEXTUAL_SHOWN` of them.
    pub rows: Vec<NamedFile>,
    /// The files naming it that this build read as text and so could not tell a
    /// definition in (#454); empty when every file naming the symbol was analysed.
    pub unread: Vec<UnanalysedFile>,
    /// What limited the run that found no definition: a read that failed or
    /// a deadline means the definition may exist, and the answer's `limits`
    /// say so.
    pub limits: Vec<crate::resource::LimitReason>,
}

#[derive(Debug, Serialize, JsonSchema, Clone)]
pub struct NamedFile {
    pub path: String,
    /// The first line that spells the name.
    pub line: u32,
}

#[derive(Clone)]
pub struct ChangedSymbol {
    pub path: String,
    pub symbol: Option<String>,
    pub lines: String,
    pub kind: String,
    /// Commits of the range that touched these lines (blame at the head);
    /// zero when the range is not `base..head`.
    pub commits: usize,
    pub callers: Vec<Caller>,
    /// The role that gives this definition an implicit caller: a test its
    /// runner runs, a script's own guard, a program's `main`, a handler a
    /// framework registers or dispatches (R3). Such a definition is no test
    /// gap and its callers are not hedged.
    pub entry: Option<String>,
    /// Why callers could not be resolved at all: a name defined in many
    /// files in a language without a binding model, a trait method reached
    /// through dynamic dispatch. Its empty `callers` is then no finding.
    pub unresolved: Option<Unknown>,
    /// What the answer says of the tests of the definition itself: none reaches it
    /// by a static link (`None` state), or the search was not closed.
    pub untested: Option<GuardState>,
    /// Call sites left working by the change: it only added fields or
    /// parameters with defaults, so they are counted, not listed (#388).
    pub compatible_callers: usize,
    /// Files in view that spell the name but were not fully analysed: a
    /// caller there is not seen, so an empty `callers` says nothing about
    /// them (#454).
    pub missed_in: Vec<UnanalysedFile>,
    /// Callers that are themselves changed in the range: `path::symbol
    /// (lines)` each. They are the diff's own lines, never callers outside
    /// it, and the definition they call is live, not one nothing calls.
    pub also_changed: Vec<String>,
    /// Users the range changes too whose binding is not resolved (a receiver
    /// nothing types, a name shared by several definitions): possibly using the
    /// definition, never named as callers. `path::symbol (lines)` each.
    pub possibly_also_changed: Vec<String>,
    /// What the cap shed from this definition, by the kind of each row.
    pub shed: crate::answer::v2::ShedRows,
    /// What the range did to it, as the cell read it.
    pub change: Option<crate::answer::v2::Change>,
    /// Callers the answer cap dropped, last dealt first: of a listed symbol, the
    /// ones missing from `callers`, which is then a lower bound; of an omitted
    /// stub, its whole caller set. The same definition asked for alone may name
    /// more.
    pub callers_omitted: usize,
    /// Only on the one entry that stands for the definitions the cap shed:
    /// each of them, `path::name`, with the callers it had.
    pub omitted: Vec<OmittedDefinition>,
    /// What the search for its calls and for its reads covered, each on its own.
    /// An absence is printed only from a complete one; `unresolved` says in words
    /// what the gaps were.
    pub(crate) coverage: decide::Coverages,
    /// Rows the change cannot have affected, counted one line per (relation
    /// kind, rule) instead of listed (the folding table of the plan); every row
    /// is listed, marked `folded_by`, when the answer is asked to unfold.
    pub folded: Vec<Folded>,
    /// The parameters a compatible signature change added: the folded calls
    /// are named by them.
    pub(crate) new_parameters: Vec<String>,
}

impl ChangedSymbol {
    /// A definition with nothing found yet; a caller states what it did find.
    fn bare(
        path: String,
        symbol: Option<String>,
        lines: String,
        kind: impl Into<String>,
        coverage: decide::Coverages,
    ) -> Self {
        Self {
            path,
            symbol,
            lines,
            kind: kind.into(),
            commits: 0,
            callers: Vec::new(),
            entry: None,
            unresolved: None,
            untested: None,
            compatible_callers: 0,
            missed_in: Vec::new(),
            also_changed: Vec::new(),
            possibly_also_changed: Vec::new(),
            shed: crate::answer::v2::ShedRows::default(),
            change: None,
            callers_omitted: 0,
            omitted: Vec::new(),
            folded: Vec::new(),
            new_parameters: Vec::new(),
            coverage,
        }
    }
}

/// Rows of one changed definition folded under one rule of the folding table.
#[derive(Clone)]
pub struct Folded {
    pub rule: &'static str,
    /// The relation kind the rows are: `calls`, `type_use`, `overrides`, ...
    pub kind: &'static str,
    /// How many rows. Exact only when `exact`; otherwise at least.
    pub count: usize,
    pub exact: bool,
    /// The first three, tests first, then by path and line, so the same change names the same ones.
    pub examples: Vec<String>,
    /// For `check-the-default`: the parameters the change added.
    pub parameters: Vec<String>,
    /// For `untyped-receiver`: the most definitions that share the name a row
    /// may be.
    pub sharing: usize,
}

/// A changed definition the answer cap shed, and the callers it was shed with.
#[derive(Clone)]
pub struct OmittedDefinition {
    pub definition: String,
    /// Where it is, so two methods of one name are two definitions.
    pub lines: String,
    pub callers_omitted: usize,
    /// The rows it was shed with, by what they were.
    pub shed: crate::answer::v2::ShedRows,
    /// Its search for calls was not complete when it was shed: a count that
    /// includes it is the least there are.
    pub incomplete: bool,
}

/// What the analysis cannot say about a definition's callers, and why
/// (#505): an unknown is named, never read as no caller. The v2 document carries
/// it as a closed `Gap`, and the text says the gap.
#[derive(Clone, Debug, PartialEq)]
pub enum Unknown {
    /// A trait method, reached through `dyn Trait` or a generic bound.
    Dispatched { trait_name: String },
    /// A name defined in many files of a language without a binding model.
    Ambiguous {
        name: String,
        definitions: usize,
        language: &'static str,
    },
    /// A removed name on too many lines of code to trace by name.
    CommonWord { name: String, lines: usize },
    /// Reads of a member through a receiver nothing types.
    UnresolvedReads { name: String, reads: usize },
    /// A member with no direct caller that a supertype, an interface or the
    /// runtime can call: `via` names that route, which nothing traces.
    Overridable { via: String },
    /// Code in a language no binding model reads: its callers are only the
    /// files the ranking graph links, so finding none proves nothing (#505).
    NotModelled { language: &'static str },
    /// The run stopped (`stops`: the deadline, a cancellation, a failed read)
    /// before this definition's search finished: what it found is a lower
    /// bound, and finding none does not mean there are none.
    Unfinished {
        stops: Vec<crate::resource::LimitReason>,
    },
    /// The range pairs the definition's base and head sides in more than one
    /// way, so what changed in it, and who it reaches, is not determined.
    Unpaired { name: String },
    /// The answer cap shed these definitions: they are changed and their
    /// callers were found, but do not fit. `needs` is the budget, in the
    /// answer's own tokens, that shows them; `count` is how many.
    /// `incomplete` of them had a search for calls that was not complete; the names
    /// kept are capped, this is not.
    Omitted {
        needs: u32,
        count: usize,
        incomplete: usize,
    },
    /// The name is a binding no tracked definition stands for (a `const` in a
    /// module-level block): its uses are not resolved, and finding none says
    /// nothing.
    Untracked { name: String },
    /// The name is a local of one definition (a nested function, class or
    /// closure that never leaves it): not a definition of its own, its uses
    /// are the ones inside the enclosing one.
    LocalOf { name: String, enclosing: String },
    /// A gap in the search, in words (`decide::Gap`): what was not read, so that
    /// finding no callers does not mean there are none.
    Uncovered { why: String },
}

/// A test that reaches a caller: its file, the test function where one is named,
/// and the function between the caller and the test when the test calls through one.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TestLink {
    pub path: String,
    pub name: Option<String>,
    pub via: Option<String>,
}

/// The tests an answer names for a caller whose tests are `tests` (path then name
/// order): the first of each file, then the rest, [`crate::answer::v2::TESTS_NAMED`]
/// at most, in path then name order. One file of many tests does not crowd out the
/// others.
pub fn named_tests(tests: &[TestLink]) -> Vec<&TestLink> {
    let cap = crate::answer::v2::TESTS_NAMED;
    let mut files = FxHashSet::default();
    let mut picked: Vec<bool> = tests.iter().map(|t| files.insert(&t.path)).collect();
    let mut room = cap.saturating_sub(picked.iter().filter(|p| **p).count());
    let mut firsts = 0;
    for p in &mut picked {
        if *p {
            firsts += 1;
            if firsts > cap {
                *p = false;
            }
        } else if room > 0 {
            *p = true;
            room -= 1;
        }
    }
    tests
        .iter()
        .zip(picked)
        .filter_map(|(t, p)| p.then_some(t))
        .collect()
}

#[derive(Clone)]
pub struct Caller {
    pub path: String,
    pub symbol: Option<String>,
    pub lines: String,
    pub weight: f64,
    /// The tests that reach this caller at the fewest hops, in path then name order.
    pub tests: Vec<TestLink>,
    /// A test reaches the caller's file but none of the paths to this caller:
    /// the one case where an absent `tests` is a finding, not a gap in
    /// what static analysis can see (a route hit over HTTP, a fixture).
    pub file_tested: bool,
    /// The decorator that registers the caller (`@register_node`,
    /// `@app.route`): what reaches it is whatever reads that registry, which
    /// no static test link follows, so its absence is no finding (#388).
    pub registered_by: Option<String>,
    /// A test file the size cap kept out that names this caller: an absent
    /// `tests` says nothing about it (#542).
    pub unread_test: Option<String>,
    /// The search for a test above this caller is open: an absent `tests` is not
    /// established.
    pub tests_open: bool,
    /// `resolved`: a binding links the reference to the changed definition.
    /// `name`: a word match — the lexical model of a language without a
    /// binding model, or a removed definition's name found by search; the
    /// text form lists it as text and never counts it as a caller.
    /// `candidate`: the binding or the receiver is not known; `reason` says
    /// which.
    pub evidence: &'static str,
    pub reason: Option<&'static str>,
    /// `reference` when the definition is named without being called: a
    /// callback, a value passed on.
    pub relation: &'static str,
    /// What the reference does (`calls`, `reads`, `writes`, `imports`,
    /// `reexports`, `overrides`, `instantiates`, `passes_as_value`), stated
    /// only where the (language, relation) cell is `precise`; elsewhere a row
    /// says `relation` alone.
    pub kind: Option<&'static str>,
    /// The rule of the folding table that would fold this row: only an answer
    /// asked to list every row carries it (`--unfold`); otherwise the row is
    /// counted in `folded`.
    pub folded_by: Option<&'static str>,
    /// The relation kind the row is folded under.
    pub fold_kind: Option<&'static str>,
    /// For a folded candidate: how many definitions share the name.
    pub fold_share: usize,
    /// The lines that reference the definition, when known.
    pub sites: Vec<u32>,
}

impl Caller {
    /// A row at one line that no folding rule, kind or test link qualifies.
    fn at_line(
        path: String,
        symbol: Option<String>,
        lines: String,
        evidence: &'static str,
        relation: &'static str,
        site: u32,
    ) -> Self {
        Self {
            path,
            symbol,
            lines,
            weight: 1.0,
            tests: Vec::new(),
            file_tested: false,
            registered_by: None,
            unread_test: None,
            tests_open: false,
            evidence,
            reason: None,
            relation,
            kind: None,
            folded_by: None,
            fold_kind: None,
            fold_share: 0,
            sites: vec![site],
        }
    }
}

/// The limits among `limits` that leave a count of callers a lower bound.
pub(crate) fn stops_of(
    limits: &[crate::resource::LimitReason],
) -> Vec<crate::resource::LimitReason> {
    limits
        .iter()
        .copied()
        .filter(|r| r.stops_a_search())
        .collect()
}

pub(crate) fn reason_names(reasons: &[crate::resource::LimitReason]) -> String {
    reasons
        .iter()
        .map(|r| match serde_json::to_value(r) {
            Ok(serde_json::Value::String(name)) => name,
            _ => format!("{r:?}"),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// A definition (or, with no symbol, a changed code file) whose callers the
/// run stopped before determining.
pub(crate) fn is_unfinished(s: &ChangedSymbol) -> bool {
    matches!(s.unresolved, Some(Unknown::Unfinished { .. }))
}

/// The one entry that stands for the definitions the answer cap shed.
pub(crate) fn is_omitted(s: &ChangedSymbol) -> bool {
    matches!(s.unresolved, Some(Unknown::Omitted { .. }))
}

/// What the search for one definition's callers did not cover: the cell's own
/// gaps (which carry its occurrence account), or, for a definition no cell
/// answers for, the reasons the answer names for it (`unresolved`) and the
/// `outside` files that name it beyond the group (a script that embeds its code,
/// a file outside code). The one place that decides whether an absence may be
/// claimed for it.
fn search_coverage(
    verdict: Option<&decide::Verdict>,
    unresolved: Option<&Unknown>,
    outside: usize,
) -> decide::Coverages {
    // What the cell's own gaps said is already in its coverage: only a reason it
    // did not give is a route nothing traces.
    let explained = verdict.is_some_and(|v| v.gap().is_some());
    let own_gap = match unresolved {
        Some(Unknown::Unfinished { stops }) if !explained => Some(decide::Gap::Stopped {
            reasons: stops.clone(),
        }),
        Some(Unknown::Unfinished { .. }) | None => None,
        Some(_) => (!explained).then_some(decide::Gap::NotTraced),
    };
    let named_outside = (outside > 0).then_some(decide::Gap::Unaccounted {
        reason: decide::Reason::OtherLanguage,
        files: outside,
    });
    match verdict {
        Some(verdict) => decide::Coverages::of_verdict(verdict)
            .with(own_gap)
            .with(named_outside),
        None => decide::Coverages::unowned(own_gap.into_iter().chain(named_outside).collect()),
    }
}

/// Untracked directories the answer names in its coverage; the rest are counted.
const MAX_UNREAD_DIRECTORIES_NAMED: usize = 10;

/// What the run as a whole did not cover: a stop or limit that can have lost a
/// caller, files withheld by ignore rules. A definition's own gaps are its own.
pub(crate) fn scope_coverage(output: &ImpactOutput) -> decide::Coverage {
    let untracked = output
        .unanalysed_files
        .iter()
        .filter(|f| f.reason == OUTSIDE_DEFINITIONS)
        .count();
    let mut gaps = Vec::new();
    if !output.limits.is_empty() {
        gaps.push(decide::Gap::Stopped {
            reasons: output.limits.clone(),
        });
    }
    if output.withheld_files > 0 {
        gaps.push(decide::Gap::Withheld {
            files: output.withheld_files,
        });
    }
    if untracked > 0 {
        gaps.push(decide::Gap::Untracked { files: untracked });
    }
    if !output.unread_directories.is_empty() {
        gaps.push(decide::Gap::UnreadDirectories {
            directories: output
                .unread_directories
                .iter()
                .take(MAX_UNREAD_DIRECTORIES_NAMED)
                .cloned()
                .collect(),
            total: output.unread_directories.len(),
        });
    }
    decide::Coverage::close(decide::Evidence::Run, gaps)
}

/// A definition the answer lists as one nothing calls: only a complete search
/// makes one.
fn absence(named: String, _complete: decide::Complete) -> String {
    named
}

fn unfinished_symbol(
    path: String,
    symbol: Option<String>,
    lines: String,
    kind: String,
    stops: &[crate::resource::LimitReason],
) -> ChangedSymbol {
    ChangedSymbol {
        unresolved: Some(Unknown::Unfinished {
            stops: stops.to_vec(),
        }),
        ..ChangedSymbol::bare(
            path,
            symbol,
            lines,
            kind,
            decide::Coverages::partial(decide::Gap::Stopped {
                reasons: stops.to_vec(),
            }),
        )
    }
}

#[derive(Clone)]
pub struct StaleReference {
    pub path: String,
    pub line: u32,
    /// The deleted or renamed path the line still names.
    pub target: String,
    /// `deleted`, or `renamed to <new path>`.
    pub kind: String,
    // The kind of place the line sits in: a `Dockerfile` or a script names the
    // path as a command, a document only talks about it.
    pub class: MentionClass,
}

#[derive(Debug, Serialize, JsonSchema, Clone)]
pub struct CollectionRead {
    pub path: String,
    pub line: u32,
    /// The collection the call names; absent when the name is computed at
    /// run time, so the call may read any collection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema, Clone)]
pub struct PathReference {
    pub path: String,
    pub line: u32,
    /// The changed file the line names.
    pub target: String,
    /// A key the change touched in `target`, which the line spells: a
    /// textual match, not a resolved binding (#686).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    // The kind of place the line sits in: `code`, `config` or `command`
    // (what reads the path), as `named_by` lists nothing else.
    pub class: MentionClass,
}

/// A changed file outside code whose changed keys were searched as text.
#[derive(Debug, Serialize, JsonSchema, Clone)]
pub struct KeySearch {
    pub path: String,
    pub keys: Vec<String>,
    /// The lines outside the diff naming any of them.
    pub named: usize,
}

#[derive(Debug, Serialize, JsonSchema, Clone)]
pub struct UnanalysedFile {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<&'static str>,
    /// `no_grammar`, `parse_timeout`, `parse_failed` (nothing read),
    /// `parse_partial` (read but for the error lines), `too_large` (over the
    /// size cap), `unreadable`, `content_untraced`: a file outside code
    /// whose keys, values or rules something may read, of which only the
    /// lines naming the file by path were searched, or `outside_definitions`:
    /// code the range changed that no tracked definition holds (a module-level
    /// statement, a `__main__` block), whose effect is not traced.
    pub reason: &'static str,
    /// Where the parse broke, the first few lines; for `outside_definitions`,
    /// the first and last line of each changed run, in pairs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lines: Vec<u32>,
}

#[derive(Debug, Serialize, JsonSchema, Clone)]
pub struct Contract {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    pub kind: &'static str,
}

/// Whether `code` binds `name` as a local — `let name`, `let mut name` — so
/// that its calls of `name` reach the local, not a definition elsewhere.
fn binds_locally(code: &str, name: &str) -> bool {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    code.match_indices(name).any(|(at, _)| {
        let before = code[..at].trim_end();
        let head = before.strip_suffix("mut").map_or(before, str::trim_end);
        let Some(rest) = head.strip_suffix("let") else {
            return false;
        };
        !rest.chars().next_back().is_some_and(word)
            && !code[at + name.len()..].chars().next().is_some_and(word)
    })
}

/// The line of `f` that first spells `name` as a whole word.
fn first_naming_line(f: &Fragment, name: &str) -> Option<u32> {
    f.content
        .lines()
        .position(|l| names_word(l, name))
        .map(|i| f.start_line() + i as u32)
}

/// Whether `line` holds a call of `name`: the whole word, then `(`.
fn calls_word(line: &str, name: &str) -> bool {
    let word = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    line.match_indices(name).any(|(at, _)| {
        !line[..at].chars().next_back().is_some_and(word)
            && line[at + name.len()..].starts_with('(')
    })
}

/// Whether `text` holds `name` as a whole word; `$` is a word character,
/// as in a JavaScript identifier.
fn names_word(text: &str, name: &str) -> bool {
    let word = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    text.match_indices(name).any(|(at, _)| {
        !text[..at].chars().next_back().is_some_and(word)
            && !text[at + name.len()..].chars().next().is_some_and(word)
    })
}

fn is_code_file(path: &str) -> bool {
    let ext = Path::new(path).extension().map_or_else(String::new, |e| {
        format!(".{}", e.to_string_lossy().to_lowercase())
    });
    crate::config::extensions::CODE_EXTENSIONS.contains(ext.as_str())
}

/// Who may stand before a `.` in front of the name and still mean it.
enum Receiver<'a> {
    /// A method: any object can carry it.
    Any,
    /// A module-level definition: only its own module, by stem or by an
    /// alias the caller's file binds to it. `re.search` is not a call to a
    /// changed `download.search` (#345).
    Module {
        stem: &'a str,
        aliases: &'a [String],
    },
}

/// `content` as code: comments and string literals blanked, interpolations
/// kept, read off the file's grammar (#460, #475). A name inside
/// `"must be refused"` or a comment is not a reference (#345). A file
/// without a grammar is read as written.
pub(crate) fn code_text(path: &str, content: &str) -> std::sync::Arc<str> {
    crate::parsers::code_only(path, content).unwrap_or_else(|| content.into())
}

fn receiver_before(code: &str, at: usize) -> Option<&str> {
    let head = code[..at].strip_suffix('.')?;
    let head = head.strip_suffix('?').unwrap_or(head);
    let start = head
        .rfind(|c: char| !(c.is_alphanumeric() || c == '_'))
        .map_or(0, |i| i + 1);
    Some(&head[start..])
}

/// The caller must name the symbol itself, in code: the language builders
/// link a call by name to every definition of that name in the repository
/// (case-folded), so an edge alone says "same name somewhere", a comment
/// mention says nothing, and `Cli` in a doc line is not `Cli::parse()`.
fn mentions(caller: &Fragment, symbol: Option<&str>, receiver: &Receiver) -> bool {
    let Some(sym) = symbol else { return true };
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    code_text(caller.path(), &caller.content)
        .lines()
        .any(|code| {
            let bytes = code.as_bytes();
            code.match_indices(sym).any(|(at, _)| {
                let before = at.checked_sub(1).map(|i| bytes[i]);
                let after = bytes.get(at + sym.len()).copied();
                if before.is_some_and(is_word) || after.is_some_and(is_word) {
                    return false;
                }
                match (receiver, receiver_before(&code, at)) {
                    (Receiver::Module { stem, aliases }, Some(r)) => {
                        r.eq_ignore_ascii_case(stem) || aliases.iter().any(|a| a == r)
                    }
                    _ => true,
                }
            })
        })
}

/// Names the caller's file binds the module to: `import a.stem as R`,
/// `from a import stem as R`, `import * as R from "./stem"`.
fn module_aliases(file_fragments: &[&Fragment], stem: &str) -> Vec<String> {
    let mut aliases = Vec::new();
    for f in file_fragments {
        for line in f.content.lines() {
            let words: Vec<&str> = line
                .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '*'))
                .filter(|w| !w.is_empty())
                .collect();
            if !words.iter().any(|w| w.eq_ignore_ascii_case(stem)) {
                continue;
            }
            for pair in words.windows(2) {
                if pair[0] == "as" && pair[1] != "*" {
                    aliases.push(pair[1].to_string());
                }
            }
        }
    }
    aliases.sort();
    aliases.dedup();
    aliases
}

/// A method lives inside a type, or names its receiver in the declaration
/// (Kotlin `fun String.pad()`); anything else is reached through its module.
/// `file`: the fragments of the core's own file, in fragment order.
pub(crate) fn is_member(core: &Fragment, file: &[&Fragment]) -> bool {
    if enclosing_class(core, file).is_some() {
        return true;
    }
    let head = without_decorators(&core.content).trim_start();
    let receiver = head
        .strip_prefix("fun ")
        .is_some_and(|rest| rest.split('(').next().is_some_and(|n| n.contains('.')));
    receiver
        || file.iter().any(|f| {
            f.path() == core.path()
                && f.id != core.id
                && (f.kind.is_container() || f.kind == FragmentKind::Impl)
                && f.start_line() <= core.start_line()
                && f.end_line() >= core.end_line()
        })
}

/// Languages where a directory is a namespace: a sibling file reaches the
/// changed symbol with no import to name it by. Rust, Python and JavaScript
/// siblings still name the module (`use`, `import`), so for them it must
/// appear among the caller file's identifiers.
const PACKAGE_SCOPED_EXTENSIONS: &[&str] = &[
    "java", "kt", "kts", "scala", "cs", "php", "c", "cc", "cpp", "cxx", "h", "hpp", "m", "mm",
    "swift", "dart",
];

/// What impact knows of each file in view: its fragments, for the import lines,
/// and, through the run's token index, what the whole file spells (an import sits
/// outside the calling fragment).
struct Files<'a> {
    fragments: FxHashMap<&'a str, Vec<&'a Fragment>>,
    cells: &'a Cells,
    state: &'a ScoredState,
}

impl Files<'_> {
    /// Whether the file spells one of `words` anywhere. A file the index did not
    /// read, or one of no group, is not known to spell none: it counts as
    /// spelling them.
    fn spells(&self, path: &str, words: &[&str]) -> bool {
        let shown = rel(self.state, path);
        let Some(group) = decide::Group::of_path(&shown) else {
            return true;
        };
        let Some(tokens) = self.cells.group_tokens(self.state, group) else {
            return true;
        };
        tokens
            .spells(&shown, &Names::new(words.iter().copied()))
            .unwrap_or(true)
    }
}

/// A caller that never names the changed file's module and lives in another
/// directory is a name match, not a call: `Command` in a binary crate is not
/// `std::process::Command`, however many files construct one. Same-directory
/// callers in a package-scoped language (Java, C#, Swift) pass, because a
/// package needs no import to reach its neighbours.
fn references_module(
    caller: &Fragment,
    files: &Files,
    symbol: Option<&str>,
    changed_path: &str,
) -> bool {
    let changed = Path::new(changed_path);
    let caller_path = Path::new(caller.path());
    if caller_path == changed {
        return true;
    }
    // The import sits at the top of the file, outside the calling fragment.
    let ext = caller_path
        .extension()
        .map_or_else(String::new, |e| e.to_string_lossy().to_lowercase());
    if caller_path.parent() == changed.parent() && PACKAGE_SCOPED_EXTENSIONS.contains(&ext.as_str())
    {
        return true;
    }
    let Some(stem) = changed.file_stem().and_then(|s| s.to_str()) else {
        return true;
    };
    let stem = stem.to_lowercase();
    // A shell `lib.sh` is sourced by its own name, not its directory (#497).
    let sourced_by_name = changed.extension().is_some_and(|e| {
        matches!(
            e.to_string_lossy().to_lowercase().as_str(),
            "sh" | "bash" | "zsh" | "ksh"
        )
    });
    if !sourced_by_name
        && matches!(
            stem.as_str(),
            "mod" | "lib" | "main" | "index" | "init" | "__init__"
        )
    {
        // A module named for its role (`mod.rs`, `index.ts`, `__init__.py`)
        // is imported by its directory, and a binary root is not imported.
        let dir = changed
            .parent()
            .and_then(|d| d.file_name())
            .and_then(|d| d.to_str())
            .map(str::to_lowercase)
            .filter(|d| {
                !matches!(
                    d.as_str(),
                    "src" | "lib" | "app" | "bin" | "tests" | "test" | "pkg" | "internal"
                )
            });
        return dir.is_some_and(|d| files.spells(caller.path(), &[d.as_str()]));
    }
    files.spells(caller.path(), &[stem.as_str()])
        || symbol.is_some_and(|s| imported_from_package(changed, files, caller.path(), s))
}

/// `from models import Steps` reaches `models/wearables.py::Steps` through
/// the package that re-exports it: the caller names the package, never the
/// module (#351). Only an import line naming both the package and the symbol
/// counts — `models` alone is a common word. The resolver of #329 replaces
/// this.
fn imported_from_package(changed: &Path, files: &Files, caller_path: &str, symbol: &str) -> bool {
    let Some(dir) = changed
        .parent()
        .and_then(|d| d.file_name())
        .and_then(|d| d.to_str())
    else {
        return false;
    };
    let has_word = |line: &str, word: &str| {
        line.split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .any(|w| w == word)
    };
    files.fragments.get(caller_path).is_some_and(|frags| {
        frags.iter().flat_map(|f| f.content.lines()).any(|line| {
            let head = line.trim_start();
            ["from ", "import ", "use ", "export "]
                .iter()
                .any(|k| head.starts_with(k))
                && has_word(head, dir)
                && has_word(head, symbol)
        })
    })
}

/// A fragment that only imports is where a name comes in, not where it is
/// called: the module docstring and the import block of a file link to every
/// changed symbol the file uses and would be listed as callers with nothing
/// to check.
fn is_import_block(content: &str) -> bool {
    let mut saw_code = false;
    let mut in_docstring = false;
    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if in_docstring {
            if line.contains("\"\"\"") || line.contains("'''") {
                in_docstring = false;
            }
            continue;
        }
        if line.starts_with("\"\"\"") || line.starts_with("'''") {
            let rest = &line[3..];
            if !rest.contains("\"\"\"") && !rest.contains("'''") {
                in_docstring = true;
            }
            continue;
        }
        if line.starts_with("//")
            || line.starts_with('#')
            || line.starts_with("/*")
            || line.starts_with('*')
        {
            continue;
        }
        let is_import = line.starts_with("import ")
            || line.starts_with("from ")
            || line.starts_with("use ")
            || line.starts_with("pub use ")
            || line.starts_with("require ")
            || line.starts_with("require(")
            || line.starts_with("const ") && line.contains("require(")
            || line.starts_with("export ") && line.contains(" from ")
            || line.starts_with("package ")
            || line.starts_with("using ")
            || line.starts_with("extern crate ")
            || line.starts_with("mod ")
            || line == ")"
            || line == "};"
            || line == "}"
            || line.ends_with(",") && !line.contains('(');
        if !is_import {
            saw_code = true;
            break;
        }
    }
    !saw_code
}

fn extension(path: &str) -> String {
    path.rsplit_once('.')
        .map_or("", |(_, e)| e)
        .to_ascii_lowercase()
}

/// A file read for its prose, whose consumers find it by its path. Not
/// `.txt`: `requirements.txt` and `CMakeLists.txt` are read by tools.
fn is_document(path: &str) -> bool {
    matches!(extension(path).as_str(), "md" | "markdown" | "rst" | "adoc")
}

/// The changed files this run did not read whole (#454), and the changed
/// files outside code whose content something reads — config keys, a
/// template, a query, a stylesheet — which nothing here traces (#559),
/// ordered by language then path so the listed ones and the counted rest are
/// stable.
/// The reason of a changed file's lines that no tracked definition holds.
pub(crate) const OUTSIDE_DEFINITIONS: &str = "outside_definitions";

/// Runs of changed lines listed per file before the rest are dropped.
const MAX_OUTSIDE_RUNS: usize = 8;

/// The changed code the cells' definitions do not hold, one entry per file with
/// its runs of lines.
fn outside_definitions(cells: &Cells) -> Vec<UnanalysedFile> {
    cells
        .iter()
        .flat_map(decide::Cell::outside_definitions)
        .map(|(path, spans)| UnanalysedFile {
            language: crate::languages::get_language_for_file(&path),
            path,
            reason: OUTSIDE_DEFINITIONS,
            lines: spans
                .into_iter()
                .take(MAX_OUTSIDE_RUNS)
                .flat_map(|(first, last)| [first, last])
                .collect(),
        })
        .collect()
}

fn unanalysed_files(state: &ScoredState, changed: &[String]) -> Vec<UnanalysedFile> {
    let mut fragmented: FxHashMap<String, bool> = FxHashMap::default();
    for f in state.run.bounded(&state.all_fragments) {
        *fragmented.entry(rel(state, f.path())).or_default() |= f.kind.is_definition_kind();
    }
    let source = state.run.source();
    let too_large = state
        .run
        .limited_by(crate::resource::LimitReason::FileTooLarge);
    let layout: FxHashSet<&str> = state
        .change_classes
        .iter()
        .filter(|(_, class, _)| *class == crate::change_class::ChangeClass::Layout)
        .map(|(p, _, _)| p.as_str())
        .collect();
    let mut out: Vec<UnanalysedFile> = state
        .run
        .bounded(changed)
        .filter_map(|path| {
            // Only the lines naming the file by path are searched for it. A
            // document or a binary has no other consumer, a schema is listed
            // as a contract and a layout edit reaches no reader; anything
            // else read as a clean zero while its readers went unsearched.
            // An extensionless shell script is code by its shebang, and the
            // shell model reads it (#541).
            let script = || {
                Path::new(path).extension().is_none()
                    && source
                        .read_to_string(&state.root_dir.join(path))
                        .is_some_and(|t| crate::languages::sniff_language(path, &t) == Some("bash"))
            };
            if !is_code_file(path) && !script() {
                let binary = || {
                    source
                        .read_bytes(&state.root_dir.join(path))
                        .is_some_and(|b| b.iter().take(8000).any(|&c| c == 0))
                };
                if is_document(path)
                    || is_schema_file(path)
                    || has_data_extension(path)
                    || layout.contains(path.as_str())
                    || binary()
                {
                    return None;
                }
                return Some(UnanalysedFile {
                    path: path.clone(),
                    language: crate::languages::get_language_for_file(path),
                    reason: "content_untraced",
                    lines: Vec::new(),
                });
            }
            // No fragment at all. A deleted or withheld file never reaches
            // `changed`; a code file over the size cap or one that could not
            // be read was never analysed, and saying nothing made its callers
            // a clean zero. An empty file has nothing to analyse.
            // Over the cap on either side: a head read in full still had its
            // removals skipped when the base was too large to parse.
            if too_large.contains(path) {
                return Some(UnanalysedFile {
                    path: path.clone(),
                    language: crate::languages::get_language_for_file(path),
                    reason: "too_large",
                    lines: Vec::new(),
                });
            }
            let Some(&has_definition) = fragmented.get(path) else {
                let reason = if source.read_bytes(&state.root_dir.join(path)).is_none() {
                    "unreadable"
                } else {
                    return None;
                };
                return Some(UnanalysedFile {
                    path: path.clone(),
                    language: crate::languages::get_language_for_file(path),
                    reason,
                    lines: Vec::new(),
                });
            };
            let (reason, lines) = crate::parsers::unanalysed_reason(path, has_definition, || {
                source.read_bytes(&state.root_dir.join(path))
            })?;
            Some(UnanalysedFile {
                path: path.clone(),
                language: crate::languages::get_language_for_file(path),
                reason,
                lines,
            })
        })
        .collect();
    // A deleted file over the size cap had its removals skipped: as unanalysed as a
    // changed one, and never in `changed`.
    out.extend(
        state
            .deleted_files
            .iter()
            .filter(|p| too_large.contains(*p) && !changed.contains(p))
            .map(|p| UnanalysedFile {
                path: p.clone(),
                language: crate::languages::get_language_for_file(p),
                reason: "too_large",
                lines: Vec::new(),
            }),
    );
    out.sort_by(|a, b| {
        a.language
            .cmp(&b.language)
            .then_with(|| a.path.cmp(&b.path))
    });
    out
}

/// Where a name is spelled outside the code impact traces: config, data,
/// templates, markup, and, for a module's name, another runtime's scripts
/// (`python -m controls.foo_check` in a CronWorkflow). A changed definition
/// no traced caller reaches, but such a file names, is no "nothing calls
/// it": a zero needs a witness that the name is read nowhere it went
/// unsearched (#505).
#[derive(Default)]
struct UntracedNamers {
    /// Each needle, by the lines naming it as a whole word.
    rows: FxHashMap<String, Vec<(String, u32)>>,
}

/// A name spelled in more untraced files than this is a common word, not a
/// reference: it witnesses nothing.
const MAX_UNTRACED_NAMERS: usize = 12;

/// Module names that say a file's role, not which file it is.
const ROLE_STEMS: &str = "index main mod lib init __init__ __main__ app util utils helpers common core base types config settings test tests conftest setup";

/// A name only code would spell: `foo_check`, `formatDate`, `LIMIT`, `v2`.
/// A plain word (`naming`, `total`) is prose in a dataset or a paper as
/// often as a reference, and witnesses nothing.
pub(crate) fn distinctive(word: &str) -> bool {
    let has = |f: fn(&char) -> bool| word.chars().any(|c| f(&c));
    let shaped = word.contains('_')
        || has(char::is_ascii_digit)
        || (has(char::is_ascii_uppercase) && has(char::is_ascii_lowercase))
        || !has(char::is_ascii_lowercase);
    shaped
        && word.chars().count() >= 4
        && !word.chars().all(|c| c.is_ascii_digit() || c == '_')
        && !crate::stopwords::CODE_STOPWORDS.contains(&word.to_lowercase())
        && !ROLE_STEMS.split(' ').any(|r| r == word)
}

fn module_stem(path: &str) -> Option<&str> {
    Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| distinctive(s))
}

impl UntracedNamers {
    fn build(state: &ScoredState, cores: &[&Fragment], diff_range: Option<&str>) -> Self {
        let mut needles: Vec<String> = cores
            .iter()
            .filter_map(|c| c.symbol_name.as_deref().map(base_symbol))
            .chain(cores.iter().filter_map(|c| module_stem(c.path())))
            .filter(|w| distinctive(w))
            .map(str::to_string)
            .collect();
        needles.sort();
        needles.dedup();
        if needles.is_empty() {
            return Self::default();
        }
        let head = diff_range
            .map(crate::snapshot::split_diff_range)
            .and_then(|(_, head)| head);
        let wanted: FxHashSet<&str> = needles.iter().map(String::as_str).collect();
        let changed: FxHashSet<String> = state
            .changed_files
            .iter()
            .map(|p| rel(state, &p.to_string_lossy()))
            .collect();
        let grepped = read_or_note(
            state,
            crate::grep::grep_lines(
                &state.root_dir,
                &state.run.source(),
                &needles,
                head.as_deref(),
            ),
        );
        let mut rows: FxHashMap<String, Vec<(String, u32)>> = FxHashMap::default();
        for (path, line, text) in &grepped.rows {
            // A comment or a document mentions a name without reading it.
            if changed.contains(path)
                || is_document(path)
                || matches!(
                    extension(path).as_str(),
                    "diff"
                        | "patch"
                        | "lock"
                        | "tex"
                        | "bib"
                        | "txt"
                        | "csv"
                        | "tsv"
                        | "log"
                        | "svg"
                )
                || commented(text)
            {
                continue;
            }
            let mut seen: FxHashSet<&str> = FxHashSet::default();
            for word in text.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
                if wanted.contains(word) && seen.insert(word) {
                    rows.entry(word.to_string())
                        .or_default()
                        .push((path.clone(), *line));
                }
            }
        }
        Self { rows }
    }

    /// The lines naming `word` in the files `admit` keeps, one per file;
    /// none when the word is too common to witness anything.
    fn naming(&self, word: &str, admit: impl Fn(&str) -> bool) -> Vec<(String, u32)> {
        let mut by_path: Vec<(String, u32)> = Vec::new();
        for (path, line) in self.rows.get(word).into_iter().flatten() {
            if admit(path) && !by_path.iter().any(|(p, _)| p == path) {
                by_path.push((path.clone(), *line));
            }
        }
        if by_path.len() > MAX_UNTRACED_NAMERS {
            return Vec::new();
        }
        by_path.sort();
        by_path
    }

    /// The files outside code that spell `core`'s name.
    fn of(&self, core: &Fragment) -> Vec<UnanalysedFile> {
        let Some(name) = core.symbol_name.as_deref().map(base_symbol) else {
            return Vec::new();
        };
        self.naming(name, |p| !is_code_file(p))
            .into_iter()
            .map(|(path, line)| UnanalysedFile {
                language: crate::languages::get_language_for_file(&path),
                path,
                reason: "not_traced",
                lines: vec![line],
            })
            .collect()
    }

    /// The lines naming a changed module by its name outside the code that
    /// imports it: a config value, a `python -m pkg.mod` in another
    /// runtime's script (#613). Named by path, a path's own spelling, is
    /// already in `known`.
    fn module_refs(
        &self,
        state: &ScoredState,
        cores: &[&Fragment],
        known: &[PathReference],
    ) -> Vec<PathReference> {
        let mut out: Vec<PathReference> = Vec::new();
        let mut files: Vec<&str> = cores.iter().map(|c| c.path()).collect();
        files.sort_unstable();
        files.dedup();
        for own in files {
            let Some(stem) = module_stem(own) else {
                continue;
            };
            let target = rel(state, own);
            for (path, line) in self
                .naming(stem, |p| !is_code_file(p) || !same_runtime(p, own))
                .into_iter()
                .take(MAX_NAMED_BY_PER_TARGET)
            {
                if !known.iter().any(|k| k.path == path && k.target == target) {
                    let class = class_of(&state.root_dir, &state.run.source(), &path, "", None);
                    out.push(PathReference {
                        path,
                        line,
                        target: target.clone(),
                        key: None,
                        class,
                    });
                }
            }
        }
        out
    }
}

/// The most files of a definition's group the account could not place that are
/// listed in `missed_in`: the gap carries the count, the rows are a sample.
const MAX_UNACCOUNTED_ROWS: usize = 20;

/// The files the account could not read, as the answer's rows: by path, the first few.
fn unaccounted_rows(verdict: &decide::Verdict) -> Vec<UnanalysedFile> {
    let mut rows: Vec<UnanalysedFile> = (verdict.hedges().iter())
        .filter_map(|(_, hedge)| {
            let (file, reason, lines) = match hedge {
                Hedge::Candidate {
                    file,
                    line,
                    reason: Reason::Embedded,
                } => (file, "embedded_code", vec![*line]),
                Hedge::Unread { file, loss } => match decide::effect_of(loss) {
                    Effect::File(reason) | Effect::Subtree(reason, _) => {
                        (file, reason.row_reason(), Vec::new())
                    }
                    Effect::Run(_) => return None,
                },
                _ => return None,
            };
            Some(UnanalysedFile {
                language: crate::languages::get_language_for_file(file.as_str()),
                path: file.as_str().to_string(),
                reason,
                lines,
            })
        })
        .collect();
    rows.sort_by(|a, b| a.path.cmp(&b.path));
    rows.dedup_by(|a, b| a.path == b.path);
    rows.truncate(MAX_UNACCOUNTED_ROWS);
    rows
}

/// The files of the definition's group that spell one of `names`, by path as the
/// answer prints it: what the token index holds, whatever a builder linked. A file
/// of no group, or a group the run could not read, has none.
fn spelling_files(state: &ScoredState, cells: &Cells, file: &str, names: &[String]) -> Vec<String> {
    let Some(tokens) =
        decide::Group::of_path(file).and_then(|group| cells.group_tokens(state, group))
    else {
        return Vec::new();
    };
    let wanted = Names::new(names.iter().map(String::as_str));
    tokens
        .spelling(&wanted)
        .into_iter()
        .map(|at| tokens.files()[at as usize].as_str().to_string())
        .collect()
}

/// The names a use of a definition the fragments hold could carry: its own, and the
/// last part of a qualified one (a method is called as `m`, not `A.m`).
fn names_of(symbol: &str) -> Vec<String> {
    let name = base_symbol(symbol);
    let last = name.rsplit(['.', ':']).next().unwrap_or(name);
    let mut names = vec![name.to_string()];
    if last != name {
        names.push(last.to_string());
    }
    names
}

/// The runtime a language runs in, for the languages that call across
/// their own boundary: JVM languages call each other, JavaScript runs the
/// HTML and single-file components that embed it, Swift, Objective-C, C and
/// C++ bridge and link. Anything else is its own runtime.
pub(crate) fn runtime_of(path: &str) -> String {
    let language = crate::languages::get_language_for_file(path).unwrap_or("");
    match language {
        "java" | "kotlin" | "scala" | "groovy" | "clojure" => "jvm",
        "javascript" | "typescript" | "tsx" | "jsx" | "html" | "svelte" | "vue" => "js",
        "python" | "cython" => "python",
        "csharp" | "c_sharp" | "fsharp" => "dotnet",
        "swift" | "objectivec" | "c" | "cpp" => "native",
        other => other,
    }
    .to_string()
}

fn children_of<'t>(node: tree_sitter::Node<'t>, kind: &str) -> Vec<tree_sitter::Node<'t>> {
    let mut cursor = node.walk();
    (node.named_children(&mut cursor))
        .filter(|child| child.kind() == kind)
        .collect()
}

/// The line of the first command in `script` that runs `runtime`'s
/// interpreter on code naming `name`: a heredoc fed to it, or its `-c`/`-e`
/// argument. The line, and the bytes of the command.
pub(crate) fn embedded_naming(
    script: &str,
    runtime: &str,
    name: &str,
) -> Option<(u32, std::ops::Range<u32>)> {
    let (interpreters, as_file): (&[&str], &str) = match runtime {
        "python" => (&["python", "pypy"], "embedded.py"),
        "js" => (&["node", "deno", "bun"], "embedded.js"),
        _ => return None,
    };
    let tree = crate::parsers::parse_tree("bash", script)?;
    let text = |n: tree_sitter::Node| n.utf8_text(script.as_bytes()).unwrap_or("");
    // Only code names it: a comment or a string in the embedded program
    // does not (`# column-aligned`).
    let names = |embedded: &str| names_word(&code_text(as_file, embedded), name);
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
        if node.kind() != "command" {
            continue;
        }
        let mut cursor = node.walk();
        let words: Vec<&str> = node.named_children(&mut cursor).map(text).collect();
        if !words.iter().any(|w| {
            let base = w.rsplit('/').next().unwrap_or(w);
            interpreters.iter().any(|i| base.starts_with(i))
        }) {
            continue;
        }
        let inline = words
            .iter()
            .position(|w| matches!(*w, "-c" | "-e"))
            .and_then(|i| words.get(i + 1))
            .is_some_and(|code| names(code.trim_matches(['\'', '"'])));
        let fed = node.parent().is_some_and(|p| {
            p.kind() == "redirected_statement"
                && children_of(p, "heredoc_redirect")
                    .iter()
                    .any(|r| (children_of(*r, "heredoc_body").iter()).any(|b| names(text(*b))))
        });
        if inline || fed {
            let at = |offset: usize| u32::try_from(offset).unwrap_or(u32::MAX);
            return Some((
                at(node.start_position().row + 1),
                at(node.start_byte())..at(node.end_byte()),
            ));
        }
    }
    None
}

/// No runtime crosses from one to the other: a Kotlin test cannot run a
/// Swift function, and a Python name in a Kotlin file is not a call of it
/// (#458, #487).
fn same_runtime(a: &str, b: &str) -> bool {
    let (ra, rb) = (runtime_of(a), runtime_of(b));
    ra.is_empty() || rb.is_empty() || ra == rb
}

fn rel(state: &ScoredState, path: &str) -> String {
    crate::paths::display_rel_or_abs(&state.root_dir, Path::new(path))
}

fn lines_of(id: &FragmentId) -> String {
    format!("{}-{}", id.start_line, id.end_line)
}

/// Visibility the source states: `pub`, `export`, `public`, a Go
/// capitalised name. Python and Ruby state none, so no contract is claimed
/// for them rather than a guessed one.
fn is_public(path: &str, symbol: Option<&str>, content: &str) -> bool {
    if path.ends_with(".go") {
        return symbol.is_some_and(|s| s.chars().next().is_some_and(|c| c.is_ascii_uppercase()));
    }
    let head = content.trim_start();
    // `pub(crate)`, `pub(super)` and `pub(in …)` reach no other crate.
    head.starts_with("pub ")
        || head.starts_with("export ")
        || head.starts_with("public ")
        || head.starts_with("module.exports")
        || head.starts_with("exports.")
}

/// An added run whose lines, blank ones aside, are a run removed elsewhere in
/// the same file: the definition moved, its text and indentation did not.
fn moved_verbatim(
    run: &crate::change_class::HunkText,
    hunks: &[crate::change_class::HunkText],
) -> bool {
    let text = |lines: &[String]| -> Vec<String> {
        lines
            .iter()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.trim_end().to_string())
            .collect()
    };
    let added = text(&run.added);
    run.removed.is_empty()
        && !added.is_empty()
        && hunks
            .iter()
            .any(|h| h.path == run.path && h.added.is_empty() && text(&h.removed) == added)
}

fn is_schema_file(path: &str) -> bool {
    // A test named for the schema it checks is not a schema.
    if crate::testfiles::is_test_path(Path::new(path)) {
        return false;
    }
    let lower = path.to_lowercase();
    if SCHEMA_FILE_EXTENSIONS
        .iter()
        .any(|ext| lower.ends_with(ext))
    {
        return true;
    }
    let p = Path::new(&lower);
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
    let in_schema_dir = p.parent().is_some_and(|d| {
        d.components()
            .any(|c| SCHEMA_DIRECTORIES.contains(&c.as_os_str().to_str().unwrap_or("")))
    });
    // A migration is code by extension and a schema by where it lives; so is
    // the Rails schema dump. A bare `versions/` is not one (`.yarn/versions`),
    // alembic's is named whole.
    let in_db = |name: &str| {
        p.parent()
            .and_then(|d| d.file_name())
            .is_some_and(|d| d == name)
    };
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    // Prose and tables beside the migrations (a plan, an inventory) are no
    // schema (#463).
    let prose = matches!(
        ext,
        "md" | "markdown" | "rst" | "txt" | "adoc" | "tsv" | "csv"
    );
    let migration = !prose
        && p.components()
            .any(|c| matches!(c.as_os_str().to_str(), Some("migrations" | "migration")))
        || lower.contains("/alembic/versions/")
        || lower.contains("db/migrate/")
        || (in_db("db") && matches!(stem, "schema" | "structure"));
    if migration {
        return true;
    }
    if !SCHEMA_DATA_EXTENSIONS.contains(&ext) {
        return false;
    }
    let file = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
    in_schema_dir
        || file
            .split('.')
            .any(|part| SCHEMA_FILE_STEMS.contains(&part))
}

/// A fragment without its decorators. The decorator belongs to the fragment
/// (Python `@app.route(…, methods={"GET"})`, a TS `@Component({…})`) but not
/// to the declaration: `export` comes after it, and a `{` in it would end the
/// declaration's head before the signature began.
fn without_decorators(content: &str) -> &str {
    let mut rest = content;
    while let Some(line) = rest.lines().next() {
        let t = line.trim_start();
        if !(t.is_empty() || t.starts_with('@')) {
            break;
        }
        rest = rest.get(line.len() + 1..).unwrap_or("");
    }
    rest
}

/// The declaration a caller depends on: from the definition's first line
/// through the one that opens its body (`{`, `=>`, a trailing `:`), cut
/// there, so a body on the same line is not part of it.
fn declaration_head(content: &str) -> String {
    let mut head = Vec::new();
    for line in without_decorators(content)
        .lines()
        .take(MAX_SIGNATURE_LINES)
    {
        if let Some(at) = line.find("=>") {
            head.push(&line[..at + 2]);
            break;
        }
        if let Some(at) = line.find('{') {
            head.push(&line[..=at]);
            break;
        }
        head.push(line);
        if line.trim_end().ends_with(':') || line.trim_end().ends_with(';') {
            break;
        }
    }
    head.join("\n")
}

/// The base side's text of the files whose public signatures impact compares,
/// read through the analysis' base reader and kept: the check of each public
/// symbol read the same few files again and again. Only a read that succeeded
/// is kept, so a failure is met again as the first one was.
struct BaseTexts<'a> {
    root: &'a Path,
    source: crate::source::Source,
    texts: std::cell::RefCell<FxHashMap<String, std::rc::Rc<str>>>,
}

impl<'a> BaseTexts<'a> {
    fn new(root: &'a Path, base: crate::source::Source) -> Self {
        Self {
            root,
            source: base,
            texts: std::cell::RefCell::default(),
        }
    }

    /// `Ok(None)`: the base has no such file.
    fn read(&self, rel_path: &str) -> crate::snapshot::Result<Option<std::rc::Rc<str>>> {
        if let Some(text) = self.texts.borrow().get(rel_path) {
            return Ok(Some(text.clone()));
        }
        let Some(text) = self.source.read_lossy(&self.root.join(rel_path))? else {
            return Ok(None);
        };
        let text: std::rc::Rc<str> = text.into();
        self.texts
            .borrow_mut()
            .insert(rel_path.to_string(), text.clone());
        Ok(Some(text))
    }
}

/// Whether an exported symbol's declaration moved: its head is looked for in
/// the file at the base revision, whitespace-insensitively. A body-only edit
/// is what the callers list already says, and a contracts section repeated
/// on every refactor teaches the reader to skip it (#325). A file the base
/// does not have is new, and every export in it is a new contract.
fn signature_changed(state: &ScoredState, base_texts: &BaseTexts, core: &Fragment) -> bool {
    let rel_path = rel(state, core.path());
    let old = match base_texts.read(&rel_path) {
        Ok(Some(old)) => old,
        Ok(None) => return true,
        Err(e) => {
            state.run.note(crate::resource::reason_for(&e));
            return true;
        }
    };
    let squash = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let head = squash(&declaration_head(&core.content));
    head.is_empty() || !squash(&old).contains(&head)
}

/// `(base, head)` for blame: a two-sided range as given, a single revision
/// against HEAD (its commits are `rev..HEAD`), nothing for the working tree.
fn commit_bounds(diff_range: Option<&str>) -> Option<(String, String)> {
    let range = diff_range?;
    if range == "HEAD" {
        return None;
    }
    match crate::snapshot::split_diff_range(range) {
        (Some(base), Some(head)) => Some((base, head)),
        (None, None) => Some((range.to_string(), "HEAD".to_string())),
        _ => None,
    }
}

/// Distinct commits of the range that touched the fragment's lines. The
/// line log follows the span back through the range, so a commit whose
/// lines a later one overwrote still counts; blame at the head would not
/// see it.
fn commits_touching(state: &ScoredState, base: &str, head: &str, frag: &Fragment) -> usize {
    let rel_path = rel(state, frag.path());
    let span = format!("{},{}:{}", frag.id.start_line, frag.id.end_line, rel_path);
    let range = format!("{base}..{head}");
    let Ok(out) = crate::snapshot::run_git(
        &state.root_dir,
        &["log", "--format=%H", "--no-patch", "-L", &span, &range],
    ) else {
        return 0;
    };
    out.lines()
        .map(str::trim)
        .filter(|l| (l.len() == 40 || l.len() == 64) && l.bytes().all(|b| b.is_ascii_hexdigit()))
        .collect::<FxHashSet<_>>()
        .len()
}

/// Whether the first fragment references the second's definition: through
/// a resolved binding in the languages that have a binding model, through
/// the lexical model elsewhere.
type Link<'l> = dyn Fn(&Fragment, &Fragment) -> bool + 'l;

/// How far up the call graph a test may sit and still count as reaching a
/// caller: the test, a function it calls, and one more.
pub(crate) const MAX_TEST_HOPS: usize = 2;

/// The chunks of one long function (`main[88]`, `main[120]`) are one caller
/// with several call sites, not several callers, and so is a file's
/// module-level code split into unnamed chunks (#349); distinct functions
/// stay distinct, and so do evidence levels and stated kinds.
#[allow(
    clippy::disallowed_methods,
    reason = "`Caller.lines` is `a-b`, written by this module: a field that is no number has no call site to add"
)]
fn merge_by_symbol(callers: Vec<Caller>) -> Vec<Caller> {
    let mut merged: Vec<Caller> = Vec::new();
    for mut c in callers {
        if c.sites.is_empty() {
            c.sites = c
                .lines
                .split('-')
                .next()
                .and_then(|l| l.parse().ok())
                .into_iter()
                .collect();
        }
        let same = |m: &&mut Caller| {
            m.path == c.path
                && m.evidence == c.evidence
                && m.kind == c.kind
                && m.folded_by == c.folded_by
                && m.fold_kind == c.fold_kind
                && m.fold_share == c.fold_share
                && m.symbol.as_deref().map(base_symbol) == c.symbol.as_deref().map(base_symbol)
        };
        match merged.iter_mut().find(|m| same(m)) {
            Some(m) => {
                // The caller spans every chunk it was split into, or a call
                // site in a later chunk sits outside the range shown (#472).
                let span = |l: &str| -> Option<(u32, u32)> {
                    let (a, b) = l.split_once('-')?;
                    Some((a.parse().ok()?, b.parse().ok()?))
                };
                if let (Some((a, b)), Some((c2, d))) = (span(&m.lines), span(&c.lines)) {
                    m.lines = format!("{}-{}", a.min(c2), b.max(d));
                }
                m.symbol = m.symbol.as_deref().map(|s| base_symbol(s).to_string());
                m.weight = m.weight.max(c.weight);
                m.sites.extend(c.sites);
                m.sites.sort_unstable();
                m.sites.dedup();
                if c.relation == "call" {
                    m.relation = "call";
                }
                if m.tests.is_empty() {
                    m.tests = c.tests;
                }
                m.unread_test = m.unread_test.take().or(c.unread_test);
                m.file_tested = m.tests.is_empty() && (m.file_tested || c.file_tested);
                m.tests_open = m.tests.is_empty() && (m.tests_open || c.tests_open);
            }
            None => merged.push(c),
        }
    }
    merged
}

pub(crate) fn base_symbol(symbol: &str) -> &str {
    symbol.split('[').next().unwrap_or(symbol)
}

/// What can call a member nothing calls directly, in a language whose model
/// does not trace dispatch: the supertype it overrides or implements, the
/// runtime for a Python operator, any interface a Go method set satisfies.
/// Read as "nothing calls them", an override of `Runnable.run` or an
/// `__eq__` a test exercises was reported as dead (#566). A private or
/// static member is reached only directly, so its zero stands.
fn dispatch_route(state: &ScoredState, core: &Fragment, file: &[&Fragment]) -> Option<String> {
    let name = base_symbol(core.symbol_name.as_deref()?);
    let ext = Path::new(core.path())
        .extension()?
        .to_str()?
        .to_ascii_lowercase();
    match ext.as_str() {
        // Traits are the Rust cell's (`Gap::Dispatched`).
        "rs" => return None,
        "go" => {
            return core
                .content
                .trim_start()
                .starts_with("func (")
                .then(|| "any interface its type's method set satisfies".to_string());
        }
        "py" | "pyi" if name.len() > 4 && name.starts_with("__") && name.ends_with("__") => {
            return Some(format!(
                "the operator or protocol the runtime maps to `{name}`"
            ));
        }
        "py" | "pyi" if name.starts_with('_') => return None,
        _ => {}
    }
    if !is_member(core, file) {
        return None;
    }
    let text = state.run.source().read_to_string(Path::new(core.path()))?;
    let lines: Vec<&str> = text.lines().collect();
    let first = core.start_line().saturating_sub(1) as usize;
    let above = lines[..first.min(lines.len())]
        .iter()
        .rev()
        .take_while(|l| l.trim_start().starts_with('@'))
        .copied();
    let declaration = lines.get(first).copied().unwrap_or("");
    let head: String = above.chain([declaration]).collect::<Vec<_>>().join(" ");
    let words: Vec<&str> = head
        .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '@'))
        .collect();
    if words
        .iter()
        .any(|w| matches!(*w, "private" | "static" | "@staticmethod"))
    {
        return None;
    }
    let supers =
        enclosing_class(core, file).map_or_else(Vec::new, |c| supertypes(&c.content, &ext));
    let quoted = || format!("`{}`", supers[..supers.len().min(2)].join("`, `"));
    if words.iter().any(|w| matches!(*w, "@Override" | "override")) {
        return Some(if supers.is_empty() {
            "the supertype it overrides".to_string()
        } else {
            quoted()
        });
    }
    (!supers.is_empty()).then(quoted)
}

/// The types a class header names as its supertypes: `extends`,
/// `implements` and `with` clauses, a Python base list, a Kotlin, Swift or
/// C# `: Base` after the primary constructor. The root types every class
/// has are not routes.
pub(crate) fn supertypes(class: &str, ext: &str) -> Vec<String> {
    let Some(at) = class.find("class ") else {
        return Vec::new();
    };
    let header = &class[at..];
    let python = matches!(ext, "py" | "pyi");
    let header = header
        .split(if python { ":\n" } else { "{" })
        .next()
        .unwrap_or(header);
    let clause: String = if python {
        header
            .split_once('(')
            .and_then(|(_, rest)| rest.rsplit_once(')'))
            .map_or_else(String::new, |(bases, _)| bases.to_string())
    } else {
        // A primary constructor's `(x: Int)` holds colons of its own.
        let mut depth = 0usize;
        let flat: String = header
            .chars()
            .filter(|c| {
                match c {
                    '(' | '<' => depth += 1,
                    ')' | '>' => depth = depth.saturating_sub(1),
                    _ => return depth == 0,
                }
                false
            })
            .collect();
        let after = ["extends ", "implements ", " with ", ":"]
            .iter()
            .filter_map(|k| flat.find(k).map(|i| i + k.len()))
            .min();
        after.map_or_else(String::new, |i| flat[i..].to_string())
    };
    clause
        .split([',', ' ', '\n', '\t'])
        .map(|t| t.trim().trim_end_matches(':'))
        .filter(|t| {
            !t.is_empty()
                && !t.contains('=')
                && !matches!(
                    *t,
                    "extends" | "implements" | "with" | "object" | "Object" | "Any" | "AnyObject"
                )
                && t.chars()
                    .next()
                    .is_some_and(|c| c.is_alphabetic() || c == '_')
        })
        .map(|t| t.split(['[', '<', '(']).next().unwrap_or(t))
        .filter(|t| !t.is_empty())
        .map(|t| t.rsplit('.').next().unwrap_or(t).to_string())
        .collect()
}

/// The class a fragment sits in, among `file`, the fragments of its own
/// file in fragment order.
/// The indentation of a text's first non-blank, non-decorator line.
fn indent_of(text: &str) -> usize {
    text.lines()
        .find(|l| !l.trim().is_empty() && !l.trim_start().starts_with('@'))
        .map_or(0, |l| l.len() - l.trim_start().len())
}

pub(crate) fn enclosing_class<'a>(f: &Fragment, file: &[&'a Fragment]) -> Option<&'a Fragment> {
    let own = indent_of(&f.content);
    let containers = || {
        file.iter().copied().filter(|c| {
            c.path() == f.path()
                && c.id != f.id
                && c.kind.is_container()
                && c.start_line() <= f.start_line()
        })
    };
    // A class fragment may be its header alone, the methods fragmented on
    // their own: an indented definition then belongs to the nearest class
    // above it at a shallower indentation.
    containers()
        .filter(|c| c.end_line() >= f.end_line())
        .min_by_key(|c| c.line_count())
        .or_else(|| {
            (own > 0)
                .then(|| {
                    containers()
                        .filter(|c| indent_of(&c.content) < own)
                        .max_by_key(|c| c.start_line())
                })
                .flatten()
        })
}

/// Decorators that shape a definition without handing it to anyone.
const SHAPING_DECORATORS: &[&str] = &[
    "property",
    "staticmethod",
    "classmethod",
    "dataclass",
    "abstractmethod",
    "override",
    "cached_property",
    "wraps",
    "lru_cache",
    "cache",
    "total_ordering",
    "final",
    "overload",
    "contextmanager",
    "asynccontextmanager",
    "Override",
    "Deprecated",
    "SuppressWarnings",
    "FunctionalInterface",
    // `@value.setter`, `@attr.s`, `@define`, `@enum.unique`, Lombok: they
    // shape what they decorate and register it nowhere (#414).
    "setter",
    "getter",
    "deleter",
    "s",
    "attrs",
    "define",
    "frozen",
    "mutable",
    "unique",
    "Data",
    "Value",
    "Getter",
    "Setter",
    "Builder",
    "Slf4j",
    "ToString",
    "EqualsAndHashCode",
    "NoArgsConstructor",
    "AllArgsConstructor",
    "RequiredArgsConstructor",
];

/// The decorator that hands a definition to a registry, a router or a
/// container, on the definition or on the class around it.
/// JVM annotations that hand a method or a bean to a framework that calls
/// it: request mappings, schedulers, listeners, bean factories. A
/// class-level `@Component` registers the class, not every method on it.
const JVM_ENTRY_POINTS: &[&str] = &[
    "RequestMapping",
    "GetMapping",
    "PostMapping",
    "PutMapping",
    "DeleteMapping",
    "PatchMapping",
    "Scheduled",
    "EventListener",
    "TransactionalEventListener",
    "KafkaListener",
    "RabbitListener",
    "JmsListener",
    "SqsListener",
    "ExceptionHandler",
    "MessageMapping",
    "SubscribeMapping",
    "QueryMapping",
    "MutationMapping",
    "SubscriptionMapping",
    "SchemaMapping",
    "BatchMapping",
    "Bean",
    "PostConstruct",
    "PreDestroy",
    "Tool",
    "GET",
    "POST",
    "PUT",
    "DELETE",
    "PATCH",
    "HEAD",
    "OPTIONS",
    "Get",
    "Post",
    "Put",
    "Delete",
    "Patch",
];

/// Packages whose annotations are trusted as entry points when imported.
const JVM_FRAMEWORK_PACKAGES: &[&str] = &[
    "org.springframework.",
    "jakarta.",
    "javax.",
    "io.micronaut.",
    "io.quarkus.",
    "org.eclipse.microprofile.",
];

fn is_jvm_path(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    [".kt", ".kts", ".java", ".scala"]
        .iter()
        .any(|e| p.ends_with(e))
}

/// The framework entry point annotating the JVM definition itself, its
/// import naming a framework package. Unknown annotations are neutral.
fn jvm_entry_point(
    f: &Fragment,
    file: &[&Fragment],
    annotations: Option<&[String]>,
) -> Option<String> {
    let names: Vec<String> = match annotations {
        Some(names) => names.to_vec(),
        None => f
            .content
            .lines()
            .map(str::trim_start)
            .take_while(|l| l.is_empty() || l.starts_with('@'))
            .filter_map(|l| {
                let name = l
                    .strip_prefix('@')?
                    .split(|c: char| c == '(' || c.is_whitespace())
                    .next()?;
                Some(name.rsplit('.').next().unwrap_or(name).to_string())
            })
            .collect(),
    };
    names.into_iter().find(|name| {
        JVM_ENTRY_POINTS.contains(&name.as_str())
            && file.iter().flat_map(|fr| fr.content.lines()).any(|line| {
                let line = line.trim_start();
                let Some(path) = line.strip_prefix("import ") else {
                    return false;
                };
                let path = path.trim_end_matches(';').trim();
                let path = path.split(" as ").next().unwrap_or(path);
                JVM_FRAMEWORK_PACKAGES.iter().any(|p| path.starts_with(p))
                    && (path.ends_with(&format!(".{name}")) || path.ends_with(".*"))
            })
    })
}

/// A Python function decorated `@pytest.fixture` / `@fixture`, with or
/// without arguments.
fn is_pytest_fixture(f: &Fragment) -> bool {
    f.path().ends_with(".py")
        && f.content
            .lines()
            .map(str::trim_start)
            .take_while(|l| l.is_empty() || l.starts_with('@'))
            .any(|l| {
                let d = l.trim_start_matches('@');
                d.starts_with("pytest.fixture") || d.starts_with("fixture")
            })
}

/// The line of `f`'s own `def` whose parameter list names `name`: how a
/// test or a fixture asks pytest for a fixture.
fn names_parameter(f: &Fragment, name: &str) -> Option<u32> {
    let header = f.content.lines().position(|l| {
        let t = l.trim_start();
        t.starts_with("def ") || t.starts_with("async def ")
    })?;
    let signature: String = f
        .content
        .lines()
        .skip(header)
        .take(6)
        .collect::<Vec<_>>()
        .join(" ");
    let params = signature.split_once('(')?.1.split_once(')')?.0;
    params
        .split(',')
        .map(|p| p.trim().split([':', '=']).next().unwrap_or("").trim())
        .any(|p| p == name)
        .then(|| f.start_line() + header as u32)
}

/// How a definition is entered by something that is no caller: its module's
/// own script guard, or a curried registration call (`tool("name")(fn)`).
enum Entered {
    Script,
    Registered(String),
}

/// A statement of `f` that enters the definition `name` of its own file:
/// the module's `if __name__ == "__main__":` guard calling it, or a curried
/// registration call naming it. Both are how it is run, not who calls it.
fn entered_by(f: &Fragment, name: &str) -> Option<Entered> {
    static CURRIED: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
        regex::Regex::new(r"^\s*([A-Za-z_][\w.]*\([^()]*\))\(\s*([A-Za-z_][\w.]*)\s*\)\s*$")
            .expect("curried registration regex")
    });
    if !f.path().ends_with(".py") && !f.path().ends_with(".pyi") {
        return None;
    }
    let first = f.content.lines().find(|l| !l.trim().is_empty())?.trim();
    let guard = first.starts_with("if __name__ ==")
        && (first.contains("\"__main__\"") || first.contains("'__main__'"));
    if guard {
        return f
            .content
            .lines()
            .any(|l| calls_word(l, name))
            .then_some(Entered::Script);
    }
    f.content.lines().find_map(|l| {
        let caps = CURRIED.captures(l)?;
        (&caps[2] == name).then(|| Entered::Registered(caps[1].to_string()))
    })
}

/// The class of a `do_<METHOD>` handler a standard-library HTTP server
/// dispatches by method name: the base it names.
fn http_handler_base(core: &Fragment, file: &[&Fragment]) -> Option<&'static str> {
    let name = base_symbol(core.symbol_name.as_deref()?);
    let method = name.strip_prefix("do_")?;
    if method.is_empty() || !method.chars().all(|c| c.is_ascii_uppercase()) {
        return None;
    }
    let class = enclosing_class(core, file)?;
    let supers = supertypes(&class.content, "py");
    [
        "BaseHTTPRequestHandler",
        "SimpleHTTPRequestHandler",
        "CGIHTTPRequestHandler",
    ]
    .into_iter()
    .find(|base| supers.iter().any(|s| s == base))
}

/// `main` of a program in a language whose runtime calls it.
fn is_program_main(core: &Fragment) -> bool {
    let Some(name) = core.symbol_name.as_deref().map(base_symbol) else {
        return false;
    };
    let ext = Path::new(core.path())
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "go" | "java" | "kt" | "kts" | "scala" | "rs" | "c" | "cc" | "cpp" | "swift" => {
            name == "main"
        }
        "cs" => name == "Main",
        _ => false,
    }
}

fn registering_decorator(f: &Fragment, file: &[&Fragment]) -> Option<String> {
    let of = |content: &str| {
        content
            .lines()
            .map(str::trim_start)
            .take_while(|l| l.is_empty() || l.starts_with('@'))
            .filter_map(|l| {
                let name = l
                    .strip_prefix('@')?
                    .split(|c: char| c == '(' || c.is_whitespace())
                    .next()?;
                let last = name.rsplit('.').next().unwrap_or(name);
                (!name.is_empty()
                    && !SHAPING_DECORATORS.contains(&last)
                    && !name.starts_with("pytest."))
                .then(|| name.to_string())
            })
            .next()
    };
    of(&f.content).or_else(|| enclosing_class(f, file).and_then(|c| of(&c.content)))
}

/// A field or parameter line that brings its own default (`x: int = 0`,
/// `x = 0`, TypeScript `x?: T`): adding one breaks no call site.
static DEFAULTED_LINE: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
    regex::Regex::new(
        r"^\s*(readonly\s+)?[A-Za-z_$][\w$]*\s*(\?\s*:[^=]*|:[^=]*=\s*\S.*|=\s*[^=\s].*)$",
    )
    .expect("defaulted line regex")
});

/// The parameters between a declaration's first `(` and its match, split at
/// top-level commas, whitespace squashed.
fn parameter_list(text: &str) -> Option<(String, Vec<String>, String)> {
    let open = text.find('(')?;
    let mut depth = 0i32;
    let mut close = None;
    for (i, c) in text[open..].char_indices() {
        match c {
            '(' | '[' | '{' | '<' => depth += 1,
            ')' | ']' | '}' | '>' => {
                depth -= 1;
                if depth == 0 && c == ')' {
                    close = Some(open + i);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close?;
    let squash = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut params = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    for c in text[open + 1..close].chars() {
        match c {
            '(' | '[' | '{' | '<' => depth += 1,
            ')' | ']' | '}' | '>' => depth -= 1,
            ',' if depth == 0 => {
                params.push(squash(&cur));
                cur.clear();
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    params.push(squash(&cur));
    params.retain(|p| !p.is_empty());
    Some((squash(&text[..open]), params, squash(&text[close..])))
}

fn defaulted(param: &str) -> bool {
    (param.contains('=') && !param.contains("=>") && !requires_value(param))
        || param.contains("?:")
        || param.starts_with('*')
        || param.starts_with("...")
}

/// `x: int = Field(...)` and `x = ...` declare a default that is a
/// required marker, not a value.
fn requires_value(line: &str) -> bool {
    let Some((_, value)) = line.split_once('=') else {
        return false;
    };
    let value = value.trim().trim_end_matches(',').trim();
    value == "..." || value.starts_with("Field(...") || value.starts_with("Field( ...")
}

/// A line that adds a field or a parameter with its own default.
fn adds_defaulted(line: &str) -> bool {
    // `__slots__ = ("y",)` or `__hash__ = None` changes how every instance
    // is built or compared; a default does not make it compatible.
    let dunder = line.trim_start().starts_with("__");
    DEFAULTED_LINE.is_match(line) && !dunder && !line.contains("=>") && !requires_value(line)
}

/// A field or parameter declaration of any kind: what may not follow an
/// inserted one, since everything after it would shift position.
fn declares_member(line: &str) -> bool {
    static MEMBER: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
        regex::Regex::new(r"^\s*(readonly\s+)?\*{0,2}[A-Za-z_$][\w$]*\s*(\??\s*:|=|,\s*$)")
            .expect("member regex")
    });
    MEMBER.is_match(line)
}

/// Whether every hunk in the definition only added fields or parameters
/// that carry a default (#388): the call sites keep working, so they are
/// counted instead of listed.
fn compatible_extension(core: &Fragment, hunks: &[&crate::change_class::HunkText]) -> bool {
    let head_end = core.start_line() + declaration_head(&core.content).lines().count() as u32;
    !hunks.is_empty()
        && hunks.iter().all(|h| {
            let (_, end) = h.new_range();
            if h.removed.is_empty() {
                // Appended only: the next declaration line after the run
                // must close the list, or the inserted member shifts every
                // positional argument behind it.
                let after = core
                    .content
                    .lines()
                    .skip((end + 1).saturating_sub(core.start_line()) as usize)
                    .find(|l| !l.trim().is_empty());
                let appended = after.is_none_or(|l| !declares_member(l));
                (core.kind.is_container() || end < head_end)
                    && appended
                    && h.added
                        .iter()
                        .filter(|l| !l.trim().is_empty())
                        .all(|l| adds_defaulted(l))
            } else {
                // A parameter list lives in the declaration head; the same
                // shape in a body line is a call (`compute(a)` →
                // `compute(a, strict=True)`), and it breaks nothing it adds.
                let (start, _) = h.new_range();
                if start < core.start_line() || end >= head_end {
                    return false;
                }
                let (Some((old_pre, old, old_post)), Some((new_pre, new, new_post))) = (
                    parameter_list(&h.removed.join("\n")),
                    parameter_list(&h.added.join("\n")),
                ) else {
                    return false;
                };
                old_pre == new_pre
                    && old_post == new_post
                    && new.len() > old.len()
                    && new[..old.len()] == old[..]
                    && new[old.len()..].iter().all(|p| defaulted(p))
            }
        })
}

/// The fragment that spans a changed definition: the one of its name
/// whose lines overlap the definition's and lie nearest them.
fn cell_anchor<'a>(frags: &[&'a Fragment], def: &decide::Changed) -> Option<&'a Fragment> {
    frags
        .iter()
        .copied()
        .filter(|f| {
            f.kind != FragmentKind::Impl
                && f.kind != FragmentKind::Excerpt
                && f.kind != FragmentKind::Chunk
                && !f.kind.is_signature()
                && f.symbol_name.as_deref().map(base_symbol) == Some(def.name.as_str())
                && f.start_line() <= def.lines.1
                && def.lines.0 <= f.end_line()
        })
        .min_by_key(|f| f.start_line().abs_diff(def.lines.0) + f.end_line().abs_diff(def.lines.1))
}

/// A `const`/`let` of a module-level block (a `for` body): a fragment the
/// facts hold no definition for, and no function around it makes it a local.
fn is_module_block_binding(core: &Fragment, file: &[&Fragment]) -> bool {
    core.kind == FragmentKind::Variable
        && !file.iter().any(|f| {
            f.symbol_name.is_some()
                && f.kind != FragmentKind::Chunk
                && f.kind != FragmentKind::Excerpt
                && !f.kind.is_signature()
                && f.line_count() > core.line_count()
                && f.start_line() <= core.start_line()
                && core.end_line() <= f.end_line()
        })
}

/// The fragment that owns a line of use: the smallest named one around it.
fn cell_caller<'a>(frags: &[&'a Fragment], line: u32) -> Option<&'a Fragment> {
    frags
        .iter()
        .copied()
        .filter(|f| {
            f.start_line() <= line
                && line <= f.end_line()
                && f.kind != FragmentKind::Excerpt
                && !f.kind.is_signature()
        })
        .min_by_key(|f| (f.symbol_name.is_none(), f.line_count()))
}

/// A use whose owner is a definition nested in the fragment that holds its line, by
/// the resolver's own scope. In TypeScript a function-valued constant or member that
/// leaves its enclosing definition (`got.extend = …` in a factory that returns `got`)
/// is a definition of its own, and the fragmenter cuts none for it: the smallest
/// fragment around the line is the factory, which may itself be changed. The row is
/// the nested definition's, never the factory's.
fn owned_by_a_nested_definition(frag: &Fragment, u: &decide::Use) -> bool {
    let crate::facts::Scope::Def(local) = &u.scope else {
        return false;
    };
    matches!(
        crate::languages::get_language_for_file(&u.file),
        Some("typescript" | "javascript")
    ) && frag
        .symbol_name
        .as_deref()
        .map(base_symbol)
        .is_some_and(|own| {
            local.name.as_str() != own && local.container.iter().any(|c| c.as_str() == own)
        })
}

/// The definition a fragment that owns a line of use is a piece of: the function
/// around a local variable, an anonymous arrow, a chunk of a long function, the
/// class holding a field. Any other fragment stands for itself.
fn lifted_caller<'a>(frags: &[&'a Fragment], source: &'a Fragment) -> &'a Fragment {
    let anonymous = source.kind == FragmentKind::Function && source.symbol_name.is_none();
    let field = source.kind == FragmentKind::Property;
    let chunk = source.kind == FragmentKind::Chunk;
    if source.kind != FragmentKind::Variable && !anonymous && !field && !chunk {
        return source;
    }
    let parent = source.symbol_name.as_deref().map(base_symbol);
    frags
        .iter()
        .copied()
        .filter(|f| {
            (f.kind == FragmentKind::Function
                || (anonymous && f.kind == FragmentKind::Variable)
                || (field && f.kind.is_container())
                || (chunk
                    && f.kind != FragmentKind::Chunk
                    && parent.is_none_or(|p| f.symbol_name.as_deref() == Some(p))))
                && f.symbol_name.is_some()
                && f.id != source.id
                && f.start_line() <= source.start_line()
                && f.end_line() >= source.end_line()
        })
        .min_by_key(|f| f.line_count())
        .unwrap_or(source)
}

/// What every row the resolver found weighs: the kinds of relation are not
/// ranked against each other, the cap deals the rows out among them.
const FINDING_WEIGHT: f64 = 1.0;

/// What the cell states of the uses one caller makes of a changed definition.
#[derive(Clone)]
struct Reach {
    standing: decide::Standing,
    relation: &'static str,
    kind: Option<vocabulary::RelationKind>,
    /// The rule of the folding table that says the change cannot have affected
    /// the use, the relation kind it is counted under, and how many definitions
    /// share the name a candidate may be.
    fold: Option<(&'static str, &'static str, usize)>,
    lines: Vec<u32>,
}

impl Reach {
    fn evidence(&self) -> &'static str {
        match self.standing {
            decide::Standing::Resolved => "resolved",
            decide::Standing::Candidate(_) => "candidate",
        }
    }

    fn reason(&self) -> Option<&'static str> {
        match self.standing {
            decide::Standing::Candidate(reason) => Some(reason),
            decide::Standing::Resolved => None,
        }
    }

    /// The kind a row built from this states for a caller in `path`: the
    /// use's own, for a precise (language, relation) cell only. A use resolves
    /// within one language, so the caller's file names the cell of the
    /// definition it reaches.
    fn stated_kind(&self, path: &str) -> Option<&'static str> {
        self.kind
            .and_then(|kind| vocabulary::stated_kind_of(path, kind))
    }
}

fn reach_of(u: &decide::Use) -> Reach {
    use crate::resolve::Relation as Uses;
    Reach {
        standing: u.standing,
        relation: match u.relation {
            Uses::Calls | Uses::Instantiates => "call",
            Uses::Overrides => "implementation",
            _ => "reference",
        },
        kind: Some(vocabulary::kind_of_use(u.relation)),
        fold: u
            .folded
            .map(|rule| (rule.name(), u.relation.name(), u.sharing)),
        lines: vec![u.line],
    }
}

/// What the answer says of a gap in the resolver's reach: a name on more lines
/// than can be told apart is too common to trace (#413), any other count is
/// reads through a receiver nothing types (#399).
fn cell_unknown(gap: &decide::Gap, name: &str) -> Unknown {
    match gap {
        decide::Gap::Unlanded { uses } if *uses > MAX_REMOVED_NAME_HITS => Unknown::CommonWord {
            name: name.to_string(),
            lines: *uses,
        },
        decide::Gap::Unlanded { uses } => Unknown::UnresolvedReads {
            name: name.to_string(),
            reads: *uses,
        },
        decide::Gap::Unpaired => Unknown::Unpaired {
            name: name.to_string(),
        },
        decide::Gap::Stopped { reasons } => Unknown::Unfinished {
            stops: reasons.clone(),
        },
        decide::Gap::Dispatched { via } => Unknown::Dispatched {
            trait_name: via.clone(),
        },
        other => Unknown::Uncovered {
            why: other.to_string(),
        },
    }
}

/// The evidence of a row for a module that re-exports the definition: where
/// it reaches the package's users, not a caller of it.
const REEXPORT: &str = "reexport";

/// A caller row as the resolver gave it, for a line no fragment owns: a
/// script the fragmenter did not cut, or a file past its fragment cap.
fn cell_row(u: &decide::Use) -> Caller {
    let reach = reach_of(u);
    let reexport = u.relation == crate::resolve::Relation::Reexports;
    let caller = match &u.scope {
        crate::facts::Scope::Def(local) => Some(local.name.as_str().to_string()),
        _ => None,
    };
    Caller {
        path: u.file.clone(),
        symbol: caller,
        lines: format!("{0}-{0}", u.line),
        weight: FINDING_WEIGHT,
        tests: Vec::new(),
        file_tested: false,
        registered_by: None,
        unread_test: None,
        tests_open: false,
        evidence: if reexport { REEXPORT } else { reach.evidence() },
        reason: reach.reason().filter(|_| !reexport),
        relation: if reexport {
            "reexports"
        } else {
            reach.relation
        },
        kind: reach.stated_kind(&u.file),
        folded_by: reach.fold.map(|(rule, _, _)| rule),
        fold_kind: reach.fold.map(|(_, kind, _)| kind),
        fold_share: reach.fold.map_or(0, |(_, _, share)| share),
        sites: reach.lines,
    }
}

/// A holder nothing uses itself is changed through what it holds: the use of a
/// held definition, or what the search could not tell of it, is that row's to
/// say. "Nothing calls it" of the holder would be an all-clear the held row's
/// gap contradicts.
fn held_has_users(cell: &decide::Cell, holder: &decide::Changed) -> bool {
    cell.changed().iter().any(|held| {
        holder.holds(held) && {
            let verdict = cell.verdict(held);
            !verdict.uses.is_empty() || !verdict.complete() || !verdict.unused()
        }
    })
}

/// A definition the answer reports without a fragment to hang the roles
/// and test links on: its callers are the resolver's rows as they stand.
fn cell_symbol(def: &decide::Changed, verdict: &decide::Verdict, kind: String) -> ChangedSymbol {
    let callers: Vec<Caller> = verdict
        .resolved()
        .chain(verdict.candidates())
        .map(cell_row)
        .collect();
    let users = |resolved: bool| -> Vec<String> {
        verdict
            .also_changed()
            .filter(|u| (u.standing == decide::Standing::Resolved) == resolved)
            .map(|u| label(&u.file, None, &u.line.to_string()))
            .collect()
    };
    let (also_changed, possibly_also_changed) = (users(true), users(false));
    ChangedSymbol {
        callers: {
            let mut merged = merge_by_symbol(callers);
            merged.sort_by_key(|c| relation_rank(c.relation));
            merged
        },
        unresolved: verdict.gap().map(|gap| cell_unknown(&gap, &def.name)),
        missed_in: unaccounted_rows(verdict),
        also_changed,
        possibly_also_changed,
        change: Some(crate::answer::v2::Change::of(def)),
        new_parameters: def.new_parameters(),
        ..ChangedSymbol::bare(
            def.file.clone(),
            def.symbol(),
            format!("{}-{}", def.lines.0, def.lines.1),
            kind,
            decide::Coverages::of_verdict(verdict),
        )
    }
}

/// `deadline`: the line log (one git call per changed symbol) runs only
/// while there is time left; commit overlap is the least actionable of the
/// three signals and the first to yield.
pub fn build_impact(
    state: &ScoredState,
    diff_range: Option<&str>,
    query: Option<&str>,
    deadline: Option<Instant>,
) -> ImpactOutput {
    let setup = crate::profile::phase("impact.setup");
    // A signature variant as long as its fragment shares its id (a Kotlin
    // class header ending `) {`); the id names the fragment, not its stub.
    let mut by_id: FxHashMap<&FragmentId, &Fragment> = FxHashMap::default();
    for f in &state.all_fragments {
        by_id
            .entry(&f.id)
            .and_modify(|seen| {
                if seen.kind.is_signature() && !f.kind.is_signature() {
                    *seen = f;
                }
            })
            .or_insert(f);
    }
    let changed_paths: FxHashSet<&str> = state
        .changed_files
        .iter()
        .filter_map(|p| p.to_str())
        .collect();
    // The cells: changed definitions from the facts layer's semantic changes,
    // callers from the resolver of their language (decide.rs). The ranking
    // graph and the text filters take no part in what they answer.
    let cells = {
        let _t = crate::profile::detail("impact.setup.cells", "");
        Cells::open(
            state,
            state.analysed_range.as_deref().or(diff_range),
            query.is_some(),
        )
    };
    let tests = {
        let _t = crate::profile::detail("impact.setup.tests", "");
        TestsReach::build(
            state,
            &cells,
            state.analysed_range.as_deref().or(diff_range),
        )
    };
    // A caller in a changed file is outside the diff unless its lines are
    // in a hunk: the reader holds the hunks, not the rest of the file (#362).
    let mut hunk_lines: FxHashMap<&str, Vec<(u32, u32)>> = FxHashMap::default();
    for h in &state.hunk_texts {
        // A pure removal sits between new lines N and N+1: as `(N+1, N)` the
        // overlap test below holds only for a fragment spanning both, not
        // for the caller that ends on N right above the deleted lines.
        let range = if h.new_len == 0 {
            (h.new_start + 1, h.new_start)
        } else {
            h.new_range()
        };
        hunk_lines.entry(h.path.as_str()).or_default().push(range);
    }
    let in_a_hunk = |id: &FragmentId| {
        if state.hunk_texts.is_empty() {
            return changed_paths.contains(id.path.as_ref());
        }
        hunk_lines
            .get(rel(state, id.path.as_ref()).as_str())
            .is_some_and(|ranges| {
                ranges
                    .iter()
                    .any(|&(a, b)| id.start_line <= b && id.end_line >= a)
            })
    };
    let hunks_in = |core: &Fragment| -> Vec<&crate::change_class::HunkText> {
        let path = rel(state, core.path());
        state
            .hunk_texts
            .iter()
            .filter(|h| {
                let (a, b) = h.new_range();
                h.path == path && core.start_line() <= b && core.end_line() >= a
            })
            .collect()
    };
    let mut untested_definitions = 0usize;
    let mut untested_unlisted: Vec<String> = Vec::new();
    let mut untested_called_in_diff: Vec<String> = Vec::new();
    let mut entry_unlisted: Vec<String> = Vec::new();
    let mut layout_only: Vec<String> = Vec::new();
    let mut traced_definitions = 0usize;

    // A hunk inside a function body seeds a nested fragment — a `let`, an
    // inner block — that nobody calls; the symbol a reader knows, and the one
    // the callers link to, is the named container around it.
    let mut cores: Vec<&Fragment> = state
        .core_ids
        .iter()
        .filter_map(|id| by_id.get(id).copied())
        .filter_map(|f| named_container_of(f, &state.all_fragments))
        .collect();
    cores.sort_by(|a, b| a.id.cmp(&b.id));
    cores.dedup_by(|a, b| a.id == b.id);

    let analysed = state.analysed_range.as_deref().or(diff_range);
    let bounds = commit_bounds(analysed).filter(|_| state.commit_count >= 2);
    let base_texts = BaseTexts::new(&state.root_dir, state.run.source().base());

    let mut changed: Vec<ChangedSymbol> = Vec::new();
    // Schema changes first, then the public symbols something calls, then
    // the rest: what the cap keeps is what a reader would look at first.
    let mut contracts: Vec<Contract> = Vec::new();
    let mut public_api: Vec<(bool, Contract)> = Vec::new();
    // Definitions are counted per language: the builders link a call to the
    // definitions of its own language, so a Python wrapper named like a Rust
    // function is not what makes the Rust name ambiguous. An `impl` block
    // extends the type it names rather than competing with it: a struct with
    // a `Display` and an `Error` impl is one definition, not three.
    let mut definitions: FxHashMap<(&str, String), usize> = FxHashMap::default();
    let mut defined_in: FxHashSet<(String, &str)> = FxHashSet::default();
    let mut file_fragments: FxHashMap<&str, Vec<&Fragment>> = FxHashMap::default();
    let indexing = crate::profile::detail("impact.setup.files", "");
    for f in &state.all_fragments {
        file_fragments.entry(f.path()).or_default().push(f);
        if let Some(name) = f
            .symbol_name
            .as_deref()
            .filter(|_| f.kind.is_definition_kind() && f.kind != FragmentKind::Impl)
        {
            let lang = crate::languages::get_language_for_file(f.path()).unwrap_or("");
            *definitions.entry((lang, name.to_lowercase())).or_default() += 1;
            defined_in.insert((name.to_string(), f.path()));
        }
    }
    drop(indexing);
    let files = Files {
        fragments: file_fragments,
        cells: &cells,
        state,
    };
    let source = state.run.source();
    // A lexical link stops at a project the workspace model says cannot
    // reference the other: a Python test naming `config` is not a test of
    // another project's `config.ts`. It crosses through a declared
    // dependency — a Maven `it/` module importing `com.acme.core.Pricing`, a
    // workspace crate writing `core_crate::pricing`.
    let one_tree =
        |user: &str, used: &str| source.may_reference(&rel(state, user), &rel(state, used));
    let one_project = |user: &Fragment, used: &Fragment| one_tree(user.path(), used.path());
    let file_of = |f: &Fragment| files.fragments.get(f.path()).map_or(&[][..], Vec::as_slice);
    let registration = |f: &Fragment| -> Option<String> {
        let path = rel(state, f.path());
        let stated_entry = f.symbol_name.as_deref().map(base_symbol).and_then(|name| {
            cells
                .of(&path)?
                .framework_entries_at(&path, name, (f.start_line(), f.end_line()))
                .into_iter()
                .find(|(kind, _)| *kind != EntryKind::Fixture)
        });
        if let Some((kind, entry)) = stated_entry {
            return Some(framework::describe(kind, &entry));
        }
        if is_jvm_path(f.path()) {
            // Kotlin's and Java's annotations are the facts' own.
            let stated = cells.of(&path).and_then(|cell| {
                cell.annotations_at(
                    &path,
                    f.symbol_name.as_deref().map(base_symbol)?,
                    (f.start_line(), f.end_line()),
                )
            });
            jvm_entry_point(f, file_of(f), stated.as_deref())
        } else {
            registering_decorator(f, file_of(f))
        }
    };
    let link = |user: &Fragment, used: &Fragment| -> bool {
        let Some(name) = used.symbol_name.as_deref().map(base_symbol) else {
            return false;
        };
        if !same_runtime(user.path(), used.path()) {
            return false;
        }
        let (user_rel, used_rel) = (rel(state, user.path()), rel(state, used.path()));
        if let Some(cell) = cells.of(&user_rel).filter(|c| c.owns(&used_rel)) {
            return cell.links(
                &user_rel,
                (user.start_line(), user.end_line()),
                &used_rel,
                name,
                (used.start_line(), used.end_line()),
            );
        }
        one_project(user, used)
            && mentions(user, Some(name), &Receiver::Any)
            && references_module(user, &files, Some(name), used.path())
    };
    let reaches_file = |test: &Fragment, caller: &Fragment| -> bool {
        if !same_runtime(test.path(), caller.path()) {
            return false;
        }
        let (test_rel, caller_rel) = (rel(state, test.path()), rel(state, caller.path()));
        if let Some(cell) = cells.of(&test_rel).filter(|c| c.owns(&caller_rel)) {
            return cell.reaches(&test_rel, &caller_rel);
        }
        one_project(test, caller) && references_module(test, &files, None, caller.path())
    };
    // A field changes its class too: how the class is built (a dataclass's
    // constructor, a column's default) is its callers' business, and what
    // reads the field is the field's (#388, #488); so does an attribute the
    // method around it sets (#426). A query names one definition.
    let holding: Vec<&Fragment> = cores
        .iter()
        .filter(|f| query.is_none() && f.kind == FragmentKind::Property)
        .filter_map(|f| {
            file_of(f)
                .iter()
                .copied()
                .filter(|c| {
                    c.id != f.id
                        && (c.kind.is_container() || c.kind == FragmentKind::Function)
                        && c.symbol_name.is_some()
                        && within(&f.id, c)
                })
                .min_by_key(|c| c.line_count())
        })
        .collect();
    let mut cores = innermost(cores);
    cores.extend(holding);
    // An `impl` block extends the type it names: with that type among the
    // cores, a second entry for the block repeats the type (#403).
    let types: FxHashSet<(&str, &str)> = cores
        .iter()
        .filter(|c| c.kind != FragmentKind::Impl)
        .filter_map(|c| Some((c.path(), c.symbol_name.as_deref()?)))
        .collect();
    let cores: Vec<&Fragment> = cores
        .into_iter()
        .filter(|c| {
            c.kind != FragmentKind::Impl
                || !c
                    .symbol_name
                    .as_deref()
                    .is_some_and(|s| types.contains(&(c.path(), s)))
        })
        .collect();
    // A deletion that only borders a definition removed whatever followed
    // it, at its own indentation or shallower: the definitions it took are
    // reported as removed, this one did not change — and it is no part of
    // the diff when it calls another changed symbol. Lines removed between
    // two lines the definition spans are its own body, at any indentation
    // (a dedented SQL string). A core no hunk overlaps was attached to a
    // deletion beside it: the hunks name its file, never its lines.
    let borders_only = |core: &Fragment| -> bool {
        let core_hunks = hunks_in(core);
        if core_hunks.is_empty() {
            let core_path = rel(state, core.path());
            return state.hunk_texts.iter().any(|h| h.path == core_path);
        }
        let header_indent = indent_of(without_decorators(&core.content));
        core_hunks.iter().all(|h| {
            let interior = core.start_line() <= h.new_start && core.end_line() > h.new_start;
            h.added.is_empty()
                && !interior
                && h.removed
                    .iter()
                    .find(|l| !l.trim().is_empty())
                    .is_some_and(|l| indent_of(l) <= header_indent)
        })
    };
    let bordering: FxHashSet<&FragmentId> = cores
        .iter()
        .filter(|c| borders_only(c))
        .map(|c| &c.id)
        .collect();
    // A constructor is called by its class's name (`cp.ComponentModel(b, 1)`,
    // `new Store()`): its callers are the class's (#318). Mapped after the
    // bordering test, which a class header with no hunk of its own would fail.
    let cores: Vec<&Fragment> = {
        let mut seen: FxHashSet<&FragmentId> = FxHashSet::default();
        cores
            .into_iter()
            .map(|c| {
                let constructor = c
                    .symbol_name
                    .as_deref()
                    .map(base_symbol)
                    .is_some_and(|s| matches!(s, "__init__" | "constructor"));
                constructor
                    .then(|| enclosing_class(c, file_of(c)))
                    .flatten()
                    .unwrap_or(c)
            })
            .filter(|c| seen.insert(&c.id))
            .collect()
    };
    // The definitions of the cells' languages are the facts layer's: one entry
    // per changed definition, anchored on the fragment that spans it, so the
    // roles, test links and contracts built on fragments still reach it.
    let mut cores = cores;
    let mut cell_defs: FxHashMap<&FragmentId, decide::Changed> = FxHashMap::default();
    let mut cell_apart: Vec<decide::Changed> = Vec::new();
    let mut cell_removed: Vec<decide::Changed> = Vec::new();
    let mut cell_untracked: Vec<&Fragment> = Vec::new();
    // A queried name in a file its cell lost (a syntax error, a read): the
    // definition is not in the facts, and the file's loss is the answer.
    let mut cell_unread: Vec<(&Fragment, decide::Reason)> = Vec::new();
    for cell in cells.iter() {
        let mut entries: Vec<decide::Changed> = Vec::new();
        if query.is_some() {
            for core in cores.iter().filter(|c| cell.owns(&rel(state, c.path()))) {
                let defined = cell.define(
                    &rel(state, core.path()),
                    core.symbol_name.as_deref().map_or("", base_symbol),
                    (core.start_line(), core.end_line()),
                );
                // A name asked for that no definition stands for is said, not
                // left out: an empty answer would read as "no callers".
                if defined.is_empty() && is_module_block_binding(core, file_of(core)) {
                    cell_untracked.push(core);
                } else if defined.is_empty() {
                    if let Some(reason) = cell.loss_about(&rel(state, core.path())) {
                        let core: &Fragment = core;
                        cell_unread.push((core, reason));
                    }
                }
                entries.extend(defined);
            }
        } else {
            entries.extend(cell.changed().iter().cloned());
        }
        let (owned, rest): (Vec<&Fragment>, Vec<&Fragment>) = std::mem::take(&mut cores)
            .into_iter()
            .partition(|c| cell.owns(&rel(state, c.path())));
        cores = rest;
        let mut seen_entries: FxHashSet<(String, String, (u32, u32))> = FxHashSet::default();
        for entry in entries {
            if !seen_entries.insert((entry.file.clone(), entry.name.clone(), entry.lines)) {
                continue;
            }
            if entry.how == decide::How::Removed {
                cell_removed.push(entry);
                continue;
            }
            let anchor = files
                .fragments
                .get(crate::paths::join_native(&state.root_dir, &entry.file).as_str())
                .and_then(|frags| cell_anchor(frags, &entry));
            match anchor {
                Some(f) if !cell_defs.contains_key(&f.id) => {
                    cores.push(f);
                    cell_defs.insert(&f.id, entry);
                }
                _ => cell_apart.push(entry),
            }
        }
        // A definition the facts layer reports no change in, whose hunks are
        // only what a formatter makes (a trailing space, a comma), is layout.
        if query.is_none() {
            for core in owned {
                if cell_defs.contains_key(&core.id) {
                    continue;
                }
                let hunks = hunks_in(core);
                if !hunks.is_empty()
                    && hunks
                        .iter()
                        .all(|h| h.layout || moved_verbatim(h, &state.hunk_texts))
                {
                    let named = label(
                        &rel(state, core.path()),
                        core.symbol_name.as_deref(),
                        &lines_of(&core.id),
                    );
                    if !layout_only.contains(&named) {
                        layout_only.push(named);
                    }
                }
            }
        }
    }
    // A family the run could not resolve: its definitions are named as not
    // determined, never answered by the paths the cells replaced.
    let (cell_lost, kept): (Vec<&Fragment>, Vec<&Fragment>) = std::mem::take(&mut cores)
        .into_iter()
        .partition(|c| cells.lost(&rel(state, c.path())));
    cores = kept;
    // A queried name has no diff: a call from elsewhere in its own file is a
    // caller like any other.
    let inside_the_diff = |id: &FragmentId| {
        (state.core_ids.contains(id) && !bordering.contains(id))
            || (query.is_none() && in_a_hunk(id))
    };
    let untraced = if query.is_none() {
        let _t = crate::profile::detail("impact.setup.untraced", "");
        UntracedNamers::build(state, &cores, diff_range)
    } else {
        UntracedNamers::default()
    };
    drop(setup);
    crate::profile::count("impact.cores", cores.len() as u64);
    let stopped_early = !state.run.stops().is_empty();
    let mut unreached: Vec<&Fragment> = cell_lost;
    let callers = crate::profile::phase("impact.callers");
    for (i, core) in cores.iter().enumerate() {
        // Past the deadline the symbols already answered are the answer:
        // `limits` then says it is partial, and a mass reformat returns what
        // it found instead of nothing (#382). The definitions it did not
        // reach are named below, never left out of the count as if nothing
        // called them.
        if !state.run.check() {
            unreached.extend(cores[i..].iter().copied());
            break;
        }
        let core_hunks = hunks_in(core);
        let py = cell_defs.get(&core.id);
        let verdict = py.and_then(|def| {
            let cell = cells.of(&def.file)?;
            let _t = crate::profile::detail("impact.callers.verdict", "");
            Some(cell.verdict(def))
        });
        if let Some(v) = &verdict {
            tracing::debug!(
                "impact {}: the cell claims {:?}",
                core.symbol_name.as_deref().unwrap_or("?"),
                v.claim(crate::resolve::Relation::Calls)
            );
        }
        // A formatter run or a comment changes nothing a caller sees (#370),
        // and neither does a definition moved verbatim within its file. The
        // facts layer says it for its languages: the hashes of the definition
        // did not move.
        // The run's own formatter test (`settle_layout`) still decides what the
        // hashes cannot see through: a trailing comma, a quote style.
        let hunk_layout = !core_hunks.is_empty()
            && core_hunks
                .iter()
                .all(|h| h.layout || moved_verbatim(h, &state.hunk_texts));
        // A decorated definition is never set aside as layout or doc: a decorator
        // may read what the edit touched. A comment-only edit is traced and its rows
        // fold as no code change, a docstring edit lists them.
        let layout = match py {
            Some(def) => {
                (matches!(def.how, decide::How::Edited { layout_only: true }) || hunk_layout)
                    && !def.decorated
            }
            None => hunk_layout,
        };
        if query.is_none() && layout {
            layout_only.push(label(
                &rel(state, core.path()),
                core.symbol_name.as_deref(),
                &lines_of(&core.id),
            ));
            continue;
        }
        if query.is_none() && py.is_none() && bordering.contains(&core.id) {
            continue;
        }
        traced_definitions += 1;
        // A function whose signature the cell found compatible is folded by its
        // rule (`check-the-default`); the text rule below stays for what the cell
        // has no signature of: the fields a class gained.
        let compatible = query.is_none()
            && py.is_none_or(|def| def.new_parameters().is_empty())
            && compatible_extension(core, &core_hunks);
        let symbol = core.symbol_name.as_deref();
        let member = is_member(core, file_of(core));
        let stem = Path::new(core.path())
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        // `build`, `new`, `run`: a name defined all over the repository links
        // by name to every definition, and its "callers" would be everyone's.
        let core_lang = crate::languages::get_language_for_file(core.path()).unwrap_or("");
        // With a cell, imports decide which definition a reference reaches; a
        // name defined in many files is ambiguous only without one.
        let binding_model = symbol.is_some() && py.is_some();
        // A qualified query (`path:name`) names its definition; only a bare
        // name or a diff leaves the lexical model guessing among many.
        let qualified = query.is_some_and(|q| q.contains(':'));
        let ambiguous = !binding_model
            && !qualified
            && symbol
                .map(|s| {
                    definitions
                        .get(&(core_lang, s.to_lowercase()))
                        .copied()
                        .unwrap_or(0)
                        > MAX_DEFINITIONS
                })
                .unwrap_or(false);
        let mut sources: Vec<(&Fragment, f64, Option<Reach>)> = Vec::new();
        // Why a linked fragment is not listed, per core, under RUST_LOG=debug:
        // the precision filters are the part of this view that field reports
        // question first.
        let mut dropped: FxHashMap<&'static str, usize> = FxHashMap::default();
        // Sources the diff holds go through the same tests as any caller: one
        // that passes is no caller outside the diff, but it makes the
        // definition live rather than one nothing calls (#441).
        let mut in_diff: FxHashSet<FragmentId> = FxHashSet::default();
        // Whether a fragment that reaches the core is a caller, and with
        // what evidence; the reason it is not, otherwise.
        let judge = |frag: &Fragment| -> Result<Option<Reach>, &'static str> {
            // A file that defines the same name calls its own.
            let shadowed = frag.path() != core.path()
                && symbol.is_some_and(|s| defined_in.contains(&(s.to_string(), frag.path())));
            if frag.kind == FragmentKind::Excerpt {
                Err("excerpt")
            } else if !same_runtime(frag.path(), core.path()) {
                Err("other_runtime")
            } else if shadowed && !binding_model {
                Err("defines_same_name")
            } else if is_import_block(&frag.content) {
                Err("import_block")
            } else if !binding_model
                && symbol.is_some_and(|s| {
                    binds_locally(&code_text(frag.path(), &frag.content), base_symbol(s))
                })
            {
                // `let read = |p| p + 1; read(1)` calls its own closure (#391).
                Err("local_binding")
            } else if !is_code_file(frag.path()) {
                Err("not_code")
            } else if !one_tree(frag.path(), core.path()) {
                Err("other_project")
            } else if !{
                let aliases = if member {
                    Vec::new()
                } else {
                    files
                        .fragments
                        .get(frag.path())
                        .map_or_else(Vec::new, |f| module_aliases(f, stem))
                };
                let receiver = if member {
                    Receiver::Any
                } else {
                    Receiver::Module {
                        stem,
                        aliases: &aliases,
                    }
                };
                mentions(frag, symbol, &receiver)
            } {
                Err("no_mention_in_code")
            } else if !references_module(frag, &files, symbol, core.path()) {
                Err("no_module_reference")
            } else {
                Ok(None)
            }
        };
        // A definition no cell reads has no resolved use to list: the files of
        // its group that spell the name are the text level's answer, each
        // fragment that names it in code a candidate and never a certainty. The
        // account below says what the search could not read. A name defined all
        // over the language says the same thing on every definition ("too common
        // to trace"), so nothing is searched for it: a text row per definition
        // would be the noise that line folds.
        if py.is_none() && !ambiguous {
            let _t = crate::profile::detail("impact.callers.text", "");
            let names = symbol.map_or_else(Vec::new, names_of);
            for path in spelling_files(state, &cells, &rel(state, core.path()), &names) {
                let Some(frags) = files
                    .fragments
                    .get(crate::paths::join_native(&state.root_dir, &path).as_str())
                else {
                    continue;
                };
                for &frag in frags.iter().filter(|f| !f.kind.is_signature()) {
                    let src = &frag.id;
                    if inside_the_diff(src) {
                        in_diff.insert(src.clone());
                    }
                    let reason = if within(src, core) {
                        "own_body"
                    } else {
                        match judge(frag) {
                            // A caller inside the diff is dropped after the search:
                            // it decides only whether the definition reads as
                            // called (#552).
                            Ok(found) => {
                                sources.push((frag, 0.0, found));
                                continue;
                            }
                            Err(reason) => reason,
                        }
                    };
                    *dropped.entry(reason).or_default() += 1;
                }
            }
        }
        let kept = sources.len();
        sources.retain(|(f, _, _)| !in_diff.contains(&f.id));
        let mut called_in_diff = sources.len() < kept;
        if called_in_diff {
            dropped.insert("inside_the_diff", kept - sources.len());
        }
        let mut also_changed: Vec<String> = Vec::new();
        let mut possibly_also_changed: Vec<String> = Vec::new();
        // Uses on lines no fragment owns keep the resolver's own row.
        let mut loose: Vec<Caller> = Vec::new();
        for u in verdict.iter().flat_map(|v| v.uses.iter()) {
            if u.relation == crate::resolve::Relation::Reexports && !u.also_changed {
                loose.push(cell_row(u));
                continue;
            }
            let owner = files
                .fragments
                .get(crate::paths::join_native(&state.root_dir, &u.file).as_str())
                .and_then(|frags| cell_caller(frags, u.line));
            if owner.is_some_and(|frag| within(&frag.id, core)) {
                continue;
            }
            let Some(frag) = owner.filter(|frag| !owned_by_a_nested_definition(frag, u)) else {
                if u.also_changed {
                    called_in_diff = true;
                    let users = if u.standing == decide::Standing::Resolved {
                        &mut also_changed
                    } else {
                        &mut possibly_also_changed
                    };
                    users.push(label(&u.file, None, &u.line.to_string()));
                } else {
                    loose.push(cell_row(u));
                }
                continue;
            };
            if u.also_changed {
                called_in_diff = true;
                let users = if u.standing == decide::Standing::Resolved {
                    &mut also_changed
                } else {
                    &mut possibly_also_changed
                };
                let owner = lifted_caller(file_of(frag), frag);
                users.push(label(
                    &u.file,
                    owner.symbol_name.as_deref(),
                    &lines_of(&owner.id),
                ));
                continue;
            }
            sources.push((frag, FINDING_WEIGHT, Some(reach_of(u))));
        }
        also_changed.sort();
        also_changed.dedup();
        possibly_also_changed.sort();
        possibly_also_changed.dedup();
        // A resolved caller inside the diff is a caller: the framework route a
        // definition nothing calls may take is not claimed over it. A possible one
        // proves nothing, and the route stands.
        let resolved_in_diff = !also_changed.is_empty();
        // A pytest fixture is called by the runner, for every test or fixture
        // that names it as a parameter — in its own file or under the
        // directory of the conftest that defines it (#401).
        if let Some(name) = symbol.filter(|_| is_pytest_fixture(core)) {
            let path = rel(state, core.path());
            let listed: FxHashSet<FragmentId> =
                sources.iter().map(|(f, _, _)| f.id.clone()).collect();
            let users = tests.fixture_askers(cells.of(&path), &path, core, base_symbol(name));
            for (frag, line) in users.into_iter().filter(|(f, _)| !listed.contains(&f.id)) {
                if inside_the_diff(&frag.id) {
                    called_in_diff = true;
                    continue;
                }
                sources.push((
                    frag,
                    FINDING_WEIGHT,
                    Some(Reach {
                        standing: decide::Standing::Resolved,
                        relation: "reference",
                        kind: None,
                        fold: None,
                        lines: vec![line],
                    }),
                ));
            }
        }
        // A local `const items = rows(xs)` is a line of its function, not a
        // caller of its own: the function is, with its test link (#452).
        // An anonymous arrow (`.map((p) => …)`) is a line of the definition
        // around it as well (#520), and a field of the class holding it. So
        // is a chunk of a long function (`main[88]`), which no test edge
        // reaches: its calls arrive at the function (#579).
        for source in &mut sources {
            source.0 = lifted_caller(file_of(source.0), source.0);
        }
        // Two lines lifted into one function are one caller with both sites.
        let mut lifted: Vec<(&Fragment, f64, Option<Reach>)> = Vec::with_capacity(sources.len());
        for (frag, weight, found) in sources {
            match lifted.iter_mut().find(|(f, _, _)| f.id == frag.id) {
                Some((_, w, known)) => {
                    *w = w.max(weight);
                    match (known.as_mut(), found) {
                        (Some(k), Some(f)) => {
                            if f.standing == decide::Standing::Resolved {
                                k.standing = decide::Standing::Resolved;
                            }
                            // Two rows of different fold stay as listed as the
                            // one the table does not fold.
                            if k.fold != f.fold {
                                k.fold = None;
                            }
                            k.lines.extend(f.lines);
                            k.lines.sort_unstable();
                            k.lines.dedup();
                        }
                        (None, f) => *known = f,
                        _ => {}
                    }
                }
                None => lifted.push((frag, weight, found)),
            }
        }
        sources = lifted;
        sources.retain(|(f, _, _)| !within(&f.id, core));
        // The module's own guard and a registration call run the definition;
        // they are no callers of it (R3).
        let entered = symbol.and_then(|name| {
            file_of(core)
                .iter()
                .find_map(|f| entered_by(f, base_symbol(name)))
        });
        if let Some(name) = symbol.filter(|_| entered.is_some()) {
            sources.retain(|(f, _, _)| {
                f.path() != core.path() || entered_by(f, base_symbol(name)).is_none()
            });
        }
        let kept = sources.len();
        // The facts layer already told a caller changed in the range from one
        // that is not: a fragment overlapping a hunk is not that.
        sources.retain(|(f, _, _)| py.is_some() || !inside_the_diff(&f.id));
        called_in_diff = called_in_diff || sources.len() < kept;
        if !dropped.is_empty() {
            tracing::debug!(
                "impact {}::{}: kept {}, dropped {:?}",
                rel(state, core.path()),
                symbol.unwrap_or("?"),
                sources.len(),
                dropped
            );
        }
        let weights: FxHashMap<&FragmentId, f64> =
            sources.iter().map(|(f, w, _)| (&f.id, *w)).collect();
        let findings: FxHashMap<&FragmentId, Reach> = sources
            .iter()
            .filter_map(|(f, _, finding)| finding.clone().map(|x| (&f.id, x)))
            .collect();
        // A fragment that reads the definition one way and calls it another
        // has a row for each kind the cell states; with no kind stated it is
        // one row, as the finding above says.
        let mut other_kinds: FxHashMap<&FragmentId, Vec<(&'static str, Reach)>> =
            FxHashMap::default();
        for (frag, _, finding) in &sources {
            let Some((finding, kind)) = finding.as_ref().and_then(|finding| {
                Some((finding, finding.stated_kind(&rel(state, frag.path()))?))
            }) else {
                continue;
            };
            let rows = other_kinds.entry(&frag.id).or_default();
            match rows.iter_mut().find(|(stated, _)| *stated == kind) {
                Some(row) => row.1 = finding.clone(),
                None => rows.push((kind, finding.clone())),
            }
        }
        // A name match links an import line too, at the import weight; the
        // function that makes the call is the caller, the import is how it got
        // the name. A binding finding skips import lines, so the module-level
        // code it names calls the definition beside that function (#449).
        let named_files: FxHashSet<&str> = sources
            .iter()
            .filter(|(f, _, _)| f.symbol_name.is_some())
            .map(|(f, _, _)| f.path())
            .collect();
        sources.retain(|(f, _, found)| {
            f.symbol_name.is_some() || found.is_some() || !named_files.contains(f.path())
        });
        let rows = crate::profile::detail("impact.callers.rows", "");
        let callers: Vec<Caller> = innermost(sources.iter().map(|(f, _, _)| *f).collect())
            .into_iter()
            .flat_map(|frag| {
                let tested = tests.tested_by(frag, state, &by_id, &link);
                let found = findings.get(&frag.id);
                let registered_by = tested.is_empty().then(|| registration(frag)).flatten();
                let row = Caller {
                    path: rel(state, frag.path()),
                    symbol: frag.symbol_name.clone(),
                    lines: lines_of(&frag.id),
                    weight: (weights[&frag.id] * 1e3).round() / 1e3,
                    file_tested: tested.is_empty()
                        && registered_by.is_none()
                        && tests.file_tested(frag, &reaches_file),
                    unread_test: tested
                        .is_empty()
                        .then(|| tests.unread_naming(frag, &rel(state, frag.path())))
                        .flatten(),
                    tests_open: tested.is_empty() && tests.open_above(frag, &link, true),
                    registered_by,
                    tests: tested,
                    evidence: found.map_or("name", Reach::evidence),
                    reason: found.and_then(Reach::reason),
                    relation: found.map_or("call", |f| f.relation),
                    kind: found.and_then(|f| f.stated_kind(&rel(state, frag.path()))),
                    folded_by: found.and_then(|f| f.fold).map(|(rule, _, _)| rule),
                    fold_kind: found.and_then(|f| f.fold).map(|(_, kind, _)| kind),
                    fold_share: found.and_then(|f| f.fold).map_or(0, |(_, _, share)| share),
                    // A word match cites the line that spells the name, not
                    // the start of the function around it.
                    sites: match found {
                        Some(f) => f.lines.clone(),
                        None => symbol
                            .and_then(|name| first_naming_line(frag, base_symbol(name)))
                            .into_iter()
                            .collect(),
                    },
                };
                let rest: Vec<Caller> = other_kinds
                    .get(&frag.id)
                    .into_iter()
                    .flatten()
                    .filter(|(kind, _)| Some(*kind) != row.kind)
                    .map(|(kind, finding)| Caller {
                        evidence: finding.evidence(),
                        reason: finding.reason(),
                        relation: finding.relation,
                        kind: Some(*kind),
                        folded_by: finding.fold.map(|(rule, _, _)| rule),
                        fold_kind: finding.fold.map(|(_, kind, _)| kind),
                        fold_share: finding.fold.map_or(0, |(_, _, share)| share),
                        sites: finding.lines.clone(),
                        ..row.clone()
                    })
                    .collect();
                std::iter::once(row).chain(rest)
            })
            .collect();
        drop(rows);
        let mut callers = callers;
        callers.extend(loose);
        let mut callers = merge_by_symbol(callers);
        callers.sort_by(|a, b| {
            b.weight
                .total_cmp(&a.weight)
                .then_with(|| a.path.cmp(&b.path))
                .then_with(|| a.lines.cmp(&b.lines))
        });
        let in_time = deadline.is_none_or(|d| Instant::now() < d);
        let commits = match (&bounds, i < MAX_BLAMED_SYMBOLS && in_time) {
            (Some((base, head)), true) => {
                let _t = crate::profile::detail("impact.callers.commits", "");
                commits_touching(state, base, head, core)
            }
            _ => 0,
        };
        // An export's place in the surface moves when it is added, renamed or
        // moved, or its kind or signature changes: the facts layer says it for
        // its languages (Kotlin's cell also says which are public), the
        // declaration text for the rest.
        let public_change = match py
            .filter(|c| crate::languages::get_language_for_file(&c.file) == Some("kotlin"))
        {
            Some(changed) => changed.api,
            None => {
                is_public(core.path(), symbol, without_decorators(&core.content))
                    && rust::module_is_public(&state.run.source(), core.path())
                    && py.map_or_else(
                        || signature_changed(state, &base_texts, core),
                        |def| def.api,
                    )
            }
        };
        // A test's declarations are no API: nothing but its runner calls
        // them (#517).
        if query.is_none() && public_change && !tests.is_test(core) {
            public_api.push((
                callers.is_empty(),
                Contract {
                    path: rel(state, core.path()),
                    symbol: core.symbol_name.clone(),
                    kind: "public_api",
                },
            ));
        }
        // A changed symbol nothing depends on is the diff itself; the reader
        // holds that already, and listing it spends the cap on nothing.
        // A queried symbol's definition is half the answer: where it is.
        // Unless nothing could be resolved: an empty list is then no finding,
        // and silence would read as one. A trait method is reached through
        // its trait, whatever else shares its name (`fmt` in `impl Display`).
        // A data field or a class is no call target a framework dispatches to.
        let dispatchable = py.is_none_or(|def| {
            matches!(
                def.kind,
                crate::facts::DefKind::Method | crate::facts::DefKind::Property
            )
        });
        let route = (callers.is_empty() && !resolved_in_diff && dispatchable)
            .then(|| dispatch_route(state, core, file_of(core)))
            .flatten();
        // The role that gives the definition an implicit caller (R3): where a
        // static test link exists it is shown, otherwise the role is named.
        let test_link = |frag| {
            let found = tests.tested_by(frag, state, &by_id, &link);
            (!found.is_empty()).then(|| text::reachable_from_tests(&found))
        };
        let mut linkless_role = false;
        // A test helper the range's own tests call is traced like any definition:
        // the runner enters only what no one calls.
        let entry: Option<String> = if tests.is_test(core) && also_changed.is_empty() {
            Some("test (run by the test runner)".to_string())
        } else if matches!(entered, Some(Entered::Script)) {
            Some("run as a script (if __name__ == \"__main__\")".to_string())
        } else if let Some(by) = registration(core).or_else(|| match &entered {
            Some(Entered::Registered(call)) => Some(call.clone()),
            _ => None,
        }) {
            Some(
                match (by.starts_with(framework::ENTRY_PREFIX), test_link(core)) {
                    (true, Some(link)) => format!("{by}; {link}"),
                    (true, None) => by,
                    (false, Some(link)) => format!("registered by `{by}`; {link}"),
                    (false, None) => format!(
                        "registered by `{by}` \u{2014} reached through the framework, not traced"
                    ),
                },
            )
        } else if let Some(base) = http_handler_base(core, file_of(core)) {
            // The class link is modelled: no test reaching it is a finding,
            // not an unmodelled gap, so the role does not excuse it.
            let link = enclosing_class(core, file_of(core)).and_then(test_link);
            Some(match link {
                Some(link) => format!("dispatched by `{base}`; {link}"),
                None => {
                    linkless_role = true;
                    format!("dispatched by `{base}`")
                }
            })
        } else if is_program_main(core) {
            Some(match test_link(core) {
                Some(link) => format!("program entry point; {link}"),
                // A test that runs the binary is modelled for Rust: none
                // reaching `main` is a finding there, not an unmodelled gap.
                None if core.path().ends_with(".rs") => {
                    linkless_role = true;
                    "program entry point".to_string()
                }
                None => "program entry point".to_string(),
            })
        } else {
            None
        };
        let unresolved = symbol
            .filter(|_| ambiguous)
            .map(|s| Unknown::Ambiguous {
                name: s.to_string(),
                definitions: definitions
                    .get(&(core_lang, s.to_lowercase()))
                    .copied()
                    .unwrap_or(0),
                language: core_lang,
            })
            .or_else(|| route.clone().map(|via| Unknown::Overridable { via }))
            .or_else(|| {
                // The rows a text search lists are no resolution: the gap stands
                // beside them, and an empty list is never read as no caller.
                (py.is_none() && is_code_file(core.path())).then(|| Unknown::NotModelled {
                    language: if core_lang.is_empty() {
                        "this language"
                    } else {
                        core_lang
                    },
                })
            });
        // An entry point is called by its runner or framework: the hedge for
        // callers nothing traces would be a false unknown.
        let unresolved = unresolved.filter(|u| {
            entry.is_none()
                || !matches!(
                    u,
                    Unknown::Overridable { .. }
                        | Unknown::NotModelled { .. }
                        | Unknown::Dispatched { .. }
                )
        });
        // What the resolver could not land is said, never read as no caller.
        let shown = py.map_or_else(
            || symbol.map(base_symbol).unwrap_or("").to_string(),
            |def| def.name.clone(),
        );
        let unresolved = verdict
            .as_ref()
            .and_then(decide::Verdict::gap)
            .map(|gap| cell_unknown(&gap, &shown))
            .or(unresolved);
        // Read after the search: a stop noted while it ran, or before it
        // began, leaves its callers a lower bound.
        let stops = state.run.stops();
        let unresolved = if stops.is_empty() {
            unresolved
        } else {
            Some(Unknown::Unfinished { stops })
        };
        // A method only dispatch reaches is reached through its class: a test
        // handing the class to the runtime (a server, a test runner, a
        // visitor) reaches the method (#578). A caller a test calls is the
        // second hop of a test link: a caller read off the syntax has no
        // graph edge for the walk to take (#503). A candidate caller counts: a
        // test that reaches it may reach the definition, and an absence is
        // claimed only where nothing could.
        let class = route
            .as_ref()
            .and_then(|_| enclosing_class(core, file_of(core)));
        let class_tested =
            || class.is_some_and(|class| !tests.tested_by(class, state, &by_id, &link).is_empty());
        let tests_absent = query.is_none()
            && tests.any()
            && !tests.is_test(core)
            && (entry.is_none() || linkless_role)
            && !callers
                .iter()
                .any(|c| c.tests.iter().any(|t| t.via.is_none()))
            && tests.tested_by(core, state, &by_id, &link).is_empty()
            && !class_tested();
        let compatible_callers = if compatible {
            let n = callers
                .iter()
                .filter(|c| !matches!(c.evidence, "candidate" | REEXPORT))
                .count();
            callers.clear();
            n
        } else {
            0
        };
        let mut missed_in: Vec<UnanalysedFile> =
            verdict.as_ref().map_or_else(Vec::new, unaccounted_rows);
        let mut outside = 0;
        if callers.is_empty()
            && compatible_callers == 0
            && (unresolved.is_none()
                || verdict
                    .as_ref()
                    .is_some_and(decide::Verdict::gaps_are_only_unaccounted))
            && missed_in.is_empty()
        {
            missed_in = untraced.of(core).into_iter().collect();
            outside += missed_in.len();
        }
        let coverage = search_coverage(verdict.as_ref(), unresolved.as_ref(), outside);
        // No test found is an absence only from a closed search: the callers
        // accounted for and every definition above them. An open search says the
        // tests are not established; incomplete callers say nothing here, their
        // own gap does.
        let untested = tests_absent
            .then(|| {
                if tests.open_above(core, &link, false)
                    || class.is_some_and(|class| tests.open_above(class, &link, false))
                {
                    Some(GuardState::NotEstablished)
                } else {
                    coverage.all().witness().map(|_| GuardState::None)
                }
            })
            .flatten();
        let absent = untested == Some(GuardState::None);
        untested_definitions += usize::from(absent);
        // A test that reaches a definition no caller does (a doctest runs it, a
        // test in its own file spells it, a test of the class a dispatch route
        // goes through) is no caller: the definition is listed, its guard stated
        // by the absence of the claim, not left to the lists of what is untested.
        let tested_without_caller = || {
            query.is_none()
                && tests.any()
                && !tests.is_test(core)
                && entry.is_none()
                && untested.is_none()
                && (class_tested() || !tests.tested_by(core, state, &by_id, &link).is_empty())
        };
        // Nothing said of a definition needs every relation of it known.
        if let Some(complete) = coverage.all().witness().filter(|_| {
            callers.is_empty()
                && compatible_callers == 0
                && commits < 2
                && query.is_none()
                && also_changed.is_empty()
                && possibly_also_changed.is_empty()
                && !tested_without_caller()
        }) {
            if entry.is_none()
                && py.is_some_and(|def| {
                    cells
                        .of(&def.file)
                        .is_some_and(|cell| held_has_users(cell, def))
                })
            {
                traced_definitions -= 1;
                continue;
            }
            // Counted in the header, so named: a count of nothing listed
            // tells the reader neither where nor what (#398).
            let named = label(
                &rel(state, core.path()),
                core.symbol_name.as_deref(),
                &lines_of(&core.id),
            );
            if absent {
                let named = match entry.as_ref().filter(|_| linkless_role) {
                    Some(role) => format!("{named} \u{2014} {role}"),
                    None => named,
                };
                if called_in_diff {
                    untested_called_in_diff.push(named);
                } else {
                    untested_unlisted.push(absence(named, complete));
                }
            } else if let Some(role) = entry.as_ref() {
                entry_unlisted.push(format!("{named} \u{2014} {role}"));
            } else if called_in_diff {
                untested_called_in_diff.push(named);
            } else {
                // No test is in view to claim a missing link against: the search
                // still found no caller, and the definition is named for that.
                untested_unlisted.push(absence(named, complete));
            }
            continue;
        }
        // Listed for the callers changed with it, which are the diff's own
        // lines: the definition is live, and a missing test link is said once,
        // in the list a definition nothing outside the diff calls goes in.
        let only_in_diff = callers.is_empty()
            && compatible_callers == 0
            && commits < 2
            && coverage.all().witness().is_some()
            && !(also_changed.is_empty() && possibly_also_changed.is_empty());
        if only_in_diff && absent {
            untested_called_in_diff.push(label(
                &rel(state, core.path()),
                core.symbol_name.as_deref(),
                &lines_of(&core.id),
            ));
        }
        changed.push(ChangedSymbol {
            path: rel(state, core.path()),
            symbol: core.symbol_name.clone(),
            lines: lines_of(&core.id),
            kind: format!("{:?}", core.kind).to_lowercase(),
            commits,
            callers,
            entry,
            unresolved,
            untested: untested.filter(|_| !only_in_diff),
            compatible_callers,
            missed_in,
            also_changed,
            possibly_also_changed,
            shed: crate::answer::v2::ShedRows::default(),
            change: py.map(crate::answer::v2::Change::of),
            callers_omitted: 0,
            omitted: Vec::new(),
            folded: Vec::new(),
            new_parameters: py.map_or_else(Vec::new, decide::Changed::new_parameters),
            coverage,
        });
    }
    drop(callers);
    cell_untracked.sort_by(|a, b| (a.path(), a.start_line()).cmp(&(b.path(), b.start_line())));
    cell_untracked.dedup_by(|a, b| a.path() == b.path() && a.start_line() == b.start_line());
    for core in cell_untracked {
        let name = core.symbol_name.as_deref().map_or("", base_symbol);
        changed.push(ChangedSymbol {
            unresolved: Some(Unknown::Untracked {
                name: name.to_string(),
            }),
            ..ChangedSymbol::bare(
                rel(state, core.path()),
                Some(name.to_string()),
                lines_of(&core.id),
                "variable",
                decide::Coverages::partial(decide::Gap::NotTraced),
            )
        });
    }
    // A name asked for that no cell read a definition of, in a group whose file
    // was lost where the name is spelled: the loss is the answer, not an empty one.
    if let Some(asked) = query.filter(|_| {
        cell_defs.is_empty()
            && cell_apart.is_empty()
            && cell_removed.is_empty()
            && cell_unread.is_empty()
            && cores.is_empty()
    }) {
        let name = asked.rsplit(':').next().unwrap_or(asked);
        for (path, reason) in cells.iter().flat_map(|cell| cell.lost_namers(name)) {
            let gap = decide::Gap::Unaccounted { reason, files: 1 };
            changed.push(ChangedSymbol {
                unresolved: Some(Unknown::Uncovered {
                    why: gap.to_string(),
                }),
                ..ChangedSymbol::bare(
                    path.as_str().to_string(),
                    Some(name.to_string()),
                    String::new(),
                    "function",
                    decide::Coverages::partial(gap),
                )
            });
        }
    }
    for (core, reason) in cell_unread {
        let name = core.symbol_name.as_deref().map_or("", base_symbol);
        let gap = decide::Gap::Unaccounted { reason, files: 1 };
        changed.push(ChangedSymbol {
            unresolved: Some(Unknown::Uncovered {
                why: gap.to_string(),
            }),
            ..ChangedSymbol::bare(
                rel(state, core.path()),
                Some(name.to_string()),
                lines_of(&core.id),
                format!("{:?}", core.kind).to_lowercase(),
                decide::Coverages::partial(gap),
            )
        });
    }
    for entry in &cell_apart {
        let Some(cell) = cells.of(&entry.file) else {
            continue;
        };
        if query.is_none() && matches!(entry.how, decide::How::Edited { layout_only: true }) {
            layout_only.push(label(
                &entry.file,
                Some(entry.name.as_str()).filter(|_| entry.symbol().is_some()),
                &format!("{}-{}", entry.lines.0, entry.lines.1),
            ));
            continue;
        }
        let verdict = cell.verdict(entry);
        let sym = cell_symbol(entry, &verdict, format!("{:?}", entry.kind).to_lowercase());
        let callerless = query.is_none()
            && sym.callers.is_empty()
            && sym.also_changed.is_empty()
            && sym.possibly_also_changed.is_empty();
        if callerless && verdict.complete() && held_has_users(cell, entry) {
            continue;
        }
        traced_definitions += 1;
        // A definition with no fragment has no test link computed: with tests in
        // view it keeps its row, which claims none; the list that says "no static
        // test link" is for the answer that has no tests to say it against.
        let named = label(&sym.path, sym.symbol.as_deref(), &sym.lines);
        let in_a_test_file = crate::testfiles::is_test_path(Path::new(&sym.path));
        let witness = sym.coverage.all().witness();
        match witness.filter(|_| callerless && verdict.complete()) {
            Some(_) if in_a_test_file => {
                entry_unlisted.push(format!("{named} \u{2014} test (run by the test runner)"));
            }
            Some(complete) if !tests.any() => untested_unlisted.push(absence(named, complete)),
            _ => changed.push(sym),
        }
    }
    {
        let stops = state.run.stops();
        for core in unreached {
            let hunks = hunks_in(core);
            let skipped = query.is_none()
                && (bordering.contains(&core.id)
                    || (!hunks.is_empty()
                        && hunks
                            .iter()
                            .all(|h| h.layout || moved_verbatim(h, &state.hunk_texts))));
            if !skipped {
                changed.push(unfinished_symbol(
                    rel(state, core.path()),
                    core.symbol_name.clone(),
                    lines_of(&core.id),
                    format!("{:?}", core.kind).to_lowercase(),
                    &stops,
                ));
            }
        }
        // A run already stopped when the callers phase began may have lost
        // fragments before it: a changed code file with no definition in
        // view is not known to have none.
        if stopped_early && query.is_none() {
            let covered: FxHashSet<String> = cores.iter().map(|c| rel(state, c.path())).collect();
            for path in state
                .changed_files
                .iter()
                .map(|p| rel(state, &p.to_string_lossy()))
                .filter(|p| is_code_file(p) && !covered.contains(p))
            {
                changed.push(unfinished_symbol(
                    path,
                    None,
                    String::new(),
                    "file".to_string(),
                    &stops,
                ));
            }
        }
    }
    let removed = crate::profile::phase("impact.removed");
    if query.is_none() && state.run.check() {
        let mut defined: FxHashMap<&str, FxHashSet<String>> = FxHashMap::default();
        for (n, p) in &defined_in {
            defined.entry(n.as_str()).or_default().insert(rel(state, p));
        }
        let head_rev = analysed
            .map(crate::snapshot::split_diff_range)
            .and_then(|(_, head)| head);
        // The facts layer answers for the files of the cells' languages
        // (`cell_removed`).
        let read_off_facts = |path: &str| cells.of(path).is_some();
        let mut listed_alone: FxHashSet<(String, String)> = FxHashSet::default();
        let mut from_facts: Vec<(ChangedSymbol, Option<Contract>)> = Vec::new();
        // A removed name is listed with its readers, or with what could not be
        // told, or alone (#699). A removed public definition is a contract that
        // moved, listed or not.
        from_facts.extend(cell_removed.iter().filter_map(|entry| {
            let verdict = cells.of(&entry.file)?.verdict(entry);
            let mut sym = cell_symbol(entry, &verdict, "removed".to_string());
            // A removed override was reached through the member it overrode: the
            // answer names that route, whatever else it could not tell.
            if let Some(via) = verdict.overridden_via() {
                if matches!(sym.unresolved, None | Some(Unknown::Uncovered { .. })) {
                    sym.unresolved = Some(Unknown::Overridable {
                        via: via.to_string(),
                    });
                }
            }
            // Every removed definition of a cell language is reported, private ones
            // too: with the readers the head still has, or as one nothing references.
            listed_alone.insert((sym.path.clone(), entry.display_name()));
            let contract = entry.public.then(|| Contract {
                path: sym.path.clone(),
                symbol: entry.symbol(),
                kind: "removed_api",
            });
            Some((sym, contract))
        }));
        for (sym, contract) in removed_definitions(
            state,
            &defined,
            head_rev.as_deref(),
            &in_a_hunk,
            &read_off_facts,
        )
        .into_iter()
        .chain(from_facts)
        {
            let mut sym = sym;
            // A caller of a removed definition is tested the way any caller
            // is: a test that reaches it, or the test file it sits in (#413).
            for c in &mut sym.callers {
                let Some(site) = c.sites.first().copied() else {
                    continue;
                };
                let Some(frag) = state
                    .all_fragments
                    .iter()
                    .filter(|f| {
                        f.symbol_name == c.symbol
                            && f.start_line() <= site
                            && f.end_line() >= site
                            && rel(state, f.path()) == c.path
                    })
                    .min_by_key(|f| f.line_count())
                else {
                    continue;
                };
                let tested = tests.tested_by(frag, state, &by_id, &link);
                c.file_tested = tested.is_empty() && tests.file_tested(frag, &reaches_file);
                c.tests_open = tested.is_empty() && tests.open_above(frag, &link, true);
                c.tests = tested;
            }
            if let Some(c) =
                contract.filter(|c| !crate::testfiles::is_test_path(Path::new(&c.path)))
            {
                contracts.push(c);
            }
            // Unresolved is an answer too: dropped, it read as no caller.
            let alone = sym
                .symbol
                .as_ref()
                .is_some_and(|name| listed_alone.contains(&(sym.path.clone(), name.clone())));
            if !sym.callers.is_empty() || sym.unresolved.is_some() || alone {
                changed.push(sym);
            }
        }
    }
    drop(removed);
    // Symbols with something to say first: callers, then commit overlap.
    changed.sort_by(|a, b| {
        b.callers
            .len()
            .cmp(&a.callers.len())
            .then_with(|| b.commits.cmp(&a.commits))
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.lines.cmp(&b.lines))
    });

    let changed_files: Vec<String> = state
        .changed_files
        .iter()
        .map(|p| rel(state, &p.to_string_lossy()))
        .collect();
    let layout_files: FxHashSet<&str> = state
        .change_classes
        .iter()
        .filter(|(_, class, _)| *class == crate::change_class::ChangeClass::Layout)
        .map(|(p, _, _)| p.as_str())
        .collect();
    // A queried symbol changes nothing: the file holding its definition is
    // no contract that moved.
    for path in changed_files.iter().filter(|_| query.is_none()) {
        if is_schema_file(path) && !layout_files.contains(path.as_str()) {
            contracts.push(Contract {
                path: path.clone(),
                symbol: None,
                kind: "schema",
            });
        }
    }

    public_api.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.path.cmp(&b.1.path)));
    contracts.extend(public_api.into_iter().map(|(_, c)| c));
    let contracts_omitted = contracts.len().saturating_sub(MAX_CONTRACTS);
    contracts.truncate(MAX_CONTRACTS);
    // Pack's rule, not a list of our own: a run-wide stop, or a per-file
    // limit on a changed file. The graph's per-node cap is routine and stays
    // out; the callers it cut an edge from are recovered per definition.
    let changed_set: FxHashSet<&str> = changed_files.iter().map(String::as_str).collect();
    let untracked: FxHashSet<String> = state
        .untracked_files
        .iter()
        .map(|p| rel(state, &p.to_string_lossy()))
        .collect();
    let (stale, stale_omitted) = if query.is_none() {
        let _stale = crate::profile::phase("impact.stale_paths");
        stale_references(state, analysed)
    } else {
        (Vec::new(), 0)
    };
    let (deleted_modules, changed) = if query.is_none() {
        dead_modules(state, &cells, changed, &stale)
    } else {
        (Vec::new(), changed)
    };
    let (named_by, named_by_omitted) = if query.is_none() {
        let _named = crate::profile::phase("impact.named_by");
        let (mut named, omitted) = named_by_path(state, &cells, analysed);
        let by_module = untraced.module_refs(state, &cores, &named);
        named.extend(by_module);
        (named, omitted)
    } else {
        (Vec::new(), 0)
    };
    let (collection_reads, collection_reads_omitted) = if query.is_none() {
        read_by_collection(state, analysed)
    } else {
        (Vec::new(), 0)
    };
    let (mut unanalysed, cell_read_configs, consumed_outside, lost_configs) = if query.is_none() {
        // A file the run did not analyse whole has its own reason, which already
        // says its effect is unknown: it is never listed twice. The rest come
        // first, so that the cap on the list keeps them.
        let mut others = unanalysed_files(state, &changed_files);
        // A base side over the size cap is never parsed: its removals go unread,
        // and the file is named at the path the answer reports (the head's, for
        // a rename), whether it changed, was renamed or was deleted.
        let old_path_of: FxHashMap<&str, &str> = state
            .renamed_files
            .iter()
            .map(|(old, new)| (new.as_str(), old.as_str()))
            .collect();
        let sides = changed_files
            .iter()
            .map(|p| {
                (
                    old_path_of.get(p.as_str()).copied().unwrap_or(p.as_str()),
                    p.as_str(),
                )
            })
            .chain(state.deleted_files.iter().map(|p| (p.as_str(), p.as_str())));
        for (old, new) in sides {
            let over = |path: &str| {
                cells.reading(path).and_then(|cell| cell.loss_about(path))
                    == Some(decide::Reason::TooLarge)
            };
            if (over(old) || over(new)) && !others.iter().any(|f| f.path == new) {
                others.push(UnanalysedFile {
                    path: new.to_string(),
                    language: crate::languages::get_language_for_file(new),
                    reason: "too_large",
                    lines: Vec::new(),
                });
            }
        }
        // A changed file a cell read whole is traced: what it holds outside every
        // definition is `outside_definitions`, and nothing else of it is untraced.
        // What the code reads of its keys is no cell's answer yet: the key search
        // still runs for it, as it did before the cell read it.
        let cell_read: Vec<String> = others
            .iter()
            .filter(|f| {
                f.reason == "content_untraced"
                    && cells.of(&f.path).is_some_and(|cell| cell.is_bound(&f.path))
            })
            .map(|f| f.path.clone())
            .collect();
        // The cell traces what is inside the files; what builds or applies them
        // lies outside, and a mention or a reader in another file is all that
        // reaches it. Such a file is a gap again unless one is found below.
        let consumed_outside: Vec<UnanalysedFile> = others
            .iter()
            .filter(|f| {
                cell_read.contains(&f.path)
                    && cells
                        .of(&f.path)
                        .is_some_and(decide::Cell::consumed_outside)
            })
            .cloned()
            .collect();
        others.retain(|f| !cell_read.contains(&f.path));
        // A file its cell lost, in part or whole, is not made traced by the key
        // search that still runs for it: its loss is what the answer says of it.
        let mut lost_configs: Vec<String> = Vec::new();
        for f in &mut others {
            let lost = (f.reason == "content_untraced")
                .then(|| {
                    cells
                        .reading(&f.path)
                        .and_then(|cell| cell.loss_about(&f.path))
                })
                .flatten();
            if let Some(reason) = lost {
                f.reason = reason.row_reason();
                lost_configs.push(f.path.clone());
            }
        }
        let mut all: Vec<UnanalysedFile> = outside_definitions(&cells)
            .into_iter()
            .filter(|o| !others.iter().any(|f| f.path == o.path))
            .collect();
        all.extend(others);
        (all, cell_read, consumed_outside, lost_configs)
    } else {
        (Vec::new(), Vec::new(), Vec::new(), Vec::new())
    };
    let mut named_by = named_by;
    let mut named_by_omitted = named_by_omitted;
    let (key_searches, key_files_unsearched) = {
        let untraced: Vec<String> = unanalysed
            .iter()
            .filter(|f| f.reason == "content_untraced")
            .map(|f| f.path.clone())
            .chain(cell_read_configs)
            .chain(lost_configs.iter().cloned())
            .collect();
        let (refs, omitted, searches, unsearched, unread) =
            key_references(state, &cells, &untraced);
        unanalysed.extend(unread);
        unanalysed.retain(|f| {
            lost_configs.contains(&f.path) || !searches.iter().any(|k| k.path == f.path)
        });
        named_by.extend(refs);
        named_by_omitted += omitted;
        (searches, unsearched)
    };
    // A config or a Dockerfile nothing names stays a gap: its builds are not
    // traced, and a clean zero would claim they are.
    for file in consumed_outside {
        let read = named_by.iter().any(|r| r.target == file.path)
            || changed
                .iter()
                .any(|c| c.path == file.path && c.callers.iter().any(|k| k.path != file.path));
        if !read {
            unanalysed.push(file);
        }
    }
    // Read after the last phase that can stop: a deadline the stale-reference
    // search hit was missing from an answer that had dropped its lines. A
    // changed file outside code holds no definition and calls nothing: one
    // over the cap is named in `unanalysed`, and no caller is missing for it
    // (#626).
    let (mut limits, _, _) = crate::resource::classify_limits(&state.run, &[], &|p| {
        changed_set.contains(p) && (is_code_file(p) || Path::new(p).extension().is_none())
    });
    // A changed file cut to the fragment cap keeps every fragment the diff
    // touched: pack loses context there, impact loses no changed definition,
    // and the cap fires on most large files (#306).
    limits.retain(|r| *r != crate::resource::LimitReason::FragmentLimit);
    let mut output = ImpactOutput {
        name: state
            .root_dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| state.root_dir.to_string_lossy().to_string()),
        range: diff_range.map(str::to_string),
        index_tree: state.index_tree.clone(),
        range_name: None,
        symbol: query.map(str::to_string),
        symbol_named: None,
        untracked_files: changed_files
            .iter()
            .filter(|f| untracked.contains(f.as_str()))
            .cloned()
            .collect(),
        changed_files: changed_files
            .into_iter()
            .filter(|f| !untracked.contains(f.as_str()))
            .collect(),
        deleted_files: state.deleted_files.clone(),
        stale_references: stale,
        stale_references_omitted: stale_omitted,
        deleted_modules,
        deleted_modules_omitted: 0,
        named_by,
        named_by_omitted,
        key_searches,
        key_files_unsearched,
        collection_reads,
        collection_reads_omitted,
        commit_count: state.commit_count,
        changed,
        contracts,
        empty: false,
        truncated: 0,
        truncated_possible: 0,
        needs: 0,
        truncated_untested: 0,
        truncated_textual: 0,
        changed_files_omitted: 0,
        untracked_files_omitted: 0,
        contracts_omitted,
        limits,
        tests_known: tests.any(),
        untested_definitions,
        untested_unlisted,
        untested_called_in_diff,
        entry_unlisted,
        layout_only,
        lockfile_changes: state.lockfile_changes.clone(),
        layout_only_omitted: 0,
        traced_definitions,
        withheld_files: state.policy_excluded_count + state.run.withheld_count(),
        unread_directories: state.run.source().unread_directories(),
        unanalysed_files: unanalysed,
        unanalysed_files_omitted: 0,
        untraced_omitted: 0,
        unfinished_omitted: 0,
        run: Some(state.run.clone()),
        cap: IMPACT_TOKEN_CAP,
        budget_needed: None,
    };
    output.empty = proves_empty(&output);
    output
}

/// Definitions the change removed and nothing at the head defines again,
/// with the lines outside the diff that still name them (#367). They have
/// no fragment at the head, so the callers graph never reaches them: an
/// optional call (`engine.setGain?.()`) silently becomes a no-op.
/// Kotlin declarations the range removed: in the base revision of a changed
/// file and gone from it at the head, matched by owner, name, kind and
/// receiver. What still uses one is found by the binding model, at the head.
/// A read the answer depends on, or nothing with the run marked: a failed
/// `git grep` read as no match turned callers and stale references into a
/// clean zero with exit 0.
fn read_or_note<T: Default>(state: &ScoredState, read: crate::snapshot::Result<T>) -> T {
    read.unwrap_or_else(|e| {
        tracing::warn!("impact: a git read failed ({e}); its answer is unknown");
        state.run.note(crate::resource::LimitReason::ReadFailed);
        T::default()
    })
}

/// The named definitions of each file in view, by repo-relative path: what
/// holds a line that names a removed definition. Looked up per line, the
/// path of every fragment was rebuilt for every grep hit (#424).
fn definition_holders(state: &ScoredState) -> FxHashMap<String, Vec<&Fragment>> {
    let mut holders: FxHashMap<String, Vec<&Fragment>> = FxHashMap::default();
    for f in state
        .all_fragments
        .iter()
        .filter(|f| f.kind.is_definition_kind() && f.symbol_name.is_some())
    {
        holders.entry(rel(state, f.path())).or_default().push(f);
    }
    holders
}

fn removed_definitions(
    state: &ScoredState,
    defined: &FxHashMap<&str, FxHashSet<String>>,
    head: Option<&str>,
    in_a_hunk: &dyn Fn(&FragmentId) -> bool,
    read_off_facts: &dyn Fn(&str) -> bool,
) -> Vec<(ChangedSymbol, Option<Contract>)> {
    // Read off the grammar, the file at the base against the file at the
    // head: a removed definition is one the base declared and the head does
    // not. Read off removed lines by regex, a match arm became a removed
    // member named `Err`, a call statement a removed API, and an indented
    // line of a free function a member of the nearest class above it (#515).
    let old_path_of: FxHashMap<&str, &str> = state
        .renamed_files
        .iter()
        .map(|(old, new)| (new.as_str(), old.as_str()))
        .collect();
    let mut seen: FxHashSet<&str> = FxHashSet::default();
    // (name, path, its path and lines at the base, exported, its class or
    // interface)
    let mut removed: Vec<(String, String, (String, String), bool, Option<String>)> = Vec::new();
    for h in &state.hunk_texts {
        // The facts answer for the files of the cells' languages.
        if h.removed.is_empty()
            || !is_code_file(&h.path)
            || read_off_facts(&h.path)
            || !seen.insert(h.path.as_str())
        {
            continue;
        }
        let old_path = old_path_of.get(h.path.as_str()).copied().unwrap_or(&h.path);
        let before = read_or_note(
            state,
            state
                .run
                .source()
                .base()
                .read_lossy(&state.root_dir.join(old_path))
                // A file with no text at the base is new there.
                .map(|text| text.into_iter().collect::<String>()),
        );
        // A read that failed is noted on the run by the source itself.
        let after = state
            .run
            .source()
            .read_to_string(&state.root_dir.join(&h.path))
            .into_iter()
            .collect::<String>();
        let (Some(at_base), Some(at_head)) = (
            crate::parsers::declarations(old_path, &before),
            crate::parsers::declarations(&h.path, &after),
        ) else {
            // Declarations the grammar could not read in time (a parse past
            // its budget on a large file) are definitions nobody can name as
            // removed: the answer says so, never "nothing removed".
            if crate::parsers::has_grammar(old_path) {
                state.run.note_file(
                    crate::resource::LimitReason::ReadFailed,
                    old_path.to_string(),
                );
            }
            continue;
        };
        let still: FxHashSet<&str> = at_head.iter().map(|d| d.name.as_str()).collect();
        let public = |d: &crate::parsers::Declaration| {
            let head = d.head.trim_start();
            is_public(old_path, Some(&d.name), head)
                || d.container.is_some_and(|c| {
                    !head.starts_with("private")
                        && !head.starts_with("protected")
                        && !head.starts_with('#')
                        && is_public(
                            old_path,
                            Some(&at_base[c].name),
                            at_base[c].head.trim_start(),
                        )
                })
        };
        for d in &at_base {
            if d.name.len() < 3
                || still.contains(d.name.as_str())
                || removed.iter().any(|(n, _, _, _, _)| *n == d.name)
            {
                continue;
            }
            removed.push((
                d.name.clone(),
                h.path.clone(),
                (
                    old_path.to_string(),
                    format!("{}-{}", d.start_line, d.end_line),
                ),
                public(d),
                d.container.map(|c| at_base[c].name.clone()),
            ));
        }
    }
    // One grep for every removed name, its lines handed to the names they
    // spell as a word: a deleted 1000-function module ran 1000 greps and
    // outlived the deadline.
    let names: Vec<String> = removed.iter().map(|(n, _, _, _, _)| n.clone()).collect();
    let wanted: FxHashSet<&str> = names.iter().map(String::as_str).collect();
    let mut hits_of: FxHashMap<&str, Vec<(String, u32, String)>> = FxHashMap::default();
    let grepped = read_or_note(
        state,
        crate::grep::grep_code_words(
            &state.root_dir,
            &state.run.source(),
            &names,
            head,
            MAX_REMOVED_HITS_PER_FILE,
        ),
    );
    state.run.note_withheld(grepped.withheld);
    for (path, line, text) in grepped.rows {
        let spelled: FxHashSet<&str> = text
            .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
            .filter_map(|w| wanted.get(w).copied())
            .collect();
        for name in spelled {
            hits_of
                .entry(name)
                .or_default()
                .push((path.clone(), line, text.clone()));
        }
    }
    let holders = definition_holders(state);
    let mut out = Vec::new();
    for (name, path_removed, (path_at_base, lines), exported, _) in removed {
        if !state.run.check() {
            break;
        }
        let hits: Vec<_> = hits_of
            .remove(name.as_str())
            .into_iter()
            .flatten()
            .collect();
        let mut callers: Vec<Caller> = Vec::new();
        // A name on this many lines is a common word, not a reference to the
        // removed definition: tracing it by name lists noise and, on a
        // monorepo, never finishes.
        let unresolved = (hits.len() > MAX_REMOVED_NAME_HITS).then(|| Unknown::CommonWord {
            name: name.to_string(),
            lines: hits.len(),
        });
        let hits = if unresolved.is_some() {
            Vec::new()
        } else {
            hits
        };
        // Defined again elsewhere: only the lines still bound to the old
        // module are broken; the rest already reach the new definition.
        // Without a binding model only a line qualified by the new module
        // (`new::tax_rate`) is known to reach it; dropping every other line
        // read a still-broken `old::tax_rate()` as fixed.
        let moved_to = defined.get(name.as_str()).filter(|at| !at.is_empty());
        let qualified_by = |text: &str, file: &str| {
            let stem = Path::new(file)
                .file_stem()
                .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
            text.contains(&format!("{stem}::{name}")) || text.contains(&format!("{stem}.{name}"))
        };
        for (path, line, text) in hits {
            if !is_code_file(&path) {
                continue;
            }
            // A caller is code that can still reach the removed definition:
            // through an import the binding model resolves to the file it
            // left, else at least from the same runtime. By name alone,
            // `subprocess.run`, another file's own `run`, a shell `die` and a
            // TypeScript `test.describe` were callers (#470, #471, #458).
            let modelled = reads_imports(&path) && reads_imports(&path_removed);
            // No model binds a name across files: of the files a removed
            // definition's language reads, only its own lines are known readers,
            // and a row no model checked is a word match (R1).
            if modelled {
                if path != path_removed {
                    continue;
                }
            } else if runtime_of(&path) != runtime_of(&path_removed)
                // A file that declares the name itself calls its own.
                || holders.get(&path).is_some_and(|fs| {
                    fs.iter().any(|f| f.symbol_name.as_deref() == Some(name.as_str()))
                })
            {
                continue;
            }
            if let Some(at) = moved_to {
                let reaches_new = if reads_imports(&path) {
                    true
                } else {
                    at.iter().any(|f| qualified_by(&text, f)) && !qualified_by(&text, &path_removed)
                };
                if reaches_new {
                    continue;
                }
            }
            let code = code_text(&path, &text);
            // The import is where the name was taken, not a use (#450).
            if is_import_line(&code) {
                continue;
            }
            let bytes = code.as_bytes();
            let Some(at) = code
                .match_indices(name.as_str())
                .map(|(at, _)| at)
                .find(|&at| {
                    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'$';
                    !at.checked_sub(1).is_some_and(|i| word(bytes[i]))
                        && !bytes.get(at + name.len()).copied().is_some_and(word)
                })
            else {
                continue;
            };
            let holder = holders
                .get(&path)
                .into_iter()
                .flatten()
                .filter(|f| f.start_line() <= line && f.end_line() >= line)
                .min_by_key(|f| f.line_count())
                .copied();
            // The line declaring the name is where it went (a move), never a
            // caller of the removed definition; a method of the same name
            // that delegates to it still is one.
            if holder.is_some_and(|f| {
                (in_a_hunk(&f.id) && f.line_count() <= 1)
                    || (f.symbol_name.as_deref() == Some(name.as_str()) && f.start_line() == line)
            }) {
                continue;
            }
            let next = code[at + name.len()..].trim_start();
            callers.push(Caller::at_line(
                path.clone(),
                holder.and_then(|f| f.symbol_name.clone()),
                holder.map_or_else(|| format!("{line}-{line}"), |f| lines_of(&f.id)),
                "name",
                if next.starts_with('(') || next.starts_with("?.(") {
                    "call"
                } else {
                    "reference"
                },
                line,
            ));
        }
        let callers = merge_by_symbol(callers);
        let contract = exported.then(|| Contract {
            path: path_at_base.clone(),
            symbol: Some(name.clone()),
            kind: "removed_api",
        });
        out.push((
            ChangedSymbol {
                callers,
                unresolved,
                ..ChangedSymbol::bare(
                    path_at_base,
                    Some(name),
                    lines,
                    "removed",
                    decide::Coverages::partial(decide::Gap::NotTraced),
                )
            },
            contract,
        ));
    }
    out
}

const PROJECT_MANIFESTS: &[&str] = &[
    "package.json",
    "pyproject.toml",
    "setup.py",
    "Cargo.toml",
    "go.mod",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "composer.json",
    "Gemfile",
    "mix.exs",
    "deno.json",
];

const MAX_STALE_REFERENCES: usize = 24;
const MAX_STALE_PER_TARGET: usize = 8;
/// What names a changed file is context, not breakage: fewer lines than
/// what still names a deleted one.
const MAX_NAMED_BY: usize = 12;
const MAX_NAMED_BY_PER_TARGET: usize = 3;

/// The modules the range deletes that no head file imports, named once instead of
/// definition by definition, and the changed list without their entries. A module
/// something imports keeps its per-definition listing, whose importers are stale
/// references. Only a module of a cell language is read this way: its imports are
/// the facts'. A computed import may reach it, so the claim says "none found"
/// whenever the facts cannot rule one out (`Cells::computed_imports_possible`).
fn dead_modules(
    state: &ScoredState,
    cells: &Cells,
    changed: Vec<ChangedSymbol>,
    stale: &[StaleReference],
) -> (Vec<DeletedModule>, Vec<ChangedSymbol>) {
    let mut dead: Vec<DeletedModule> = Vec::new();
    for file in &state.deleted_files {
        let held: Vec<&ChangedSymbol> = changed
            .iter()
            .filter(|c| c.kind == "removed" && &c.path == file)
            .collect();
        if held.is_empty()
            || cells.of(file).is_none()
            || held.iter().any(|c| !c.callers.is_empty())
            || stale.iter().any(|r| &r.target == file)
        {
            continue;
        }
        dead.push(DeletedModule {
            path: file.clone(),
            definitions: held.len(),
            dynamic_import: false,
        });
    }
    if dead.is_empty() {
        return (dead, changed);
    }
    let dynamic_import = cells.computed_imports_possible();
    for module in &mut dead {
        module.dynamic_import = dynamic_import;
    }
    let changed = changed
        .into_iter()
        .filter(|c| !(c.kind == "removed" && dead.iter().any(|m| m.path == c.path)))
        .collect();
    (dead, changed)
}

/// What a changed config's lines spell that can witness a reader: words
/// (`maxPoolSize`, `DATABASE_URL`, `max-connections`, `synchronization`,
/// never `replicas`) and values (`up{job="demo"}`, `ghcr.io/acme/web:1.2`).
/// Only a structured config has keys and values; a template's or a
/// stylesheet's words are prose and selectors.
fn changed_keys(state: &ScoredState, path: &str) -> Vec<String> {
    if !is_config_file(path) {
        return Vec::new();
    }
    let mut keys: Vec<String> = state
        .hunk_texts
        .iter()
        .filter(|h| h.path == path)
        .flat_map(|h| h.added.iter().chain(&h.removed))
        .filter(|l| !commented(l))
        .flat_map(|l| keys::terms(l))
        .collect();
    keys.sort();
    keys.dedup();
    keys
}

/// The readers the typed key table finds for what the change rewrote in the
/// changed configs `untraced`: (changed file, `route /v1/orders/{}`, the
/// untouched file naming the same key, its line). Where a key has a type (an
/// environment variable, an image, a route, a table), the join knows what a
/// textual search cannot: spellings differ (`{id}` and `<int:id>`), and only
/// compatible roles meet.
fn typed_readers(state: &ScoredState, untraced: &[String]) -> Vec<(String, String, String, u32)> {
    let mut rows: Vec<(String, String, String, u32)> = crate::keytable::counterparts_of_change(
        &state.all_fragments,
        Some(state.root_dir.as_path()),
        state.run.changed_lines(),
    )
    .into_iter()
    .map(|c| {
        (
            rel(state, &c.changed_path),
            format!("{} {}", c.kind, c.key),
            rel(state, &c.path),
            c.line.max(1) as u32,
        )
    })
    .filter(|(target, ..)| untraced.contains(target))
    .collect();
    rows.sort();
    rows.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1 && a.2 == b.2);
    rows
}

/// Terms searched for one changed config, and for all of them: `git grep`
/// matches every needle against every line, so a diff of hundreds of configs
/// (an epoch's corpus cases and workflows) turned a 10 s answer into 330 s
/// (Forgejo #109). The smallest changes are searched first, a file's longest
/// terms first; a config left out stays "not followed" and is counted.
const MAX_KEY_TERMS_PER_FILE: usize = 16;
const MAX_KEY_NEEDLES: usize = 96;

/// The lines outside the change that spell a key or a value the change
/// touched in a file outside code (#686, #609, Forgejo #109), and the files
/// the typed key table ties to it. The textual match is a search: a term too
/// common to witness a reader is not searched, a search past its caps leaves
/// the rest of the configs unsearched (the last count returned), and a file
/// none of whose terms could be searched stays `content_untraced`.
fn key_references(
    state: &ScoredState,
    cells: &Cells,
    untraced: &[String],
) -> (
    Vec<PathReference>,
    usize,
    Vec<KeySearch>,
    usize,
    Vec<UnanalysedFile>,
) {
    if !state.run.check() {
        return (Vec::new(), 0, Vec::new(), 0, Vec::new());
    }
    let typed = typed_readers(state, untraced);
    let mut candidates: Vec<(usize, &String, Vec<String>, Vec<String>)> = untraced
        .iter()
        .enumerate()
        .map(|(index, path)| {
            let readers: Vec<String> = typed
                .iter()
                .filter(|(target, ..)| target == path)
                .map(|(_, key, ..)| key.clone())
                .collect();
            (index, path, changed_keys(state, path), readers)
        })
        .filter(|(_, _, words, readers)| !words.is_empty() || !readers.is_empty())
        .collect();
    candidates.sort_by(|a, b| (a.2.len(), a.1).cmp(&(b.2.len(), b.1)));
    let mut searched: FxHashSet<String> = FxHashSet::default();
    let mut unsearched = 0;
    let mut chosen: Vec<(usize, String, Vec<String>)> = Vec::new();
    for (index, path, mut words, mut readers) in candidates {
        words.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
        words.truncate(MAX_KEY_TERMS_PER_FILE);
        let fresh = words.iter().filter(|w| !searched.contains(*w)).count();
        if searched.len() + fresh > MAX_KEY_NEEDLES {
            if readers.is_empty() {
                unsearched += 1;
                continue;
            }
            words.clear();
        }
        searched.extend(words.iter().cloned());
        words.append(&mut readers);
        words.sort();
        words.dedup();
        chosen.push((index, path.clone(), words));
    }
    chosen.sort_by_key(|(index, ..)| *index);
    let keys: Vec<(String, Vec<String>)> = chosen
        .into_iter()
        .map(|(_, path, terms)| (path, terms))
        .collect();
    let mut needles: Vec<String> = searched.into_iter().collect();
    needles.sort();
    if keys.is_empty() {
        return (Vec::new(), 0, Vec::new(), unsearched, Vec::new());
    }
    let spelled = cells.spelled_lines(state, &needles).unwrap_or_else(|e| {
        tracing::warn!(
            "impact: the head's files could not be searched ({e}); the answer is unknown"
        );
        state.run.note(crate::resource::LimitReason::ReadFailed);
        decide::Spelled::default()
    });
    let changed: FxHashSet<String> = state
        .changed_files
        .iter()
        .map(|p| rel(state, &p.to_string_lossy()))
        .collect();
    let source = state.run.source();
    // A file that could not be read or tokenised may spell a key and nothing says
    // it does not: the answer names it as unanalysed instead of staying silent.
    let unread: Vec<UnanalysedFile> = spelled
        .unread
        .iter()
        .filter(|path| {
            !changed.contains(*path) && class_of(&state.root_dir, &source, path, "", None).reads()
        })
        .map(|path| UnanalysedFile {
            path: path.clone(),
            language: crate::languages::get_language_for_file(path),
            reason: "too_large",
            lines: Vec::new(),
        })
        .collect();
    let wanted: FxHashSet<&str> = needles.iter().map(String::as_str).collect();
    let phrases: Vec<&String> = needles.iter().filter(|n| keys::is_phrase(n)).collect();
    let mut by_key: FxHashMap<String, Vec<(String, u32)>> = FxHashMap::default();
    for (path, line, text) in state.run.bounded(&spelled.rows) {
        if changed.contains(path) || !class_of(&state.root_dir, &source, path, text, None).reads() {
            continue;
        }
        let spelled = keys::key_words(text)
            .filter(|w| wanted.contains(w.as_str()))
            .chain(
                phrases
                    .iter()
                    .filter(|p| names_word(text, p.as_str()))
                    .map(|p| p.to_string()),
            );
        let mut seen: FxHashSet<String> = FxHashSet::default();
        for word in spelled {
            if seen.insert(word.clone()) {
                let rows = by_key.entry(word).or_default();
                if !rows.iter().any(|(p, _)| p == path) {
                    rows.push((path.clone(), *line));
                }
            }
        }
    }
    for (_, key, path, line) in &typed {
        let rows = by_key.entry(key.clone()).or_default();
        if !rows.iter().any(|(p, _)| p == path) {
            rows.push((path.clone(), *line));
        }
    }
    let mut refs: Vec<PathReference> = Vec::new();
    let mut searches: Vec<KeySearch> = Vec::new();
    let mut omitted = 0;
    let mut claimed: FxHashSet<&str> = FxHashSet::default();
    for (target, target_keys) in &keys {
        let searchable: Vec<&String> = target_keys
            .iter()
            .filter(|k| by_key.get(*k).map_or(0, Vec::len) <= MAX_UNTRACED_NAMERS)
            .collect();
        if searchable.is_empty() {
            continue;
        }
        let mut named = 0;
        let mut listed: FxHashSet<(String, u32)> = FxHashSet::default();
        for key in &searchable {
            if !claimed.insert(key.as_str()) {
                continue;
            }
            let mut rows = by_key.get(*key).map_or_else(Vec::new, |rows| rows.clone());
            rows.sort();
            for (path, line) in rows {
                if !listed.insert((path.clone(), line)) {
                    continue;
                }
                named += 1;
                if named > MAX_NAMED_BY_PER_TARGET || refs.len() >= MAX_NAMED_BY {
                    omitted += 1;
                    continue;
                }
                let class = class_of(&state.root_dir, &source, &path, "", None);
                refs.push(PathReference {
                    path,
                    line,
                    target: target.clone(),
                    key: Some((*key).clone()),
                    class,
                });
            }
        }
        searches.push(KeySearch {
            path: target.clone(),
            keys: searchable.into_iter().cloned().collect(),
            named,
        });
    }
    (refs, omitted, searches, unsearched, unread)
}

/// Astro's content API: a page reads the entries of a collection by its
/// name. The first argument is the name, or an object or array of them
/// with a `collection` key; anything else computes it.
static COLLECTION_CALL: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
    regex::Regex::new(
        r#"\b(?:getCollection|getEntry|getEntries|getEntryBySlug|getDataEntryById)\s*\(\s*(?:\[\s*)?(?:\{[^}]*?\bcollection\s*:\s*)?(?:'([^'$]*)'|"([^"$]*)"|`([^`$]*)`|([^\s)]))?"#,
    )
    .expect("collection call regex")
});

/// The lines outside the change that read the content collection of a
/// changed entry by name (#432). An entry under `src/content/<name>/` of a
/// directory holding an `astro.config.*` is read by `getCollection('<name>')`
/// and its kin, never imported or named by path, so its readers were a clean
/// zero. A call whose name is computed may read it and is listed so.
/// Read off the lines: an `.astro` page has no grammar here, and a file
/// that has one must hold the call in code, not in a string or comment.
fn read_by_collection(
    state: &ScoredState,
    diff_range: Option<&str>,
) -> (Vec<CollectionRead>, usize) {
    let paths = read_or_note(state, state.run.source().list_paths(&state.root_dir));
    let projects: Vec<&str> = paths
        .iter()
        .filter_map(|p| {
            let (dir, file) = p.rsplit_once('/').unwrap_or(("", p));
            file.starts_with("astro.config.").then_some(dir)
        })
        .collect();
    let content_of = |project: &str| {
        if project.is_empty() {
            "src/content/".to_string()
        } else {
            format!("{project}/src/content/")
        }
    };
    let project_of = |path: &str| {
        projects
            .iter()
            .copied()
            .filter(|p| p.is_empty() || path.starts_with(&format!("{p}/")))
            .max_by_key(|p| p.len())
    };
    let changed: FxHashSet<(&str, String)> = state
        .changed_files
        .iter()
        .map(|f| rel(state, &f.to_string_lossy()))
        .chain(state.deleted_files.iter().cloned())
        .filter_map(|path| {
            let project = project_of(&path)?;
            let (collection, _) = path.strip_prefix(&content_of(project))?.split_once('/')?;
            Some((project, collection.to_string()))
        })
        .collect();
    if changed.is_empty() {
        return (Vec::new(), 0);
    }
    let head = diff_range
        .map(crate::snapshot::split_diff_range)
        .and_then(|(_, head)| head);
    let grepped = read_or_note(
        state,
        crate::grep::grep_lines_matching(
            &state.root_dir,
            &state.run.source(),
            r"(getCollection|getEntry|getEntries|getEntryBySlug|getDataEntryById)[[:space:]]*\(",
            head.as_deref(),
            &[],
        ),
    );
    let written: FxHashSet<(&str, &str)> = state
        .hunk_texts
        .iter()
        .flat_map(|h| h.added.iter().map(move |l| (h.path.as_str(), l.trim())))
        .collect();
    let source = state.run.source();
    let mut texts: FxHashMap<&str, Option<String>> = FxHashMap::default();
    let (mut found, mut omitted) = (Vec::new(), 0);
    for (path, line, text) in state.run.bounded(&grepped.rows) {
        let Some(project) = project_of(path) else {
            continue;
        };
        if is_document(path) || commented(text) || written.contains(&(path.as_str(), text.trim())) {
            continue;
        }
        let Some(file) = texts
            .entry(path)
            .or_insert_with(|| source.read_to_string(&state.root_dir.join(path)))
        else {
            continue;
        };
        let lines: Vec<&str> = file.lines().collect();
        let code = code_text(path, file);
        let code_line = code.lines().nth(*line as usize - 1).unwrap_or("");
        // A call split over lines names its collection on the next one.
        let call = match lines.get(*line as usize) {
            Some(next) => format!("{text} {next}"),
            None => text.clone(),
        };
        for m in COLLECTION_CALL.captures_iter(&call) {
            // A call in a string or a comment reads nothing, and one on the
            // next line is that line's own.
            let at = m.get(0).map_or(0, |m| m.start());
            if at >= text.len() || code_line.get(at..at + 3) != text.get(at..at + 3) {
                continue;
            }
            let named = (1..=3)
                .find_map(|i| m.get(i))
                .map(|n| n.as_str().to_string());
            let reads = match &named {
                Some(c) => changed.contains(&(project, c.clone())),
                None => m.get(4).is_some() && changed.iter().any(|(p, _)| *p == project),
            };
            if !reads {
                continue;
            }
            if found.len() >= MAX_NAMED_BY {
                omitted += 1;
                continue;
            }
            found.push(CollectionRead {
                path: path.clone(),
                line: *line,
                collection: named,
            });
        }
    }
    (found, omitted)
}

/// The answer as JSON: `diffctx.impact.v2`, built from the answer the builders made.
fn v2_json(output: &ImpactOutput) -> String {
    serde_json::to_string(&v2_of(output))
        .unwrap_or_else(|e| format!(r#"{{"error":"the answer could not be serialized: {e}"}}"#))
}

/// The v2 document of an answer: what its text and its JSON both print.
pub(crate) fn v2_of(output: &ImpactOutput) -> crate::answer::v2::ImpactV2 {
    crate::answer::v2::ImpactV2::from_v1(output, MAX_TEST_HOPS)
}

/// Drops word matches, then possible callers, then the rows dealt last among
/// the (definition, relation kind) groups of the rest (`last_dealt`), until
/// `measure` fits `cap`, then symbols that have nothing left to say. The cap
/// is on what the reader receives, so it is measured on the delivered text.
fn fit_with(output: &mut ImpactOutput, cap: u32, measure: &dyn Fn(&ImpactOutput) -> u32) {
    output.cap = cap;
    // `empty` is what the text prints a short answer for: it holds for the answer
    // each step measures, as it will for the one delivered.
    output.empty = proves_empty(output);
    // What showing everything would take, to the next hundred tokens.
    output.needs = measure(output).div_ceil(100) * 100;
    crate::fit::shed_to_fit(output, cap, measure);
}

impl crate::fit::Shed for ImpactOutput {
    fn shed_one(&mut self) -> bool {
        shed_one(self)
    }

    fn settle(&mut self) -> bool {
        let stopped = self.run.clone().filter(|run| !run.check());
        for reason in stopped
            .iter()
            .flat_map(|run| crate::resource::classify_limits(run, &[], &|_| false).0)
        {
            if let Err(at) = self.limits.binary_search(&reason) {
                self.limits.insert(at, reason);
            }
        }
        self.empty = proves_empty(self);
        stopped.is_some()
    }
}

/// The one rule for `empty` (#505), read off the answer's v2 document: what its
/// text and its JSON both stand on.
fn proves_empty(output: &ImpactOutput) -> bool {
    let doc = v2_of(output);
    Answer::of(&doc, &RenderCtx::default()).proves_empty()
}

/// The hook's one line for the case where the only thing keeping an answer from
/// being `empty` is files the run withheld; `None` for any other.
pub fn unread_line(output: &ImpactOutput, range: &str) -> Option<String> {
    let doc = v2_of(output);
    Answer::of(&doc, &RenderCtx::default())
        .withheld_only()
        .then(|| {
            format!(
                "diffctx: {range} reaches nothing outside the diff in the files it read; not analysed: {} (what they name is unknown).",
                decide::Gap::Withheld {
                    files: doc.inventory.withheld
                }
            )
        })
}

/// One step of shedding, least needed first; `false` once nothing more can
/// go. Callers are the answer, so everything else yields to them first, and
/// the file list — which the reader holds as the diff — first of all.
fn shed_one(output: &mut ImpactOutput) -> bool {
    if output.untracked_files.len() > MAX_CHANGED_FILES_LISTED {
        output.untracked_files_omitted += output.untracked_files.len() - MAX_CHANGED_FILES_LISTED;
        output.untracked_files.truncate(MAX_CHANGED_FILES_LISTED);
    }
    if output.changed_files.len() > MAX_CHANGED_FILES_LISTED {
        output.changed_files_omitted += output.changed_files.len() - MAX_CHANGED_FILES_LISTED;
        output.changed_files.truncate(MAX_CHANGED_FILES_LISTED);
        return true;
    }
    // What names a changed file by path is context: it goes before any
    // caller, a possible one included, and its count line goes with it.
    if output.named_by.pop().is_some() {
        output.named_by_omitted += 1;
        return true;
    }
    if output.named_by_omitted > 0 {
        output.named_by_omitted = 0;
        return true;
    }
    if output.deleted_modules.pop().is_some() {
        output.deleted_modules_omitted += 1;
        return true;
    }
    if output.layout_only.len() > MAX_UNTESTED_NAMED {
        output.layout_only_omitted += output.layout_only.len() - MAX_UNTESTED_NAMED;
        output.layout_only.truncate(MAX_UNTESTED_NAMED);
        return true;
    }
    if output.unanalysed_files.len() > MAX_UNANALYSED_LISTED {
        output.unanalysed_files_omitted += output.unanalysed_files.len() - MAX_UNANALYSED_LISTED;
        output.untraced_omitted += output.unanalysed_files[MAX_UNANALYSED_LISTED..]
            .iter()
            .filter(|f| f.reason == "content_untraced")
            .count();
        output.unanalysed_files.truncate(MAX_UNANALYSED_LISTED);
        return true;
    }
    // The definitions of test files rank last among the targets: they go first,
    // whole, into the entry that names what the cap shed.
    if let Some(i) = output.changed.iter().rposition(|c| {
        !is_omitted(c) && crate::testfiles::is_test_path(Path::new(&c.path)) && !is_unfinished(c)
    }) {
        shed_definition(output, i);
        return true;
    }
    // A word match is the weakest row there is: it goes before a guess.
    if let Some((i, r)) = last_dealt(&output.changed, |_, row| is_textual(row)) {
        let row = output.changed[i].callers.remove(r);
        output.changed[i].shed.add(&row);
        output.changed[i].callers_omitted += 1;
        output.truncated += 1;
        output.truncated_textual += 1;
        return true;
    }
    // A possible caller is a guess; a changed contract is a fact (#413).
    if let Some((i, r)) = last_dealt(&output.changed, |_, row| row.evidence == "candidate") {
        let row = output.changed[i].callers.remove(r);
        output.changed[i].shed.add(&row);
        output.changed[i].callers_omitted += 1;
        output.truncated += 1;
        output.truncated_possible += 1;
        return true;
    }
    if output.contracts.pop().is_some() {
        output.contracts_omitted += 1;
        return true;
    }
    // A symbol whose callers could not be resolved says so to the last:
    // dropping it would turn "unknown" into "nothing".
    if let Some(i) = output
        .changed
        .iter()
        .rposition(|c| c.callers.is_empty() && c.unresolved.is_none() && c.compatible_callers == 0)
    {
        omit(output, i);
        return true;
    }
    // A definition the run never searched goes into a count the header keeps.
    if let Some(i) = output
        .changed
        .iter()
        .rposition(|c| c.callers.is_empty() && c.compatible_callers == 0 && is_unfinished(c))
    {
        output.changed.remove(i);
        output.unfinished_omitted += 1;
        return true;
    }
    if shed_weakest_caller(output) {
        return true;
    }
    // When even the entry outgrows the cap, its list gives up the names with the
    // fewest callers; the count and the callers stay whole, and the first name,
    // the definition with the most callers, stays: the least the answer can say.
    if let Some(entry) = output.changed.iter_mut().find(|c| is_omitted(c)) {
        if entry.omitted.len() > 1 && entry.omitted.pop().is_some() {
            return true;
        }
    }
    // A file still naming a deleted or renamed path is a defect, not a
    // caller to check: it outlives every caller row, first entries kept
    // (#600).
    if output.stale_references.pop().is_some() {
        output.stale_references_omitted += 1;
        return true;
    }
    // Last, what the counts already say: the header counts the definitions no
    // test reaches, the omitted entry its definitions and callers, so their
    // names go only when nothing else can, and the answer still fits.
    if output.untested_called_in_diff.pop().is_some() || output.untested_unlisted.pop().is_some() {
        return true;
    }
    // The first shed name outranks a contracts line that only counts: which
    // definition the answer is silent about is worth more than "and N more".
    // Without an entry to carry a name the count is all there is to say.
    if output.contracts.is_empty()
        && output.contracts_omitted > 0
        && output.changed.iter().any(is_omitted)
    {
        output.contracts_omitted = 0;
        return true;
    }
    false
}

/// The shed definitions the omitted entry names; the rest are counted.
const MAX_OMITTED_NAMED: usize = 40;

/// A shed symbol is not dropped: it joins the one entry that names the
/// definitions the cap left out, with the callers it had, and what shows them.
/// Dropped silently it would read as a definition nothing calls.
fn omit(output: &mut ImpactOutput, i: usize) {
    let needs = output.needs;
    let sym = output.changed.remove(i);
    let incomplete = sym.coverage.calls().witness().is_none();
    let gone = OmittedDefinition {
        definition: match &sym.symbol {
            Some(name) => format!("{}::{name}", sym.path),
            None => sym.path.clone(),
        },
        lines: sym.lines.clone(),
        callers_omitted: sym.callers.len() + sym.callers_omitted,
        shed: {
            let mut shed = sym.shed;
            for row in &sym.callers {
                shed.add(row);
            }
            shed
        },
        incomplete,
    };
    // The rows it folded are counted with it: the header still names them.
    let folded = sym.folded;
    match output.changed.iter_mut().find(|c| is_omitted(c)) {
        Some(entry) => {
            entry.folded.extend(folded);
            if let Some(Unknown::Omitted {
                count,
                incomplete: unsearched,
                ..
            }) = &mut entry.unresolved
            {
                *count += 1;
                *unsearched += usize::from(gone.incomplete);
            }
            entry.callers_omitted += gone.callers_omitted;
            entry.shed.merge(gone.shed);
            entry.omitted.push(gone);
            entry.omitted.sort_by(|a, b| {
                b.callers_omitted
                    .cmp(&a.callers_omitted)
                    .then_with(|| a.definition.cmp(&b.definition))
            });
            // The count stays whole; the names that fit are the ones with the
            // most callers, so a module of thirty thousand removals does not
            // outgrow the cap with its own list.
            entry.omitted.truncate(MAX_OMITTED_NAMED);
        }
        None => output.changed.push(ChangedSymbol {
            unresolved: Some(Unknown::Omitted {
                needs,
                count: 1,
                incomplete: usize::from(gone.incomplete),
            }),
            shed: gone.shed,
            callers_omitted: gone.callers_omitted,
            omitted: vec![gone],
            folded,
            ..ChangedSymbol::bare(
                "(changed definitions)".to_string(),
                None,
                String::new(),
                "omitted",
                decide::Coverages::partial(decide::Gap::Capped),
            )
        }),
    }
}

/// The order a symbol's relation kinds are dealt in and listed.
fn relation_rank(relation: &str) -> u8 {
    match relation {
        "call" => 0,
        "implementation" => 1,
        "reference" => 2,
        "reexports" => 3,
        _ => 4,
    }
}

/// The row the cap sheds next among those `accepts` offers, as (symbol, row).
/// Rows are dealt out in rounds over (changed definition, relation kind): each
/// round gives every group that still has rows its next-strongest one, in the
/// order the definitions are listed, and the row dealt last goes first. So the
/// strongest row of every kind every definition has outlasts all the others,
/// no kind is shed before another, and a larger budget shows a superset of a
/// smaller one's rows: the answer is a prefix of one fixed order.
fn last_dealt(
    changed: &[ChangedSymbol],
    accepts: impl Fn(&ChangedSymbol, &Caller) -> bool,
) -> Option<(usize, usize)> {
    let mut last: Option<((usize, usize, u8), (usize, usize))> = None;
    for (i, c) in changed.iter().enumerate() {
        let mut dealt: Vec<(&str, usize)> = Vec::new();
        for (r, row) in c
            .callers
            .iter()
            .enumerate()
            .filter(|(_, row)| accepts(c, row))
        {
            let round = match dealt.iter_mut().find(|(kind, _)| *kind == row.relation) {
                Some((_, rows)) => {
                    *rows += 1;
                    *rows
                }
                None => {
                    dealt.push((row.relation, 0));
                    0
                }
            };
            let order = (round, i, relation_rank(row.relation));
            if last.as_ref().is_none_or(|(latest, _)| order > *latest) {
                last = Some((order, (i, r)));
            }
        }
    }
    last.map(|(_, at)| at)
}

fn shed_weakest_caller(output: &mut ImpactOutput) -> bool {
    // A definition that keeps another row gives up the one dealt last: the
    // strongest row of each relation kind goes only when nothing else in the
    // answer can, and rows of one kind never outlast another kind wholesale,
    // which weights alone did (a symbol read by many and called by few lost
    // its reads altogether).
    if let Some((i, r)) = last_dealt(&output.changed, |c, _| c.callers.len() >= 2) {
        let shed = output.changed[i].callers.remove(r);
        output.changed[i].shed.add(&shed);
        output.changed[i].callers_omitted += 1;
        output.truncated_untested += usize::from(without_test_link(&shed));
        output.truncated += 1;
        return true;
    }
    // A symbol's only caller goes last: dropping a whole symbol while another
    // still has a tail to shed would make the selection look arbitrary, one
    // function listed with all its callers and another with none (#386). The
    // strongest caller of a symbol is the fact, so the symbol goes whole once
    // that is all it has left, never one caller at a time.
    match last_dealt(&output.changed, |c, _| {
        c.callers.len() == 1 && c.unresolved.is_none()
    }) {
        Some((i, _)) => {
            shed_definition(output, i);
            true
        }
        None => false,
    }
}

/// A definition shed whole, with the rows it had: they are counted as shed, and
/// the omitted entry names it.
fn shed_definition(output: &mut ImpactOutput, i: usize) {
    let gone = &output.changed[i];
    output.truncated += gone.callers.len();
    output.truncated_possible += gone
        .callers
        .iter()
        .filter(|c| c.evidence == "candidate")
        .count();
    output.truncated_textual += gone.callers.iter().filter(|c| is_textual(c)).count();
    output.truncated_untested += gone.callers.iter().filter(|c| without_test_link(c)).count();
    omit(output, i);
}

/// The answer as the reader receives it, fitted to `cap` tokens measured on
/// that very text. JSON was fitted while markdown was printed, so a 148-token
/// answer cited a 2000-token cap and lost its contracts (#451); `--budget`
/// and `budget_tokens` set the cap, which stayed 2000 whatever was asked
/// (#466). `None` keeps the default.
pub fn deliver(
    output: &mut ImpactOutput,
    markdown: bool,
    cap: Option<u32>,
    unfold: bool,
) -> String {
    deliver_framed(
        output,
        markdown,
        cap.unwrap_or(IMPACT_TOKEN_CAP),
        unfold,
        "",
    )
}

/// [`deliver`] with the text a door puts before the answer (the MCP's note that
/// its ceiling lowered the budget) charged to `cap` with the rest. The returned
/// text is the answer alone: the door puts its frame before it.
pub fn deliver_framed(
    output: &mut ImpactOutput,
    markdown: bool,
    cap: u32,
    unfold: bool,
    frame: &str,
) -> String {
    let _render = crate::profile::phase("render");
    if !unfold {
        fold_rows(output);
    }
    let markdown_tokens =
        |o: &ImpactOutput| crate::tokenizer::count_tokens(&format!("{frame}{}", render(o, false)));
    let json_tokens =
        |o: &ImpactOutput| crate::tokenizer::count_tokens(&format!("{frame}{}\n", v2_json(o)));
    let measure: &dyn Fn(&ImpactOutput) -> u32 = if markdown {
        &markdown_tokens
    } else {
        &json_tokens
    };
    fit_with(output, cap, measure);
    // The least the answer can say may outgrow the budget: it names the tokens
    // that take, cutting nothing more, and a budget of that size delivers it.
    let least = measure(output);
    if least > cap {
        output.budget_needed = Some(least);
    }
    if markdown {
        render(output, false)
    } else {
        v2_json(output)
    }
}

/// Counts the rows the folding table says the change cannot have affected in a
/// line each, by (relation kind, rule), and takes them out of the listed
/// callers. The count is exact under the definition's `Complete` witness of the
/// relation, otherwise "at least"; the examples are the first three by path and
/// line.
fn fold_rows(output: &mut ImpactOutput) {
    for sym in &mut output.changed {
        if !sym.callers.iter().any(|c| c.folded_by.is_some()) {
            continue;
        }
        let (mut rows, kept): (Vec<Caller>, Vec<Caller>) = std::mem::take(&mut sym.callers)
            .into_iter()
            .partition(|c| c.folded_by.is_some());
        sym.callers = kept;
        // A test is what the reviewer runs, so it is named first; then by path
        // and line, the same for the same change.
        let is_test = |c: &Caller| crate::testfiles::is_test_path(Path::new(&c.path));
        rows.sort_by(|a, b| {
            (
                !is_test(a),
                a.path.as_str(),
                a.sites.first(),
                a.lines.as_str(),
            )
                .cmp(&(
                    !is_test(b),
                    b.path.as_str(),
                    b.sites.first(),
                    b.lines.as_str(),
                ))
        });
        let mut groups: Vec<(&'static str, &'static str, Vec<Caller>)> = Vec::new();
        for row in rows {
            let (Some(rule), Some(kind)) = (row.folded_by, row.fold_kind) else {
                sym.callers.push(row);
                continue;
            };
            match groups.iter_mut().find(|(r, k, _)| *r == rule && *k == kind) {
                Some((_, _, group)) => group.push(row),
                None => groups.push((rule, kind, vec![row])),
            }
        }
        groups.sort_by_key(|(rule, kind, _)| (*rule, *kind));
        for (rule, kind, group) in groups {
            let coverage = match kind {
                "calls" | "instantiates" | "overrides" => sym.coverage.calls().clone(),
                "reads" | "writes" | "passes_as_value" => sym.coverage.reads().clone(),
                _ => sym.coverage.all(),
            };
            sym.folded.push(Folded {
                rule,
                kind,
                count: group.len(),
                exact: coverage.witness().is_some(),
                examples: group
                    .iter()
                    .take(3)
                    .map(|c| {
                        let line = c
                            .sites
                            .first()
                            .map_or_else(|| c.lines.clone(), |site| format!("{site}-{site}"));
                        label(&c.path, c.symbol.as_deref(), &line)
                    })
                    .collect(),
                parameters: if rule == "check-the-default" {
                    sym.new_parameters.clone()
                } else {
                    Vec::new()
                },
                sharing: group.iter().map(|c| c.fold_share).max().unwrap_or(0),
            });
        }
    }
    output.empty = output.empty && proves_empty(output);
}

/// The automatic answer's size, limits and notes included. An explicit
/// request keeps the engine's own cap.
pub const AUTOMATIC_TOKEN_CAP: u32 = 600;

/// The automatic (hook) answer: candidates counted, not listed, and the
/// whole text, limits and metadata included, within `cap` tokens.
pub fn render_automatic(output: &mut ImpactOutput, cap: u32) -> String {
    fold_rows(output);
    let measure = |o: &ImpactOutput| crate::tokenizer::count_tokens(&render(o, true));
    fit_with(output, cap, &measure);
    render(output, true)
}

fn label(path: &str, symbol: Option<&str>, lines: &str) -> String {
    match symbol {
        Some(s) => format!("{path}::{s} ({lines})"),
        None => format!("{path}:{lines}"),
    }
}

/// Names shown on the folded line before the rest is counted.
const TOO_COMMON_NAMED: usize = 8;

/// A caller no static test link reaches although one could have: the one
/// predicate behind the header count, the folded count and the line (#386).
pub(crate) fn without_test_link(c: &Caller) -> bool {
    c.evidence == "resolved" && c.tests.is_empty() && c.registered_by.is_none() && !c.tests_open
}

/// A row that rests on a word match alone: listed as text, never counted or
/// rendered as a caller (R1).
pub(crate) fn is_textual(c: &Caller) -> bool {
    c.evidence == "name"
}

/// What every row built from a word match is labelled with.
pub(crate) const TEXTUAL_MATCH: &str = "a textual match, not a resolved binding";

/// Textual rows shown before they are counted.
pub(crate) const MAX_TEXTUAL_SHOWN: usize = 12;

/// The files that spell a name nothing defines, from one line each: not
/// documents, data, ignore files or lockfiles, which mention without
/// reading. Code and config come first (Forgejo #114).
pub(crate) fn symbol_named(
    root_dir: &Path,
    source: &crate::source::Source,
    lines: Vec<(String, u32, String)>,
    unread: Vec<UnanalysedFile>,
    limits: Vec<crate::resource::LimitReason>,
) -> Option<SymbolNamed> {
    let mut rows: Vec<NamedFile> = lines
        .into_iter()
        .filter(|(path, _, _)| {
            !is_document(path) && !selects_paths(path) && !is_data(root_dir, source, path)
        })
        .map(|(path, line, _)| NamedFile { path, line })
        .collect();
    if rows.is_empty() {
        return None;
    }
    rows.sort_by_key(|r| {
        (
            !(is_code_file(&r.path) || is_config_file(&r.path)),
            r.path.clone(),
        )
    });
    let files = rows.len();
    rows.truncate(MAX_TEXTUAL_SHOWN);
    Some(SymbolNamed {
        files,
        rows,
        unread,
        limits,
    })
}

/// A `--symbol` query that only text answers: the output carries the names
/// and nothing else.
pub(crate) fn symbol_named_output(
    root_dir: &Path,
    query: &str,
    named: SymbolNamed,
) -> ImpactOutput {
    // The files read as text are the run's files not analysed: the header says
    // which, and why.
    let unread = named.unread.clone();
    ImpactOutput {
        name: root_dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| root_dir.to_string_lossy().to_string()),
        range: None,
        index_tree: None,
        range_name: None,
        symbol: Some(query.to_string()),
        limits: named.limits.clone(),
        symbol_named: Some(named),
        changed_files: Vec::new(),
        untracked_files: Vec::new(),
        deleted_files: Vec::new(),
        stale_references: Vec::new(),
        stale_references_omitted: 0,
        deleted_modules: Vec::new(),
        deleted_modules_omitted: 0,
        named_by: Vec::new(),
        named_by_omitted: 0,
        key_searches: Vec::new(),
        key_files_unsearched: 0,
        collection_reads: Vec::new(),
        collection_reads_omitted: 0,
        commit_count: 0,
        changed: Vec::new(),
        contracts: Vec::new(),
        empty: false,
        truncated: 0,
        truncated_possible: 0,
        needs: 0,
        truncated_untested: 0,
        truncated_textual: 0,
        changed_files_omitted: 0,
        untracked_files_omitted: 0,
        contracts_omitted: 0,
        tests_known: false,
        untested_definitions: 0,
        untested_unlisted: Vec::new(),
        untested_called_in_diff: Vec::new(),
        entry_unlisted: Vec::new(),
        layout_only: Vec::new(),
        lockfile_changes: Vec::new(),
        layout_only_omitted: 0,
        traced_definitions: 0,
        withheld_files: 0,
        unread_directories: Vec::new(),
        unanalysed_files: unread,
        unanalysed_files_omitted: 0,
        untraced_omitted: 0,
        unfinished_omitted: 0,
        run: None,
        cap: IMPACT_TOKEN_CAP,
        budget_needed: None,
    }
}

/// One use of a local, inside the definition it is a local of.
#[derive(Debug)]
pub(crate) struct LocalUse {
    /// The inner local or definition the use sits in.
    pub inside: Option<String>,
    pub line: u32,
    pub call: bool,
}

/// A `--symbol` name that is a local of a definition, and its uses there.
#[derive(Debug)]
pub(crate) struct LocalOf {
    pub path: String,
    pub name: String,
    /// `path::Definition`, or the file for module-level code.
    pub enclosing: String,
    pub line: u32,
    pub uses: Vec<LocalUse>,
}

/// A `--symbol` query that names only locals: one block each, never "no
/// definition found".
pub(crate) fn symbol_local_output(
    root_dir: &Path,
    query: &str,
    locals: Vec<LocalOf>,
) -> ImpactOutput {
    let mut output = symbol_named_output(
        root_dir,
        query,
        SymbolNamed {
            files: 0,
            rows: Vec::new(),
            unread: Vec::new(),
            limits: Vec::new(),
        },
    );
    output.symbol_named = None;
    output.changed = locals
        .into_iter()
        .map(|local| ChangedSymbol {
            callers: local
                .uses
                .iter()
                .map(|u| {
                    Caller::at_line(
                        local.path.clone(),
                        u.inside.clone(),
                        format!("{0}-{0}", u.line),
                        "resolved",
                        if u.call { "call" } else { "reference" },
                        u.line,
                    )
                })
                .collect(),
            unresolved: Some(Unknown::LocalOf {
                name: local.name.clone(),
                enclosing: local.enclosing,
            }),
            ..ChangedSymbol::bare(
                local.path,
                Some(local.name),
                format!("{0}-{0}", local.line),
                "local",
                decide::Coverages::partial(decide::Gap::NotTraced),
            )
        })
        .collect();
    output
}

/// Candidates shown per changed symbol before they are counted instead.
const MAX_CANDIDATES_SHOWN: usize = 3;

/// Changed callers named per symbol before the rest are counted.
const MAX_ALSO_CHANGED_SHOWN: usize = 3;

/// Unlisted definitions shown by name before they are counted.
pub(crate) const MAX_UNTESTED_NAMED: usize = 8;

/// The symbol names of `layout_only`, from its `path::symbol (lines)` labels.
pub fn layout_only_names(output: &ImpactOutput) -> impl Iterator<Item = &str> {
    output.layout_only.iter().filter_map(|l| {
        let (_, rest) = l.split_once("::")?;
        rest.rsplit_once(" (").map(|(name, _)| name)
    })
}

/// JSON Schema 2020-12 for `diffctx.impact.v2`, pinned by `tests/context_schema.rs`.
pub fn impact_schema() -> serde_json::Value {
    crate::answer::v2::impact_v2_schema()
}
