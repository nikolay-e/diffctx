//! What a Java file's syntax states, as the facts contract names it. The tree is
//! read by the walk the first facts layer ships for Java (`facts::java`:
//! declarations with owners, headers and modifiers; uses with the receiver they
//! are reached through; locals with the range they are visible in); this file
//! turns that reading into the path-free [`BlobFacts`] of the 2.0 contract, in
//! the conventions the Kotlin file keeps (`facts::lang::kotlin`), so the two
//! JVM languages read alike. What a reader of the facts cannot recover from the
//! types:
//!
//! - The package a file declares is an [`Import`] of kind `SideEffect` whose
//!   module is the package: Java ties a package to a directory by convention
//!   only, so the name is a fact of the blob, and a file without one has none.
//! - `import a.b.C;` is module `a.b`, names `[(C, None)]`; `import a.b.*;` is
//!   module `a.b` with `star: Star::Glob`; `import static a.b.C.m;` is module
//!   `a.b.C`, names `[(m, None)]`, and `import static a.b.C.*;` module `a.b.C`
//!   with `Star::Glob`. Whether `a.b` is a package or a class is for the
//!   resolver to find out.
//! - A definition's container is its owners (outermost first). Overloads and
//!   same-named members take `Overload(n)` in source order, the first keeping
//!   none. A constructor is a `Constructor` named like its class. An anonymous
//!   class is a `Class` named `new T` for what it instantiates, owned by the type
//!   around it. A record's components are `Field`s of the record, an enum's
//!   constants are `EnumMember`s of the enum, a `static final` field is a
//!   `Constant`; an annotation type is an `Interface`.
//! - A definition's `signature` is its header, whitespace collapsed (modifiers,
//!   annotations, type parameters, parameters, return type, `extends` and
//!   `implements`), up to the body. `decorators` are the annotations.
//! - A reference names the last segment and carries the written chain before it
//!   in `qualifier` (`a.b.f()` is `f` with `[a, b]`; `new a.B()` is `B`
//!   instantiated with `[a]`). The receiver describes the chain's head: `Typed(T)`
//!   for a local or parameter whose written type is `T`, `CallResult([f])` or
//!   `CallResult([r, f])` for one initialized by that call (`var x = new T()` is
//!   `CallResult([T])`), `SelfType` for `this`, `CallResult([f])` with an empty
//!   qualifier for `f().m()`, `Unknown` with an empty qualifier for any other
//!   expression, `None` for a name the resolver finds in scope, package or
//!   imports. `new T(..)` is an `Instantiate`; `T::m` and `x::m` are
//!   `PassAsValue`.
//! - A local's `site` is the range it is visible in: from the end of its binding
//!   to the end of its scope.
//! - A method that states `@Override` has an `Override` reference to its own
//!   name, owned by itself; `@Test`-like annotations and `public static void
//!   main` are entry points.

use std::ops::Range;

use tree_sitter::Node;

use super::jvm::shared_reader_methods;
use super::kotlin::{Leaf, POLL_EVERY, UNBOUND_PATH, error_ranges, has_word};
use super::parse_tree;
use crate::facts::framework;
use crate::facts::kotlin as old;
use crate::facts::{
    BlobFacts, Def, DefKind, Disambig, EntryKind, EntryPoint, Extractor, Fact, Import, ImportKind,
    Language, Local, LocalKey, LocalKind, ModulePath, Name, Namespace, Receiver, Ref, Scope, Span,
    Star, TypeName, UseKind, Visibility, expired, hash_text, hash_tokens,
};
use crate::snapshot::{Loss, RepoPath, RunBudget};

/// Annotations that make a method a test a runner calls.
const TEST_ANNOTATIONS: &[&str] = &[
    "Test",
    "ParameterizedTest",
    "RepeatedTest",
    "TestFactory",
    "TestTemplate",
];

pub struct JavaExtractor;

impl Extractor for JavaExtractor {
    fn language(&self) -> Language {
        Language::Java
    }

    fn extract(&self, text: &[u8], budget: &RunBudget) -> BlobFacts {
        extract(text, budget)
    }
}

