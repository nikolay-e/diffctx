//! A Java method nothing calls, in a class code outside the repository may extend:
//! such a class may be given an interface of the repository by a subclass the
//! repository does not hold, and the interface's callers then reach the method.

use super::{Cell, Changed, DefKind, Fact, Family, RepoPath, SideTag, Use, Visibility, facts_of};
use crate::facts::{BlobFacts, ImportKind};
use crate::resolve::Definition;
use std::collections::BTreeSet;
use std::sync::Arc;

/// A type of a Java file: what decides who may extend or implement it.
struct JavaType {
    name: String,
    file: String,
    package: String,
    kind: DefKind,
    visibility: Visibility,
    extensible: bool,
    extends: Option<String>,
}

/// The words of a declaration with its generic arguments and annotation values
/// left out.
fn words(signature: &str) -> Vec<String> {
    let mut depth = 0usize;
    let flat: String = signature
        .chars()
        .filter(|c| match c {
            '<' | '(' => {
                depth += 1;
                false
            }
            '>' | ')' => {
                depth = depth.saturating_sub(1);
                false
            }
            _ => depth == 0,
        })
        .collect();
    flat.split(|c: char| c.is_whitespace() || c == ',')
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

/// Whether a type and an interface may be used by one class: neither is private,
/// and a package-private one confines the class to its package.
fn usable_together(class: &JavaType, interface: &JavaType) -> bool {
    let mut packages = BTreeSet::new();
    for held in [class, interface] {
        match held.visibility {
            Visibility::Private => return false,
            Visibility::Package => {
                packages.insert(held.package.as_str());
            }
            _ => {}
        }
    }
    packages.len() <= 1
}

impl Cell {
    /// The interface whose callers may reach a Java method that has no caller of its
    /// own: the method is public and the class that declares it, or a class that
    /// inherits it, can be extended by a class that implements an interface of the
    /// repository declaring the method's name, in a package that sees both.
    pub(super) fn open_world_via(&self, changed: &Changed, uses: &[Use]) -> Option<String> {
        if self.family != Family::Java || !uses.is_empty() {
            return None;
        }
        changed
            .subjects
            .iter()
            .filter_map(|subject| self.definition(&subject.key))
            .find_map(|def| self.interface_route(def))
    }

    fn interface_route(&self, method: &Definition) -> Option<String> {
        if method.kind != DefKind::Method {
            return None;
        }
        let own = self.java_facts(method.file.as_str())?;
        let declared = own.facts.iter().find_map(|fact| match fact {
            Fact::Definition(d) if d.local == method.key.local => d.signature.as_deref(),
            _ => None,
        })?;
        let modifiers = words(declared);
        if !modifiers.iter().any(|w| w == "public") || modifiers.iter().any(|w| w == "static") {
            return None;
        }
        let name = method.key.local.name.as_str();
        let interfaces: Vec<&Definition> = self
            .head
            .definitions()
            .iter()
            .filter(|d| d.kind == DefKind::Method && d.key.local.name.as_str() == name)
            .filter(|d| self.holder(d).is_some_and(|o| o.kind == DefKind::Interface))
            .collect();
        if interfaces.is_empty() {
            return None;
        }
        let types = self.java_types();
        let holder = self.holder(method)?;
        let held = types
            .iter()
            .find(|t| t.file == holder.file.as_str() && t.name == holder.key.local.name.as_str())?;
        if held.kind != DefKind::Class {
            return None;
        }
        let mut family: Vec<&JavaType> = vec![held];
        let mut at = 0;
        while let Some(parent) = family.get(at).copied() {
            at += 1;
            for sub in types
                .iter()
                .filter(|t| t.extends.as_deref() == Some(parent.name.as_str()))
            {
                if !family.iter().any(|f| std::ptr::eq(*f, sub)) {
                    family.push(sub);
                }
            }
        }
        interfaces.iter().find_map(|interface| {
            let interface_type = types.iter().find(|t| {
                t.file == interface.file.as_str()
                    && self
                        .holder(interface)
                        .is_some_and(|o| o.key.local.name.as_str() == t.name)
            })?;
            family
                .iter()
                .any(|class| class.extensible && usable_together(class, interface_type))
                .then(|| format!("`{}`", interface_type.name))
        })
    }

    /// The type that declares a member, whatever its kind.
    fn holder(&self, def: &Definition) -> Option<&Definition> {
        let (name, outer) = def.key.local.container.split_last()?;
        self.defs_by_file
            .get(def.file.as_str())?
            .iter()
            .map(|at| &self.head.definitions()[*at])
            .find(|d| {
                matches!(d.kind, DefKind::Class | DefKind::Interface)
                    && d.key.local.container.as_slice() == outer
                    && &d.key.local.name == name
            })
    }

    fn java_facts(&self, file: &str) -> Option<Arc<BlobFacts>> {
        let Ok(path) = RepoPath::new(file) else {
            return None;
        };
        match facts_of(&self.ctx, SideTag::Head, &self.cache, &path) {
            Ok(Some(found)) => Some(found.blob),
            _ => None,
        }
    }

    fn java_types(&self) -> Vec<JavaType> {
        let mut files: Vec<&String> = self
            .defs_by_file
            .keys()
            .filter(|file| file.ends_with(".java"))
            .collect();
        files.sort();
        let mut found = Vec::new();
        for file in files {
            let Some(facts) = self.java_facts(file) else {
                continue;
            };
            let package = facts
                .facts
                .iter()
                .find_map(|fact| match fact {
                    Fact::Import(i) if i.kind == ImportKind::SideEffect => {
                        Some(i.module.as_str().to_string())
                    }
                    _ => None,
                })
                .unwrap_or_else(String::new);
            for fact in &facts.facts {
                let Fact::Definition(d) = fact else {
                    continue;
                };
                if !matches!(d.kind, DefKind::Class | DefKind::Interface) {
                    continue;
                }
                let modifiers = words(d.signature.as_deref().unwrap_or(""));
                let extends = modifiers
                    .iter()
                    .position(|w| w == "extends")
                    .and_then(|at| modifiers.get(at + 1))
                    .map(|w| w.rsplit('.').next().unwrap_or(w).to_string());
                found.push(JavaType {
                    name: d.local.name.as_str().to_string(),
                    file: file.clone(),
                    package: package.clone(),
                    kind: d.kind,
                    visibility: d.visibility,
                    extensible: d.kind == DefKind::Class
                        && !d.local.name.as_str().contains(' ')
                        && !modifiers
                            .iter()
                            .any(|w| matches!(w.as_str(), "final" | "record" | "sealed")),
                    extends,
                });
            }
        }
        found
    }
}
