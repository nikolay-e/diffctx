//! What a `Complete` verdict and a `not_ours` row stand on, on hand-built accounts: only this crate
//! can build them. Every other test of the accounting reads a real analyzer run.

use std::collections::HashSet;

use diffctx_facts::facts::{Name, Span};
use diffctx_resolve::resolve::{CandidateReason, Relation};
use diffctx_snapshot::snapshot::{Loss, RepoPath, SideTag};

use super::{
    Account, Accounted, Alias, AliasBound, AliasScope, Basis, Class, Entry, Hedge, Implicit,
    MAX_ALIAS_HOPS, Occurrence, Outcome, Reason, Settled, Unread, ValuePlace, Verdict, Witness,
};

fn path(name: &str) -> RepoPath {
    RepoPath::new(name).expect("a repository path")
}

fn account(relation: Relation, entries: Vec<Entry>) -> Account {
    Account {
        entries,
        ..Account::new(relation)
    }
}

fn entry(line: u32, relation: Option<Relation>, outcome: Outcome) -> Entry {
    let (file, bytes, class) = (path("app/x.py"), 10..11, Class::Code);
    Entry {
        occurrence: Occurrence {
            file,
            bytes,
            line,
            class,
        },
        relation,
        outcome,
        basis: Basis::Resolved,
        passed_to: None,
    }
}

fn candidate(line: u32, relation: Option<Relation>) -> Entry {
    entry(line, relation, Outcome::Candidate(resolver()))
}

fn resolver() -> Reason {
    Reason::Resolver(CandidateReason::UntypedReceiver)
}

fn hedges(verdict: Verdict) -> Vec<Hedge> {
    match verdict {
        Verdict::Complete(_) => Vec::new(),
        Verdict::Partial(hedges) => hedges,
    }
}

fn not_ours(witness: Witness) -> Outcome {
    Outcome::NotOurs { witness }
}

#[test]
fn an_account_with_every_occurrence_explained_is_complete() {
    let alias = Alias {
        name: "g".into(),
        scope: AliasScope::File(path("app/x.py")),
        hops: 1,
    };
    let local = not_ours(Witness::Local(Span::default()));
    let entries = [
        Outcome::Ours,
        Outcome::Declaration,
        local,
        Outcome::Alias(alias),
    ]
    .into_iter()
    .map(|outcome| entry(1, None, outcome))
    .collect();
    let explained = account(Relation::Calls, entries);
    assert!(explained.verdict().is_complete());
    assert_eq!(explained.verdict().as_str(), "complete");
    assert!(account(Relation::Reads, Vec::new()).verdict().is_complete());
}

#[test]
fn a_candidate_blocks_the_relation_it_carries_and_all_when_it_carries_none() {
    for relation in Relation::ALL {
        let blocked = |carried| {
            !account(relation, vec![candidate(7, carried)])
                .verdict()
                .is_complete()
        };
        assert_eq!(
            blocked(Some(Relation::Calls)),
            relation == Relation::Calls,
            "{relation:?}"
        );
        assert!(blocked(None), "{relation:?}");
    }
    let calls = account(Relation::Calls, vec![candidate(7, Some(Relation::Calls))]);
    assert_eq!(calls.verdict().as_str(), "partial");
    assert_eq!(
        hedges(calls.verdict())[0].to_string(),
        "candidate at app/x.py:7"
    );
}

#[test]
fn a_leak_and_an_implicit_kind_block_calls_and_only_calls() {
    let leak = entry(
        5,
        Some(Relation::PassesAsValue),
        Outcome::Leak(ValuePlace::Argument),
    );
    let calls = account(Relation::Calls, vec![leak.clone()]);
    assert_eq!(hedges(calls.verdict())[0].to_string(), "leak at app/x.py:5");
    assert!(account(Relation::Reads, vec![leak]).verdict().is_complete());
    for kind in Implicit::ALL {
        let implicit = |relation| Account {
            implicit: vec![kind],
            ..account(relation, Vec::new())
        };
        assert_eq!(
            hedges(implicit(Relation::Calls).verdict()),
            [Hedge::Implicit(kind)]
        );
        assert!(
            implicit(Relation::Reads).verdict().is_complete(),
            "{kind:?}"
        );
    }
}

#[test]
fn what_is_unread_or_unbounded_blocks_every_relation() {
    let file = path("app/y.py");
    let loss = Loss::ReadFailed {
        path: file.clone(),
        side: SideTag::Head,
    };
    for relation in Relation::ALL {
        let blocks = |account: Account| !account.verdict().is_complete();
        let base = || account(relation, Vec::new());
        let unread = Unread {
            file: file.clone(),
            loss: loss.clone(),
        };
        let lost = Account {
            unread: vec![unread],
            ..base()
        };
        assert_eq!(
            hedges(lost.verdict())[0].to_string(),
            "app/y.py was not read"
        );
        let bound = AliasBound::exceeded(MAX_ALIAS_HOPS + 1, 2);
        assert!(blocks(Account {
            alias_bound: bound,
            ..base()
        }));
        assert!(blocks(Account {
            too_common: Some(900),
            ..base()
        }));
    }
}

