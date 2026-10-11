//! The Kotlin binding model: whether a use site reaches a declaration, read
//! off the shared syntax facts (`facts::kotlin`) rather than the text.
//!
//! A use binds through what Kotlin
//! itself resolves it by — an import or its alias, the declared package, the
//! receiver's declared type (a local, a parameter, a property of the class
//! or of a source-visible base class), an object or companion qualifier, the
//! implicit `this` of the class or a subclass. What the syntax cannot settle
//! — an inferred receiver, a call through an interface, an overload, a
//! wildcard import, a package shared across projects with no known
//! dependency — stays a candidate with its reason; what it settles against
//! the declaration is rejected. Compiler-grade typing is out of reach here:
//! a receiver typed by inference is unresolved, never guessed.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::bindings::{Evidence, Finding, Relation};
use crate::facts::kotlin::{
    Declaration, InitCall, KotlinFacts, Receiver, Use, UseKind, Visibility,
};
use crate::source::Source;

/// Supertype hops followed to find an inherited member.
const MAX_INHERITANCE_DEPTH: usize = 6;
/// Files a type name is looked up in before the name counts as external.
const MAX_TYPE_FILES: usize = 24;
/// Project references followed to establish a dependency.
const MAX_PROJECT_HOPS: usize = 6;
/// Initializers followed through their receivers to type one value.
const MAX_TYPING_DEPTH: usize = 4;

pub fn is_kotlin(path: &str) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("kt") || e.eq_ignore_ascii_case("kts"))
}

/// A declaration and where it lives.
#[derive(Clone, Debug)]
pub struct KDef {
    pub path: String,
    pub decl: Declaration,
    pub package: Option<String>,
}

impl KDef {
    fn qualified(&self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        if let Some(p) = self.package.as_deref() {
            parts.push(p);
        }
        parts.extend(self.decl.owners.iter().map(String::as_str));
        parts.push(&self.decl.name);
        parts.join(".")
    }

    /// The type a member belongs to: its innermost owner other than an
    /// unnamed companion's implicit holder.
    fn owner(&self) -> Option<&str> {
        self.decl.owners.last().map(String::as_str)
    }
}

/// A type in view: the file that declares it and its declaration.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct TypeRef {
    path: String,
    name: String,
    owners: Vec<String>,
}

pub struct KotlinResolver<'a> {
    root: &'a Path,
    source: &'a Source,
    facts: RefCell<FxHashMap<String, Option<Arc<KotlinFacts>>>>,
    /// Facts of other revisions, by (revision, path): the base side of a
    /// range is asked for once per changed definition.
    facts_at: RefCell<FxHashMap<(String, String), Option<Arc<KotlinFacts>>>>,
    /// One `git cat-file --batch` for those reads, not a process per file.
    batch: RefCell<Option<crate::git::CatFileBatch>>,
    type_files: RefCell<FxHashMap<String, Vec<String>>>,
    projects: RefCell<FxHashMap<PathBuf, Option<PathBuf>>>,
    deps: RefCell<FxHashMap<PathBuf, Vec<PathBuf>>>,
    typing_depth: Cell<usize>,
}

impl<'a> KotlinResolver<'a> {
    pub fn new(root: &'a Path, source: &'a Source) -> Self {
        Self {
            root,
            source,
            facts: RefCell::default(),
            facts_at: RefCell::default(),
            batch: RefCell::default(),
            type_files: RefCell::default(),
            projects: RefCell::default(),
            deps: RefCell::default(),
            typing_depth: Cell::default(),
        }
    }

    /// The facts of a repository-relative Kotlin file at the run's snapshot.
    pub fn facts(&self, rel: &str) -> Option<Arc<KotlinFacts>> {
        if let Some(known) = self.facts.borrow().get(rel) {
            return known.clone();
        }
        let read = self
            .source
            .read_bytes(&self.root.join(rel))
            .and_then(|b| String::from_utf8(b).ok())
            .and_then(|text| crate::facts::kotlin::facts(&text));
        self.facts
            .borrow_mut()
            .insert(rel.to_string(), read.clone());
        read
    }

    /// The declaration a fragment of `rel` named `name` starting at `line`
    /// stands for.
    pub fn definition(&self, rel: &str, name: &str, line: u32) -> Option<KDef> {
        let facts = self.facts(rel)?;
        let decl = facts.declaration_named(name, line)?.clone();
        Some(KDef {
            path: rel.to_string(),
            decl,
            package: facts.package.clone(),
        })
    }

