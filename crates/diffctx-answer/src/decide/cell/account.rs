//! The verdict of a changed definition and the occurrence accounting behind it.

use std::cell::OnceCell;

use std::rc::Rc;

use rustc_hash::FxHashSet;

use crate::fold::{self};

use diffctx_facts::facts::{DefKey, DefKind, Fact, Name, Scope, facts_of};

use diffctx_occurrence::tokens::occurrence::debug::{Shadow, sink};

use diffctx_occurrence::tokens::occurrence::{Known, Standing, View};

use diffctx_resolve::resolve::{Binding, Definition, Relation, Resolution, UnresolvedReason};

use diffctx_resolve::vocabulary::family::{Family, Resolver};

use diffctx_snapshot::snapshot::{RepoPath, SideTag};

use super::super::gap::Gap;

use super::super::losses::{lost_in, stopped_by};

use super::super::verdict::{Accounting, Reach, Use, Verdict};

use super::super::changed::Changed;

use super::super::cells::strands;

use super::Cell;
use super::Ground;
use super::MAX_UNPLACED_LISTED;
use super::REBOUND;
use super::UNTYPED_RECEIVER;
use super::loads_lazily;
use super::root_scope;
use super::standing_of;
use super::within;
use super::within_scope;

impl Cell {
    /// Who uses the definition, and what the answer cannot say about it.
    pub(crate) fn verdict(&self, changed: &Changed) -> Verdict {
        let mut uses: Vec<Use> = Vec::new();
        for subject in &changed.subjects {
            for b in self.head.references_to(&subject.key) {
                if !within_scope(&self.scope, b.file.as_str()) {
                    continue;
                }
                let reaches = subject.reach.admits(b.relation)
                    || (subject.reach == Reach::Every
                        && (loads_lazily(b) || self.names_type(b) || self.reexports(b)));
                // The guard of a script runs its entry function; a use in its own
                // body is not a caller of it. The rest of the file's definitions
                // are used there like anywhere else, and so is anything the guard
                // of another file runs.
                if !reaches
                    || self.leaves_out(b, subject)
                    || (b.entry_guard && self.runs_as_entry(changed, subject, b))
                    || subject.within.is_some_and(|own| {
                        b.file.as_str() == changed.file && within(b.site.lines.0, own)
                    })
                {
                    continue;
                }
                uses.extend(self.use_of(b));
            }
        }
        // What the search could not tell of every relation. A use that landed on
        // nothing is a candidate of the occurrence account, which closes the rest.
        let lead: Vec<Gap> = changed
            .unpaired
            .then_some(Gap::Unpaired)
            .into_iter()
            .collect();
        let overridden = self
            .overridden_via(changed)
            .or_else(|| self.open_world_via(changed, &uses));
        let tail: Vec<Gap> = overridden.iter().map(|_| Gap::NotTraced).collect();
        let (by_relation, found, hedges) = self.coverage(changed, lead, tail, &uses);
        uses.extend(found.callers.iter().cloned());
        uses.sort_by(|a, b| {
            (a.file.as_str(), a.line, a.relation.name()).cmp(&(
                b.file.as_str(),
                b.line,
                b.relation.name(),
            ))
        });
        uses.dedup_by(|a, b| a.file == b.file && a.line == b.line && a.standing == b.standing);
        if let Some(basis) = &changed.basis {
            for u in uses.iter_mut().filter(|u| !u.also_changed) {
                let candidate = matches!(u.standing, Standing::Candidate(_));
                let runtime = self.runtime_annotation(u);
                u.folded = fold::rule(basis, u.relation, candidate, runtime);
            }
        }
        Verdict {
            uses,
            by_relation,
            overridden,
            found,
            hedges,
        }
    }

    /// The occurrence accounting of each definition of `changed`: what stands in the
    /// way of each relation's absence. `gaps` is what else stands in the way of
    /// every relation. Under `DIFFCTX_DEBUG_OCCURRENCES` its lines are written too.
    pub(super) fn occurrences(&self, changed: &Changed, gaps: &[Gap]) -> Rc<Accounting> {
        let key = changed.key();
        let known = key.and_then(|key| self.accounted.borrow().get(key).cloned());
        if let Some(known) = known {
            return known;
        }
        let done = Rc::new(self.account_occurrences(changed, gaps, true));
        if let Some(key) = key {
            self.accounted
                .borrow_mut()
                .insert(key.clone(), Rc::clone(&done));
        }
        done
    }

