//! The contract types of absence by occurrences, on hand-built accounts: what a `Complete` verdict and a
//! `not_ours` row stand on. Only this crate can build them, so these tests live here; every
//! other test of the accounting reads a real analyzer run.

use diffctx_facts::facts::{DefKey, Language, LocalKey, ModulePath, Name, ProjectId, Span};
use diffctx_resolve::resolve::{CandidateReason, Relation};
use diffctx_snapshot::snapshot::{Loss, RepoPath, SideTag};

use super::{
    Account, Alias, AliasBound, AliasScope, Basis, Class, Entry, Hedge, Implicit, MAX_ALIAS_HOPS,
    Occurrence, Outcome, Reason, Unread, ValuePlace, Verdict, Witness,
};
use super::{Accounted, Settled};

fn path(name: &str) -> RepoPath {
    RepoPath::new(name).expect("a repository path")
}

fn key() -> DefKey {
    DefKey {
        project: ProjectId::default(),
        language: Language::Python,
        module: ModulePath::new("sample"),
        local: LocalKey::value("f"),
    }
}

fn account(relation: Relation) -> Account {
    Account::new(key(), relation, SideTag::Head)
}

fn entry(line: u32, relation: Option<Relation>, outcome: Outcome, basis: Basis) -> Entry {
    Entry {
        occurrence: Occurrence {
            file: path("app/x.py"),
            bytes: 10..11,
            line,
            class: Class::Code,
        },
        relation,
        outcome,
        basis,
        passed_to: None,
    }
}

fn candidate(line: u32, relation: Option<Relation>) -> Entry {
    entry(
        line,
        relation,
        Outcome::Candidate(Reason::Resolver(CandidateReason::UntypedReceiver)),
        Basis::Unproven,
    )
}

fn hedges(verdict: Verdict) -> Vec<Hedge> {
    match verdict {
        Verdict::Complete(_) => Vec::new(),
        Verdict::Partial(hedges) => hedges,
    }
}

#[test]
fn an_account_with_every_occurrence_explained_is_complete() {
    let mut account = account(Relation::Calls);
    account.entries = vec![
        entry(1, Some(Relation::Calls), Outcome::Ours, Basis::Resolved),
        entry(2, None, Outcome::Declaration, Basis::DefinitionName),
        entry(
            3,
            None,
            Outcome::NotOurs {
                witness: Witness::Local(Span::default()),
            },
            Basis::Witnessed,
        ),
        entry(
            4,
            None,
            Outcome::Alias(Alias {
                name: "g".into(),
                scope: AliasScope::File(path("app/x.py")),
                hops: 1,
            }),
            Basis::RenamedImport,
        ),
    ];
    account.mentions = 3;
    assert!(account.verdict().is_complete());
    assert_eq!(account.verdict().as_str(), "complete");
    assert!(account_of_nothing().verdict().is_complete());
}

fn account_of_nothing() -> Account {
    account(Relation::Reads)
}

#[test]
fn a_candidate_names_its_file_and_line() {
    let mut account = account(Relation::Calls);
    account.entries = vec![candidate(7, Some(Relation::Calls))];
    let verdict = account.verdict();
    assert_eq!(verdict.as_str(), "partial");
    let hedges = hedges(verdict);
    assert_eq!(hedges.len(), 1);
    assert_eq!(hedges[0].to_string(), "candidate at app/x.py:7");
}

#[test]
fn a_candidate_blocks_the_relation_it_carries_and_all_when_it_carries_none() {
    for relation in Relation::ALL {
        let mut other = account(relation);
        other.entries = vec![candidate(1, Some(Relation::Calls))];
        assert_eq!(
            other.verdict().is_complete(),
            relation != Relation::Calls,
            "a calls candidate against {relation:?}"
        );
        let mut open = account(relation);
        open.entries = vec![candidate(1, None)];
        assert!(
            !open.verdict().is_complete(),
            "no relation against {relation:?}"
        );
    }
}

#[test]
fn a_leak_makes_calls_partial_and_names_the_place() {
    let leak = entry(
        5,
        Some(Relation::PassesAsValue),
        Outcome::Leak(ValuePlace::Argument),
        Basis::ValuePosition,
    );
    let mut calls = account(Relation::Calls);
    calls.entries = vec![leak.clone()];
    let hedges = hedges(calls.verdict());
    assert_eq!(hedges.len(), 1);
    assert_eq!(hedges[0].to_string(), "leak at app/x.py:5");
    let mut reads = account(Relation::Reads);
    reads.entries = vec![leak];
    assert!(reads.verdict().is_complete());
}

#[test]
fn each_implicit_kind_blocks_calls_and_only_calls() {
    for kind in Implicit::ALL {
        let mut calls = account(Relation::Calls);
        calls.implicit = vec![kind];
        assert_eq!(hedges(calls.verdict()), vec![Hedge::Implicit(kind)]);
        let mut reads = account(Relation::Reads);
        reads.implicit = vec![kind];
        assert!(reads.verdict().is_complete(), "{kind:?} against reads");
    }
    let names: Vec<&str> = Implicit::ALL.iter().map(|kind| kind.as_str()).collect();
    let mut distinct = names.clone();
    distinct.sort_unstable();
    distinct.dedup();
    assert_eq!(distinct.len(), names.len());
}

