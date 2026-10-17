//! Go: a package is its directory, an import binds it by the path `go.mod`
//! gives it, and uses come from the syntax (`go_bindings::GoResolver`).

use std::path::Path;

use super::{At, DefId, LanguageFacts, Relation, Table, UseSite};
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

const MODELLED: &[Relation] = &[Relation::Callers, Relation::Uses];

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
}
