//! Swift: definitions, uses and supertypes from its syntax facts
//! (`facts::swift`, Kotlin's shape), resolved by
//! `kotlin_bindings::KotlinResolver` (#526). Kotlin's and Java's are the facts
//! layer's (`decide`'s Kotlin and Java cells): their callers, removals,
//! implementers (overrides), public signatures and entry annotations.

use std::path::Path;

use super::{At, DefId, LanguageFacts, Relation, Table, UseSite};
use crate::bindings::Finding;
use crate::impact::base_symbol;
use crate::kotlin_bindings::{KDef, KotlinResolver};
use crate::source::Source;

pub(super) struct KotlinModel<'a> {
    resolver: KotlinResolver<'a>,
    modelled: &'static [Relation],
    defs: Table<(String, String, u32), KDef>,
}

impl<'a> KotlinModel<'a> {
    /// The model of the Swift files among `files`.
    pub(super) fn swift<'f>(
        root: &'a Path,
        source: &'a Source,
        files: impl IntoIterator<Item = &'f String>,
    ) -> Self {
        Self::of(KotlinResolver::swift(root, source, files), CALLS)
    }

    fn of(resolver: KotlinResolver<'a>, modelled: &'static [Relation]) -> Self {
        Self {
            resolver,
            modelled,
            defs: Table::new(),
        }
    }

    fn answers(&self, relation: Relation) -> Option<()> {
        self.modelled.contains(&relation).then_some(())
    }
}

/// Swift needs no `override` to implement a member, so the facts cannot list
/// its implementations; its uses and removals are read off the fragments.
const CALLS: &[Relation] = &[Relation::Callers, Relation::Uses];

impl LanguageFacts for KotlinModel<'_> {
    fn modelled(&self) -> &'static [Relation] {
        self.modelled
    }

    fn covers(&self, path: &str) -> bool {
        self.resolver.covers(path)
    }

    fn definition(&self, at: &At) -> Option<DefId> {
        let name = at.frag.symbol_name.as_deref().map(base_symbol)?;
        let line = at.frag.start_line();
        self.defs
            .intern((at.path.to_string(), name.to_string(), line), || {
                self.resolver.definition(at.path, name, line)
            })
    }

    fn classify(&self, site: &UseSite, def: DefId) -> Option<Finding> {
        self.resolver
            .classify(site.path, site.lines, &self.defs.get(def))
    }

    /// The facts do not say yet which file a test imports: a test is
    /// linked by its calls alone (#530).
    fn reaches_file(&self, _test: &str, _caller: &str) -> bool {
        false
    }

    fn use_lines(&self, path: &str, def: DefId) -> Option<Vec<u32>> {
        Some(self.resolver.use_lines(path, &self.defs.get(def)))
    }

    fn annotations(&self, def: DefId) -> Option<Vec<String>> {
        Some(self.defs.get(def).decl.annotations.clone())
    }
}
