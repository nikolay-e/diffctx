use crate::impact::output::{ChangedSymbol, Unknown};

use super::{
    Change, ChangeKind, Count, Definition, Fold, Gap, Guard, LocalOf, Row, Search, Searched, refs,
};

pub(super) fn definition(c: &ChangedSymbol, tests_known: bool) -> Definition {
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
        // lines to trace, candidates at their sites, a member of a trait, a run
        // stopped before the search.
        Some(Unknown::CommonWord { name, lines }) => Some(Gap::Unlanded {
            name: name.clone(),
            uses: *lines,
            sites: Vec::new(),
        }),
        Some(Unknown::Candidates { name, uses, sites }) => Some(Gap::Unlanded {
            name: name.clone(),
            uses: *uses,
            sites: sites.clone(),
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
