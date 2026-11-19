//! What the search for a definition's uses covered: its coverage and the names it looked for.

use std::path::Path;

use std::rc::Rc;

use diffctx_facts::facts::{DefKind, Disambig, Name, Scope};

use diffctx_occurrence::tokens::occurrence::{Hedge, Reason as OccurrenceReason, Standing};

use diffctx_resolve::resolve::{Binding, Relation, USES};

use diffctx_resolve::vocabulary::family::{Family, Group, Resolver};

use super::super::gap::Gap;

use super::super::coverage::{Coverage, Evidence};

use super::super::verdict::{Accounting, Use};

use super::super::changed::{Changed, How};

use super::Cell;

impl Cell {
    /// What covered the search of each relation of use: the gaps of every
    /// relation (`lead`, `tail`), the files outside the run's pathspecs that could
    /// name the definition, a read the cell lost, and the hedges of the occurrence
    /// account. `Coverage::close` makes the `Complete`, and so the cell never does.
    /// Whether the definition is unused is the occurrence account's.
    pub(super) fn coverage(
        &self,
        changed: &Changed,
        lead: Vec<Gap>,
        tail: Vec<Gap>,
        uses: &[Use],
    ) -> (
        [(Relation, Coverage); 6],
        Rc<Accounting>,
        Vec<(Relation, Hedge)>,
    ) {
        let mut tail = tail;
        let outside = self.outside_scope(changed);
        if outside > 0 {
            tail.push(Gap::OutsideScope { files: outside });
        }
        // A module's variables and outputs are what a call reaches: a call whose
        // source is outside the repository may be a caller of any of them.
        let external = self.head.external_sources();
        let interface = changed.subjects.iter().any(|s| {
            matches!(
                s.key.local.container.first().map(Name::as_str),
                Some("var" | "output")
            )
        });
        if external > 0 && interface {
            tail.push(Gap::ExternalSource { pointers: external });
        }
        if self.family == Family::Rust {
            let macros = self
                .macro_files
                .iter()
                .filter(|file| self.ctx.workspace.may_reference(file, &changed.file))
                .count();
            if macros > 0 {
                tail.push(Gap::Macros {
                    files: macros,
                    what: "declare items through `cfg_if!`, which is not expanded",
                });
            }
            let dispatched = changed
                .subjects
                .iter()
                .filter_map(|s| self.definition(&s.key))
                .find_map(|def| self.dispatched_via(def));
            if let Some(via) = dispatched {
                tail.push(Gap::Dispatched { via });
            }
        }
        if self.family == Family::C {
            let macros = self
                .macro_files
                .iter()
                .filter(|file| self.ctx.workspace.may_reference(file, &changed.file))
                .count();
            if macros > 0 {
                tail.push(Gap::Macros {
                    files: macros,
                    what: "call or use it through macros, whose expansion is not read",
                });
            }
            // What a build configures may not exist: the definition is under `#if`.
            let conditional = changed.subjects.iter().any(|s| {
                matches!(&s.key.local.disambig, Some(Disambig::Cfg(cond)) if cond != "declaration")
            });
            if conditional {
                tail.push(Gap::Macros {
                    files: 1,
                    what: "define it under conditional compilation, which is not evaluated",
                });
            }
        }
        // What a configuration object's readers could be, outside what the cell
        // reads: a place a file names outside the repository, a name a template
        // writes.
        if let Resolver::Config(config) = &self.head {
            if config.external_sources() > 0 {
                tail.push(Gap::ExternalSource {
                    pointers: config.external_sources(),
                });
            }
            if config.templated_files() > 0 {
                tail.push(Gap::Templated {
                    files: config.templated_files(),
                });
            }
            if config.unbound_values() > 0 {
                tail.push(Gap::ContextUnbound {
                    files: config.unbound_values(),
                });
            }
        }
        // A script that sources a path nothing settles may source any name.
        if let Resolver::Shell(shell) = &self.head {
            if shell.unknown_sources() > 0 {
                tail.push(Gap::SourceFlag {
                    files: shell.unknown_sources(),
                });
            }
        }
        // A name the code computes (`getattr(obj, name)`, `eval`, `globals()[key]`) may
        // be the definition's, and nothing in the text says so.
        let member = changed
            .subjects
            .iter()
            .any(|s| !s.key.local.container.is_empty());
        let classes: Vec<Name> = (changed.subjects.iter())
            .filter_map(|s| s.key.local.container.last().cloned())
            .collect();
        let builds = (changed.subjects.iter())
            .filter_map(|s| self.definition(&s.key))
            .any(|def| matches!(def.kind, DefKind::Class | DefKind::Constructor));
        let reach = |file: &str| self.ctx.workspace.may_reference(file, &changed.file);
        if self.head.name_computers(member, builds, &classes, reach) > 0 {
            tail.push(Gap::NotTraced);
        }
        let reasons = self.stops();
        if !reasons.is_empty() {
            tail.push(Gap::Stopped { reasons });
        }
        let mut gaps = lead;
        gaps.extend(tail);
        if let Some(gap) = self.base_gap(changed).filter(|gap| !gaps.contains(gap)) {
            gaps.push(gap);
        }
        let found = self.occurrences(changed, &gaps);
        // A resolved use settles the occurrences of its own site: the name inside its
        // span, or one no binding names beside it (a keyword argument's name, the
        // second spelling of a shorthand). A candidate in a span of its own is
        // another use that no binding settles, on a line already listed: counted,
        // never listed again. A value the use hands on (returned, collected) still
        // hedges the calls it leaves.
        let resolved: Vec<&Use> = uses
            .iter()
            .chain(&found.callers)
            .filter(|u| u.standing == Standing::Resolved)
            .collect();
        let hedges: Vec<(Relation, Hedge)> = (found.hedges.iter())
            .filter_map(|(relation, hedge)| {
                let Hedge::Candidate {
                    file,
                    line,
                    bytes,
                    reason,
                } = hedge
                else {
                    return Some((*relation, hedge.clone()));
                };
                let beside: Vec<&&Use> = (resolved.iter())
                    .filter(|u| {
                        u.file == file.as_str() && u.line == *line && u.relation == *relation
                    })
                    .collect();
                let own_site = matches!(
                    reason,
                    OccurrenceReason::Unexplained | OccurrenceReason::WideBinding
                ) || beside
                    .iter()
                    .any(|u| u.site.start < bytes.end && bytes.start < u.site.end);
                if beside.is_empty() {
                    Some((*relation, hedge.clone()))
                } else {
                    (!own_site).then_some((*relation, Hedge::Unlisted { occurrences: 1 }))
                }
            })
            .collect();
        let by_relation = USES.map(|relation| {
            let own: Vec<Hedge> = (hedges.iter())
                .filter(|(of, _)| *of == relation)
                .map(|(_, hedge)| hedge.clone())
                .collect();
            let coverage = Coverage::close(Evidence::Occurrences(relation, &own), gaps.clone());
            (relation, coverage)
        });
        (by_relation, found, hedges)
    }

