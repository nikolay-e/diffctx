//! The impact answer's data: what a change to a definition did and who it reaches (`ImpactOutput` and the
//! rows it holds), with the findings a stale reference, a collection read or an unanalysed file add.

use rustc_hash::FxHashSet;

use schemars::JsonSchema;

use serde::Serialize;

use crate::answer::v2::GuardState;

use crate::decide::{self};

use super::MentionClass;

/// What a change reaches that its own diff does not show (#310): the
/// callers outside the diff of each changed symbol, whether a test guards
/// each caller, which symbols more than one commit of the range touched,
/// and the public contracts the range edits.
#[derive(Clone, Default)]
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
    pub named_by_path_omitted: usize,
    pub named_by_key_omitted: usize,
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
    pub limits: Vec<diffctx_snapshot::resource::LimitReason>,
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
    /// Lock files the range changes; what that reaches is not traced. Dropped
    /// from every count, a dependency bump answered "No changes to analyse"
    /// (#495); named "dependency versions moved", a lock whose only change was
    /// a local path source claimed a bump nobody made (#593).
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
    /// Files outside the run's pathspecs that spell the name of a changed
    /// definition: they were not read, so a zero says nothing about them (fj#221).
    pub outside_scope_files: usize,
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
    pub run: Option<diffctx_snapshot::resource::RunContext>,
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
    pub fn definitions(&self) -> usize {
        self.changed
            .iter()
            .map(|c| match &c.unresolved {
                Some(Unknown::Omitted { count, .. }) => *count,
                _ => 1,
            })
            .sum()
    }
}

/// A module the range deletes, answered as a module.
///
/// The files that still import it, or none, and the definitions it held counted
/// once. A definition something outside the diff still uses keeps its own row.
#[derive(Debug, Serialize, JsonSchema, Clone)]
pub struct DeletedModule {
    pub path: String,
    /// The definitions it held.
    pub definitions: usize,
    /// How many of them have a row of their own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listed: Option<usize>,
    /// The files at the head that import it, a few by path.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub importers: Vec<String>,
    /// Importers beyond those named.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub more_importers: Option<usize>,
    /// No importer was found and an import whose name is computed at run time
    /// could reach it: "no importers" is what was found, not a proof.
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
    pub limits: Vec<diffctx_snapshot::resource::LimitReason>,
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
    pub(crate) coverage: decide::coverage::Coverages,
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
    pub(super) fn bare(
        path: String,
        symbol: Option<String>,
        lines: String,
        kind: impl Into<String>,
        coverage: decide::coverage::Coverages,
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
    /// Uses of the name no binding settles, the first of them named.
    Candidates {
        name: String,
        uses: usize,
        sites: Vec<decide::gap::Site>,
    },
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
        stops: Vec<diffctx_snapshot::resource::LimitReason>,
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
    /// A gap in the search, in words (`decide::gap::Gap`): what was not read, so that
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

/// The tests an answer names for a caller whose tests are `tests` (the ones the
/// change touches first, then path then name order): the first of each file, then
/// the rest, [`crate::answer::v2::TESTS_NAMED`] at most, in that order. One file of many tests does not crowd out the
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

#[derive(Clone, Default)]
pub struct Caller {
    pub path: String,
    pub symbol: Option<String>,
    pub lines: String,
    pub weight: f64,
    /// The tests that reach this caller at the fewest hops: the ones the change
    /// touches, then path then name order.
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
    /// The `tests` are the nearest only: a user above them was left unwalked, or a
    /// use on the walked path is hedged.
    pub tests_partial: bool,
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
    /// The definition the sites sit in, when the resolver names it: its `lines` are then
    /// the sites, not that definition's span.
    pub(crate) within: Option<diffctx_facts::facts::LocalKey>,
}

impl Caller {
    /// A row at one line that no folding rule, kind or test link qualifies.
    pub(super) fn at_line(
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
            evidence,
            relation,
            sites: vec![site],
            ..Self::default()
        }
    }
}

/// The limits among `limits` that leave a count of callers a lower bound.
pub(crate) fn stops_of(
    limits: &[diffctx_snapshot::resource::LimitReason],
) -> Vec<diffctx_snapshot::resource::LimitReason> {
    limits
        .iter()
        .copied()
        .filter(|r| r.stops_a_search())
        .collect()
}

pub fn reason_names(reasons: &[diffctx_snapshot::resource::LimitReason]) -> String {
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
pub fn is_unfinished(s: &ChangedSymbol) -> bool {
    matches!(s.unresolved, Some(Unknown::Unfinished { .. }))
}

/// The one entry that stands for the definitions the answer cap shed.
pub(crate) fn is_omitted(s: &ChangedSymbol) -> bool {
    matches!(s.unresolved, Some(Unknown::Omitted { .. }))
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

impl UnanalysedFile {
    pub(crate) fn new(path: String, reason: &'static str, lines: Vec<u32>) -> Self {
        let language = diffctx_base::languages::get_language_for_file(&path);
        Self {
            path,
            language,
            reason,
            lines,
        }
    }
}

#[derive(Debug, Serialize, JsonSchema, Clone)]
pub struct Contract {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    pub kind: &'static str,
}
