//! Kotlin, Java and Swift: definitions, uses and supertypes from their
//! syntax facts (`facts::kotlin`, `facts::java`, `facts::swift`, one shape),
//! resolved by `kotlin_bindings::KotlinResolver` (#335, #526).

use std::path::Path;

use super::{At, DefId, Implementer, LanguageFacts, Relation, RemovedDef, Table, UseSite};
use crate::bindings::Finding;
use crate::facts::kotlin::Visibility;
use crate::impact::base_symbol;
use crate::kotlin_bindings::{KDef, KotlinResolver};
use crate::source::Source;

pub(super) struct KotlinModel<'a> {
    resolver: KotlinResolver<'a>,
    modelled: &'static [Relation],
    defs: Table<(String, String, u32), KDef>,
}

impl<'a> KotlinModel<'a> {
    pub(super) fn kotlin(root: &'a Path, source: &'a Source) -> Self {
        Self::of(KotlinResolver::new(root, source), KOTLIN)
    }

    pub(super) fn java(root: &'a Path, source: &'a Source) -> Self {
        Self::of(KotlinResolver::java(root, source), CALLS)
    }

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

const KOTLIN: &[Relation] = &[
    Relation::Callers,
    Relation::Uses,
    Relation::Implementations,
    Relation::PublicApi,
    Relation::Removals,
];

/// Java needs no `@Override` and Swift no `override` to implement a member,
/// so the facts cannot list their implementations; their API and removals
/// are still read off the fragments.
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

    fn implementers(&self, def: DefId, paths: &[String]) -> Option<Vec<Implementer>> {
        self.answers(Relation::Implementations)?;
        let def = self.defs.get(def);
        if !def.decl.abstract_member {
            return Some(Vec::new());
        }
        Some(
            self.resolver
                .implementers(&def, paths.iter().map(String::as_str))
                .into_iter()
                .map(|(path, decl)| Implementer {
                    path,
                    name: decl.name,
                    start_line: decl.start_line,
                })
                .collect(),
        )
    }

    fn annotations(&self, def: DefId) -> Option<Vec<String>> {
        Some(self.defs.get(def).decl.annotations.clone())
    }

    fn public_change(&self, def: DefId, base: &str) -> Option<bool> {
        self.answers(Relation::PublicApi)?;
        let def = self.defs.get(def);
        Some(
            def.decl.effective == Some(Visibility::Public)
                && self
                    .resolver
                    .facts_at(base, &def.path)
                    .and_then(|old| {
                        old.declarations
                            .iter()
                            .find(|d| d.key() == def.decl.key())
                            .map(|d| d.signature != def.decl.signature)
                    })
                    .unwrap_or(true),
        )
    }

    fn removed(&self, base: &str, pairs: &[(String, Option<String>)]) -> Option<Vec<RemovedDef>> {
        self.answers(Relation::Removals)?;
        let mut out = Vec::new();
        for (old_path, new_path) in pairs.iter().filter(|(old, _)| self.resolver.covers(old)) {
            let Some(old) = self.resolver.facts_at(base, old_path) else {
                continue;
            };
            let new = new_path.as_deref().and_then(|p| self.resolver.facts(p));
            let path = new_path.clone().unwrap_or_else(|| old_path.clone());
            for d in &old.declarations {
                let kept = new
                    .as_ref()
                    .is_some_and(|n| n.declarations.iter().any(|x| x.key() == d.key()));
                if kept || d.kind == "constructor" {
                    continue;
                }
                let def = KDef {
                    path: path.clone(),
                    decl: d.clone(),
                    package: old.package.clone(),
                };
                let removed = RemovedDef {
                    path: path.clone(),
                    name: d.name.clone(),
                    lines: (d.start_line, d.end_line),
                    public: d.effective == Some(Visibility::Public),
                    def: self.defs.push(def)?,
                };
                out.push(removed);
            }
        }
        Some(out)
    }
}