    /// The names a use of the definition could carry in a file of its group: its
    /// own, the ones it had (`stale`), the script or module it is, and what the
    /// neighbouring languages of the group make of it. No length or frequency
    /// floor applies: a short or common name makes a large gap with a count, never
    /// a silent pass. A superset is safe; its cost is hedge share.
    pub(super) fn reference_names(&self, changed: &Changed) -> Vec<String> {
        let mut own: Vec<String> = vec![changed.name.clone()];
        own.extend(
            changed
                .subjects
                .iter()
                .map(|s| s.key.local.name.as_str().to_string()),
        );
        own.extend(
            changed
                .stale
                .iter()
                .map(|key| key.local.name.as_str().to_string()),
        );
        let mut names = own.clone();
        let stem = Path::new(&changed.file)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(str::to_string);
        if changed.name == changed.file {
            names.extend(stem.clone());
        }
        match self.family.group() {
            Group::Jvm => self.jvm_names(changed, &own, stem.as_deref(), &mut names),
            Group::Native => {
                // `A::m` is called as `m`, and a selector `a:b:` by each keyword.
                for name in &own {
                    names.extend(
                        name.split(':')
                            .filter(|part| !part.is_empty())
                            .map(str::to_string),
                    );
                }
            }
            Group::Web => {
                // A default export is imported under any name: only the module's
                // own (the directory's for an `index`) is written.
                if own.iter().any(|name| name == "default") {
                    let module = if stem.as_deref() == Some("index") {
                        Path::new(&changed.file)
                            .parent()
                            .and_then(Path::file_name)
                            .and_then(|dir| dir.to_str())
                            .map(str::to_string)
                    } else {
                        stem
                    };
                    names.extend(module);
                }
            }
            Group::Family(_) | Group::Language(_) => {}
        }
        names.sort();
        names.dedup();
        names
    }

