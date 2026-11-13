//! What the resolvers of neighbouring families write alike, once each: the engines differ
//! in their indexes, not in how they read a candidate list or a reach, or in what a name lands on.

use super::{CandidateReason, Evidence, EvidenceKind, UnresolvedReason};

/// A type a value has, for the engines that follow a value to its type's methods.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Typed {
    /// A definition that holds the methods.
    Def(usize),
    /// An interface, trait object or bound: any type that has its methods.
    Dyn(usize),
    /// A type the repository does not hold.
    External,
    Unknown,
}

/// What a name lands on, before it is a binding.
pub(super) enum Outcome {
    Found(usize, EvidenceKind),
    Candidates(Vec<usize>, CandidateReason),
    Unresolved(UnresolvedReason),
}

/// What a name lands on, before it is a binding, for the engines whose finds are entities:
/// `E` is the engine's entity.
pub(super) enum EntityOutcome<E> {
    Found {
        entity: E,
        evidence: Evidence,
    },
    Candidates {
        ids: Vec<usize>,
        reason: CandidateReason,
    },
    Unresolved(UnresolvedReason),
}

/// A name's entity, how it was found and the modules it passed through.
pub(super) struct Hit<E> {
    pub entity: E,
    pub kind: EvidenceKind,
    pub through: Vec<usize>,
}

/// What a resolved or candidate resolution names, for the engines that keep an index of definitions.
macro_rules! ground_methods {
    () => {
        fn resolved(&self, id: usize, how: EvidenceKind) -> Resolution {
            Resolution::Resolved {
                target: self.ix.defs[id].key.clone(),
                evidence: Evidence::new(how),
            }
        }

        fn candidates(&self, ids: &[usize], reason: CandidateReason) -> Resolution {
            let mut targets: Vec<DefKey> = Vec::new();
            for id in ids {
                let key = self.ix.defs[*id].key.clone();
                if !targets.contains(&key) {
                    targets.push(key);
                }
            }
            Resolution::Candidate { targets, reason }
        }
    };
}

/// Of the definitions, those the file may use (what `reach` allows); the reason when none is.
macro_rules! reachable_by_reach {
    () => {
        fn reachable(
            &self,
            f: usize,
            ids: Vec<usize>,
        ) -> std::result::Result<Vec<usize>, UnresolvedReason> {
            let mut blocked = None;
            let kept: Vec<usize> = ids
                .into_iter()
                .filter(|id| match self.reach(f, *id) {
                    Ok(()) => true,
                    Err(reason) => {
                        blocked = Some(reason);
                        false
                    }
                })
                .collect();
            match (kept.is_empty(), blocked) {
                (true, Some(reason)) => Err(reason),
                _ => Ok(kept),
            }
        }
    };
}

/// Whether a name used in file `f` may refer to the definition: one project, a declared
/// dependency, or no project at all.
macro_rules! reachable_by_project {
    () => {
        fn reachable(&self, f: usize, def: usize) -> bool {
            let used = self.ix.defs[def].file;
            used == f
                || self
                    .workspace
                    .may_reference(self.path_of(f).as_str(), self.path_of(used).as_str())
        }
    };
}

