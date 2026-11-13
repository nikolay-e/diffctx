//! What an impact answer says, and what its text may print.
//!
//! The document is [`v2::ImpactV2`]; a renderer prints only facts that document
//! carries, with the presentation parameters of a [`RenderCtx`]. A number it
//! prints is a field of [`Tally`], computed here from the leaves the document
//! stores (rows, folds, the shed ledger), or a view of one list ([`Page`]):
//! never a count taken where it is printed (gh#721).
//!
//! A failed read is never an empty value in this module or its children: the lint
//! that says so is denied here.

#![deny(clippy::disallowed_methods)]

pub mod envelope;
pub mod render;
pub mod v2;

use rustc_hash::FxHashSet;

use v2::{Class, Count, Definition, ImpactV2, Row, ShedRows, Standing};

/// What the text of an answer varies by, apart from the answer.
#[derive(Clone, Debug, Default)]
pub struct RenderCtx {
    /// The range as the header names it, when the door knows it better than the
    /// document does (the hook's session edits).
    pub label: Option<String>,
    /// The hook's profile: candidates counted, not listed.
    pub compact: bool,
}

/// An impact document and its totals: what every renderer reads.
pub struct Answer<'a> {
    pub doc: &'a ImpactV2,
    pub ctx: &'a RenderCtx,
    pub tally: Tally,
}

impl<'a> Answer<'a> {
    pub fn of(doc: &'a ImpactV2, ctx: &'a RenderCtx) -> Self {
        Self {
            doc,
            ctx,
            tally: Tally::of(doc),
        }
    }
}

impl Answer<'_> {
    /// What the files it read say: nothing outside the diff depends on the change.
    /// Whether the files it did not read could say otherwise is
    /// [`Self::proves_empty`].
    pub fn reaches_nothing(&self) -> bool {
        let doc = self.doc;
        doc.spelled.is_none()
            && doc.omitted.is_none()
            && doc.definitions.iter().all(v2::Definition::only_in_the_diff)
            && doc.contracts.total == 0
            && doc.stale.total == 0
            && doc
                .deleted_modules
                .items
                .iter()
                .all(|m| !m.dynamic_import && m.importers.is_empty())
            && doc.named.by_path.total == 0
            && doc.named.by_key.total == 0
            && doc.named.key_searches.is_empty()
            && doc.named.collections.total == 0
            && self.tally.shed == 0
            && doc.inventory.lockfiles.total == 0
    }

    /// The files it read reach nothing outside the diff, and the only thing keeping
    /// that from being `empty` is files the run withheld: what a withheld file
    /// names is unknown, so the answer says what was not read instead of "reaches
    /// nothing" (#464, gh#713). A limit or an unanalysed file leaves the answer as
    /// it was.
    pub fn withheld_only(&self) -> bool {
        let doc = self.doc;
        doc.inventory.withheld > 0
            && doc.coverage.limits.is_empty()
            && doc.coverage.unanalysed.total == 0
            && self.reaches_nothing()
    }

    /// The one rule for `empty` (#505): nothing outside the diff depends on the
    /// change, no contract moved, and nothing was cut, left unread or capped. It is
    /// an absence claim, so it holds only with the witness: every changed
    /// definition's search was complete, and so was the run's scope and its read of
    /// the changed files.
    pub fn proves_empty(&self) -> bool {
        self.reaches_nothing()
            && self.doc.coverage.scope.complete
            && self.doc.coverage.unanalysed.total == 0
    }
}