fn unbound() -> RepoPath {
    RepoPath::new(UNBOUND_PATH).expect("a valid path")
}

fn extract(text: &[u8], budget: &RunBudget) -> BlobFacts {
    let mut blob = BlobFacts::new(Language::Java);
    let Some(tree) = parse_tree(Language::Java, text, budget) else {
        if expired(budget) {
            blob.deadline();
        } else {
            blob.losses.push(Loss::Unsupported { path: unbound() });
        }
        return blob;
    };
    let root = tree.root_node();
    let mut read = crate::facts::java::extract(root, text);
    enum_members(root, text, &mut read);
    let lines = error_ranges(&read.error_lines);
    if !lines.is_empty() {
        blob.parse_error(lines);
    }
    let mut reader = Reader::new(text, root, &read, budget);
    reader.run();
    if reader.halted {
        blob.deadline();
    }
    blob.facts = reader.facts;
    framework::derive(&mut blob);
    blob.record_lexical(root);
    blob.in_source_order();
    blob
}

/// The constants of every enum, which the first walk does not declare: each is a
/// member of the enum around it, named by its identifier.
fn enum_members(root: Node<'_>, bytes: &[u8], read: &mut old::KotlinFacts) {
    let mut found: Vec<old::Declaration> = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "enum_constant" {
            if let Some(member) = enum_member(node, bytes) {
                found.push(member);
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    if found.is_empty() {
        return;
    }
    read.declarations.extend(found);
    read.declarations
        .sort_by_key(|d| (d.span.start, std::cmp::Reverse(d.span.end)));
}

fn enum_member(node: Node<'_>, bytes: &[u8]) -> Option<old::Declaration> {
    let name = old::text(node.child_by_field_name("name")?, bytes)?.to_string();
    let mut owners: Vec<String> = Vec::new();
    let mut current = node.parent();
    while let Some(parent) = current {
        match parent.kind() {
            "class_declaration"
            | "interface_declaration"
            | "enum_declaration"
            | "record_declaration"
            | "annotation_type_declaration" => {
                if let Some(owner) = parent
                    .child_by_field_name("name")
                    .and_then(|n| old::text(n, bytes))
                {
                    owners.push(owner.to_string());
                }
            }
            "object_creation_expression" => {
                if let Some(t) = parent
                    .child_by_field_name("type")
                    .and_then(|t| old::text(t, bytes))
                {
                    owners.push(format!("new {t}"));
                }
            }
            _ => {}
        }
        current = parent.parent();
    }
    owners.reverse();
    let header_end = node
        .child_by_field_name("body")
        .map_or_else(|| node.end_byte(), |b| b.start_byte());
    Some(old::Declaration {
        type_name: owners.last().cloned(),
        name,
        kind: "constant",
        owners,
        receiver: None,
        init_call: None,
        visibility: Some(old::Visibility::Public),
        effective: Some(old::Visibility::Public),
        is_override: false,
        inner: false,
        abstract_member: false,
        annotations: Vec::new(),
        required_params: None,
        params: None,
        start_line: old::line_of(node),
        end_line: node.end_position().row as u32 + 1,
        span: node.byte_range(),
        header: node.start_byte()..header_end,
        signature: old::squash(bytes, node.start_byte()..header_end),
    })
}

struct Reader<'a> {
    bytes: &'a [u8],
    read: &'a old::KotlinFacts,
    budget: &'a RunBudget,
    leaves: Vec<Leaf>,
    line_starts: Vec<usize>,
    keys: Vec<LocalKey>,
    facts: Vec<Fact>,
    halted: bool,
    ticks: usize,
}

impl<'a> Reader<'a> {
    fn new(
        bytes: &'a [u8],
        root: Node<'_>,
        read: &'a old::KotlinFacts,
        budget: &'a RunBudget,
    ) -> Self {
        let mut line_starts = vec![0];
        line_starts.extend(
            bytes
                .iter()
                .enumerate()
                .filter(|(_, b)| **b == b'\n')
                .map(|(at, _)| at + 1),
        );
        let mut leaves = Vec::new();
        let mut cursor = root.walk();
        let mut descending = true;
        loop {
            let node = cursor.node();
            if descending && node.child_count() == 0 && node.end_byte() > node.start_byte() {
                leaves.push(Leaf {
                    start: node.start_byte(),
                    end: node.end_byte(),
                    comment: matches!(node.kind(), "line_comment" | "block_comment"),
                });
            }
            if descending && cursor.goto_first_child() {
                continue;
            }
            if cursor.goto_next_sibling() {
                descending = true;
                continue;
            }
            if !cursor.goto_parent() {
                break;
            }
            descending = false;
        }
        Self {
            bytes,
            read,
            budget,
            leaves,
            line_starts,
            keys: Vec::new(),
            facts: Vec::new(),
            halted: false,
            ticks: 0,
        }
    }

    shared_reader_methods!();

    fn run(&mut self) {
        self.package();
        self.imports();
        self.definitions();
        if self.halted {
            return;
        }
        self.references();
        if self.halted {
            return;
        }
        self.locals();
    }

    // ---- package and imports

    fn imports(&mut self) {
        for import in &self.read.imports {
            let line = import.line.saturating_sub(1) as usize;
            let start = self.line_starts.get(line).copied().unwrap_or(0);
            let end = self
                .line_starts
                .get(line + 1)
                .copied()
                .unwrap_or(self.bytes.len());
            let site = self.span(start..end.max(start + 1));
            let (module, names, star) = if import.wildcard {
                (import.path.clone(), Vec::new(), Star::Glob)
            } else {
                let (module, last) = match import.path.rsplit_once('.') {
                    Some((module, last)) => (module.to_string(), last),
                    None => (String::new(), import.path.as_str()),
                };
                (module, vec![(Name::new(last), None)], Star::None)
            };
            self.facts.push(Fact::Import(Import {
                site,
                enclosing: Scope::Module,
                module: ModulePath::new(module),
                relative: 0,
                kind: ImportKind::Static,
                names,
                star,
                default: None,
            }));
        }
    }

    // ---- definitions

    fn definitions(&mut self) {
        let decls = &self.read.declarations;
        let mut keys: Vec<LocalKey> = Vec::with_capacity(decls.len());
        let mut seen: Vec<(Vec<Name>, Name, Namespace, u16)> = Vec::new();
        for d in decls {
            let container: Vec<Name> = d.owners.iter().map(|o| Name::new(o.as_str())).collect();
            let ns = if d.is_type() {
                Namespace::Type
            } else {
                Namespace::Value
            };
            let name = Name::new(d.name.as_str());
            let at = seen
                .iter_mut()
                .find(|(c, n, s, _)| *c == container && *n == name && *s == ns);
            let twin = match at {
                Some(entry) => {
                    entry.3 += 1;
                    entry.3
                }
                None => {
                    seen.push((container.clone(), name.clone(), ns, 0));
                    0
                }
            };
            let mut key = LocalKey::new(container, name, ns);
            if twin > 0 {
                key = key.with_disambig(Disambig::Overload(twin));
            }
            keys.push(key);
        }
        self.keys = keys;
        for index in 0..self.read.declarations.len() {
            if self.tick() {
                return;
            }
            let def = self.definition(index);
            self.facts.push(Fact::Definition(def));
            self.entry_point(index);
        }
    }

    fn definition(&self, index: usize) -> Def {
        let decls = &self.read.declarations;
        let d = &decls[index];
        let key = self.keys[index].clone();
        let kind = kind_of(d);
        let nested = self.nested(index);
        let header = d.header.start.min(d.span.end)..d.header.end.min(d.span.end);
        let body_range = header.end..d.span.end;
        let first = self.leaves.partition_point(|l| l.start < body_range.start);
        let mut tokens: Vec<&[u8]> = Vec::new();
        let mut skip = 0;
        for leaf in &self.leaves[first..] {
            if leaf.end > body_range.end {
                break;
            }
            while skip < nested.len() && nested[skip].end <= leaf.start {
                skip += 1;
            }
            let inside = nested
                .get(skip)
                .is_some_and(|n| leaf.start >= n.start && leaf.end <= n.end);
            if !leaf.comment && !inside {
                tokens.push(&self.bytes[leaf.start..leaf.end]);
            }
        }
        let body = hash_tokens(tokens);
        let mut text = Vec::new();
        let mut at = d.span.start;
        for range in &nested {
            text.extend_from_slice(&self.bytes[at.min(range.start)..range.start]);
            at = range.end;
        }
        text.extend_from_slice(&self.bytes[at.min(d.span.end)..d.span.end]);
        Def {
            local: key,
            kind,
            span: self.span(d.span.clone()),
            name_span: self.name_span(d),
            signature: Some(d.signature.clone()),
            decorators: d
                .annotations
                .iter()
                .map(|a| Name::new(a.as_str()))
                .collect(),
            visibility: visibility_of(d),
            body_hash: body.unwrap_or(0),
            text_hash: hash_text(&text),
            doc_hash: self.doc_hash(d.span.start),
        }
    }

    /// Where the name is written in the header: a type's after its keyword, a
    /// method's or a constructor's before the parameter list, a field's before
    /// its initializer, a separator or the end.
    fn name_span(&self, d: &old::Declaration) -> Span {
        let header = self.slice(d.header.start..d.header.end.min(self.bytes.len()));
        let limit = match d.kind {
            "fun" | "constructor" => header.find('(').unwrap_or(header.len()),
            _ => header.len(),
        };
        let boundary =
            |c: Option<char>| c.is_none_or(|c| !(c.is_alphanumeric() || c == '_' || c == '$'));
        let mut found: Option<usize> = None;
        let mut from = 0;
        while let Some(at) = header[from..limit.max(from)].find(d.name.as_str()) {
            let start = from + at;
            let end = start + d.name.len();
            if boundary(header[..start].chars().next_back())
                && boundary(header[end..].chars().next())
            {
                match d.kind {
                    "class" | "interface" | "constant" => {
                        found = Some(start);
                        break;
                    }
                    "property" => {
                        let next = header[end..].trim_start().chars().next();
                        if next.is_none_or(|c| matches!(c, '=' | ';' | ',' | '[')) {
                            found = Some(start);
                            break;
                        }
                    }
                    _ => found = Some(start),
                }
            }
            from = end;
        }
        let start = d.header.start + found.unwrap_or(0);
        let end = if found.is_some() {
            start + d.name.len()
        } else {
            start
        };
        self.span(start..end)
    }

    fn entry_point(&mut self, index: usize) {
        let d = &self.read.declarations[index];
        if d.kind != "fun" {
            return;
        }
        let kind = if d
            .annotations
            .iter()
            .any(|a| TEST_ANNOTATIONS.contains(&a.as_str()))
        {
            EntryKind::Test
        } else if d.name == "main" && has_word(&d.signature, "static") {
            EntryKind::Main
        } else {
            return;
        };
        self.facts.push(Fact::EntryPoint(EntryPoint {
            site: self.name_span(d),
            kind,
            target: Scope::Def(self.keys[index].clone()),
            framework: None,
        }));
    }

    // ---- references

    fn references(&mut self) {
        let uses = &self.read.uses;
        let positions: Vec<usize> = uses.iter().map(|u| u.at).collect();
        let owners = self.owners(&positions);
        for (u, owner) in uses.iter().zip(owners) {
            if self.tick() {
                return;
            }
            let instantiates = u.kind == old::UseKind::Call && self.after_new(u.at);
            let (receiver, qualifier) = if instantiates && u.receiver.is_none() {
                (Receiver::None, self.written_qualifier(u))
            } else {
                self.receiver_of(u)
            };
            let use_kind = match u.kind {
                old::UseKind::Call if instantiates => UseKind::Instantiate,
                old::UseKind::Call => UseKind::Call,
                old::UseKind::TypeUse => UseKind::TypeUse,
                old::UseKind::Reference => UseKind::PassAsValue,
                old::UseKind::Access if self.assigned(u) => UseKind::Write,
                old::UseKind::Access => UseKind::Read,
            };
            let site = self.span(u.at..u.at + u.name.len());
            self.facts.push(Fact::Reference(Ref {
                site,
                enclosing: self.scope_of(owner),
                receiver,
                use_kind,
                name: Name::new(u.name.as_str()),
                qualifier,
            }));
        }
        for (index, d) in self.read.declarations.iter().enumerate() {
            if d.is_override && d.kind == "fun" {
                self.facts.push(Fact::Reference(Ref {
                    site: self.name_span(d),
                    enclosing: Scope::Def(self.keys[index].clone()),
                    receiver: Receiver::None,
                    use_kind: UseKind::Override,
                    name: Name::new(d.name.as_str()),
                    qualifier: Vec::new(),
                }));
            }
        }
    }

    /// Whether the type written at `at` follows the keyword `new`.
    fn after_new(&self, at: usize) -> bool {
        let before = &self.bytes[..at.min(self.bytes.len())];
        let trimmed = before.trim_ascii_end();
        trimmed.ends_with(b"new")
            && trimmed.len() < before.len()
            && trimmed[..trimmed.len() - 3]
                .last()
                .is_none_or(|b| !(b.is_ascii_alphanumeric() || *b == b'_' || *b == b'$'))
    }

    /// The package or outer types written before a constructed type:
    /// `new a.b.Foo()` names `Foo` with `[a, b]`.
    fn written_qualifier(&self, u: &old::Use) -> Vec<Name> {
        let written: String = self
            .slice(u.at..(u.at + 256).min(self.bytes.len()))
            .chars()
            .take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '$' | '.'))
            .collect();
        match written.strip_suffix(u.name.as_str()) {
            Some(prefix) if prefix.ends_with('.') => prefix
                .trim_end_matches('.')
                .split('.')
                .map(Name::new)
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Whether the name is the target of an assignment.
    fn assigned(&self, u: &old::Use) -> bool {
        let rest = self.slice(u.at + u.name.len()..self.bytes.len().min(u.at + u.name.len() + 8));
        let rest = rest.trim_start_matches([' ', '\t']);
        (rest.starts_with('=') && !rest.starts_with("=="))
            || ["+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "++", "--"]
                .iter()
                .any(|op| rest.starts_with(op))
    }

    /// A written chain and what its head is, when the head is a local.
    fn chain(&self, at: usize, parts: &[String]) -> (Receiver, Vec<Name>) {
        let qualifier: Vec<Name> = parts.iter().map(|p| Name::new(p.as_str())).collect();
        let Some(head) = parts.first() else {
            return (Receiver::None, qualifier);
        };
        let Some(binding) = self.read.binding_at(head, at) else {
            return (Receiver::None, qualifier);
        };
        let receiver = if let Some(written) = &binding.type_name {
            Receiver::Typed(TypeName::new(written.as_str()))
        } else if let Some(call) = &binding.init_call {
            let mut chain: Vec<Name> = Vec::new();
            if let Some(receiver) = &call.receiver {
                chain.push(Name::new(receiver.as_str()));
            }
            chain.push(Name::new(call.callee.as_str()));
            Receiver::CallResult(chain)
        } else {
            Receiver::Unknown
        };
        (receiver, qualifier)
    }

    // ---- locals
}

fn kind_of(d: &old::Declaration) -> DefKind {
    match d.kind {
        "class" if has_word(&d.signature, "enum") => DefKind::Enum,
        "class" => DefKind::Class,
        "interface" => DefKind::Interface,
        "constructor" => DefKind::Constructor,
        "fun" => DefKind::Method,
        "constant" => DefKind::EnumMember,
        "property" if has_word(&d.signature, "static") && has_word(&d.signature, "final") => {
            DefKind::Constant
        }
        _ => DefKind::Field,
    }
}

/// What the declaration reaches: Java's unmarked, package-private declaration is
/// the one the shared walk spells `Internal`.
fn visibility_of(d: &old::Declaration) -> Visibility {
    match d.effective.or(d.visibility) {
        Some(old::Visibility::Private) => Visibility::Private,
        Some(old::Visibility::Protected) => Visibility::Protected,
        Some(old::Visibility::Internal) => Visibility::Package,
        Some(old::Visibility::Public) | None => Visibility::Public,
    }
}
