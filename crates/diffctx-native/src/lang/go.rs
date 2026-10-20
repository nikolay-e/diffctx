//! Go: a package is its directory, an import binds it by the path `go.mod`
//! gives it, and uses come from the syntax (`go_bindings::GoResolver`).

use std::path::Path;

use super::{At, DefId, LanguageFacts, Relation, RemovedDef, Table, UseSite};
use crate::bindings::Finding;
use crate::go_bindings::{GoDef, GoResolver, is_go};
use crate::impact::base_symbol;
use crate::source::Source;

pub(super) struct GoModel<'a> {
    resolver: GoResolver<'a>,
    defs: Table<(String, String, u32), GoDef>,
}

impl<'a> GoModel<'a> {
    pub(super) fn new(root: &'a Path, source: &'a Source) -> Self {
        Self {
            resolver: GoResolver::new(root, source),
            defs: Table::new(),
        }
    }
}

const MODELLED: &[Relation] = &[Relation::Callers, Relation::Uses, Relation::Removals];

impl LanguageFacts for GoModel<'_> {
    fn modelled(&self) -> &'static [Relation] {
        MODELLED
    }

    fn covers(&self, path: &str) -> bool {
        is_go(path)
    }

    fn definition(&self, at: &At) -> Option<DefId> {
        let name = at.frag.symbol_name.as_deref().map(base_symbol)?;
        // A method fragment may carry its receiver: `Ledger.Reconcile`.
        let name = name.rsplit('.').next().unwrap_or(name);
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

    fn reaches_file(&self, test: &str, caller: &str) -> bool {
        self.resolver.sees(test, caller)
    }

    fn use_lines(&self, path: &str, def: DefId) -> Option<Vec<u32>> {
        Some(self.resolver.use_lines(path, &self.defs.get(def)))
    }

    fn removed(&self, base: &str, pairs: &[(String, Option<String>)]) -> Option<Vec<RemovedDef>> {
        let mut out = Vec::new();
        for (old, new) in pairs.iter().filter(|(old, _)| is_go(old)) {
            let at = new.as_deref().unwrap_or(old);
            // A package-level name declared twice does not compile: one moved
            // to another file of the package was added there in the range.
            let changed = pairs.iter().filter_map(|(_, new)| new.as_deref());
            for def in self.resolver.removed(base, old, at, changed) {
                out.push(RemovedDef {
                    path: old.clone(),
                    name: def.name().to_string(),
                    lines: def.lines(),
                    public: def.name().starts_with(char::is_uppercase),
                    def: self.defs.push(def)?,
                });
            }
        }
        Some(out)
    }
}
