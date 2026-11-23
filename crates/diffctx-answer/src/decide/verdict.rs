//! What a definition's uses add up to: the claim, the uses, the accounting and the verdict.

use std::collections::HashSet;

use std::ops::Range;

use std::rc::Rc;

use crate::fold::Rule;

use diffctx_facts::facts::{DefKey, Scope};

use diffctx_occurrence::tokens::occurrence::{Hedge, Standing};

use diffctx_resolve::resolve::Relation;

use diffctx_snapshot::snapshot::RepoPath;

use super::coverage::{Complete, Coverage};
use super::gap::Gap;

/// What a statement about a definition's uses may be.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(
    dead_code,
    reason = "the witness is held so that an absence needs one, never read"
)]
pub(crate) enum Claim {
    Resolved,
    Candidate,
    Unresolved,
    Partial,
    /// Nothing calls the definition and nothing that could was left unread: the
    /// only claim an answer may print as an absence.
    Absent(Complete),
}

/// One use of a definition: the line, how it uses it, and how sure the
/// resolver is. `scope` names who owns the line.
#[derive(Clone, Debug)]
pub(crate) struct Use {
    pub file: String,
    pub line: u32,
    /// The bytes of the name at the line, in the file as the resolver read it.
    pub site: Range<u32>,
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

/// What the occurrence accounting of a changed definition found, with what only the
/// answer knows: the callers of what the range removed and whether any of them is
/// stranded.
pub(crate) struct Accounting {
    pub(super) hedges: Vec<(Relation, Hedge)>,
    pub(super) unused: bool,
    pub(super) whole: bool,
    /// The lines that still spell what the range removed, at the places the base
    /// resolved a use to it: its callers whatever they land on now.
    pub(super) callers: Vec<Use>,
    /// One of them is at a place the base resolved a use to what the range removed.
    pub(super) stranded: bool,
    /// The lines, at the head, whose quoted spelling resolved to another definition: a
    /// literal on one of them is that use, not a mention the cell could not read.
    pub(super) elsewhere: HashSet<(RepoPath, u32)>,
    /// The whole strings that spell a name of the definition and no binding explains.
    pub(super) quoted: usize,
}

/// The uses of a definition and what covered the search for them: the one
/// coverage of the relations that use it (`Relation::uses`), which is every
/// relation an absence is claimed for.
pub(crate) struct Verdict {
    pub uses: Vec<Use>,
    pub(super) by_relation: [(Relation, Coverage); 6],
    pub(super) overridden: Option<String>,
    pub(super) found: Rc<Accounting>,
    /// What stands in the way of each relation's absence, less the occurrences a
    /// resolved use already settles: a site has one standing, the strongest.
    pub(super) hedges: Vec<(Relation, Hedge)>,
}

impl Verdict {
    /// Whether the accounting found no use of the definition and nothing it could
    /// not explain: what a holder needs of what it holds to be said unused.
    pub(crate) fn unused(&self) -> bool {
        self.found.unused
    }

    /// Whether a literal spelled on `line` of `file` is a use the accounting resolved to
    /// another definition (an `include` of a second chart's template of the same name).
    pub(crate) fn names_another(&self, file: &str, line: u32) -> bool {
        RepoPath::new(file).is_ok_and(|file| self.found.elsewhere.contains(&(file, line)))
    }

    /// Whether a whole string in the group's code spells a name of the definition and
    /// no binding explains it: a reflective use no tree reads.
    pub(crate) fn names_quoted(&self) -> bool {
        self.found.quoted > 0
    }

    /// The type calls reach the definition through, which no model traces: what
    /// called the member a removed override overrode, or the interface a class
    /// outside the repository may implement with a method nothing calls.
    pub(crate) fn overridden_via(&self) -> Option<&str> {
        self.overridden.as_deref()
    }

    /// Callers outside the diff with a static chain to the definition.
    pub(crate) fn resolved(&self) -> impl Iterator<Item = &Use> {
        self.uses
            .iter()
            .filter(|u| !u.also_changed && u.standing == Standing::Resolved)
    }