    /// Whether a head line sits where the base resolved a use to what `gone`
    /// removed. Asked while the changes are read, so nothing of it is memoized or
    /// written.
    pub(super) fn stranded(&self, gone: &Changed) -> bool {
        self.account_occurrences(gone, &[], false).stranded
    }

    /// Whether the account holds a hedge, read like `stranded`: no write, no memo.
    pub(crate) fn hedged(&self, changed: &Changed) -> bool {
        let done = self.account_occurrences(changed, &[], false);
        !done.accounted.hedges().is_empty()
    }

    fn account_occurrences(&self, changed: &Changed, gaps: &[Gap], write: bool) -> Accounting {
        let out = sink().filter(|_| write);
        let Ground { files, index } = self
            .ground
            .get_or_init(|| Ground::read(&self.ctx, &self.cache, &self.head));
        let mut view = View::over(Rc::clone(index), files, self.head.bindings());
        // A base that could not be read leaves no removed definition to account:
        // `base_gap` says so in every relation.
        let mut base = None;
        if !changed.stale.is_empty() {
            base = match self.base() {
                Ok(base) => Some(base),
                Err(gap) => {
                    tracing::debug!("a removed definition is not accounted, {gap}");
                    None
                }
            };
            view = view.removed_sites(
                base.map(Resolver::bindings),
                &changed.stale,
                |b| match (self.family, standing_of(&b.resolution)) {
                    (Family::Python, _) | (_, None) => Standing::Resolved,
                    (_, Some(standing)) => standing,
                },
                |b| self.moved(b),
            );
        }
        let names = self.reference_names(changed);
        let reach = |file: &str| {
            within_scope(&self.scope, file) && (!changed.local || file == changed.file)
        };
        let outside: Vec<RepoPath> = self
            .outside_group
            .iter()
            .flat_map(|file| RepoPath::new(file))
            .collect();
        // A holder's line says whether it is whole: its own rows are and what it
        // holds is too, and unused. Only the line needs it; the answer asks a
        // holder's held definitions itself.
        let held: Vec<&Changed> = self.changed.iter().filter(|h| changed.holds(h)).collect();
        let nested = out.is_none()
            || held.iter().all(|inner| {
                let verdict = self.verdict(inner);
                verdict.complete() && verdict.unused()
            });
        let (fixed, moving) = (self.fixed_losses(), self.moving_losses());
        let lost = lost_in(fixed, &moving);
        #[cfg(debug_assertions)]
        let pathspec = |file: &str| within_scope(&self.scope, file);
        let shown: Vec<&dyn std::fmt::Display> = (gaps.iter())
            .map(|gap| gap as &dyn std::fmt::Display)
            .collect();
        let shadow = Shadow {
            ctx: &self.ctx,
            cache: &self.cache,
            family: self.family,
            view: &view,
            files,
            lost: &lost,
            tokens: &self.tokens,
            embedded: OnceCell::new(),
            reach: &reach,
            outside: &outside,
            names: &names,
            gaps: &shown,
            held: held.len(),
            nested,
            #[cfg(debug_assertions)]
            identity: write.then(
                || diffctx_occurrence::tokens::occurrence::identity::Bounds {
                    pathspec: &pathspec,
                    own_file: changed.local.then_some(changed.file.as_str()),
                    outside_scope: (gaps.iter()).any(|gap| matches!(gap, Gap::OutsideScope { .. })),
                },
            ),
        };
        let heads = changed
            .subjects
            .iter()
            .filter_map(|subject| self.definition(&subject.key))
            .map(|definition| (definition, SideTag::Head));
        let removed = base
            .into_iter()
            .flat_map(Resolver::definitions)
            .filter(|definition| changed.stale.contains(&definition.key))
            .map(|definition| (definition, SideTag::Base));
        let (callers, stranded) = self.stranded_uses(&view, changed);
        let mut written = self.shadow_written.borrow_mut();
        let known = |definition: &Definition| Known {
            dispatched: self.dispatched_via(definition).is_some(),
            consumed_outside: self.consumed_outside(),
            root_module: self.head.root_module(definition.file.as_str()),
            exported: false,
        };
        let accounted = shadow.write_all(
            out,
            &mut written,
            heads
                .chain(removed)
                .map(|(definition, side)| (definition, side, known(definition))),
        );
        Accounting {
            accounted,
            callers,
            stranded,
        }
    }