    /// The facts of `rel` at another revision of the repository.
    pub fn facts_at(&self, rev: &str, rel: &str) -> Option<Arc<KotlinFacts>> {
        let key = (rev.to_string(), rel.to_string());
        if let Some(known) = self.facts_at.borrow().get(&key) {
            return known.clone();
        }
        let read = {
            let mut batch = self.batch.borrow_mut();
            if batch.is_none() {
                *batch = crate::git::CatFileBatch::new(self.root).ok();
            }
            batch
                .as_mut()
                .and_then(|b| b.get(rev, Path::new(rel)).ok())
                .or_else(|| crate::git::show_file_at_revision(self.root, rev, Path::new(rel)).ok())
        }
        .and_then(|text| crate::facts::kotlin::facts(&text));
        self.facts_at.borrow_mut().insert(key, read.clone());
        read
    }

    /// The lines of `rel` where a use could reach `def` by spelling: its
    /// name, or the alias the file imports it under.
    pub fn use_lines(&self, rel: &str, def: &KDef) -> Vec<u32> {
        let Some(facts) = self.facts(rel) else {
            return Vec::new();
        };
        let names = self.names_for(&facts, def);
        let mut lines: Vec<u32> = facts
            .uses
            .iter()
            .filter(|u| names.contains(&u.name))
            .map(|u| u.line)
            .collect();
        lines.dedup();
        lines
    }

    /// How the uses inside `lines` of `rel` relate to `def`: the strongest
    /// evidence any of them carries, with the lines that carry it.
    pub fn classify(&self, rel: &str, lines: (u32, u32), def: &KDef) -> Option<Finding> {
        let facts = self.facts(rel)?;
        let names = self.names_for(&facts, def);
        let mut best: Option<Finding> = None;
        let first = facts.uses.partition_point(|u| u.line < lines.0);
        for u in facts.uses[first..]
            .iter()
            .take_while(|u| u.line <= lines.1)
            .filter(|u| names.contains(&u.name))
        {
            // The definition's own declaration is no use of it.
            if rel == def.path && def.decl.span.contains(&u.at) {
                continue;
            }
            let Some(relation) = relation_of(u, def) else {
                continue;
            };
            let evidence = self.bind(rel, &facts, u, def);
            if evidence == Evidence::Rejected {
                continue;
            }
            // Reading `x.prop` through a receiver nothing types says nothing.
            if relation == Relation::Reference
                && u.kind == UseKind::Access
                && matches!(evidence, Evidence::Candidate("receiver_unresolved"))
            {
                continue;
            }
            absorb(&mut best, evidence, relation, u.line);
        }
        best
    }