#[test]
fn what_is_unread_or_unbounded_blocks_every_relation() {
    for relation in Relation::ALL {
        let mut unread = account(relation);
        unread.unread = vec![Unread {
            file: path("app/y.py"),
            loss: Loss::ReadFailed {
                path: path("app/y.py"),
                side: SideTag::Head,
            },
        }];
        assert_eq!(
            hedges(unread.verdict())[0].to_string(),
            "app/y.py was not read"
        );
        let mut bound = account(relation);
        bound.alias_bound = AliasBound::exceeded(MAX_ALIAS_HOPS + 1, 2);
        assert!(!bound.verdict().is_complete());
        let mut common = account(relation);
        common.too_common = Some(900);
        assert!(!common.verdict().is_complete());
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
        let entry = entry(1, None, Outcome::NotOurs { witness }, Basis::Witnessed);
        assert_eq!(entry.basis_str(), row);
    }
    assert_eq!(
        entry(1, None, Outcome::Ours, Basis::Resolved).basis_str(),
        "resolved"
    );
    assert_eq!(
        Outcome::NotOurs {
            witness: Witness::Local(Span::default()),
        }
        .as_str(),
        "not_ours"
    );
    assert_eq!(
        Outcome::Candidate(Reason::Unexplained).as_str(),
        "candidate"
    );
}

fn accounted(hedges: Vec<(Relation, Hedge)>) -> Accounted {
    Accounted {
        hedges,
        unused: true,
        elsewhere: Default::default(),
        quoted: 0,
    }
}

fn candidate_at(line: u32, bytes: std::ops::Range<u32>, reason: Reason) -> Hedge {
    Hedge::Candidate {
        file: path("app/x.py"),
        line,
        bytes,
        reason,
    }
}

fn settled(line: u32, site: std::ops::Range<u32>, relation: Relation) -> Settled<'static> {
    Settled {
        file: "app/x.py",
        line,
        site,
        relation,
    }
}

fn sealed_of(settlement: &super::Settlement, relation: Relation) -> &super::Sealed {
    &settlement
        .by_relation()
        .iter()
        .find(|(of, _)| *of == relation)
        .expect("a relation of use")
        .1
}

#[test]
fn an_account_without_a_hedge_seals_complete_for_every_relation() {
    let settlement = accounted(Vec::new()).settle(&[], false);
    for (relation, sealed) in settlement.by_relation() {
        assert!(sealed.proof().is_some(), "{relation:?}");
        assert!(sealed.hedges().is_empty());
    }
    assert!(settlement.hedges.is_empty());
}

#[test]
fn a_hedge_seals_only_the_relation_it_stands_against() {
    let hedge = candidate_at(3, 10..11, Reason::Unexplained);
    let settlement = accounted(vec![(Relation::Calls, hedge.clone())]).settle(&[], false);
    for (relation, sealed) in settlement.by_relation() {
        assert_eq!(
            sealed.proof().is_some(),
            *relation != Relation::Calls,
            "{relation:?}"
        );
    }
    assert_eq!(sealed_of(&settlement, Relation::Calls).hedges(), [hedge]);
}

#[test]
fn a_resolved_use_settles_the_candidate_in_its_own_span_and_counts_the_one_beside() {
    let own = candidate_at(
        3,
        10..11,
        Reason::Resolver(CandidateReason::UntypedReceiver),
    );
    let beside = candidate_at(
        3,
        30..31,
        Reason::Resolver(CandidateReason::UntypedReceiver),
    );
    let elsewhere = candidate_at(
        4,
        10..11,
        Reason::Resolver(CandidateReason::UntypedReceiver),
    );
    let account = accounted(vec![
        (Relation::Calls, own),
        (Relation::Calls, beside),
        (Relation::Calls, elsewhere.clone()),
    ]);
    let settlement = account.settle(&[settled(3, 8..12, Relation::Calls)], false);
    assert_eq!(
        settlement.hedges,
        vec![
            (Relation::Calls, Hedge::Unlisted { occurrences: 1 }),
            (Relation::Calls, elsewhere),
        ]
    );
}

#[test]
fn a_resolved_use_of_another_relation_or_file_settles_nothing() {
    let hedge = candidate_at(
        3,
        10..11,
        Reason::Resolver(CandidateReason::UntypedReceiver),
    );
    let account = accounted(vec![(Relation::Calls, hedge.clone())]);
    let other_relation = settled(3, 8..12, Relation::Reads);
    let other_file = Settled {
        file: "app/y.py",
        ..settled(3, 8..12, Relation::Calls)
    };
    let settlement = account.settle(&[other_relation, other_file], false);
    assert_eq!(settlement.hedges, vec![(Relation::Calls, hedge)]);
}

#[test]
fn an_unexplained_name_on_a_resolved_line_is_the_use_that_settled_it() {
    for reason in [Reason::Unexplained, Reason::WideBinding] {
        let account = accounted(vec![(Relation::Calls, candidate_at(3, 40..41, reason))]);
        let settlement = account.settle(&[settled(3, 8..12, Relation::Calls)], false);
        assert!(settlement.hedges.is_empty(), "{reason:?}");
    }
}

#[test]
fn a_hedge_that_is_no_candidate_is_never_settled_by_a_use() {
    let leak = Hedge::Leak {
        file: path("app/x.py"),
        line: 3,
        place: ValuePlace::Argument,
        callee: None,
    };
    let settlement = accounted(vec![(Relation::Calls, leak.clone())])
        .settle(&[settled(3, 0..99, Relation::Calls)], false);
    assert_eq!(settlement.hedges, vec![(Relation::Calls, leak)]);
}

#[test]
fn an_unread_base_hedges_every_relation_even_when_nothing_else_does() {
    let settlement = accounted(Vec::new()).settle(&[], true);
    for (relation, sealed) in settlement.by_relation() {
        assert!(sealed.proof().is_none(), "{relation:?}");
        assert_eq!(sealed.hedges(), [Hedge::BaseUnread]);
    }
    assert!(settlement.hedges.is_empty());
}
