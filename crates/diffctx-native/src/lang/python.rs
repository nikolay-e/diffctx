//! Python: definitions and the lines that spell their names come from the
//! syntax facts (`facts::python`); imports are resolved by
//! `bindings::Resolver`, which decides what each spelling refers to.

use std::cell::RefCell;
use std::path::Path;
use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};

use super::{At, DefId, LanguageFacts, Relation, Table, UseSite};
use crate::bindings::{self, Definition, Finding, Lang, Resolution, Resolver, Site, Target};
use crate::facts::python::{DeclKind, PythonFacts, facts};
use crate::impact::{base_symbol, enclosing_class, is_member};
use crate::source::Source;
use crate::types::{Fragment, FragmentId, FragmentKind};

/// A Python module- or class-level assignment is a definition impact and
/// `--symbol` resolve (#420, #488). Pack keeps its fragments as they are,
/// sized for a reader's budget, and an impact run lays these over them; a
/// fragment spanning exactly the statement becomes the definition.
pub fn overlay_assignments(fragments: &mut Vec<Fragment>, source: &Source) {
    let mut paths: Vec<Arc<str>> = fragments
        .iter()
        .filter(|f| bindings::lang_of(f.path()) == Some(Lang::Python))
        .map(|f| Arc::clone(&f.id.path))
        .collect();
    paths.sort();
    paths.dedup();
    let mut at: FxHashMap<FragmentId, usize> = fragments
        .iter()
        .enumerate()
        .map(|(i, f)| (f.id.clone(), i))
        .collect();
    for path in paths {
        let Some(text) = source.read_to_string(Path::new(path.as_ref())) else {
            continue;
        };
        let Some(facts) = facts(&text) else {
            continue;
        };
        let lines: Vec<&str> = text.split('\n').collect();
        for d in facts
            .declarations
            .iter()
            .filter(|d| d.kind == DeclKind::Assignment)
        {
            let id = FragmentId::new(Arc::clone(&path), d.lines.0, d.lines.1);
            if let Some(&i) = at.get(&id) {
                let same = &mut fragments[i];
                if same.symbol_name.is_none() {
                    same.kind = FragmentKind::Property;
                    same.symbol_name = Some(d.name.clone());
                }
                continue;
            }
            let Some(content) = lines
                .get(d.lines.0 as usize - 1..d.lines.1 as usize)
                .map(|l| l.join("\n"))
            else {
                continue;
            };
            at.insert(id.clone(), fragments.len());
            fragments.push(Fragment {
                id,
                kind: FragmentKind::Property,
                identifiers: crate::types::extract_identifiers(
                    &content,
                    crate::config::tokenization::TOKENIZATION.fragment_min_identifier_length,
                ),
                content: Arc::from(content),
                token_count: 0,
                symbol_name: Some(d.name.clone()),
            });
        }
    }
}

struct PyDef {
    path: String,
    name: String,
    class: Option<String>,
    /// A function defined inside another is a local name: only that
    /// function's lines can call it, never through a receiver.
    scope: Option<(u32, u32)>,
    /// The lines that define it, which are no use of it.
    lines: (u32, u32),
}

pub(super) struct PythonModel<'a> {
    root: &'a Path,
    source: &'a Source,
    resolver: Resolver<'a>,
    facts: RefCell<FxHashMap<String, Option<Arc<PythonFacts>>>>,
    defs: Table<(String, String, u32, u32), PyDef>,
}

impl<'a> PythonModel<'a> {
    pub(super) fn new(root: &'a Path, source: &'a Source, files: FxHashSet<String>) -> Self {
        Self {
            root,
            source,
            resolver: Resolver::new(root, source, files),
            facts: RefCell::default(),
            defs: Table::new(),
        }
    }

    fn facts(&self, rel: &str) -> Option<Arc<PythonFacts>> {
        if let Some(known) = self.facts.borrow().get(rel) {
            return known.clone();
        }
        let read = self
            .source
            .read_to_string(&self.root.join(rel))
            .and_then(|text| facts(&text));
        self.facts
            .borrow_mut()
            .insert(rel.to_string(), read.clone());
        read
    }
}

const MODELLED: &[Relation] = &[Relation::Callers, Relation::Uses, Relation::Imports];

impl LanguageFacts for PythonModel<'_> {
    fn modelled(&self) -> &'static [Relation] {
        MODELLED
    }

    fn covers(&self, path: &str) -> bool {
        bindings::lang_of(path) == Some(Lang::Python)
    }

    fn definition(&self, at: &At) -> Option<DefId> {
        let used = at.frag;
        let name = used.symbol_name.as_deref().map(base_symbol)?;
        let lines = (used.start_line(), used.end_line());
        let key = (at.path.to_string(), name.to_string(), lines.0, lines.1);
        self.defs.intern(key, || {
            let stated = self.facts(at.path).and_then(|f| {
                f.declaration(name, lines)
                    .map(|d| (d.class.clone(), d.scope))
            });
            // A file the grammar could not read: the fragments around the
            // definition say what they can.
            let (class, scope) = stated.unwrap_or_else(|| {
                let scope = at.file.iter().find(|f| {
                    f.id != used.id
                        && f.kind == FragmentKind::Function
                        && f.start_line() < used.start_line()
                        && f.end_line() >= used.end_line()
                });
                let class = scope
                    .is_none()
                    .then(|| is_member(used, at.file).then(|| enclosing_class(used, at.file)))
                    .flatten()
                    .flatten()
                    .and_then(|c| c.symbol_name.clone());
                (class, scope.map(|s| (s.start_line(), s.end_line())))
            });
            Some(PyDef {
                path: at.path.to_string(),
                name: name.to_string(),
                class,
                scope,
                lines,
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
        let mut found = self.resolver.classify(
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
        )?;
        if site.path == def.path {
            found.lines.retain(|l| *l < def.lines.0 || *l > def.lines.1);
        }
        (!found.lines.is_empty()).then_some(found)
    }

    fn reaches_file(&self, test: &str, caller: &str) -> bool {
        self.resolver.imports_file(test, caller)
    }

    /// Every line of `path` spelling the definition's name, or a local an
    /// import binds to it, in code: the resolver decides which are uses.
    fn use_lines(&self, path: &str, def: DefId) -> Option<Vec<u32>> {
        let def = self.defs.get(def);
        let facts = self.facts(path)?;
        let mut names = vec![def.name.clone()];
        if def.class.is_none() {
            names.extend(
                self.resolver
                    .bindings(path)
                    .names
                    .iter()
                    .filter(|(local, t)| {
                        **local != def.name
                            && matches!(t, Target::Member { name, .. } if *name == def.name)
                    })
                    .map(|(local, _)| local.clone()),
            );
        }
        let mut lines: Vec<u32> = names
            .iter()
            .flat_map(|n| facts.lines_naming(n).iter().copied())
            .collect();
        lines.sort_unstable();
        lines.dedup();
        Some(lines)
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
