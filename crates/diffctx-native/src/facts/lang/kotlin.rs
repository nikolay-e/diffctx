//! What a Kotlin file's syntax states, as the facts contract names it. The tree
//! is read by the walk the first facts layer already ships (`facts::kotlin`:
//! declarations with owners, receivers, headers and modifiers; uses with the
//! receiver they are reached through; locals with the range they shadow in);
//! this file turns that reading into the path-free [`BlobFacts`] of the 2.0
//! contract. Conventions a reader of the facts cannot recover from the types:
//!
//! - The package a file declares is an [`Import`] of kind `SideEffect` whose
//!   module is the package: Kotlin ties a package to no directory, so the name
//!   is a fact of the blob, and a file without one has none.
//! - `import a.b.C as D` is module `a.b`, names `[(C, Some(D))]`; `import a.b.*`
//!   is module `a.b` with `star: Star::Glob`. Whether `a.b` is a package, a
//!   class or an object is for the resolver to find out.
//! - A definition's container is its owners (outermost first); an extension
//!   `fun String.pad()` has its receiver type as the last container name and
//!   the disambiguator `ImplOf(receiver)`. Overloads take `Overload(n)` in
//!   source order, the first keeping none. A secondary constructor is a
//!   `Constructor` named like its class; a companion without a name is
//!   `Companion`. The primary constructor is the class.
//! - A definition's `signature` is its header, whitespace collapsed (modifiers,
//!   annotations, type parameters, parameters, return type, supertypes), up to
//!   the body; when no type is written and the initializer or expression body
//!   is a plain call, ` = callee()` or ` = receiver.callee()` follows it, since
//!   that call is what types the value. `decorators` are the annotations.
//! - A reference names the last segment and carries the written chain before
//!   it in `qualifier` (`a.b.f()` is `f` with `[a, b]`; `A.B` as a type is `B`
//!   with `[A]`). The receiver describes the chain's head: `Typed(T)` for a
//!   local or parameter whose written type is `T`, `CallResult([f])` or
//!   `CallResult([r, f])` for one initialized by that call, `SelfType` for
//!   `this`, `CallResult([f])` with an empty qualifier for `f().m()`,
//!   `Unknown` with an empty qualifier for any other expression, `None` for a
//!   name the resolver finds in scope, package or imports. A `::name`
//!   reference is `PassAsValue`.
//! - A local's `site` is the range it is visible in: from the end of its
//!   binding to the end of its scope.
//! - A member that states `override` has an `Override` reference to its own
//!   name, owned by itself; `@Test`-like annotations and a top-level `main`
//!   are entry points.

use std::cell::RefCell;
use std::ops::Range;

use tree_sitter::{Language as Grammar, Node, ParseOptions, ParseState, Parser, Tree};

use crate::facts::framework;
use crate::facts::kotlin as old;
use crate::facts::{
    BlobFacts, Def, DefKind, Disambig, EntryKind, EntryPoint, Extractor, Fact, Import, ImportKind,
    Language, Local, LocalKey, LocalKind, ModulePath, Name, Namespace, Receiver, Ref, Scope, Span,
    Star, TypeName, UseKind, Visibility, expired, hash_text, hash_tokens,
};
use crate::snapshot::{Loss, RepoPath, RunBudget};

const UNBOUND_PATH: &str = "<blob>";
const POLL_EVERY: usize = 256;
const MAX_ERROR_RANGES: usize = 32;
/// Names an unwrapped binding is followed back through to the one that types it.
const MAX_ALIAS_HOPS: usize = 4;

/// Annotations that make a function a test a runner calls.
const TEST_ANNOTATIONS: &[&str] = &[
    "Test",
    "ParameterizedTest",
    "RepeatedTest",
    "TestFactory",
    "TestTemplate",
];

pub struct KotlinExtractor;

impl Extractor for KotlinExtractor {
    fn language(&self) -> Language {
        Language::Kotlin
    }

    fn extract(&self, text: &[u8], budget: &RunBudget) -> BlobFacts {
        extract(text, budget)
    }
}

fn unbound() -> RepoPath {
    RepoPath::new(UNBOUND_PATH).expect("a valid path")
}

#[cfg(feature = "brokk-tree-sitter-kotlin")]
fn grammar() -> Option<Grammar> {
    Some(Grammar::new(brokk_tree_sitter_kotlin::LANGUAGE))
}