/// What a row states, apart from the definition the document lists it under.
///
/// Two rows with one key are one fact said twice. The one place that says what
/// a fact is; `tests/support/fact_keys.rs` states it again over the printed JSON.
#[derive(PartialEq, Eq, Hash)]
pub enum FactKey<'a> {
    /// A line spells the name of a changed definition and no cell binds it: one
    /// fact whichever of the definitions of that name it was found for.
    Word {
        word: &'a str,
        path: &'a str,
        at: String,
    },
    /// Files in view spell the name and were not read whole.
    Unread {
        word: &'a str,
        path: &'a str,
        at: Option<u32>,
    },
    /// A use of one definition.
    Use {
        definition: (&'a str, Option<&'a str>, &'a str),
        path: &'a str,
        symbol: Option<&'a str>,
        lines: &'a str,
        sites: &'a [u32],
        class: Class,
        kind: Option<&'a str>,
        standing: Standing,
    },
}

impl<'a> FactKey<'a> {
    pub fn of_row(def: &'a Definition, row: &'a Row) -> Self {
        if row.standing == Standing::Name {
            // The line a word match cites: its first site, else where its function starts.
            let at = row.sites.first().map_or_else(
                || row.lines.split('-').next().unwrap_or("").to_string(),
                ToString::to_string,
            );
            return Self::Word {
                word: def.word(),
                path: &row.path,
                at,
            };
        }
        Self::Use {
            definition: (&def.path, def.symbol.as_deref(), &def.lines),
            path: &row.path,
            symbol: row.symbol.as_deref(),
            lines: &row.lines,
            sites: &row.sites,
            class: row.class,
            kind: row.kind.as_deref(),
            standing: row.standing,
        }
    }
}

/// Keeps the first row of each fact.
///
/// The definitions of one name are searched one after the other, and the line that spells the name is found for each of
/// them: the later rows are the same fact, with the same fragment and the same
/// site, so the first definition's row stays and the rest are dropped (never at
/// render: the totals count rows).
pub fn one_row_per_fact(definitions: &mut [Definition]) {
    let (mut seen, mut first) = (FxHashSet::default(), Vec::new());
    for def in definitions.iter() {
        for row in &def.rows {
            first.push(seen.insert(FactKey::of_row(def, row)));
        }
        for file in &def.missed_in {
            first.push(seen.insert(FactKey::Unread {
                word: def.word(),
                path: &file.path,
                at: file.lines.first().copied(),
            }));
        }
    }
    let mut first = first.into_iter();
    for def in definitions {
        def.rows.retain(|_| first.next() == Some(true));
        def.missed_in.retain(|_| first.next() == Some(true));
    }
}

/// The totals of an impact document.
#[derive(Clone, Copy, Debug)]
pub struct Tally {
    /// Every changed definition the answer stands for, listed or shed.
    pub definitions: usize,
    /// Callers found, listed, folded or shed, word matches and possible ones apart;
    /// exact only when the run's scope and every search of calls were complete.
    pub callers: Count,
    /// Possible callers, listed, folded or shed.
    pub possible: usize,
    /// Callers no static test link reaches, listed or shed.
    pub untested_callers: usize,
    /// Callers the folding table says the change cannot have affected.
    pub folded_callers: usize,
    /// Word matches, listed or shed: no callers, and no zero either.
    pub textual: usize,
    /// Of those, the ones the cap shed.
    pub shed_textual: usize,
    /// Definitions whose callers the run stopped before determining.
    pub undetermined: usize,
    /// Rows the cap shed from definitions still listed.
    pub shed_from_listed: usize,
    /// Rows the cap shed, from listed definitions and from shed ones.
    pub shed: usize,
    /// Definitions in the document, the omitted entry among them.
    pub listed: usize,
    /// Changed files, listed or not, deleted ones included.
    pub changed_files: usize,
    pub deleted_files: usize,
    /// Untracked files, listed or not.
    pub untracked: usize,
    /// Reads by collection name, listed or not.
    pub collection_reads: usize,
    /// Files the run did not read as code, by what became of them.
    pub not_analysed: usize,
    pub analysed_in_part: usize,
    pub not_followed: usize,
    pub outside_definitions: usize,
}

