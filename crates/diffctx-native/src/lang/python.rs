//! Python: definitions and the lines that spell their names come from the
//! syntax facts (`facts::python`); imports are resolved by
//! `bindings::Resolver`, which decides what each spelling refers to.

use std::cell::RefCell;
use std::path::Path;
use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};

use super::{At, DefId, LanguageFacts, Relation, RemovedDef, Table, UseSite};
use crate::bindings::{self, Definition, Finding, Lang, Resolution, Resolver, Site, Target};
use crate::facts::python::{DeclKind, PythonFacts, facts};
use crate::impact::{base_symbol, enclosing_class, is_member};
use crate::source::Source;
use crate::types::FragmentKind;

/// Module- and class-level assignments, which impact and `--symbol`
/// resolve as definitions (#420, #488).
pub(super) fn assignments(text: &str) -> Vec<(Option<String>, (u32, u32), FragmentKind)> {
    facts(text).map_or_else(Vec::new, |facts| {
        facts
            .declarations
            .iter()
            .filter(|d| d.kind == DeclKind::Assignment)
            .map(|d| (Some(d.name.clone()), d.lines, FragmentKind::Property))
            .collect()
    })
}

struct PyDef {
    path: String,
    name: String,
    class: Option<String>,
    /// A function defined inside another is a local name: only that
    /// function's lines can call it, never through a receiver.
    scope: Option<(u32, u32)>,
    /// The lines that define it, which are no use of it; none for a
    /// definition the range removed.
    lines: Option<(u32, u32)>,
}

pub(super) struct PythonModel<'a> {
    root: &'a Path,
    resolver: Resolver<'a>,
    facts: RefCell<FxHashMap<String, Option<Arc<PythonFacts>>>>,
    defs: Table<(String, String, u32, u32), PyDef>,
}

impl<'a> PythonModel<'a> {
    pub(super) fn new(root: &'a Path, source: &'a Source, files: FxHashSet<String>) -> Self {
        Self {
            root,
            resolver: Resolver::new(root, source, files),
            facts: RefCell::default(),
            defs: Table::new(),
        }
    }

    fn facts(&self, rel: &str) -> Option<Arc<PythonFacts>> {
        if let Some(known) = self.facts.borrow().get(rel) {
            return known.clone();
        }
        let read = self.resolver.text(rel).and_then(|text| facts(&text));
        self.facts
            .borrow_mut()
            .insert(rel.to_string(), read.clone());
        read
    }

    /// A line a text search found, read in the function around it: its
    /// parameters and locals are names of its own, and a word the syntax
    /// does not spell as an identifier — a docstring's, a comment's — is no
    /// use at all (#536).
    fn classify_lines(&self, site: &UseSite, def: &PyDef) -> Option<Finding> {
        let facts = self.facts(site.path)?;
        let (first, last) = site.lines;
        let named = self
            .resolver
            .bindings(site.path)
            .names
            .iter()
            .filter(|(_, t)| matches!(t, Target::Member { name, .. } if *name == def.name))
            .map(|(local, _)| local.as_str())
            .chain([def.name.as_str()])
            .any(|n| {
                facts
                    .lines_naming(n)
                    .iter()
                    .any(|l| (first..=last).contains(l))
            });
        // The file that lost the definition and imports the name now took
        // it from where it went.
        let moved = def.lines.is_none()
            && site.path == def.path
            && self
                .resolver
                .bindings(site.path)
                .names
                .contains_key(&def.name);
        if !named || moved {
            return None;
        }
        let around = facts
            .declarations
            .iter()
            .filter(|d| d.kind == DeclKind::Function && d.lines.0 <= first && d.lines.1 >= last)
            .min_by_key(|d| d.lines.1 - d.lines.0);
        let span = around.map_or(site.lines, |d| d.lines);
        let text = self.resolver.text(site.path)?;
        let lines: Vec<&str> = text.split('\n').collect();
        let content = lines
            .get(span.0.saturating_sub(1) as usize..(span.1 as usize).min(lines.len()))?
            .join("\n");
        let class = around.and_then(|d| {
            let name = d.class.as_deref()?;
            let declared = facts.declarations.iter().find(|c| {
                c.kind == DeclKind::Class
                    && c.name == name
                    && c.lines.0 <= d.lines.0
                    && c.lines.1 >= d.lines.1
            })?;
            let header = lines
                .get(declared.lines.0.saturating_sub(1) as usize..declared.lines.1 as usize)?
                .iter()
                .find(|l| l.trim_start().starts_with("class "))?;
            Some((name, *header))
        });
        let mut found = self.resolver.classify(
            &Site {
                path: site.path,
                content: &content,
                start_line: span.0,
                class,
            },
            &Definition {
                path: &def.path,
                name: &def.name,
                class: def.class.as_deref(),
            },
        )?;
        found.lines.retain(|l| (first..=last).contains(l));
        self.outside_imports(site.path, found)
    }

    /// `from m import (\n    name,\n)`: a line an import spans binds the
    /// name and uses nothing.
    fn outside_imports(&self, path: &str, mut found: Finding) -> Option<Finding> {
        if let Some(facts) = self.facts(path) {
            found
                .lines
                .retain(|l| !facts.imports.iter().any(|&(a, b)| a <= *l && *l <= b));
        }
        (!found.lines.is_empty()).then_some(found)
    }
}

const MODELLED: &[Relation] = &[
    Relation::Callers,
    Relation::Uses,
    Relation::Imports,
    Relation::Removals,
];

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
                lines: Some(lines),
            })
        })
    }

    fn classify(&self, site: &UseSite, def: DefId) -> Option<Finding> {
        let def = self.defs.get(def);
        let Some((user, file)) = site.frag else {
            return self.classify_lines(site, &def);
        };
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
        if let Some((start, end)) = def.lines.filter(|_| site.path == def.path) {
            found.lines.retain(|l| *l < start || *l > end);
        }
        self.outside_imports(site.path, found)
    }

    fn local(&self, def: DefId) -> bool {
        self.defs.get(def).scope.is_some()
    }

    /// The module- and class-level definitions the base declared and the
    /// head no longer does. A function local to another went with the
    /// function around it, which the range changed.
    fn removed(&self, base: &str, pairs: &[(String, Option<String>)]) -> Option<Vec<RemovedDef>> {
        let mut out = Vec::new();
        for (old_path, new_path) in pairs.iter().filter(|(old, _)| self.covers(old)) {
            let Some(old) = crate::git::show_file_at_revision(self.root, base, Path::new(old_path))
                .ok()
                .and_then(|text| facts(&text))
            else {
                continue;
            };
            let new = new_path.as_deref().and_then(|p| self.facts(p));
            let path = new_path.clone().unwrap_or_else(|| old_path.clone());
            let mut seen: FxHashSet<(&str, Option<&str>)> = FxHashSet::default();
            for d in old.declarations.iter().filter(|d| d.scope.is_none()) {
                let key = (d.name.as_str(), d.class.as_deref());
                let kept = new.as_ref().is_some_and(|n| {
                    n.declarations
                        .iter()
                        .any(|x| x.scope.is_none() && (x.name.as_str(), x.class.as_deref()) == key)
                });
                if kept || d.name.len() < 3 || !seen.insert(key) {
                    continue;
                }
                let def = self.defs.push(PyDef {
                    path: path.clone(),
                    name: d.name.clone(),
                    class: d.class.clone(),
                    scope: None,
                    lines: None,
                })?;
                out.push(RemovedDef {
                    def,
                    path: path.clone(),
                    name: d.name.clone(),
                    lines: d.lines,
                    public: false,
                });
            }
        }
        Some(out)
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