#[cfg(not(feature = "brokk-tree-sitter-kotlin"))]
fn grammar() -> Option<Grammar> {
    None
}

thread_local! {
    static PARSER: RefCell<Option<Parser>> = const { RefCell::new(None) };
}

#[allow(
    clippy::disallowed_methods,
    reason = "a grammar that does not load or a parse that ends is `None`, which `extract` turns into a named loss; past the end the read callback gives no bytes"
)]
fn parse(text: &[u8], budget: &RunBudget) -> Option<Tree> {
    let language = grammar()?;
    PARSER.with(|slot| {
        let mut slot = slot.borrow_mut();
        let parser = slot.get_or_insert_with(Parser::new);
        parser.set_language(&language).ok()?;
        let mut progress = |_: &ParseState| expired(budget);
        let len = text.len();
        let tree = parser.parse_with_options(
            &mut |i, _| (i < len).then(|| &text[i..]).unwrap_or_default(),
            None,
            Some(ParseOptions::new().progress_callback(&mut progress)),
        );
        if tree.is_none() {
            parser.reset();
        }
        tree
    })
}

fn extract(text: &[u8], budget: &RunBudget) -> BlobFacts {
    let mut blob = BlobFacts::new(Language::Kotlin);
    let Some(tree) = parse(text, budget) else {
        if expired(budget) {
            blob.deadline();
        } else {
            blob.losses.push(Loss::Unsupported { path: unbound() });
        }
        return blob;
    };
    let root = tree.root_node();
    let read = old::extract(root, text);
    blob_of(Language::Kotlin, Dialect::Kotlin, text, root, &read, budget)
}

/// What differs between the languages this reader serves: Swift's syntax facts
/// take Kotlin's shape (`facts::lang::swift`), and the contract's conventions
/// are the same but for the ones the types of the Swift file say.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Dialect {
    Kotlin,
    Swift,
}

/// The facts of one blob, from the syntax facts the language's walk made of it.
pub(super) fn blob_of(
    language: Language,
    dialect: Dialect,
    text: &[u8],
    root: Node<'_>,
    read: &old::KotlinFacts,
    budget: &RunBudget,
) -> BlobFacts {
    let mut blob = BlobFacts::new(language);
    let lines = error_ranges(&read.error_lines);
    if !lines.is_empty() {
        blob.parse_error(lines);
    }
    let mut reader = Reader::new(dialect, text, root, read, budget);
    reader.run();
    if reader.halted {
        blob.deadline();
    }
    blob.facts = reader.facts;
    framework::derive(&mut blob);
    blob.in_source_order();
    blob
}

/// The error lines (sorted, distinct) as merged ranges.
fn error_ranges(lines: &[u32]) -> Vec<(u32, u32)> {
    let mut ranges: Vec<(u32, u32)> = Vec::new();
    for &line in lines {
        match ranges.last_mut() {
            Some(last) if line <= last.1 + 1 => last.1 = line,
            _ => ranges.push((line, line)),
        }
    }
    ranges.truncate(MAX_ERROR_RANGES);
    ranges
}

/// A leaf of the tree: its bytes, and whether it is a comment.
struct Leaf {
    start: usize,
    end: usize,
    comment: bool,
}

