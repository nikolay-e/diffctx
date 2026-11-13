//! Engine methods that the resolvers of neighbouring families write alike, once each: the
//! engines differ in their indexes, not in how they read a candidate list or a reach.

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

        /// Of the definitions, those the file may use; the reason when none is.
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