#[test]
fn a_not_ours_names_its_row_by_its_witness() {
    let witnesses = [
        (Witness::Local(Span::default()), "local"),
        (Witness::Builtin(path("app/x.py")), "builtin"),
        (Witness::Compound(Name::new("lint.sh")), "builtin"),
        (Witness::CommandWord(Name::new("cat")), "builtin"),
        (Witness::ExternalName(Name::new("f")), "external_name"),
        (Witness::OtherProject(path("app/x.py")), "other_project"),
        (Witness::BareName(vec![Name::new("C")]), "bare_name"),
        (Witness::Namespace(Name::new("f")), "other_module"),
        (Witness::Statement(Name::new("drop")), "statement"),
    ];
    for (witness, row) in witnesses {
        let outcome = not_ours(witness);
        assert_eq!(outcome.as_str(), "not_ours");
        assert_eq!(entry(1, None, outcome).basis_str(), row);
    }
    assert_eq!(entry(1, None, Outcome::Ours).basis_str(), "resolved");
}

fn accounted(hedges: Vec<(Relation, Hedge)>) -> Accounted {
    Accounted {
        hedges,
        definitions: 1,
        unused: true,
        elsewhere: HashSet::new(),
        quoted: 0,
    }
}

fn candidate_at(line: u32, bytes: std::ops::Range<u32>, reason: Reason) -> Hedge {
    let file = path("app/x.py");
    Hedge::Candidate {
        file,
        line,
        bytes,
        reason,
    }
}

fn settled(line: u32, relation: Relation) -> Settled<'static> {
    Settled {
        file: "app/x.py",
        line,
        site: 8..12,
        relation,
    }
}

fn same(relation: Relation, _: &Hedge) -> Relation {
    relation
}

#[test]
fn a_hedge_seals_only_the_relation_it_stands_in() {
    let clear = accounted(Vec::new()).settle(&[], same);
    assert_eq!(clear.hedges, []);
    assert!(
        clear
            .by_relation
            .iter()
            .all(|(_, sealed)| sealed.proof().is_some())
    );
    let none = Accounted {
        definitions: 0,
        ..accounted(Vec::new())
    };
    let nothing = none.settle(&[], same);
    assert!(nothing.by_relation.iter().all(|(_, s)| s.proof().is_none()));
    let hedge = candidate_at(3, 10..11, Reason::Unexplained);
    let stood = accounted(vec![(Relation::TypeUses, hedge.clone())]);
    let moved = |relation, _: &Hedge| match relation {
        Relation::TypeUses => Relation::Instantiates,
        other => other,
    };
    for (settlement, standing) in [
        (stood.settle(&[], same), Relation::TypeUses),
        (stood.settle(&[], moved), Relation::Instantiates),
    ] {
        assert_eq!(settlement.hedges, [(standing, hedge.clone())]);
        for (relation, sealed) in &settlement.by_relation {
            assert_eq!(
                sealed.proof().is_none(),
                *relation == standing,
                "{relation:?}"
            );
            assert_eq!(sealed.hedges().len(), usize::from(*relation == standing));
        }
    }
}

#[test]
fn a_resolved_use_settles_the_candidate_in_its_own_span_and_counts_the_one_beside() {
    let (own, beside, elsewhere) = (
        candidate_at(3, 10..11, resolver()),
        candidate_at(3, 30..31, resolver()),
        candidate_at(4, 10..11, resolver()),
    );
    let account = accounted(
        [own.clone(), beside, elsewhere.clone()]
            .map(|hedge| (Relation::Calls, hedge))
            .to_vec(),
    );
    let settlement = account.settle(&[settled(3, Relation::Calls)], same);
    assert_eq!(
        settlement.hedges,
        [
            (Relation::Calls, Hedge::Unlisted { occurrences: 1 }),
            (Relation::Calls, elsewhere),
        ]
    );
    let other_file = Settled {
        file: "app/y.py",
        ..settled(3, Relation::Calls)
    };
    let unsettled = account.settle(&[settled(3, Relation::Reads), other_file], same);
    assert_eq!(unsettled.hedges.len(), 3);
    assert_eq!(unsettled.hedges[0].1, own);
}

#[test]
fn an_unexplained_name_on_a_resolved_line_is_the_use_that_settled_it() {
    for reason in [Reason::Unexplained, Reason::WideBinding] {
        let account = accounted(vec![(Relation::Calls, candidate_at(3, 40..41, reason))]);
        let settlement = account.settle(&[settled(3, Relation::Calls)], same);
        assert!(settlement.hedges.is_empty(), "{reason:?}");
    }
    let leak = Hedge::Leak {
        file: path("app/x.py"),
        line: 3,
        place: ValuePlace::Argument,
        callee: None,
    };
    let account = accounted(vec![(Relation::Calls, leak.clone())]);
    let settlement = account.settle(&[settled(3, Relation::Calls)], same);
    assert_eq!(settlement.hedges, [(Relation::Calls, leak)]);
}