    /// Classes among `paths` that implement `def`, an abstract member: a
    /// supertype reaching `def`'s owner and an `override` of its name.
    pub fn implementers<'p>(
        &self,
        def: &KDef,
        paths: impl IntoIterator<Item = &'p str>,
    ) -> Vec<(String, Declaration)> {
        let Some(owner) = def.owner() else {
            return Vec::new();
        };
        let target = TypeRef {
            path: def.path.clone(),
            name: owner.to_string(),
            owners: def.decl.owners[..def.decl.owners.len() - 1].to_vec(),
        };
        let mut found = Vec::new();
        for rel in paths {
            let Some(facts) = self.facts(rel) else {
                continue;
            };
            for d in &facts.declarations {
                let Some((class, outer)) = d.owners.split_last() else {
                    continue;
                };
                if !(d.is_override && d.name == def.decl.name && d.kind == def.decl.kind) {
                    continue;
                }
                let class_ref = TypeRef {
                    path: rel.to_string(),
                    name: class.clone(),
                    owners: outer.to_vec(),
                };
                if class_ref != target && self.is_subtype(&class_ref, &target, 0) {
                    found.push((rel.to_string(), d.clone()));
                }
            }
        }
        found
    }

    /// The names a file can reach `def` under: its own, and an alias.
    fn names_for(&self, facts: &KotlinFacts, def: &KDef) -> Vec<String> {
        let mut names = vec![def.decl.name.clone()];
        if def.decl.kind == "constructor" {
            if let Some(owner) = def.owner() {
                names.push(owner.to_string());
            }
        }
        let qualified = def.qualified();
        names.extend(
            facts
                .imports
                .iter()
                .filter(|i| i.path == qualified)
                .filter_map(|i| i.alias.clone()),
        );
        names
    }

    fn bind(&self, rel: &str, facts: &KotlinFacts, u: &Use, def: &KDef) -> Evidence {
        // A local name shadows whatever it shares a spelling with.
        if u.receiver.is_none() && facts.binding_at(&u.name, u.at).is_some() {
            return Evidence::Rejected;
        }
        if let Some(rejected) = self.visibility_rejects(rel, def) {
            return rejected;
        }
        let evidence = if def.decl.is_type() || def.decl.kind == "constructor" {
            self.bind_type(rel, facts, u, def)
        } else if def.decl.is_member() {
            self.bind_member(rel, facts, u, def)
        } else if let Some(receiver) = def.decl.receiver.as_deref() {
            self.bind_extension(rel, facts, u, def, receiver)
        } else {
            self.bind_top_level(rel, facts, u, def)
        };
        match evidence {
            Evidence::Resolved if self.overloaded(def) => Evidence::Candidate("overloaded"),
            other => other,
        }
    }

    /// `internal` stops at the project, which its test source set shares;
    /// `private` at the file, or at the class for a member.
    fn visibility_rejects(&self, rel: &str, def: &KDef) -> Option<Evidence> {
        match def.decl.effective {
            Some(Visibility::Private) if rel != def.path => Some(Evidence::Rejected),
            Some(Visibility::Internal) => {
                let same = self.project_of(rel) == self.project_of(&def.path);
                (!same).then_some(Evidence::Rejected)
            }
            _ => None,
        }
    }

    fn overloaded(&self, def: &KDef) -> bool {
        def.decl.kind == "fun"
            && self.facts(&def.path).is_some_and(|f| {
                f.declarations
                    .iter()
                    .filter(|d| {
                        d.kind == "fun"
                            && d.name == def.decl.name
                            && d.owners == def.decl.owners
                            && d.receiver == def.decl.receiver
                    })
                    .count()
                    > 1
            })
    }

    /// A top-level function or property: visible through an import, the
    /// package, or a wildcard; a receiver means someone else's member.
    fn bind_top_level(&self, rel: &str, facts: &KotlinFacts, u: &Use, def: &KDef) -> Evidence {
        match &u.receiver {
            None => self.visible_top_level(rel, facts, &u.name, def),
            Some(Receiver::Path(p)) if Some(p.join(".")) == def.package => Evidence::Resolved,
            Some(_) => Evidence::Rejected,
        }
    }

    fn visible_top_level(
        &self,
        rel: &str,
        facts: &KotlinFacts,
        local: &str,
        def: &KDef,
    ) -> Evidence {
        let qualified = def.qualified();
        if let Some(import) = facts.imported(local) {
            return if import.path == qualified {
                Evidence::Resolved
            } else {
                Evidence::Rejected
            };
        }
        if rel == def.path {
            return Evidence::Resolved;
        }
        // A declaration of the same name in the using file or its own
        // package wins over a wildcard.
        if facts
            .declarations
            .iter()
            .any(|d| d.owners.is_empty() && d.name == local && d.kind == def.decl.kind)
        {
            return Evidence::Rejected;
        }
        if facts.package == def.package {
            return self.across_projects(rel, def);
        }
        let wildcards: Vec<&str> = facts
            .imports
            .iter()
            .filter(|i| i.wildcard)
            .map(|i| i.path.as_str())
            .collect();
        match def.package.as_deref() {
            Some(pkg) if wildcards.contains(&pkg) => {
                if wildcards.len() == 1 {
                    Evidence::Resolved
                } else {
                    Evidence::Candidate("wildcard_import")
                }
            }
            _ => Evidence::Rejected,
        }
    }

    /// The same package in another project reaches the declaration only
    /// when the using project depends on the defining one.
    fn across_projects(&self, rel: &str, def: &KDef) -> Evidence {
        let (from, to) = (self.project_of(rel), self.project_of(&def.path));
        if from == to || to.is_none() {
            return Evidence::Resolved;
        }
        match (from, to) {
            (Some(f), Some(t)) if self.depends(&f, &t) => Evidence::Resolved,
            _ => Evidence::Candidate("project_dependency_unknown"),
        }
    }

    fn bind_extension(
        &self,
        rel: &str,
        facts: &KotlinFacts,
        u: &Use,
        def: &KDef,
        receiver: &str,
    ) -> Evidence {
        let visible = self.visible_top_level(rel, facts, &u.name, def);
        if visible == Evidence::Rejected {
            return Evidence::Rejected;
        }
        let Some(target) = self.resolve_type(rel, facts, receiver).or_else(|| {
            // `String`, `List`: a type outside the repository, matched by name.
            Some(TypeRef {
                path: String::new(),
                name: receiver.to_string(),
                owners: Vec::new(),
            })
        }) else {
            return Evidence::Candidate("receiver_unresolved");
        };
        let actual = match &u.receiver {
            Some(r) => self.receiver_type(rel, facts, u, r),
            None => self.enclosing_type(rel, facts, u.at),
        };
        let Some(actual) = actual else {
            return Evidence::Candidate("receiver_unresolved");
        };
        let fits = actual.name == target.name && (target.path.is_empty() || actual == target)
            || self.is_subtype(&actual, &target, 0);
        if !fits {
            return Evidence::Rejected;
        }
        // A member of the receiver's own type wins over an extension.
        if self.member_of(&actual, &def.decl.name, 0).is_some() {
            return Evidence::Rejected;
        }
        visible
    }

    fn bind_member(&self, rel: &str, facts: &KotlinFacts, u: &Use, def: &KDef) -> Evidence {
        let Some(owner) = self.owner_ref(def) else {
            return Evidence::Candidate("receiver_unresolved");
        };
        let actual = match &u.receiver {
            None => {
                let implicit = self.implicit_receivers(rel, facts, u.at);
                implicit
                    .iter()
                    .find(|t| **t == owner || self.is_subtype(t, &owner, 0))
                    .or_else(|| implicit.first())
                    .cloned()
            }
            Some(Receiver::This) => self.enclosing_type(rel, facts, u.at),
            Some(r) => self.receiver_type(rel, facts, u, r),
        };
        let Some(actual) = actual else {
            return match u.receiver {
                // A bare name outside any class is a top-level name.
                None => Evidence::Rejected,
                // An untyped receiver can hold the owner only where the file
                // names the owner's type or shares its package.
                _ if (u.kind == UseKind::Call || u.kind == UseKind::Reference)
                    && self.may_hold(rel, facts, def, &owner) =>
                {
                    Evidence::Candidate("receiver_unresolved")
                }
                _ => Evidence::Rejected,
            };
        };
        if actual == owner || self.is_subtype(&actual, &owner, 0) {
            return Evidence::Resolved;
        }
        // `C.member` for a member of `C`'s companion.
        if let Some(class) = self.companion_holder(def) {
            if actual == class || self.is_subtype(&actual, &class, 0) {
                return Evidence::Resolved;
            }
        }
        // A call through a supertype the owner implements may dispatch here.
        if self.is_subtype(&owner, &actual, 0) && def.decl.is_override {
            return Evidence::Candidate("dispatch");
        }
        Evidence::Rejected
    }

    /// Whether a file can hold a value of `owner` without spelling a type:
    /// it names the owner, one of its supertypes, or shares its package.
    fn may_hold(&self, rel: &str, facts: &KotlinFacts, def: &KDef, owner: &TypeRef) -> bool {
        if facts.package == def.package {
            return true;
        }
        let mut related: FxHashSet<String> = FxHashSet::default();
        related.insert(owner.name.clone());
        if let Some(owner_facts) = self.facts(&owner.path) {
            related.extend(owner_facts.supertypes_of(&owner.name).iter().cloned());
        }
        facts.uses.iter().any(|u| related.contains(&u.name))
            || facts
                .imports
                .iter()
                .any(|i| related.contains(i.target_name()))
            || rel == def.path
    }

    /// A class, interface, object or type alias: a type use, a constructor
    /// call, a qualifier, an import.
    fn bind_type(&self, rel: &str, facts: &KotlinFacts, u: &Use, def: &KDef) -> Evidence {
        let target = if def.decl.kind == "constructor" {
            self.owner_ref(def)
        } else {
            Some(TypeRef {
                path: def.path.clone(),
                name: def.decl.name.clone(),
                owners: def.decl.owners.clone(),
            })
        };
        let Some(target) = target else {
            return Evidence::Rejected;
        };
        let resolved = match &u.receiver {
            Some(Receiver::Path(qualifier)) => {
                let mut parts = qualifier.clone();
                parts.push(u.name.clone());
                (Some(parts.join(".")) == Some(def.qualified())
                    || qualifier.last() == target.owners.last())
                .then(|| target.clone())
            }
            Some(_) => return Evidence::Rejected,
            None => self.resolve_type(rel, facts, &u.name),
        };
        match resolved {
            Some(t) if t == target => {
                if t.path != rel
                    && facts.imported(&u.name).is_none()
                    && facts.package == def.package
                {
                    self.across_projects(rel, def)
                } else {
                    Evidence::Resolved
                }
            }
            _ => Evidence::Rejected,
        }
    }

    fn owner_ref(&self, def: &KDef) -> Option<TypeRef> {
        let (name, outer) = def.decl.owners.split_last()?;
        Some(TypeRef {
            path: def.path.clone(),
            name: name.clone(),
            owners: outer.to_vec(),
        })
    }

    /// The class around a companion `def` is a member of.
    fn companion_holder(&self, def: &KDef) -> Option<TypeRef> {
        let (companion, outer) = def.decl.owners.split_last()?;
        let (holder, rest) = outer.split_last()?;
        let facts = self.facts(&def.path)?;
        facts
            .declarations
            .iter()
            .any(|d| d.kind == "companion" && d.name == *companion && d.owners == outer)
            .then(|| TypeRef {
                path: def.path.clone(),
                name: holder.clone(),
                owners: rest.to_vec(),
            })
    }

    /// The innermost type around a byte of `rel`.
    fn enclosing_type(&self, rel: &str, facts: &KotlinFacts, at: usize) -> Option<TypeRef> {
        self.implicit_receivers(rel, facts, at).into_iter().next()
    }

    /// The instances a bare name reaches at `at`: the innermost class and,
    /// through each `inner class`, the instance that holds it.
    fn implicit_receivers(&self, rel: &str, facts: &KotlinFacts, at: usize) -> Vec<TypeRef> {
        let mut holders: Vec<&Declaration> = facts
            .declarations
            .iter()
            .filter(|d| d.is_type() && d.kind != "typealias" && d.span.contains(&at))
            .collect();
        holders.sort_by_key(|d| d.span.len());
        let mut out = Vec::new();
        for d in holders {
            out.push(TypeRef {
                path: rel.to_string(),
                name: d.name.clone(),
                owners: d.owners.clone(),
            });
            if !d.inner {
                break;
            }
        }
        out
    }

    /// The type a receiver expression has, as far as declarations say.
    fn receiver_type(
        &self,
        rel: &str,
        facts: &KotlinFacts,
        u: &Use,
        r: &Receiver,
    ) -> Option<TypeRef> {
        match r {
            Receiver::This => self.enclosing_type(rel, facts, u.at),
            Receiver::Expr => None,
            Receiver::Call(callee) => self.call_type(rel, facts, callee, u.at),
            Receiver::Name(n) => self.name_type(rel, facts, n, u.at),
            Receiver::Path(parts) => {
                let (head, rest) = parts.split_first()?;
                let mut current = if head == "this" {
                    self.enclosing_type(rel, facts, u.at)?
                } else {
                    self.name_type(rel, facts, head, u.at)?
                };
                for segment in rest {
                    current = self.property_type(&current, segment, 0)?;
                }
                Some(current)
            }
        }
    }

    /// What a single name stands for at `at`: a typed local, a property of
    /// the enclosing class (inherited ones included), a top-level property,
    /// or a type named as a qualifier (`Obj.f()`, `Companion.f()`).
    fn name_type(&self, rel: &str, facts: &KotlinFacts, name: &str, at: usize) -> Option<TypeRef> {
        if let Some(b) = facts.binding_at(name, at) {
            // The initializer reads names from before the binding.
            let from = b.start.saturating_sub(1);
            return self.value_type(
                rel,
                facts,
                b.type_name.as_deref(),
                b.init_call.as_ref(),
                from,
            );
        }
        for class in self.implicit_receivers(rel, facts, at) {
            if let Some(t) = self.property_type(&class, name, 0) {
                return Some(t);
            }
        }
        if let Some(p) = facts
            .declarations
            .iter()
            .find(|d| d.kind == "property" && d.owners.is_empty() && d.name == name)
        {
            return self.value_type(rel, facts, p.type_name.as_deref(), p.init_call.as_ref(), at);
        }
        if name.chars().next().is_some_and(char::is_uppercase) {
            return self.resolve_type(rel, facts, name);
        }
        None
    }

    /// The declared type of `name` as a property of `class` or of a type it
    /// inherits from.
    fn property_type(&self, class: &TypeRef, name: &str, depth: usize) -> Option<TypeRef> {
        let (decl_path, decl) = self.member_of(class, name, depth)?;
        if decl.kind != "property" {
            return None;
        }
        let facts = self.facts(&decl_path)?;
        self.value_type(
            &decl_path,
            &facts,
            decl.type_name.as_deref(),
            decl.init_call.as_ref(),
            decl.span.start,
        )
    }

    /// A value's written type or, failing that, what its initializer returns:
    /// a plain call, or a call on a name whose type is known.
    fn value_type(
        &self,
        rel: &str,
        facts: &KotlinFacts,
        written: Option<&str>,
        init: Option<&InitCall>,
        at: usize,
    ) -> Option<TypeRef> {
        if let Some(t) = written {
            return Some(self.type_or_external(rel, facts, t));
        }
        let init = init?;
        let Some(receiver) = &init.receiver else {
            return self.call_type(rel, facts, &init.callee, at);
        };
        // `val a = b.f()` beside `val b = a.g()` does not compile, but must
        // not recurse forever when it is written.
        if self.typing_depth.get() >= MAX_TYPING_DEPTH {
            return None;
        }
        self.typing_depth.set(self.typing_depth.get() + 1);
        let held = self.name_type(rel, facts, receiver, at);
        self.typing_depth.set(self.typing_depth.get() - 1);
        let (path, decl) = self
            .member_of(&held?, &init.callee, 0)
            .filter(|(_, d)| d.kind == "fun")?;
        let returned = decl.type_name.as_deref()?;
        let decl_facts = self.facts(&path)?;
        Some(self.type_or_external(&path, &decl_facts, returned))
    }

    /// A written type: the repository's declaration of it, or the bare name
    /// of a type from outside the repository (`HttpClient`), which is then
    /// known not to be any type declared here.
    fn type_or_external(&self, rel: &str, facts: &KotlinFacts, name: &str) -> TypeRef {
        self.resolve_type(rel, facts, name)
            .unwrap_or_else(|| TypeRef {
                path: String::new(),
                name: name.to_string(),
                owners: Vec::new(),
            })
    }

    /// What a plain call returns: a constructor its type, a function its
    /// written return type — a function of the enclosing class (inherited
    /// ones included) or of the file. An inferred return type is unknown.
    fn call_type(
        &self,
        rel: &str,
        facts: &KotlinFacts,
        callee: &str,
        at: usize,
    ) -> Option<TypeRef> {
        if callee.chars().next().is_some_and(char::is_uppercase) {
            return self.resolve_type(rel, facts, callee);
        }
        let function = self
            .implicit_receivers(rel, facts, at)
            .iter()
            .find_map(|class| {
                self.member_of(class, callee, 0)
                    .filter(|(_, d)| d.kind == "fun")
            })
            .or_else(|| {
                facts
                    .declarations
                    .iter()
                    .find(|d| d.kind == "fun" && d.owners.is_empty() && d.name == callee)
                    .map(|d| (rel.to_string(), d.clone()))
            })?;
        let (path, decl) = function;
        let returned = decl.type_name.as_deref()?;
        let decl_facts = self.facts(&path)?;
        Some(self.type_or_external(&path, &decl_facts, returned))
    }

    /// `name` declared in `class` or, failing that, in a supertype.
    fn member_of(
        &self,
        class: &TypeRef,
        name: &str,
        depth: usize,
    ) -> Option<(String, Declaration)> {
        if depth > MAX_INHERITANCE_DEPTH || class.path.is_empty() {
            return None;
        }
        let facts = self.facts(&class.path)?;
        let mut owners = class.owners.clone();
        owners.push(class.name.clone());
        if let Some(d) = facts
            .declarations
            .iter()
            .find(|d| d.name == name && d.owners == owners && !d.is_type())
        {
            return Some((class.path.clone(), d.clone()));
        }
        for sup in facts.supertypes_of(&class.name).to_vec() {
            if let Some(t) = self.resolve_type(&class.path, &facts, &sup) {
                if let Some(found) = self.member_of(&t, name, depth + 1) {
                    return Some(found);
                }
            }
        }
        None
    }

    /// Whether `sub` names `sup` among its supertypes, transitively.
    fn is_subtype(&self, sub: &TypeRef, sup: &TypeRef, depth: usize) -> bool {
        if depth > MAX_INHERITANCE_DEPTH || sub.path.is_empty() {
            return false;
        }
        let Some(facts) = self.facts(&sub.path) else {
            return false;
        };
        facts
            .supertypes_of(&sub.name)
            .to_vec()
            .into_iter()
            .any(|s| match self.resolve_type(&sub.path, &facts, &s) {
                Some(t) => t == *sup || self.is_subtype(&t, sup, depth + 1),
                None => sup.path.is_empty() && s == sup.name,
            })
    }

    /// The declaration a type name in `rel` refers to: declared in the file,
    /// imported (alias resolved), in the file's package, or behind a wildcard
    /// import. `None` for a type outside the repository.
    fn resolve_type(&self, rel: &str, facts: &KotlinFacts, name: &str) -> Option<TypeRef> {
        if let Some(d) = facts
            .declarations
            .iter()
            .filter(|d| d.is_type() && d.name == name)
            .min_by_key(|d| d.owners.len())
        {
            return Some(TypeRef {
                path: rel.to_string(),
                name: d.name.clone(),
                owners: d.owners.clone(),
            });
        }
        let (package, simple, owners) = match facts.imported(name) {
            Some(import) => {
                let parts: Vec<&str> = import.path.split('.').collect();
                // `a.b.Outer.Inner`: segments after the package start upper-case.
                let split = parts
                    .iter()
                    .position(|p| p.chars().next().is_some_and(char::is_uppercase))
                    .unwrap_or_else(|| parts.len().saturating_sub(1));
                let package = parts[..split].join(".");
                let owners: Vec<String> = parts[split..parts.len() - 1]
                    .iter()
                    .map(|s| (*s).to_string())
                    .collect();
                (Some(package), import.target_name().to_string(), owners)
            }
            None => (None, name.to_string(), Vec::new()),
        };
        let candidates = self.files_declaring(&simple);
        let found = |pkg: &str| -> Option<TypeRef> {
            candidates.iter().find_map(|path| {
                let f = self.facts(path)?;
                (f.package.as_deref().unwrap_or("") == pkg)
                    .then(|| {
                        f.declarations
                            .iter()
                            .find(|d| d.is_type() && d.name == simple && d.owners == owners)
                            .map(|d| TypeRef {
                                path: path.clone(),
                                name: d.name.clone(),
                                owners: d.owners.clone(),
                            })
                    })
                    .flatten()
            })
        };
        if let Some(pkg) = package {
            return found(&pkg);
        }
        if let Some(t) = found(facts.package.as_deref().unwrap_or("")) {
            return Some(t);
        }
        facts
            .imports
            .iter()
            .filter(|i| i.wildcard)
            .find_map(|i| found(&i.path))
    }

    /// Kotlin files that declare a type named `name`, at the run's snapshot:
    /// one grep builds the repository's index of type declarations the first
    /// time any name is asked for.
    fn files_declaring(&self, name: &str) -> Vec<String> {
        if self.type_files.borrow().is_empty() {
            let mut index: FxHashMap<String, Vec<String>> = FxHashMap::default();
            let rev = self.source.rev().map(str::to_string);
            let declared = crate::git::grep_lines_matching(
                self.root,
                r"(class|interface|object|typealias)[[:space:]]+[A-Za-z_]",
                rev.as_deref(),
                &["*.kt", "*.kts"],
            )
            .unwrap_or_default();
            for (path, _, text) in declared {
                for name in declared_type_names(&text) {
                    let files = index.entry(name).or_default();
                    if files.last() != Some(&path) && files.len() < MAX_TYPE_FILES {
                        files.push(path.clone());
                    }
                }
            }
            // An empty repository index still marks the lookup as done.
            index.entry(String::new()).or_default();
            *self.type_files.borrow_mut() = index;
        }
        self.type_files
            .borrow()
            .get(name)
            .cloned()
            .unwrap_or_default()
    }

    /// The Gradle or Maven project a file belongs to: the nearest directory
    /// above it holding a build file.
    fn project_of(&self, rel: &str) -> Option<PathBuf> {
        let mut dir = Path::new(rel).parent()?.to_path_buf();
        // Every directory walked through shares the answer, so a sibling
        // file stops at the first directory already asked about.
        let mut walked: Vec<PathBuf> = Vec::new();
        let found = loop {
            if let Some(known) = self.projects.borrow().get(&dir) {
                break known.clone();
            }
            walked.push(dir.clone());
            let has_build = ["build.gradle.kts", "build.gradle", "pom.xml"]
                .iter()
                .any(|b| {
                    self.source
                        .read_bytes(&self.root.join(&dir).join(b))
                        .is_some()
                });
            if has_build {
                break Some(dir.clone());
            }
            if !dir.pop() {
                break None;
            }
        };
        let mut projects = self.projects.borrow_mut();
        for d in walked {
            projects.insert(d, found.clone());
        }
        found
    }

    /// Whether project `from` reaches project `to` through `project(":…")`
    /// references in its build files.
    fn depends(&self, from: &Path, to: &Path) -> bool {
        let mut seen: FxHashSet<PathBuf> = FxHashSet::default();
        let mut frontier = vec![from.to_path_buf()];
        for _ in 0..MAX_PROJECT_HOPS {
            let mut next = Vec::new();
            for p in frontier {
                if !seen.insert(p.clone()) {
                    continue;
                }
                for dep in self.project_deps(&p) {
                    if dep == to {
                        return true;
                    }
                    next.push(dep);
                }
            }
            if next.is_empty() {
                return false;
            }
            frontier = next;
        }
        false
    }

    fn project_deps(&self, project: &Path) -> Vec<PathBuf> {
        if let Some(known) = self.deps.borrow().get(project) {
            return known.clone();
        }
        let settings = {
            let mut dir = project.to_path_buf();
            loop {
                let has = ["settings.gradle.kts", "settings.gradle"].iter().any(|s| {
                    self.source
                        .read_bytes(&self.root.join(&dir).join(s))
                        .is_some()
                });
                if has || !dir.pop() {
                    break dir;
                }
            }
        };
        let mut out = Vec::new();
        for build in ["build.gradle.kts", "build.gradle"] {
            let Some(text) = self
                .source
                .read_bytes(&self.root.join(project).join(build))
                .and_then(|b| String::from_utf8(b).ok())
            else {
                continue;
            };
            for (at, _) in text.match_indices("project(") {
                let rest = &text[at + "project(".len()..];
                let rest = rest.trim_start().trim_start_matches("path =").trim_start();
                let Some(quoted) = rest.strip_prefix('"').or_else(|| rest.strip_prefix('\''))
                else {
                    continue;
                };
                let Some(end) = quoted.find(['"', '\'']) else {
                    continue;
                };
                let gradle_path = &quoted[..end];
                let rel: PathBuf = gradle_path.split(':').filter(|s| !s.is_empty()).collect();
                out.push(settings.join(rel));
            }
        }
        self.deps
            .borrow_mut()
            .insert(project.to_path_buf(), out.clone());
        out
    }
}