    pub(crate) fn candidates(&self) -> impl Iterator<Item = &Use> {
        self.uses
            .iter()
            .filter(|u| !u.also_changed && matches!(u.standing, Standing::Candidate(_)))
    }

    pub(crate) fn also_changed(&self) -> impl Iterator<Item = &Use> {
        self.uses.iter().filter(|u| u.also_changed)
    }

    /// Whether the search says a route no model traces can reach the definition.
    pub(crate) fn not_traced(&self) -> bool {
        self.coverage().gaps().contains(&Gap::NotTraced)
    }

    /// Whether every gap of the search is a file of the group it could not place:
    /// nothing else stands in the way of what a file outside code says of it.
    pub(crate) fn gaps_are_only_unaccounted(&self) -> bool {
        self.coverage()
            .gaps()
            .iter()
            .all(|gap| matches!(gap, Gap::Unaccounted { .. }))
    }

    /// What stands in the way of each relation's absence, occurrence by occurrence.
    pub(crate) fn hedges(&self) -> &[(Relation, Hedge)] {
        &self.hedges
    }

    /// What covered the search of one relation of use.
    fn relation(&self, relation: Relation) -> &Coverage {
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
    pub(super) fn calls(&self) -> Coverage {
        self.merged([Relation::Calls, Relation::Instantiates, Relation::Overrides])
    }

    /// The reads of the definition: reads, writes and a value passed on.
    pub(super) fn reads(&self) -> Coverage {
        self.merged([Relation::Reads, Relation::Writes, Relation::PassesAsValue])
    }

    /// Every relation of use at once.
    fn coverage(&self) -> Coverage {
        self.calls().both(self.reads())
    }

    /// The gap the answer names first.
    pub(crate) fn gap(&self) -> Option<Gap> {
        self.coverage().gaps().first().cloned()
    }

    /// The strongest thing the evidence carries of one relation. An absence is
    /// the one claim only a complete search of that relation makes.
    pub(crate) fn claim(&self, relation: Relation) -> Claim {
        if self.resolved().any(|u| u.relation == relation) {
            Claim::Resolved
        } else if self.candidates().any(|u| u.relation == relation) {
            Claim::Candidate
        } else {
            match self.relation(relation) {
                Coverage::Complete(witness) => Claim::Absent(*witness),
                Coverage::Partial(gaps) => match gaps.first() {
                    Some(Gap::Candidates { .. }) => Claim::Unresolved,
                    _ => Claim::Partial,
                },
            }
        }
    }

    pub(crate) fn complete(&self) -> bool {
        self.coverage().witness().is_some()
    }
}

/// Which relations of a subject reach the definition.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Reach {
    Every,
    /// Building the class: what a constructor's callers do.
    Construction,
    /// What a changed or removed constructor reaches: the class's
    /// instantiations, the class handed on as a value (whoever receives it builds
    /// it: `map(Point, rows)`, `raise Error`, `kind = Cat`) and the explicit and
    /// `super()` calls of the constructor. Annotations and `isinstance` are uses
    /// of the class a constructor change leaves alone.
    Constructor,
}

impl Reach {
    pub(super) fn admits(self, relation: Relation) -> bool {
        match self {
            Self::Every => relation.uses(),
            Self::Construction => matches!(
                relation,
                Relation::Instantiates | Relation::Calls | Relation::PassesAsValue
            ),
            Self::Constructor => matches!(
                relation,
                Relation::Instantiates
                    | Relation::Calls
                    | Relation::Overrides
                    | Relation::PassesAsValue
            ),
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct Subject {
    pub(super) key: DefKey,
    pub(super) reach: Reach,
    /// The span of the definition whose change this is, when it has no members:
    /// a use inside it is the definition's own (a function calling itself). A
    /// class has none, since the members around the change are other
    /// definitions: the unchanged ones are callers, the changed ones
    /// `also_changed`.
    pub(super) within: Option<(u32, u32)>,
}