impl Tally {
    pub fn of(doc: &ImpactV2) -> Self {
        let defs = &doc.definitions;
        let rows = || defs.iter().flat_map(|d| d.rows.iter());
        let caller = |r: &&v2::Row| r.is_caller();
        let folded = |untyped: bool| -> usize {
            defs.iter()
                .flat_map(|d| d.folded.iter())
                .filter(|f| (f.rule == "untyped-receiver") == untyped)
                .map(|f| f.count.n)
                .sum()
        };
        let omitted: ShedRows = doc
            .omitted
            .as_ref()
            .map_or_else(ShedRows::default, |o| o.shed);
        let shed = |pick: fn(&ShedRows) -> usize| -> usize {
            defs.iter().map(|d| pick(&d.shed)).sum::<usize>() + pick(&omitted)
        };
        let folded_callers = folded(false);
        let callers = rows().filter(caller).count() + folded_callers + shed(|s| s.resolved);
        let exact = doc.coverage.scope.complete
            && defs.iter().all(|d| d.coverage.calls.complete)
            && doc.omitted.as_ref().is_none_or(|o| o.incomplete == 0);
        let reason = |reason: &str| {
            doc.coverage
                .unanalysed
                .items
                .iter()
                .filter(|f| f.reason == reason)
                .count()
        };
        let analysed_in_part = reason("parse_partial");
        let not_followed = reason("content_untraced") + doc.coverage.untraced_omitted;
        let outside_definitions = reason(crate::impact::OUTSIDE_DEFINITIONS);
        let shed_from_listed: usize = defs.iter().map(|d| d.shed.total()).sum();
        Self {
            definitions: defs.len() + doc.omitted.as_ref().map_or(0, |o| o.definitions.total),
            callers: Count { n: callers, exact },
            possible: rows().filter(|r| r.standing == Standing::Candidate).count()
                + folded(true)
                + shed(|s| s.candidate),
            untested_callers: rows().filter(|r| r.without_link()).count() + shed(|s| s.untested),
            folded_callers,
            textual: rows().filter(|r| r.standing == Standing::Name).count()
                + defs.iter().map(|d| d.missed_in.len()).sum::<usize>()
                + shed(|s| s.name),
            shed_textual: shed(|s| s.name),
            undetermined: defs
                .iter()
                .filter(|d| d.gaps.iter().any(|g| matches!(g, v2::Gap::Stopped { .. })))
                .count()
                + doc.coverage.not_searched,
            shed_from_listed,
            shed: shed_from_listed + omitted.total(),
            listed: defs.len() + usize::from(doc.omitted.is_some()),
            changed_files: doc.inventory.changed.total + doc.inventory.deleted.total,
            deleted_files: doc.inventory.deleted.total,
            untracked: doc.inventory.untracked.total,
            collection_reads: doc.named.collections.total,
            not_analysed: doc.coverage.unanalysed.total
                - analysed_in_part
                - not_followed
                - outside_definitions,
            analysed_in_part,
            not_followed,
            outside_definitions,
        }
    }

    /// The totals as the document states them.
    pub fn summary(&self, traced: usize) -> v2::Summary {
        v2::Summary {
            definitions: self.definitions,
            callers: self.callers,
            possible: self.possible,
            folded: self.folded_callers,
            shed: self.shed,
            untested_callers: self.untested_callers,
            files: self.changed_files,
            traced,
        }
    }
}

/// One list read a page at a time: the first `cap` of its items, and what is left
/// of it, the items the list lost elsewhere (`omitted`) counted in. The only place
/// a list's length is taken for a renderer.
pub struct Page<'a, T> {
    pub shown: &'a [T],
    /// Every item the list stands for, shown or not.
    pub total: usize,
}

impl<'a, T> Page<'a, T> {
    pub fn of(items: &'a [T], cap: usize, omitted: usize) -> Self {
        let shown = &items[..items.len().min(cap)];
        Self {
            shown,
            total: items.len() + omitted,
        }
    }

    /// Items not shown.
    pub fn rest(&self) -> usize {
        self.total - self.shown.len()
    }
}