/// The type names a source line declares: the word after each `class`,
/// `interface`, `object` or `typealias` keyword.
fn declared_type_names(line: &str) -> Vec<String> {
    let words: Vec<&str> = line
        .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '`'))
        .filter(|w| !w.is_empty())
        .collect();
    words
        .windows(2)
        .filter(|w| matches!(w[0], "class" | "interface" | "object" | "typealias"))
        .map(|w| w[1].trim_matches('`').to_string())
        .collect()
}

/// How a use relates to the definition, `None` when the use cannot reach a
/// declaration of its kind at all.
fn relation_of(u: &Use, def: &KDef) -> Option<Relation> {
    let kind = def.decl.kind;
    match u.kind {
        UseKind::Reference => Some(Relation::Reference),
        UseKind::Call if kind == "fun" || kind == "constructor" => Some(Relation::Call),
        // `Type(...)` constructs it; `val f = …; f()` invokes a property.
        UseKind::Call if def.decl.is_type() || kind == "property" => Some(Relation::Call),
        UseKind::Access if kind == "property" => Some(Relation::Call),
        UseKind::Access if def.decl.is_type() => Some(Relation::Reference),
        UseKind::TypeUse if def.decl.is_type() || kind == "constructor" => {
            Some(Relation::Reference)
        }
        _ => None,
    }
}

fn absorb(best: &mut Option<Finding>, evidence: Evidence, relation: Relation, line: u32) {
    let rank = |e: Evidence| match e {
        Evidence::Rejected => 0,
        Evidence::Candidate(_) => 1,
        Evidence::Resolved => 2,
    };
    match best {
        Some(b) if rank(b.evidence) > rank(evidence) => {}
        Some(b) if rank(b.evidence) == rank(evidence) => {
            if relation == Relation::Call {
                b.relation = Relation::Call;
            }
            if !b.lines.contains(&line) {
                b.lines.push(line);
            }
        }
        _ => {
            *best = Some(Finding {
                evidence,
                relation,
                lines: vec![line],
            });
        }
    }
}
