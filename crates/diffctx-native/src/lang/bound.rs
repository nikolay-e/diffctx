//! JavaScript/TypeScript: imports resolved by `bindings::Resolver`, and the
//! owner of a definition read off the fragments around it.

use std::path::Path;
use std::rc::Rc;

use rustc_hash::FxHashSet;

use super::{At, DefId, LanguageFacts, Relation, Table, UseSite};
use crate::bindings::{self, Definition, Finding, Lang, Resolution, Resolver, Site, Target};
use crate::impact::{base_symbol, enclosing_class, is_member, is_object_property, object_owner};
use crate::source::Source;
use crate::types::FragmentKind;

pub(super) fn models<'a>(
    root: &'a Path,
    source: &'a Source,
    files: FxHashSet<String>,
) -> Vec<Box<dyn LanguageFacts + 'a>> {
    let resolver = Rc::new(Resolver::new(root, source, files));
    std::iter::once(Lang::Js)
        .map(|lang| {
            Box::new(BoundModel {
                lang,
                resolver: resolver.clone(),
                defs: Table::new(),
            }) as Box<dyn LanguageFacts + 'a>
        })
        .collect()
}

struct BoundDef {
    path: String,
    name: String,
    class: Option<String>,
    /// A function defined inside another is a local name: only that
    /// function's lines can call it, never through a receiver.
    scope: Option<(u32, u32)>,
}

struct BoundModel<'a> {
    lang: Lang,
    resolver: Rc<Resolver<'a>>,
    defs: Table<(String, String, u32, u32), BoundDef>,
}

const MODELLED: &[Relation] = &[Relation::Callers, Relation::Imports];
/// JavaScript/TypeScript read their uses off the syntax (#506).
const JS_MODELLED: &[Relation] = &[Relation::Callers, Relation::Uses, Relation::Imports];

impl LanguageFacts for BoundModel<'_> {
    fn modelled(&self) -> &'static [Relation] {
        match self.lang {
            Lang::Js => JS_MODELLED,
            Lang::Python => MODELLED,
        }
    }

    fn use_lines(&self, path: &str, def: DefId) -> Option<Vec<u32>> {
        let def = self.defs.get(def);
        (self.lang == Lang::Js).then(|| {
            self.resolver
                .js_use_lines(path, &def.path, &def.name, def.class.is_some())
        })
    }

    fn covers(&self, path: &str) -> bool {
        bindings::lang_of(path) == Some(self.lang)
    }

    fn definition(&self, at: &At) -> Option<DefId> {
        let used = at.frag;
        let name = used.symbol_name.as_deref().map(base_symbol)?;
        let key = (
            at.path.to_string(),
            name.to_string(),
            used.start_line(),
            used.end_line(),
        );
        self.defs.intern(key, || {
            let scope = at.file.iter().find(|f| {
                f.id != used.id
                    && f.kind == FragmentKind::Function
                    && f.start_line() < used.start_line()
                    && f.end_line() >= used.end_line()
            });
            // An object literal holding the method is its owner before any
            // class: a class kept as its header alone would otherwise claim
            // every indented definition below it. An enum, a record or a
            // module around a method is no scope (`Op.PLUS.apply`).
            let class = if scope.is_some() {
                None
            } else if let Some(owner) = object_owner(used, at.file) {
                Some(owner)
            } else if is_object_property(used, name) {
                Some(bindings::UNKNOWN_OWNER.to_string())
            } else if is_member(used, at.file) {
                enclosing_class(used, at.file).and_then(|c| c.symbol_name.clone())
            } else {
                None
            };
            Some(BoundDef {
                path: at.path.to_string(),
                name: name.to_string(),
                class,
                scope: scope.map(|s| (s.start_line(), s.end_line())),
            })
        })
    }

    fn classify(&self, site: &UseSite, def: DefId) -> Option<Finding> {
        let def = self.defs.get(def);
        let (user, file) = site.frag?;
        if let Some((start, end)) = def.scope {
            if site.path != def.path || user.start_line() < start || user.end_line() > end {
                return None;
            }
        }
        let header = enclosing_class(user, file).and_then(|c| {
            c.symbol_name
                .as_deref()
                .map(|n| (n, c.content.lines().next().unwrap_or("")))
        });
        self.resolver.classify(
            &Site {
                path: site.path,
                content: &user.content,
                start_line: user.start_line(),
                class: header,
            },
            &Definition {
                path: &def.path,
                name: &def.name,
                class: def.class.as_deref(),
            },
        )
    }

    fn reaches_file(&self, test: &str, caller: &str) -> bool {
        self.resolver.imports_file(test, caller)
    }

    fn binds(&self, importer: &str, text: &str, name: &str, from: &str) -> Option<bool> {
        let is_from = |spec: &str| matches!(self.resolver.resolve(importer, spec), Resolution::File(f) if f == from);
        let b = self.resolver.bindings(importer);
        Some(
            b.star_from.iter().any(|m| is_from(m))
                || b.names.iter().any(|(local, t)| match t {
                    Target::Member { module, name: n } => n == name && is_from(module),
                    Target::Module(m) => text.contains(&format!("{local}.{name}")) && is_from(m),
                }),
        )
    }
}