    /// Whether the range's own new lines spell the use's name in its file: they moved the use
    /// themselves.
    fn moved(&self, b: &Binding) -> bool {
        self.added.get(b.file.as_str()).is_some_and(|lines| {
            (lines.iter()).any(|(_, text)| {
                (text.split(|c: char| !(c.is_alphanumeric() || c == '_')))
                    .any(|word| word == b.name.as_str())
            })
        })
    }

    /// Where a removed definition's callers stand now. A head line at a place the base
    /// resolved a use to it is a caller whatever the head lands it on: nothing (a
    /// name that finds nothing) or another definition (a re-export, an inherited or
    /// shadowing name, a candidate, unless the range's own new lines spell the name:
    /// they moved the use themselves). An import is named only when no use is.
    fn stranded_uses(&self, view: &View, changed: &Changed) -> (Vec<Use>, bool) {
        if changed.stale.is_empty() {
            return (Vec::new(), false);
        }
        let ours: Vec<&DefKey> = changed.subjects.iter().map(|s| &s.key).collect();
        let mut found: Vec<(&Binding, Standing)> = view
            .stranded()
            .filter(|(b, _)| {
                strands(self.family, b.relation)
                    && !b.entry_guard
                    && within_scope(&self.scope, b.file.as_str())
                    && !b.targets().iter().any(|target| ours.contains(target))
            })
            .filter_map(|(b, standing)| match b.resolution {
                Resolution::Unresolved { .. } => Some((b, standing)),
                _ if standing == Standing::Resolved && !self.moved(b) => {
                    Some((b, Standing::Candidate(REBOUND)))
                }
                _ => None,
            })
            .collect();
        // The base never placed these: lines that still spell the name where nothing
        // lands it, in files that may reach it, may each be a use that broke. An
        // untyped read of a top-level TypeScript name is none: only a member is read so.
        let top_level = self.family == Family::TypeScript
            && changed
                .key()
                .is_some_and(|key| key.local.container.is_empty());
        let stranded = !found.is_empty();
        let placed: FxHashSet<(&str, u32)> = (found.iter())
            .map(|(b, _)| (b.file.as_str(), b.site.lines.0))
            .collect();
        let unplaced: Vec<&Binding> = (self.head.bindings().iter())
            .filter(|b| {
                let Resolution::Unresolved { reason } = b.resolution else {
                    return false;
                };
                let read = reason == UnresolvedReason::UntypedRead
                    || (self.family == Family::TypeScript
                        && matches!(
                            reason,
                            UnresolvedReason::NoCandidates | UnresolvedReason::TooManyCandidates
                        ));
                !changed.unpaired
                    && b.name.as_str() == changed.name
                    && b.relation.uses()
                    && reason.is_unknown()
                    && !(top_level && read)
                    && within_scope(&self.scope, b.file.as_str())
                    && self
                        .ctx
                        .workspace
                        .may_reference(b.file.as_str(), &changed.file)
                    && !placed.contains(&(b.file.as_str(), b.site.lines.0))
            })
            .collect();
        if unplaced.len() <= MAX_UNPLACED_LISTED {
            let standing = Standing::Candidate(UNTYPED_RECEIVER);
            found.extend(unplaced.into_iter().map(|b| (b, standing)));
        }
        let used: FxHashSet<(&RepoPath, &Name)> = (found.iter())
            .filter(|(b, _)| b.relation != Relation::Imports)
            .map(|(b, _)| (&b.file, &b.name))
            .collect();
        let uses = found
            .into_iter()
            .filter(|(b, _)| b.relation != Relation::Imports || !used.contains(&(&b.file, &b.name)))
            .map(|(b, standing)| Use {
                also_changed: false,
                file: b.file.as_str().to_string(),
                line: b.site.lines.0,
                site: b.site.bytes.clone(),
                relation: b.relation,
                standing,
                scope: b.from.clone(),
                folded: None,
                sharing: 0,
            })
            .collect();
        (uses, stranded)
    }

