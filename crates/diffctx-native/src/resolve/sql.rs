//! SQL: the routines and views of the repository's SQL files, and the calls and
//! reads of them, bound (#459).
//!
//! A name is bound by its spelling and its schema, never by the file: a
//! migration that redefines a routine (`CREATE OR REPLACE` in a later file) is
//! the same object, so a use lands on every definition of it, one binding for
//! each. A name written with a schema reaches the definitions of that schema; an
//! unqualified name reaches the `public` ones, and a definition of another schema
//! only as a `Candidate` (`SearchPath`): the schemas it resolves through are the
//! session's, which no file states. A call lands on routines and a read on views;
//! a name of another schema than every definition's is no use of them. A name no
//! file defines is the database's or an extension's: `External`.

use std::collections::HashMap;

use super::{
    Binding, CandidateReason, Definition, Evidence, EvidenceKind, Relation, Resolution, Solved,
    UnresolvedReason,
};
use crate::facts::{
    DefKey, DefKind, Fact, FactsCache, FileFacts, Language, Name, Ref, UseKind, expired, facts_of,
};
use crate::snapshot::{AnalysisContext, Loss, Result, SideTag, Stage};

/// The bindings of one snapshot's SQL files.
pub struct SqlResolver {
    solved: Solved,
}

impl SqlResolver {
    /// Resolves every SQL file of `side`.
    pub fn open(ctx: &AnalysisContext, side: SideTag, cache: &FactsCache) -> Result<Self> {
        let reader = match side {
            SideTag::Base => &ctx.base,
            SideTag::Head => &ctx.head,
        };
        let listing = cache.listing(reader)?;
        let mut files: Vec<FileFacts> = Vec::new();
        let mut losses = Vec::new();
        let mut read = Vec::new();
        for entry in &listing.entries {
            if Language::from_path(&entry.path) != Some(Language::Sql) {
                continue;
            }
            if expired(&ctx.run) {
                losses.push(Loss::Deadline {
                    stage: Stage::Search,
                });
                break;
            }
            if let Some(file) = facts_of(ctx, side, cache, &entry.path)? {
                losses.extend(file.losses());
                read.push(entry.path.clone());
                files.push(file);
            }
        }
        let mut definitions = Vec::new();
        let mut by_name: HashMap<(Name, bool), Vec<DefKey>> = HashMap::new();
        for file in &files {
            for def in file.defs() {
                let key = file.key(&def.local);
                by_name
                    .entry((def.local.name.clone(), is_routine(def.kind)))
                    .or_default()
                    .push(key.clone());
                definitions.push(Definition {
                    file: file.path.clone(),
                    key,
                    kind: def.kind,
                    span: def.span.clone(),
                    name_span: def.name_span.clone(),
                });
            }
        }
        let mut bindings = Vec::new();
        for file in &files {
            for fact in &file.blob.facts {
                if let Fact::Reference(r) = fact {
                    let call = r.use_kind == UseKind::Call;
                    let relation = if call {
                        Relation::Calls
                    } else {
                        Relation::Reads
                    };
                    let found = by_name.get(&(r.name.clone(), call));
                    bind(file, r, relation, found, &mut bindings);
                }
            }
        }
        Ok(Self {
            solved: Solved::new(definitions, bindings, losses, read),
        })
    }
}

solved_by!(SqlResolver);

fn is_routine(kind: DefKind) -> bool {
    kind == DefKind::Function
}

/// How a spelled schema reaches a definition's: the same schema, or both the
/// default one, is a resolution; an unqualified name against another schema is
/// the search path's to say.
fn reach(written: Option<&Name>, defined: Option<&Name>) -> Option<bool> {
    let public = |s: Option<&Name>| s.is_none_or(|s| s.as_str() == "public");
    match (written, defined) {
        (a, b) if a == b => Some(true),
        (a, b) if public(a) && public(b) => Some(true),
        (None, _) | (_, None) => Some(false),
        _ => None,
    }
}

fn bind(
    file: &FileFacts,
    r: &Ref,
    relation: Relation,
    found: Option<&Vec<DefKey>>,
    out: &mut Vec<Binding>,
) {
    let written = r.qualifier.last();
    let mut any = false;
    for key in found.into_iter().flatten() {
        let Some(resolved) = reach(written, key.local.container.last()) else {
            continue;
        };
        any = true;
        let resolution = if resolved {
            Resolution::Resolved {
                target: key.clone(),
                evidence: Evidence::new(if written.is_some() {
                    EvidenceKind::ModuleAttribute
                } else {
                    EvidenceKind::SameScope
                }),
            }
        } else {
            Resolution::Candidate {
                targets: vec![key.clone()],
                reason: CandidateReason::SearchPath,
            }
        };
        out.push(binding(file, r, relation, resolution));
    }
    if !any {
        out.push(binding(
            file,
            r,
            relation,
            Resolution::Unresolved {
                reason: UnresolvedReason::External,
            },
        ));
    }
}

fn binding(file: &FileFacts, r: &Ref, relation: Relation, resolution: Resolution) -> Binding {
    Binding {
        file: file.path.clone(),
        site: r.site.clone(),
        from: r.enclosing.clone(),
        relation,
        name: r.name.clone(),
        resolution,
        entry_guard: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_schema_is_matched_or_left_to_the_search_path() {
        let lib = Name::new("lib");
        let public = Name::new("public");
        let other = Name::new("other");
        assert_eq!(reach(Some(&lib), Some(&lib)), Some(true));
        assert_eq!(reach(None, Some(&public)), Some(true));
        assert_eq!(reach(None, None), Some(true));
        assert_eq!(reach(None, Some(&lib)), Some(false));
        assert_eq!(reach(Some(&lib), None), Some(false));
        assert_eq!(reach(Some(&other), Some(&lib)), None);
    }
}
