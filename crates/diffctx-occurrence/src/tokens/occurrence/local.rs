//! The account of a local: a name its enclosing body binds, which nothing outside that body can
//! spell. Every spelling in the file is one record, sealed like a definition's.

use std::collections::HashSet;
use std::ops::Range;

use diffctx_facts::facts::{DefKind, Fact, FileFacts, Local, Span};
use diffctx_resolve::resolve::{Relation, USES};

use super::debug::quoted;
use super::{
    Account, Accounted, Basis, Case, Entry, Outcome, Reason, Sealed, Verdict, Witness, scan,
};

/// The sealed account of `local`, which lives in `body`. `calls` are the spellings the scope reading
/// found calling it, `hidden` those another local of the name holds (with its site), `beyond` those it
/// puts where the local is not in force. Any other spelling in the body is a candidate; one outside it
/// is another name.
pub fn account_local(
    file: &FileFacts,
    text: &[u8],
    local: &Local,
    body: &Range<u32>,
    calls: &[Range<u32>],
    hidden: &[(Range<u32>, Span)],
    beyond: &[Range<u32>],
) -> [Sealed; 6] {
    let found = scan(
        file,
        text,
        &[local.name.as_str()],
        Case::of(file.language()),
    );
    let lost = found.lost;
    let declared = |at: &Range<u32>| {
        file.blob.facts.iter().find_map(|fact| match fact {
            Fact::LocalBinding(bound) if bound.site.bytes == *at => Some(Basis::LocalSite),
            Fact::Definition(def) if def.name_span.bytes == *at => Some(Basis::DefinitionName),
            _ => None,
        })
    };
    let not_ours = |witness| (None, Outcome::NotOurs { witness }, Basis::Witnessed);
    let read = |at: &Range<u32>| {
        if lost {
            return (None, Outcome::Candidate(Reason::FileLost), Basis::Unproven);
        }
        if let Some(basis) = declared(at) {
            return (None, Outcome::Declaration, basis);
        }
        if calls.contains(at) {
            return (Some(Relation::Calls), Outcome::Ours, Basis::Resolved);
        }
        if let Some((_, site)) = hidden.iter().find(|(held, _)| held == at) {
            return not_ours(Witness::Local(site.clone()));
        }
        if beyond.contains(at) || at.start < body.start || body.end < at.end {
            return not_ours(Witness::Scope(local.site.clone()));
        }
        (
            None,
            Outcome::Candidate(Reason::Unexplained),
            Basis::Unproven,
        )
    };
    let entries: Vec<Entry> = found
        .code
        .into_iter()
        .map(|occurrence| {
            let (relation, outcome, basis) = read(&occurrence.bytes);
            Entry {
                occurrence,
                relation,
                outcome,
                basis,
                passed_to: None,
            }
        })
        .collect();
    #[cfg(debug_assertions)]
    if let Some(mut ledger) =
        super::identity::Ledger::open(&format!("{}::{}", file.path, local.name.as_str()), "head")
    {
        ledger.name(&local.name, None);
        ledger.file(
            &local.name,
            file,
            text,
            &entries,
            found.mentions.len(),
            found.comments,
        );
        ledger.finish(0);
    }
    let hedges = USES
        .into_iter()
        .flat_map(
            |relation| match Account::over(relation, DefKind::Function, &entries).verdict() {
                Verdict::Partial(stuck) => stuck
                    .into_iter()
                    .map(|hedge| (relation, hedge))
                    .collect::<Vec<_>>(),
                Verdict::Complete(_) => Vec::new(),
            },
        )
        .collect();
    let used = entries.iter().any(|entry| {
        !matches!(
            entry.outcome,
            Outcome::Declaration | Outcome::NotOurs { .. }
        )
    });
    let accounted = Accounted {
        hedges,
        definitions: 1,
        unused: !used,
        elsewhere: HashSet::new(),
        quoted: (found.mentions.iter())
            .filter(|mention| quoted(text, &mention.bytes))
            .count(),
    };
    accounted.settle(&[], |relation, _| relation).by_relation
}
