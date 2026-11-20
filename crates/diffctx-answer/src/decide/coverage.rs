//! The witness that a search left nothing unread and the one place that makes it (`Coverage::close`),
//! with the counts and claims that hold it.

use std::collections::{BTreeMap, BTreeSet};

use diffctx_occurrence::tokens::occurrence::{
    Hedge, Implicit, Reason as OccurrenceReason, ValuePlace,
};

use diffctx_resolve::resolve::{Relation, UnresolvedReason};

use super::gap::{Gap, Reason, Site, Via};
use super::losses::{Effect, effect_of};
use super::verdict::Verdict;

/// The witness that the search for a definition's uses covered everything that
/// could name it. It has no public field and no `Default`, and `Coverage::close`
/// is the one place that makes it, so an absence or an exact count cannot be
/// written without the coverage computation having found no gap and, for a
/// definition, every file of its group accounted for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Complete(());

/// What a coverage rests on besides the gaps the search found.
#[derive(Clone, Copy)]
pub enum Evidence<'a> {
    /// A definition's occurrence account of one relation: what stands in the way of
    /// its absence.
    Occurrences(Relation, &'a [Hedge]),
    /// Not a definition: the run's own limits, the files it withheld, code it left
    /// untraced.
    Run,
}

/// What each hedge of an occurrence account of `relation` says in the answer's
/// words: a candidate occurrence names its site, a file lost is unaccounted for by
/// its loss, the rest are routes no model traces.
fn hedge_gaps(relation: Relation, hedges: &[Hedge], dispatched: bool) -> Vec<Gap> {
    let (mut common, mut sites) = (0, Vec::new());
    let (mut lost, mut rest) = (BTreeMap::<Reason, usize>::new(), Vec::new());
    let mut passed: Option<BTreeSet<String>> = None;
    let reads = matches!(
        relation,
        Relation::Reads | Relation::Writes | Relation::PassesAsValue
    );
    for hedge in hedges {
        let gap = match hedge {
            Hedge::Candidate {
                file, line, reason, ..
            } => {
                let via = match (*reason, reads) {
                    (OccurrenceReason::Embedded, _) => Via::Embedded,
                    (OccurrenceReason::Unresolved(UnresolvedReason::NoBinding), true) => {
                        Via::Unbound
                    }
                    (_, true) => Via::Read,
                    (_, false) => Via::Call,
                };
                let path = file.as_str().to_string();
                sites.push(Site {
                    path,
                    line: *line,
                    via,
                });
                continue;
            }
            Hedge::TooCommon { occurrences } | Hedge::Unlisted { occurrences } => {
                common += occurrences;
                continue;
            }
            // The call through a trait is the `Dispatched` gap's to name: a
            // `Display::fmt` that `format!` reaches is that one route, not a
            // second "constructed implicitly" beside it.
            Hedge::Implicit(Implicit::Override | Implicit::Protocol) if dispatched => continue,
            Hedge::Unread { loss, .. } => match effect_of(loss) {
                Effect::File(reason) | Effect::Subtree(reason, _) => {
                    *lost.entry(reason).or_default() += 1;
                    continue;
                }
                Effect::Run(gap) => gap,
            },
            Hedge::Leak {
                place: ValuePlace::Argument,
                callee,
                ..
            } => {
                let to = passed.get_or_insert_default();
                to.extend(callee.iter().map(|name| name.as_str().to_string()));
                continue;
            }
            Hedge::Implicit(Implicit::Protocol) => Gap::ImplicitlyConstructed,
            Hedge::Leak { .. } | Hedge::Implicit(_) | Hedge::AliasBound(_) => Gap::NotTraced,
        };
        if !rest.contains(&gap) {
            rest.push(gap);
        }
    }
    rest.extend(passed.map(|to| Gap::PassedAsValue {
        to: to.into_iter().collect(),
    }));
    let lost = lost
        .into_iter()
        .map(|(reason, files)| Gap::Unaccounted { reason, files });
    sites.sort();
    sites.dedup();
    let uses = sites.len() + common;
    sites.truncate(crate::impact::symbol::MAX_CANDIDATES_SHOWN);
    let candidates = (uses > 0).then_some(Gap::Candidates { uses, sites });
    lost.chain(candidates).chain(rest).collect()
}

/// What the search for a definition's uses covered.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Coverage {
    Complete(Complete),
    Partial(Vec<Gap>),
}

impl Coverage {
    /// The one place a `Complete` is made: the search found no gap, and, for a
    /// definition, no occurrence of its names is left unexplained (the hedges join
    /// the search's own gaps, so that a partial coverage always says what stood in
    /// the way).
    pub fn close(evidence: Evidence<'_>, mut gaps: Vec<Gap>) -> Self {
        if let Evidence::Occurrences(relation, hedges) = evidence {
            let dispatched = gaps.iter().any(|gap| matches!(gap, Gap::Dispatched { .. }));
            for gap in hedge_gaps(relation, hedges, dispatched) {
                if !gaps.contains(&gap) {
                    gaps.push(gap);
                }
            }
        }
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
pub(crate) struct Coverages {
    calls: Coverage,
    reads: Coverage,
}

impl Coverages {
    /// A search incomplete for one reason it already knows, of both.
    pub(crate) fn partial(gap: Gap) -> Self {
        Self {
            calls: Coverage::partial(gap.clone()),
            reads: Coverage::partial(gap),
        }
    }

    /// What the cell covered of a definition.
    pub(crate) fn of_verdict(verdict: &Verdict) -> Self {
        Self {
            calls: verdict.calls(),
            reads: verdict.reads(),
        }
    }

    /// The coverage of a definition no cell answers for: nothing accounts for the
    /// occurrences of its names, so it is never complete. Its gaps are the ones the
    /// answer knows, or a route no model traces.
    pub(crate) fn unowned(mut gaps: Vec<Gap>) -> Self {
        if gaps.is_empty() {
            gaps.push(Gap::NotTraced);
        }
        Self {
            calls: Coverage::Partial(gaps.clone()),
            reads: Coverage::Partial(gaps),
        }
    }

    /// The coverage of a local: every use of it lies in the body it is a local of, which the
    /// answer lists, so the gaps are the ones that body leaves (a use that hands it on, a file
    /// read in part).
    pub(crate) fn local(gaps: Vec<Gap>) -> Self {
        Self {
            calls: Coverage::close(Evidence::Run, gaps.clone()),
            reads: Coverage::close(Evidence::Run, gaps),
        }
    }

    /// The same coverages with one more gap, when there is one.
    #[must_use]
    pub(crate) fn with(self, gap: Option<Gap>) -> Self {
        Self {
            calls: self.calls.with(gap.clone()),
            reads: self.reads.with(gap),
        }
    }

    pub(crate) const fn calls(&self) -> &Coverage {
        &self.calls
    }

    pub(crate) const fn reads(&self) -> &Coverage {
        &self.reads
    }

    /// Both groups, as one coverage: what a claim about every use needs.
    pub(crate) fn all(&self) -> Coverage {
        self.calls.clone().both(self.reads.clone())
    }

    /// Whether a candidate gap that counts more uses than it names gave up its
    /// sites: the count stays.
    pub(crate) fn shed_sites(&mut self) -> bool {
        let mut shed = false;
        for coverage in [&mut self.calls, &mut self.reads] {
            if let Coverage::Partial(gaps) = coverage {
                gaps.iter_mut().for_each(|gap| shed |= gap.shed_sites());
            }
        }
        shed
    }
}