/// The Java and Kotlin engines' common methods.
macro_rules! jvm_engine_methods {
    () => {
        fn run(&self, ctx: &AnalysisContext) -> (Vec<Definition>, Vec<Binding>, bool) {
            let mut bindings = Vec::new();
            let mut stopped = false;
            'files: for (f, file) in self.ix.files.iter().enumerate() {
                if file.foreign {
                    continue;
                }
                for fact in &file.facts.blob.facts {
                    if expired(&ctx.run) {
                        stopped = true;
                        break 'files;
                    }
                    if let Fact::Reference(r) = fact {
                        self.reference(f, r, &mut bindings);
                    }
                }
                for import in &file.imports {
                    self.import(f, import, &mut bindings);
                }
            }
            let definitions = self
                .ix
                .defs
                .iter()
                .filter(|rec| !self.ix.files[rec.file].foreign)
                .map(|rec| Definition {
                    file: self.ix.files[rec.file].facts.path.clone(),
                    key: rec.key.clone(),
                    kind: rec.def.kind,
                    span: rec.def.span.clone(),
                    name_span: rec.def.name_span.clone(),
                })
                .collect();
            (definitions, bindings, stopped)
        }

        reachable_by_reach!();

        fn pick(&self, f: usize, ids: Vec<usize>, how: EvidenceKind) -> Found {
            match self.reachable(f, ids) {
                Ok(kept) if !kept.is_empty() => Found::Defs(kept, how),
                Ok(_) => Found::Nothing,
                Err(reason) => Found::Blocked(reason),
            }
        }

        /// The definitions an explicit import names: a type of a package, or a
        /// static member or nested type of a type.
        fn import_targets(&self, import: &ImportRec) -> Vec<usize> {
            let Some((name, owners)) = import.path.split_last() else {
                return Vec::new();
            };
            self.named_in(owners, &Name::new(name.as_str()))
        }

        /// What an on-demand import brings under `name`: the types of a package, or
        /// the static members of a type.
        fn wildcard_targets(&self, import: &ImportRec, name: &Name) -> Vec<usize> {
            self.named_in(&import.path, name)
        }

        /// Whether `sub` is `sup` or inherits from it.
        fn is_subtype(&self, sub: usize, sup: usize, depth: usize) -> bool {
            sub == sup
                || (depth < MAX_DEPTH
                    && self.supers_of(sub).iter().any(|s| match s {
                        Super::Repo(id) => self.is_subtype(*id, sup, depth + 1),
                        Super::Foreign(_) => false,
                    }))
        }

        /// Whether some type the class inherits from lives outside the repository,
        /// where an unfound member most likely is.
        fn open_above(&self, class: usize, depth: usize) -> bool {
            depth < MAX_DEPTH
                && self.supers_of(class).iter().any(|s| match s {
                    Super::Foreign(_) => true,
                    Super::Repo(id) => self.open_above(*id, depth + 1),
                })
        }

        fn local_visible(&self, f: usize, name: &Name, at: u32) -> bool {
            self.ix.files[f]
                .locals
                .get(name)
                .is_some_and(|ranges| ranges.iter().any(|r| r.contains(&at)))
        }

        fn reference(&self, f: usize, r: &Ref, out: &mut Vec<Binding>) {
            let file = &self.ix.files[f];
            let scope = scope_path(&r.enclosing);
            let (relation, resolution) = match r.use_kind {
                UseKind::Override => self.override_of(f, r),
                UseKind::TypeUse => self.type_use(f, &scope, r),
                _ => self.value_use(f, &scope, r),
            };
            out.push(Binding {
                file: file.facts.path.clone(),
                site: r.site.clone(),
                from: r.enclosing.clone(),
                relation,
                name: r.name.clone(),
                resolution,
                entry_guard: false,
            });
        }

        /// The type a member is declared in.
        fn holder_of(&self, member: usize) -> Option<usize> {
            let rec = &self.ix.defs[member];
            let container = &rec.def.local.container;
            if container.is_empty() {
                return None;
            }
            self.ix
                .owner(rec.file, container)
                .filter(|h| is_type(self.ix.defs[*h].def.kind))
        }

        /// Whether a file can hold a value of the member's type without spelling it:
        /// it is that file, shares the package, or names the type or one it inherits
        /// from.
        fn may_hold(&self, f: usize, member: usize) -> bool {
            let rec = &self.ix.defs[member];
            let file = &self.ix.files[f];
            if rec.file == f || self.ix.files[rec.file].package == file.package {
                return true;
            }
            let Some(owner) = self.holder_of(member) else {
                return true;
            };
            let mut related: Vec<Name> = vec![self.ix.defs[owner].def.local.name.clone()];
            related.extend(self.supers_of(owner).iter().map(|s| match s {
                Super::Repo(id) => self.ix.defs[*id].def.local.name.clone(),
                Super::Foreign(n) => n.clone(),
            }));
            related.iter().any(|n| file.names.contains(n))
                || file.imports.iter().any(|i| {
                    i.path
                        .last()
                        .is_some_and(|l| related.iter().any(|n| n.as_str() == l))
                })
        }
    };
}

/// The text of a file of the index, read once; a failed read is a loss, and no text.
macro_rules! file_text {
    () => {
        fn text(&self, f: usize) -> Option<Bytes> {
            if let Some(known) = self.texts.borrow().get(&f) {
                return known.clone();
            }
            let path = &self.ix.files[f].facts.path;
            let read = match self.reader.read(path) {
                Ok(read) => read,
                Err(_) => {
                    self.losses.borrow_mut().push(Loss::ReadFailed {
                        path: path.clone(),
                        side: self.side,
                    });
                    None
                }
            };
            self.texts.borrow_mut().insert(f, read.clone());
            read
        }
    };
}