    /// What kept the base side of the range from saying all of it: it was not
    /// resolved, what it read was cut short, or it lacks a name the range moved.
    /// A removed or moved definition's uses are accounted at the head, but only
    /// against a base that was read and holds what is owed.
    pub(super) fn base_gap(&self, changed: &Changed) -> Option<Gap> {
        if changed.stale.is_empty() {
            return None;
        }
        let base = match self.base() {
            Ok(base) => base,
            Err(gap) => return Some(gap),
        };
        let reasons = stopped_by(base.losses());
        if !reasons.is_empty() {
            return Some(Gap::Stopped { reasons });
        }
        let missing =
            (changed.stale.iter()).any(|key| base.definitions().iter().all(|d| d.key != *key));
        missing.then(|| Gap::BaseUnresolved {
            reason: "a definition the range moved is not among the base's".to_string(),
        })
    }

    /// Whether a head file of the family holds an import whose target the code
    /// computes. A file whose facts cannot be read is not known to hold none.
    pub(crate) fn computes_an_import(&self) -> bool {
        self.files.iter().any(|file| {
            RepoPath::new(file).map_or(true, |path| {
                facts_of(&self.ctx, SideTag::Head, &self.cache, &path).map_or(true, |facts| {
                    facts.is_some_and(|f| {
                        f.blob
                            .facts
                            .iter()
                            .any(|fact| matches!(fact, Fact::ComputedImport(_)))
                    })
                })
            })
        })
    }

    /// Whether a type use is an annotation a framework may execute at run time
    /// (Python): a class body's field annotation, one in the signature of a
    /// decorated function, or any in a file that imports pydantic, fastapi,
    /// dataclasses, attrs, marshmallow, typer or click. Unsure counts as yes.
    fn runtime_annotation(&self, u: &Use) -> bool {
        if self.family != Family::Python || u.relation != Relation::TypeUses {
            return false;
        }
        if self.imports_runtime_library(&u.file) {
            return true;
        }
        let Scope::Def(_) = root_scope(&u.scope) else {
            return false;
        };
        let Some(key) = self.key_in(&u.file, &u.scope) else {
            return true;
        };
        let Some(def) = self.definition(&key) else {
            return true;
        };
        match def.kind {
            DefKind::Class => true,
            DefKind::Field | DefKind::Variable | DefKind::Constant => {
                !key.local.container.is_empty()
            }
            DefKind::Function | DefKind::Method | DefKind::Constructor => RepoPath::new(&u.file)
                .map_or(true, |file| {
                    self.declared(SideTag::Head, &file, &key)
                        .map_or(true, |found| {
                            found.is_some_and(|d| !d.decorators.is_empty())
                        })
                }),
            _ => false,
        }
    }

    fn imports_runtime_library(&self, file: &str) -> bool {
        const LIBRARIES: [&str; 7] = [
            "pydantic",
            "fastapi",
            "dataclasses",
            "attrs",
            "attr",
            "marshmallow",
            "typer",
        ];
        if let Some(known) = self.runtime_files.borrow().get(file) {
            return *known;
        }
        let found = RepoPath::new(file).map_or(true, |path| {
            facts_of(&self.ctx, SideTag::Head, &self.cache, &path).map_or(true, |facts| {
                facts.is_some_and(|f| {
                    f.blob.facts.iter().any(|fact| match fact {
                        Fact::Import(i) => {
                            let root = i.module.as_str().split('.').next().unwrap_or("");
                            LIBRARIES.contains(&root) || root == "click"
                        }
                        _ => false,
                    })
                })
            })
        });
        self.runtime_files
            .borrow_mut()
            .insert(file.to_string(), found);
        found
    }
}
