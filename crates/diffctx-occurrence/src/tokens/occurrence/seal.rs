//! The seal: what the accounting of a definition says of each relation of use once the resolved uses
//! have settled the occurrences beside them. Another crate gets the verdict as a [`Sealed`], never
//! the means to make one.

use std::collections::HashSet;
use std::ops::Range;

use diffctx_resolve::resolve::{Relation, USES};
use diffctx_snapshot::snapshot::RepoPath;

use super::{Hedge, Proof, Reason, Verdict};

/// A use the resolver settled: the occurrences of its own site are explained by it.
pub struct Settled<'a> {
    pub file: &'a str,
    pub line: u32,
    pub site: Range<u32>,
    pub relation: Relation,
}

/// One relation's account, closed. Its proof is the only evidence of a definition's `Complete` that
/// anything outside this crate holds.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Sealed(Verdict);

impl Sealed {
    pub const fn proof(&self) -> Option<Proof> {
        match &self.0 {
            Verdict::Complete(proof) => Some(*proof),
            Verdict::Partial(_) => None,
        }
    }

    /// What stands in the way: nothing when it is complete.
    pub fn hedges(&self) -> &[Hedge] {
        match &self.0 {
            Verdict::Complete(_) => &[],
            Verdict::Partial(hedges) => hedges,
        }
    }
}

/// The accounting of a changed definition after the resolved uses settled their own sites.
pub struct Settlement {
    pub by_relation: [(Relation, Sealed); 6],
    /// What stands in the way of each relation's absence, less the occurrences a resolved use
    /// settles: a site has one standing, the strongest.
    pub hedges: Vec<(Relation, Hedge)>,
}

/// What the accounting of a definition found, on each side it is read at. The hedges are private:
/// [`Accounted::settle`] closes them.
pub struct Accounted {
    pub(super) hedges: Vec<(Relation, Hedge)>,
    /// How many definitions were accounted: over none, nothing is proven and no relation seals complete.
    pub(super) definitions: usize,
    /// No occurrence is a use, an alias, a leak or a candidate.
    pub unused: bool,
    /// The lines, at the head, whose quoted spelling resolved to another definition: a
    /// literal on one of them is that use, not a mention the cell could not read.
    pub elsewhere: HashSet<(RepoPath, u32)>,
    /// The whole strings that spell a name of the definition and no binding explains.
    pub quoted: usize,
}

impl Accounted {
    /// What stands in the way of each relation of use, before the resolved uses settle their sites.
    pub fn hedges(&self) -> &[(Relation, Hedge)] {
        &self.hedges
    }

    /// Closes the account. A resolved use settles the occurrences of its own site: the name inside
    /// its span, or one no binding names beside it (a keyword argument's name, the second spelling
    /// of a shorthand). A candidate in a span of its own is another use that no binding settles, on
    /// a line already listed: counted, never listed again. A value the use hands on (returned,
    /// collected) still hedges the calls it leaves. `relation_of` says which relation a hedge stands
    /// in: it moves a hedge, never drops one. Only the relations of use are sealed: a hedge that
    /// stands in another (type uses, imports, re-exports) stays in `Settlement::hedges` and blocks
    /// no seal.
    pub fn settle(
        &self,
        resolved: &[Settled<'_>],
        relation_of: impl Fn(Relation, &Hedge) -> Relation,
    ) -> Settlement {
        let hedges: Vec<(Relation, Hedge)> = (self.hedges.iter())
            .map(|(relation, hedge)| (relation_of(*relation, hedge), hedge))
            .filter_map(|(relation, hedge)| {
                let Hedge::Candidate {
                    file,
                    line,
                    bytes,
                    reason,
                } = hedge
                else {
                    return Some((relation, hedge.clone()));
                };
                let beside: Vec<&Settled> = (resolved.iter())
                    .filter(|u| {
                        u.file == file.as_str() && u.line == *line && u.relation == relation
                    })
                    .collect();
                let own_site = matches!(reason, Reason::Unexplained | Reason::WideBinding)
                    || (beside.iter())
                        .any(|u| u.site.start < bytes.end && bytes.start < u.site.end);
                if beside.is_empty() {
                    Some((relation, hedge.clone()))
                } else {
                    (!own_site).then_some((relation, Hedge::Unlisted { occurrences: 1 }))
                }
            })
            .collect();
        let by_relation = USES.map(|relation| {
            let own = (hedges.iter())
                .filter(|(of, _)| *of == relation)
                .map(|(_, hedge)| hedge.clone());
            (
                relation,
                Sealed(Verdict::of(own.collect(), self.definitions)),
            )
        });
        Settlement {
            by_relation,
            hedges,
        }
    }
}