    /// What Kotlin and Java make of each other's names: a property is read as
    /// `getX`, `setX`, `isX` and an accessor as the property; a top-level
    /// declaration of `F.kt` is a member of `FKt`; the members of an `object` are
    /// reached through `INSTANCE` and `Companion`.
    fn jvm_names(
        &self,
        changed: &Changed,
        own: &[String],
        stem: Option<&str>,
        out: &mut Vec<String>,
    ) {
        let capitalised = |name: &str| -> String {
            let mut chars = name.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect(),
                None => String::new(),
            }
        };
        let decapitalised = |name: &str| -> String {
            let mut chars = name.chars();
            match chars.next() {
                Some(first) => first.to_lowercase().chain(chars).collect(),
                None => String::new(),
            }
        };
        let property = matches!(
            changed.kind,
            DefKind::Property | DefKind::Field | DefKind::Variable | DefKind::Constant
        );
        for name in own {
            if property {
                let up = capitalised(name);
                out.extend(["get", "set", "is"].map(|prefix| format!("{prefix}{up}")));
            }
            for prefix in ["get", "set", "is"] {
                if let Some(rest) = name
                    .strip_prefix(prefix)
                    .filter(|rest| rest.chars().next().is_some_and(char::is_uppercase))
                {
                    // `getX` is the property `x`, and a Kotlin `isFoo` has `setFoo`.
                    out.push(decapitalised(rest));
                    if prefix == "is" {
                        out.push(format!("set{rest}"));
                    }
                }
            }
        }
        let top_level = changed
            .subjects
            .iter()
            .all(|s| s.key.local.container.is_empty());
        if top_level && (changed.file.ends_with(".kt") || changed.file.ends_with(".kts")) {
            out.extend(stem.map(|stem| format!("{}Kt", capitalised(stem))));
        }
        for subject in &changed.subjects {
            let Some((holder, outer)) = subject.key.local.container.split_last() else {
                continue;
            };
            let in_object = self
                .defs_by_file
                .get(changed.file.as_str())
                .into_iter()
                .flatten()
                .map(|at| &self.head.definitions()[*at])
                .any(|d| {
                    d.kind == DefKind::Object
                        && d.key.local.container.as_slice() == outer
                        && &d.key.local.name == holder
                });
            if in_object {
                out.push("INSTANCE".to_string());
                out.push("Companion".to_string());
                out.extend(
                    subject
                        .key
                        .local
                        .container
                        .iter()
                        .map(|name| name.as_str().to_string()),
                );
            }
        }
    }

    /// The trait a Rust definition belongs to: the one it implements, or the one
    /// that declares it. A PHP trait's method is copied into the class that uses it,
    /// so a call of it is a resolved call, not a dispatch.
    pub(super) fn dispatched_via(
        &self,
        def: &diffctx_resolve::resolve::Definition,
    ) -> Option<String> {
        // Only a Rust `ImplOf` names a trait a call may dispatch through: a Kotlin
        // one names an extension's receiver, and a call reaches it by its name.
        if self.family != Family::Rust {
            return None;
        }
        let last = |name: &str| name.rsplit("::").next().unwrap_or(name).to_string();
        if let Some(Disambig::ImplOf(tr)) = &def.key.local.disambig {
            return Some(last(tr.as_str()));
        }
        let (name, outer) = def.key.local.container.split_last()?;
        self.defs_by_file
            .get(def.file.as_str())?
            .iter()
            .map(|at| &self.head.definitions()[*at])
            .find(|d| {
                d.kind == DefKind::Trait
                    && d.key.local.container.as_slice() == outer
                    && &d.key.local.name == name
            })
            .map(|tr| tr.key.local.name.as_str().to_string())
    }

    /// The type that declared the member a removed override overrode, as the base
    /// side bound it: calls of that member reached the override, and at the head
    /// nothing says they no longer do. `the supertype it overrides` when the base
    /// could not name it (an external supertype).
    pub(super) fn overridden_via(&self, changed: &Changed) -> Option<String> {
        if changed.how != How::Removed {
            return None;
        }
        let Ok(base) = self.base() else {
            return None;
        };
        let own = |b: &&Binding| {
            b.relation == Relation::Overrides
                && b.file.as_str() == changed.file
                && changed
                    .stale
                    .iter()
                    .any(|key| matches!(&b.from, Scope::Def(local) if *local == key.local))
        };
        let binding = base.bindings().iter().find(own)?;
        Some(
            binding
                .targets()
                .first()
                .and_then(|target| target.local.container.last())
                .map_or_else(
                    || "the supertype it overrides".to_string(),
                    |holder| format!("`{}`", holder.as_str()),
                ),
        )
    }
}