struct Reader<'a> {
    dialect: Dialect,
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
        dialect: Dialect,
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
                    comment: matches!(
                        node.kind(),
                        "line_comment" | "multiline_comment" | "comment"
                    ),
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
            dialect,
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

    fn tick(&mut self) -> bool {
        self.ticks += 1;
        if self.ticks % POLL_EVERY == 0 && expired(self.budget) {
            self.halted = true;
        }
        self.halted
    }

    fn line_at(&self, byte: usize) -> u32 {
        self.line_starts.partition_point(|start| *start <= byte) as u32
    }

    fn span(&self, range: Range<usize>) -> Span {
        let first = self.line_at(range.start);
        let last = self.line_at(range.end.saturating_sub(1).max(range.start));
        Span::new(
            range.start as u32..range.end as u32,
            (first, last.max(first)),
        )
    }

    fn slice(&self, range: Range<usize>) -> &'a str {
        let end = range.end.min(self.bytes.len());
        let start = range.start.min(end);
        std::str::from_utf8(&self.bytes[start..end]).unwrap_or("")
    }

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

    fn package(&mut self) {
        let Some(package) = self.read.package.as_deref() else {
            return;
        };
        let at = self
            .line_starts
            .iter()
            .copied()
            .find(|start| {
                self.slice(*start..(*start + 7).min(self.bytes.len()))
                    .starts_with("package")
            })
            .unwrap_or(0);
        let end = self
            .slice(at..(at + 512).min(self.bytes.len()))
            .find('\n')
            .map_or((at + 512).min(self.bytes.len()), |n| at + n);
        self.facts.push(Fact::Import(Import {
            site: self.span(at..end.max(at + 1)),
            enclosing: Scope::Module,
            module: ModulePath::new(package),
            relative: 0,
            kind: ImportKind::SideEffect,
            names: Vec::new(),
            star: Star::None,
            default: None,
        }));
    }

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
                let alias = import.alias.as_deref().map(Name::new);
                (module, vec![(Name::new(last), alias)], Star::None)
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
        let mut seen: Vec<(Vec<Name>, Name, Namespace, bool, u16)> = Vec::new();
        for d in decls {
            // A Swift `extension T` is a definition that extends `T`, as a Kotlin
            // extension function extends its receiver type.
            let extends = d.kind == "extension";
            let extension = d.receiver.is_some() || extends;
            let mut container: Vec<Name> = d.owners.iter().map(|o| Name::new(o.as_str())).collect();
            if let Some(receiver) = &d.receiver {
                container.push(Name::new(receiver.as_str()));
            }
            let ns = if d.is_type() || extends {
                Namespace::Type
            } else {
                Namespace::Value
            };
            let name = Name::new(d.name.as_str());
            let at = seen.iter_mut().find(|(c, n, s, e, _)| {
                *c == container && *n == name && *s == ns && *e == extension
            });
            let twin = match at {
                Some(entry) => {
                    entry.4 += 1;
                    entry.4
                }
                None => {
                    seen.push((container.clone(), name.clone(), ns, extension, 0));
                    0
                }
            };
            let mut key = LocalKey::new(container, name, ns);
            if let Some(receiver) = &d.receiver {
                key = key.with_disambig(Disambig::ImplOf(TypeName::new(receiver.as_str())));
            } else if extends {
                key = key.with_disambig(Disambig::ImplOf(TypeName::new(d.name.as_str())));
            } else if twin > 0 {
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
        let kind = self.kind_of(d);
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
            signature: Some(self.signature(d)),
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

    fn kind_of(&self, d: &old::Declaration) -> DefKind {
        match d.kind {
            "extension" => DefKind::Class,
            "class" if has_word(&d.signature, "enum") => DefKind::Enum,
            "class" if self.dialect == Dialect::Swift && has_word(&d.signature, "struct") => {
                DefKind::Struct
            }
            "class" => DefKind::Class,
            "interface" => DefKind::Interface,
            "object" | "companion" => DefKind::Object,
            "typealias" => DefKind::TypeAlias,
            "constructor" => DefKind::Constructor,
            "fun" if d.owners.is_empty() || d.receiver.is_some() => DefKind::Function,
            "fun" => DefKind::Method,
            "property" if d.owners.is_empty() && d.receiver.is_none() => {
                if has_word(&d.signature, "const") {
                    DefKind::Constant
                } else {
                    DefKind::Variable
                }
            }
            _ => DefKind::Property,
        }
    }

    /// The header as the reader meets it, and the call that types a value when
    /// no type is written.
    fn signature(&self, d: &old::Declaration) -> String {
        let mut text = d.signature.clone();
        if d.kind == "property" {
            // An accessor written on the same line is in the node, not in the
            // header.
            let cut = [
                " get(",
                " set(",
                " get ",
                " set ",
                " private set",
                " protected set",
                " internal set",
            ]
            .iter()
            .filter_map(|marker| text.find(marker))
            .min();
            if let Some(at) = cut {
                text.truncate(at);
            }
        }
        if d.type_name.is_none() {
            if let Some(call) = &d.init_call {
                text.push_str(" = ");
                if let Some(receiver) = &call.receiver {
                    text.push_str(receiver);
                    text.push('.');
                }
                text.push_str(&call.callee);
                text.push_str("()");
            }
        }
        text
    }

    /// The ranges of the declarations written inside `index`, outermost only.
    fn nested(&self, index: usize) -> Vec<Range<usize>> {
        let decls = &self.read.declarations;
        let span = &decls[index].span;
        let mut out: Vec<Range<usize>> = Vec::new();
        for other in &decls[index + 1..] {
            if other.span.start >= span.end {
                break;
            }
            if other.span.end > span.end || other.span.start < span.start {
                continue;
            }
            if out.last().is_some_and(|last| other.span.start < last.end) {
                continue;
            }
            out.push(other.span.clone());
        }
        out
    }

    /// Where the name is written in the header.
    fn name_span(&self, d: &old::Declaration) -> Span {
        let header = self.slice(d.header.start..d.header.end.min(self.bytes.len()));
        let limit = match d.kind {
            "fun" => header.find('(').unwrap_or(header.len()),
            "property" => header.find([':', '=', '\n']).unwrap_or(header.len()),
            _ => header.len(),
        };
        let name = if d.kind == "constructor" {
            "constructor"
        } else {
            d.name.as_str()
        };
        let mut found: Option<usize> = None;
        let mut from = 0;
        while let Some(at) = header[from..limit.max(from)].find(name) {
            let start = from + at;
            let end = start + name.len();
            let before = header[..start].chars().next_back();
            let after = header[end..].chars().next();
            let boundary = |c: Option<char>| c.is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
            if boundary(before) && boundary(after) {
                found = Some(start);
                if matches!(
                    d.kind,
                    "class" | "interface" | "object" | "companion" | "typealias" | "extension"
                ) {
                    break;
                }
            }
            from = end;
        }
        let start = d.header.start + found.unwrap_or(0);
        let end = if found.is_some() {
            start + name.len()
        } else {
            start
        };
        self.span(start..end)
    }

    fn doc_hash(&self, start: usize) -> Option<u64> {
        let before = self.leaves.partition_point(|l| l.end <= start);
        let leaf = self.leaves[..before].last()?;
        if !leaf.comment || !self.slice(leaf.start..leaf.end).starts_with("/**") {
            return None;
        }
        if !self.slice(leaf.end..start).trim().is_empty() {
            return None;
        }
        let text = self.slice(leaf.start..leaf.end);
        hash_tokens(
            text.split(|c: char| c.is_whitespace() || c == '*' || c == '/')
                .filter(|w| !w.is_empty())
                .map(str::as_bytes),
        )
    }

    /// A Swift type marked `@main` is the program's entry; a method named
    /// `test…` of an `XCTestCase` is a test a runner calls.
    fn swift_entry(&self, d: &old::Declaration) -> Option<EntryKind> {
        if d.kind == "class" && d.annotations.iter().any(|a| a == "main") {
            return Some(EntryKind::Main);
        }
        let owner = d.owners.last()?;
        let xctest = self
            .read
            .supertypes_of(owner)
            .iter()
            .any(|s| s == "XCTestCase");
        (d.kind == "fun" && d.name.starts_with("test") && xctest).then_some(EntryKind::Test)
    }

    fn entry_point(&mut self, index: usize) {
        let d = &self.read.declarations[index];
        if self.dialect == Dialect::Swift {
            if let Some(kind) = self.swift_entry(d) {
                self.facts.push(Fact::EntryPoint(EntryPoint {
                    site: self.name_span(d),
                    kind,
                    target: Scope::Def(self.keys[index].clone()),
                    framework: None,
                }));
                return;
            }
        }
        if d.kind != "fun" {
            return;
        }
        let kind = if d
            .annotations
            .iter()
            .any(|a| TEST_ANNOTATIONS.contains(&a.as_str()))
        {
            EntryKind::Test
        } else if d.owners.is_empty() && d.receiver.is_none() && d.name == "main" {
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

    /// The innermost declaration holding each of the (ascending) bytes asked.
    fn owners(&self, bytes: &[usize]) -> Vec<Option<usize>> {
        let decls = &self.read.declarations;
        let mut out = Vec::with_capacity(bytes.len());
        let mut stack: Vec<usize> = Vec::new();
        let mut next = 0;
        for &byte in bytes {
            while next < decls.len() && decls[next].span.start <= byte {
                stack.push(next);
                next += 1;
            }
            while let Some(&top) = stack.last() {
                if decls[top].span.end <= byte {
                    stack.pop();
                } else {
                    break;
                }
            }
            out.push(stack.last().copied());
        }
        out
    }

    fn scope_of(&self, owner: Option<usize>) -> Scope {
        owner.map_or(Scope::Module, |at| Scope::Def(self.keys[at].clone()))
    }

    fn references(&mut self) {
        let uses = &self.read.uses;
        let positions: Vec<usize> = uses.iter().map(|u| u.at).collect();
        let owners = self.owners(&positions);
        for (u, owner) in uses.iter().zip(owners) {
            if self.tick() {
                return;
            }
            let (receiver, qualifier) = self.receiver_of(u);
            let use_kind = match u.kind {
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
            if d.is_override && d.kind != "constructor" {
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

    /// Whether the name is the target of an assignment.
    fn assigned(&self, u: &old::Use) -> bool {
        let rest = self.slice(u.at + u.name.len()..self.bytes.len().min(u.at + u.name.len() + 8));
        let rest = rest.trim_start_matches([' ', '\t']);
        (rest.starts_with('=') && !rest.starts_with("=="))
            || ["+=", "-=", "*=", "/=", "%=", "++", "--"]
                .iter()
                .any(|op| rest.starts_with(op))
    }

    fn receiver_of(&self, u: &old::Use) -> (Receiver, Vec<Name>) {
        let name = |s: &String| Name::new(s.as_str());
        match &u.receiver {
            None => (Receiver::None, Vec::new()),
            Some(old::Receiver::This) => (Receiver::SelfType, Vec::new()),
            Some(old::Receiver::Expr) => (Receiver::Unknown, Vec::new()),
            Some(old::Receiver::Call(callee)) => {
                (Receiver::CallResult(vec![name(callee)]), Vec::new())
            }
            Some(old::Receiver::Name(head)) => self.chain(u.at, std::slice::from_ref(head)),
            Some(old::Receiver::Path(parts)) => match parts.split_first() {
                Some((first, rest)) if first == "this" => {
                    (Receiver::SelfType, rest.iter().map(name).collect())
                }
                _ => self.chain(u.at, parts),
            },
        }
    }

    /// A written chain and what its head is, when the head is a local.
    fn chain(&self, at: usize, parts: &[String]) -> (Receiver, Vec<Name>) {
        let mut qualifier: Vec<Name> = parts.iter().map(|p| Name::new(p.as_str())).collect();
        let Some(head) = parts.first() else {
            return (Receiver::None, qualifier);
        };
        let Some(mut binding) = self.read.binding_at(head, at) else {
            return (Receiver::None, qualifier);
        };
        // `if let t = maybe` and `guard let t = maybe` bind a name that has the
        // type of the one it unwraps: the chain goes on from that name, a local
        // of its own or one the resolver finds in scope.
        for _ in 0..MAX_ALIAS_HOPS {
            let (None, None, Some(from)) = (&binding.type_name, &binding.init_call, &binding.alias)
            else {
                break;
            };
            qualifier[0] = Name::new(from.as_str());
            match self.read.binding_at(from, at) {
                Some(next) => binding = next,
                None => return (Receiver::None, qualifier),
            }
        }
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

    fn locals(&mut self) {
        let mut order: Vec<usize> = (0..self.read.bindings.len()).collect();
        order.sort_by_key(|i| self.read.bindings[*i].start);
        let starts: Vec<usize> = order.iter().map(|i| self.read.bindings[*i].start).collect();
        let owners = self.owners(&starts);
        for (i, owner) in order.into_iter().zip(owners) {
            if self.tick() {
                return;
            }
            let b = &self.read.bindings[i];
            let end = b.scope.end.max(b.start + 1);
            self.facts.push(Fact::LocalBinding(Local {
                name: Name::new(b.name.as_str()),
                site: self.span(b.start..end),
                enclosing: self.scope_of(owner),
                kind: if b.start == b.scope.start {
                    LocalKind::Parameter
                } else {
                    LocalKind::Variable
                },
            }));
        }
    }
}

fn visibility_of(d: &old::Declaration) -> Visibility {
    match d.effective.or(d.visibility) {
        Some(old::Visibility::Private) => Visibility::Private,
        Some(old::Visibility::Protected) => Visibility::Protected,
        Some(old::Visibility::Internal) => Visibility::Crate,
        Some(old::Visibility::Public) | None => Visibility::Public,
    }
}

fn has_word(text: &str, word: &str) -> bool {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .any(|w| w == word)
}
