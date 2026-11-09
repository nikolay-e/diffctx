//! What a PHP file's syntax states, as the facts contract names it.
//!
//! Conventions a reader of the facts cannot recover from the types:
//! - A namespace is an [`Import`] of kind `SideEffect` whose module is the name
//!   with `\` separators; its `site` is the range it is in effect for (a braced
//!   namespace: the block; `namespace A;`: up to the next namespace or the end of
//!   the file). Code before any namespace is in the global one and has no import.
//! - `use A\B\C;` is module `A\B`, names `[(C, None)]`; `use A\B\C as D;` aliases
//!   the name; a grouped `use A\{B, C as D};` is one import per clause. Whether
//!   `A\B` is a class or a namespace is for the resolver. `use function` and
//!   `use const` are `FunctionOnly` and `ConstOnly` imports. A literal `require`, `include` (`__DIR__ . 'f.php'` included) is a
//!   `Require` import whose module is the path written, `relative` set when it
//!   starts with `__DIR__`.
//! - Definitions are keyed by name in their container (the class, interface,
//!   trait or enum around, an anonymous class being `new B` for what it extends
//!   or implements); the namespace is no part of a key. A property is named
//!   without its `$`, and so is a constructor-promoted parameter, which is both
//!   a `Property` of the class and a `Parameter` local. Methods and properties
//!   sharing a name take `Overload(n)` in source order. `__construct` is a
//!   `Constructor`. An enum's cases are `EnumMember`s, `const` members are
//!   `Constant`s.
//! - A definition's `signature` is its header, whitespace collapsed: modifiers,
//!   parameters with their types and defaults, return type, and for a type
//!   `extends` and `implements`; the traits a class uses follow as `use T1, T2`
//!   (`use T {}` when the use has an adaptation block: `insteadof`, `as`). A
//!   property's is its modifiers, type and `$name`. `decorators` are the
//!   attributes, as written without a leading `\`.
//! - A reference names the last segment and carries the written namespace path
//!   before it in `qualifier` (`\A\f()` is `f` with `["", A]`; `A\B::m()` is `m`
//!   with `[A, B]`; `self::m()` is `m` with `[self]`; `parent::__construct()`
//!   has `[parent]`). A function called through a namespace (`A\f()`) has the
//!   receiver `Module(A)`, which tells it from the static call `A::f()`. `new T`
//!   is an `Instantiate` of `T`; `new self`, `new static` name `self` and `static`. A property read is `Read`, an assigned
//!   one (`$this->p = ..`, `$this->p[] = ..`, `++`) `Write`.
//! - The receiver of a member access describes the head of the chain: `SelfType`
//!   for `$this`; `Typed(T)` for a parameter or a catch variable whose written
//!   type is `T` (`T` as written, `?T` and `T|null` read as `T`; any other union
//!   or intersection is no type); `New(T)` for a variable every assignment of
//!   which is `new T`; `CallResult(path)` for one every assignment of which is
//!   the same call (`f()` is `[f]`, `A::m()` is `[A, m]`, `$this->m()` is
//!   `[self, m]`, `$typed->m()` is `[T, m]`, `$this->p->m()` is `[self, ->, p,
//!   m]`) and for a call written in the chain itself; `Unknown` for any other expression. The properties written after
//!   the head go to the `qualifier`, an `Unknown` chain has none.
//! - A name that is not written (`$o->$m()`, `new $c`, `$f()`, `$c::m()`) is the
//!   variable's text, `$m`, with the receiver of the part written: the resolver
//!   knows a dynamic name by its `$`.
//! - A method of a type that states supertypes has an `Override` reference to its
//!   own name, owned by itself; the resolver keeps it where a supertype has the
//!   member.
//! - `#[Test]` methods and `test*` methods of a `*Test` class are `Test` entry
//!   points.
//! - Hashes are FNV-1a over the definition's own parts: members that are
//!   definitions themselves are left out of their container's.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use tree_sitter::{Language as TsLanguage, Node, ParseOptions, ParseState, Parser, Tree};

use crate::facts::{
    BlobFacts, Def, DefKind, Disambig, EntryKind, EntryPoint, Extractor, Fact, Hasher, Import,
    ImportKind, Language, Local, LocalKey, LocalKind, ModulePath, Name, Namespace, Receiver, Ref,
    Scope, Span, Star, TypeName, UseKind, Visibility, expired, hash_text,
};
use crate::snapshot::{Loss, RepoPath, RunBudget};

const MAX_DEPTH: u16 = 256;
const ERROR_RANGES_KEPT: usize = 32;
const POLL_EVERY: usize = 256;

pub struct PhpExtractor;

impl Extractor for PhpExtractor {
    fn language(&self) -> Language {
        Language::Php
    }

    fn extract(&self, text: &[u8], budget: &RunBudget) -> BlobFacts {
        extract(text, budget)
    }
}

fn unbound() -> RepoPath {
    RepoPath::new("<blob>").expect("a valid path")
}

#[cfg(feature = "tree-sitter-php")]
fn grammar() -> Option<TsLanguage> {
    Some(TsLanguage::new(tree_sitter_php::LANGUAGE_PHP))
}

#[cfg(not(feature = "tree-sitter-php"))]
fn grammar() -> Option<TsLanguage> {
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
    let mut blob = BlobFacts::new(Language::Php);
    let Some(tree) = parse(text, budget) else {
        if expired(budget) {
            blob.deadline();
        } else {
            blob.losses.push(Loss::Unsupported { path: unbound() });
        }
        return blob;
    };
    let root = tree.root_node();
    if root.has_error() {
        blob.parse_error(error_lines(root));
    }
    let mut walker = Walker::new(text, budget);
    walker.run(root);
    if walker.halted {
        blob.deadline();
    }
    if walker.too_deep {
        blob.losses.push(Loss::ExtractorFailed { path: unbound() });
    }
    blob.facts = std::mem::take(&mut walker.facts);
    blob.in_source_order();
    blob
}

/// The line ranges the parse could not read, merged.
fn error_lines(root: Node) -> Vec<(u32, u32)> {
    let mut ranges = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let mut cursor = node.walk();
        if node.is_error() || node.is_missing() {
            ranges.push(lines_of(node));
        } else if node.has_error() {
            stack.extend(node.children(&mut cursor));
        }
    }
    ranges.sort_unstable();
    let mut merged: Vec<(u32, u32)> = Vec::new();
    for (from, to) in ranges {
        match merged.last_mut() {
            Some(last) if from <= last.1 + 1 => last.1 = last.1.max(to),
            _ => merged.push((from, to)),
        }
    }
    merged.truncate(ERROR_RANGES_KEPT);
    merged
}

fn lines_of(node: Node) -> (u32, u32) {
    let start = node.start_position();
    let end = node.end_position();
    let first = start.row as u32 + 1;
    let mut last = end.row as u32 + 1;
    if end.column == 0 && end.row > start.row {
        last -= 1;
    }
    (first, last.max(first))
}

fn span(node: Node) -> Span {
    Span {
        bytes: node.start_byte() as u32..node.end_byte() as u32,
        lines: lines_of(node),
    }
}

/// The type a variable is known by, from what its assignments and declaration
/// state.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Ty {
    Typed(String),
    New(String),
    Call(Vec<String>),
}

type Locals = HashMap<String, Option<Ty>>;

#[derive(Clone)]
struct Cx {
    container: Vec<String>,
    scope: Scope,
    /// Inside a type's body: the type's name.
    class: Option<String>,
    /// Whether the type around states supertypes.
    has_supers: bool,
    locals: Rc<Locals>,
}

impl Cx {
    fn root() -> Self {
        Self {
            container: Vec::new(),
            scope: Scope::Module,
            class: None,
            has_supers: false,
            locals: Rc::new(HashMap::new()),
        }
    }
}

/// What the written chain of an access starts from.
struct Head {
    receiver: Receiver,
    qualifier: Vec<Name>,
}

impl Head {
    fn unknown() -> Self {
        Self {
            receiver: Receiver::Unknown,
            qualifier: Vec::new(),
        }
    }
}

fn is_function_like(kind: &str) -> bool {
    matches!(
        kind,
        "function_definition" | "method_declaration" | "anonymous_function" | "arrow_function"
    )
}

fn is_type_declaration(kind: &str) -> bool {
    matches!(
        kind,
        "class_declaration"
            | "interface_declaration"
            | "trait_declaration"
            | "enum_declaration"
            | "anonymous_class"
    )
}

/// Nodes a signature reads as one token: a name written with backslashes, a
/// variable, a string.
fn is_atom(kind: &str) -> bool {
    matches!(
        kind,
        "qualified_name"
            | "relative_name"
            | "namespace_name"
            | "variable_name"
            | "dynamic_variable_name"
            | "string"
            | "encapsed_string"
            | "heredoc"
            | "nowdoc"
    )
}

fn is_name(kind: &str) -> bool {
    matches!(kind, "name" | "qualified_name" | "relative_name")
}

fn split_path(text: &str) -> Vec<String> {
    text.split('\\').map(str::to_string).collect()
}

struct Walker<'t> {
    bytes: &'t [u8],
    budget: &'t RunBudget,
    ticks: usize,
    halted: bool,
    too_deep: bool,
    facts: Vec<Fact>,
    twins: HashMap<LocalKey, u16>,
}

impl<'t> Walker<'t> {
    fn new(bytes: &'t [u8], budget: &'t RunBudget) -> Self {
        Self {
            bytes,
            budget,
            ticks: 0,
            halted: false,
            too_deep: false,
            facts: Vec::new(),
            twins: HashMap::new(),
        }
    }

    fn run(&mut self, root: Node<'t>) {
        self.statements(root, &Cx::root(), 0);
    }

    fn tick(&mut self) -> bool {
        if self.ticks % POLL_EVERY == 0 && expired(self.budget) {
            self.halted = true;
        }
        self.ticks += 1;
        !self.halted
    }

    fn deeper(&mut self, depth: u16) -> bool {
        if depth > MAX_DEPTH {
            self.too_deep = true;
            return false;
        }
        self.tick()
    }

    fn text(&self, node: Node<'_>) -> &'t str {
        let bytes: &'t [u8] = self.bytes;
        std::str::from_utf8(&bytes[node.start_byte()..node.end_byte()]).unwrap_or("")
    }

    fn named_children(node: Node<'t>) -> Vec<Node<'t>> {
        let mut cursor = node.walk();
        node.named_children(&mut cursor).collect()
    }

    // ---- statements and declarations

    /// The children of a container (file, block, class body), each with the doc
    /// comment before it. A flat `namespace A;` holds the siblings after it.
    fn statements(&mut self, parent: Node<'t>, cx: &Cx, depth: u16) {
        if !self.deeper(depth) {
            return;
        }
        let children = Self::named_children(parent);
        let mut doc: Option<&'t str> = None;
        for (at, child) in children.iter().enumerate() {
            if child.kind() == "comment" {
                let text = self.text(*child);
                doc = text.starts_with("/**").then_some(text);
                continue;
            }
            if child.kind() == "namespace_definition" && child.child_by_field_name("body").is_none()
            {
                let end = children[at + 1..]
                    .iter()
                    .find(|c| {
                        c.kind() == "namespace_definition"
                            && c.child_by_field_name("body").is_none()
                    })
                    .map_or(parent.end_byte(), |c| c.start_byte());
                self.namespace(*child, Some(end), cx);
                doc = None;
                continue;
            }
            self.visit(*child, cx, doc, depth + 1);
            doc = None;
        }
    }

    fn visit(&mut self, node: Node<'t>, cx: &Cx, doc: Option<&'t str>, depth: u16) {
        if !self.deeper(depth) {
            return;
        }
        match node.kind() {
            "namespace_definition" => self.namespace(node, None, cx),
            "namespace_use_declaration" => self.use_declaration(node, cx),
            "function_definition" => self.function(node, cx, doc, depth),
            "class_declaration"
            | "interface_declaration"
            | "trait_declaration"
            | "enum_declaration" => self.type_declaration(node, cx, doc, depth),
            "const_declaration" => self.constants(node, cx, doc, depth),
            "php_tag" | "text" | "text_interpolation" | "comment" | "empty_statement" => {}
            _ => self.expr(node, cx, depth),
        }
    }

    fn namespace(&mut self, node: Node<'t>, until: Option<usize>, cx: &Cx) {
        let name = node
            .child_by_field_name("name")
            .map_or_else(String::new, |n| {
                self.text(n).trim_start_matches('\\').to_string()
            });
        let mut site = span(node);
        if let Some(end) = until {
            site.bytes.end = end as u32;
        }
        if !name.is_empty() {
            self.facts.push(Fact::Import(Import {
                site,
                enclosing: Scope::Module,
                module: ModulePath::new(name.as_str()),
                relative: 0,
                kind: ImportKind::SideEffect,
                names: Vec::new(),
                star: Star::None,
                default: None,
            }));
        }
        if let Some(body) = node.child_by_field_name("body") {
            self.statements(body, cx, 1);
        }
    }

    /// The namespace a `use function` or `use const` imports into; none for a
    /// class.
    fn import_kind(&self, node: Node<'t>) -> Option<ImportKind> {
        node.child_by_field_name("type")
            .map(|t| match self.text(t) {
                "function" => ImportKind::FunctionOnly,
                _ => ImportKind::ConstOnly,
            })
    }

    fn use_declaration(&mut self, node: Node<'t>, cx: &Cx) {
        let outer = self.import_kind(node);
        let children = Self::named_children(node);
        let prefix = children
            .iter()
            .find(|c| c.kind() == "namespace_name")
            .map(|c| self.text(*c).trim_start_matches('\\').to_string());
        let mut clauses: Vec<(Node<'t>, Option<String>)> = Vec::new();
        for child in &children {
            match child.kind() {
                "namespace_use_clause" => clauses.push((*child, prefix.clone())),
                "namespace_use_group" => {
                    for clause in Self::named_children(*child) {
                        if clause.kind() == "namespace_use_clause" {
                            clauses.push((clause, prefix.clone()));
                        }
                    }
                }
                _ => {}
            }
        }
        for (clause, prefix) in clauses {
            let Some(written) = Self::named_children(clause)
                .into_iter()
                .find(|c| is_name(c.kind()))
            else {
                continue;
            };
            let mut segments = split_path(self.text(written).trim_start_matches('\\'));
            if let Some(prefix) = prefix {
                let mut all = split_path(&prefix);
                all.extend(segments);
                segments = all;
            }
            let Some(last) = segments.pop() else { continue };
            let alias = clause
                .child_by_field_name("alias")
                .map(|a| Name::new(self.text(a)));
            let kind = self
                .import_kind(clause)
                .or(outer)
                .unwrap_or(ImportKind::Static);
            self.facts.push(Fact::Import(Import {
                site: span(clause),
                enclosing: cx.scope.clone(),
                module: ModulePath::new(segments.join("\\").as_str()),
                relative: 0,
                kind,
                names: vec![(Name::new(last.as_str()), alias)],
                star: Star::None,
                default: None,
            }));
        }
    }

    fn visibility(&self, node: Node<'t>) -> Visibility {
        Self::named_children(node)
            .into_iter()
            .find(|c| c.kind() == "visibility_modifier")
            .map_or(Visibility::Public, |v| match self.text(v) {
                "private" => Visibility::Private,
                "protected" => Visibility::Protected,
                _ => Visibility::Public,
            })
    }

    fn key_for(&mut self, cx: &Cx, name: &str, ns: Namespace) -> LocalKey {
        let key = LocalKey::new(
            cx.container.iter().map(|c| Name::new(c.as_str())).collect(),
            name,
            ns,
        );
        let seen = self.twins.entry(key.clone()).or_insert(0);
        let nth = *seen;
        *seen += 1;
        if nth == 0 {
            key
        } else {
            key.with_disambig(Disambig::Overload(nth))
        }
    }

    fn inside(&self, cx: &Cx, key: &LocalKey) -> Cx {
        let mut container = cx.container.clone();
        container.push(key.name.as_str().to_string());
        Cx {
            container,
            scope: Scope::Def(key.clone()),
            ..cx.clone()
        }
    }

    /// The attributes written on a declaration, as names.
    fn attributes(&self, node: Node<'t>) -> Vec<Node<'t>> {
        let mut found = Vec::new();
        for child in Self::named_children(node) {
            if child.kind() == "attribute_list" {
                for group in Self::named_children(child) {
                    for attribute in Self::named_children(group) {
                        if attribute.kind() == "attribute" {
                            found.push(attribute);
                        }
                    }
                }
            }
        }
        found
    }

    fn attribute_name(&self, attribute: Node<'t>) -> Option<Node<'t>> {
        Self::named_children(attribute)
            .into_iter()
            .find(|c| is_name(c.kind()))
    }

    fn doc_hash(&self, doc: Option<&'t str>) -> Option<u64> {
        let doc = doc?;
        let mut hasher = Hasher::new();
        for word in doc.split_whitespace() {
            hasher.token(word.trim_matches(|c| c == '/' || c == '*').as_bytes());
        }
        hasher.finish()
    }

    /// Emits a definition and returns its key. `body` is what its body hash is of;
    /// `members` are the nodes inside it that are definitions themselves.
    #[allow(clippy::too_many_arguments)]
    fn define(
        &mut self,
        node: Node<'t>,
        name_node: Node<'t>,
        name: &str,
        cx: &Cx,
        kind: DefKind,
        ns: Namespace,
        visibility: Visibility,
        signature: String,
        body: Option<Node<'t>>,
        members: &[Node<'t>],
        doc: Option<&'t str>,
    ) -> LocalKey {
        let key = self.key_for(cx, name, ns);
        let decorators = self
            .attributes(node)
            .into_iter()
            .filter_map(|a| self.attribute_name(a))
            .map(|n| Name::new(self.text(n).trim_start_matches('\\')))
            .collect();
        self.facts.push(Fact::Definition(Def {
            local: key.clone(),
            kind,
            span: span(node),
            name_span: span(name_node),
            signature: Some(signature),
            decorators,
            visibility,
            body_hash: body.map_or(0, |b| self.hash_tree(b, members, None)),
            text_hash: self.text_hash(node, members),
            doc_hash: self.doc_hash(doc),
        }));
        key
    }

    /// The declaration's tokens without its body, comments and attributes.
    fn signature(&self, node: Node<'t>, skip: &[Node<'t>]) -> String {
        let mut tokens: Vec<&'t str> = Vec::new();
        let mut stack = vec![node];
        while let Some(n) = stack.pop() {
            if n.kind() == "comment"
                || n.kind() == "attribute_list"
                || skip.iter().any(|s| s.id() == n.id())
            {
                continue;
            }
            if n.child_count() == 0 || is_atom(n.kind()) {
                let token = self.text(n);
                if !token.is_empty() {
                    tokens.push(token);
                }
            } else {
                let mut cursor = n.walk();
                let kids: Vec<Node<'t>> = n.children(&mut cursor).collect();
                stack.extend(kids.into_iter().rev());
            }
        }
        while tokens.last() == Some(&";") {
            tokens.pop();
        }
        join_tokens(&tokens)
    }

    fn hash_tree(&self, root: Node<'t>, members: &[Node<'t>], skip: Option<Node<'t>>) -> u64 {
        let mut hash = Hasher::new();
        let mut stack = vec![(root, false)];
        while let Some((node, closing)) = stack.pop() {
            if closing {
                hash.bytes(&[2]);
                continue;
            }
            let held = node.id() != root.id()
                && (members.iter().any(|m| m.id() == node.id())
                    || skip.is_some_and(|s| s.id() == node.id()));
            if node.kind() == "comment" || held {
                continue;
            }
            if node.child_count() == 0 {
                hash.bytes(&[3]).token(node.kind().as_bytes());
                hash.token(self.text(node).as_bytes());
            } else {
                hash.bytes(&[1]).token(node.kind().as_bytes());
                stack.push((node, true));
                let mut cursor = node.walk();
                let kids: Vec<Node<'t>> = node.children(&mut cursor).collect();
                stack.extend(kids.into_iter().rev().map(|k| (k, false)));
            }
        }
        hash.finish().unwrap_or(0)
    }

    fn text_hash(&self, node: Node<'t>, members: &[Node<'t>]) -> u64 {
        let mut cuts: Vec<(usize, usize)> = members
            .iter()
            .map(|m| (m.start_byte(), m.end_byte()))
            .collect();
        cuts.sort_unstable();
        let mut pieces: Vec<&[u8]> = Vec::new();
        let mut at = node.start_byte();
        for (from, to) in cuts {
            if from >= at {
                pieces.push(self.bytes[at..from].trim_ascii());
                at = to;
            }
        }
        pieces.push(self.bytes[at..node.end_byte()].trim_ascii());
        hash_text(&pieces.join(&b'\n'))
    }

    // ---- types

    fn type_declaration(&mut self, node: Node<'t>, cx: &Cx, doc: Option<&'t str>, depth: u16) {
        let anonymous = node.kind() == "anonymous_class";
        let (kind, ns) = match node.kind() {
            "interface_declaration" => (DefKind::Interface, Namespace::Type),
            "trait_declaration" => (DefKind::Trait, Namespace::Type),
            "enum_declaration" => (DefKind::Enum, Namespace::Type),
            _ => (DefKind::Class, Namespace::Type),
        };
        let supers = self.supertypes(node);
        let body = node.child_by_field_name("body");
        let (name, name_node) = if anonymous {
            let base = supers.first().map_or("class", |s| s.2.as_str());
            (format!("new {base}"), node)
        } else {
            let Some(n) = node.child_by_field_name("name") else {
                return;
            };
            (self.text(n).to_string(), n)
        };
        let members: Vec<Node<'t>> = body.map_or_else(Vec::new, |b| {
            Self::named_children(b)
                .into_iter()
                .filter(|c| c.kind() != "comment")
                .collect()
        });
        let traits = self.trait_uses(&members);
        let mut signature = self.signature(node, &body.into_iter().collect::<Vec<_>>());
        if !traits.is_empty() {
            let adapted = traits.iter().any(|t| t.1);
            let names: Vec<&str> = traits.iter().map(|t| t.0.as_str()).collect();
            signature.push_str(&format!(" use {}", names.join(", ")));
            if adapted {
                signature.push_str(" {}");
            }
        }
        let key = self.define(
            node,
            name_node,
            &name,
            cx,
            kind,
            ns,
            Visibility::Public,
            signature,
            Some(node),
            &members,
            doc,
        );
        let mut inner = self.inside(cx, &key);
        inner.class = Some(name);
        inner.has_supers = !supers.is_empty() || !traits.is_empty();
        for attribute in self.attributes(node) {
            self.attribute(attribute, &inner, depth);
        }
        // The header's names are the type's own uses.
        for (_, node, text) in &supers {
            self.type_use_name(*node, &inner, text);
        }
        for member in Self::named_children(node)
            .into_iter()
            .filter(|c| c.kind() == "arguments")
        {
            self.arguments(member, &inner, depth + 1);
        }
        if let Some(body) = body {
            self.members(body, &inner, depth + 1);
        }
    }

    /// The names an `extends` and an `implements` clause list, with their nodes.
    fn supertypes(&self, node: Node<'t>) -> Vec<(bool, Node<'t>, String)> {
        let mut found = Vec::new();
        for clause in Self::named_children(node) {
            if matches!(clause.kind(), "base_clause" | "class_interface_clause") {
                for name in Self::named_children(clause) {
                    if is_name(name.kind()) {
                        found.push((
                            clause.kind() == "base_clause",
                            name,
                            self.text(name).to_string(),
                        ));
                    }
                }
            }
        }
        found
    }

    /// The traits a body uses, each with whether an adaptation block follows.
    fn trait_uses(&self, members: &[Node<'t>]) -> Vec<(String, bool)> {
        let mut found = Vec::new();
        for member in members.iter().filter(|m| m.kind() == "use_declaration") {
            let children = Self::named_children(*member);
            let adapted = children.iter().any(|c| c.kind() == "use_list");
            for name in children.iter().filter(|c| is_name(c.kind())) {
                found.push((self.text(*name).to_string(), adapted));
            }
        }
        found
    }

    fn members(&mut self, body: Node<'t>, cx: &Cx, depth: u16) {
        let children = Self::named_children(body);
        let mut doc: Option<&'t str> = None;
        for child in children {
            if !self.deeper(depth) {
                return;
            }
            match child.kind() {
                "comment" => {
                    let text = self.text(child);
                    doc = text.starts_with("/**").then_some(text);
                    continue;
                }
                "method_declaration" => self.function(child, cx, doc, depth),
                "property_declaration" => self.properties(child, cx, doc, depth),
                "const_declaration" => self.constants(child, cx, doc, depth),
                "enum_case" => self.enum_case(child, cx, doc, depth),
                "use_declaration" => self.trait_use(child, cx),
                _ => self.expr(child, cx, depth + 1),
            }
            doc = None;
        }
    }

    fn trait_use(&mut self, node: Node<'t>, cx: &Cx) {
        for name in Self::named_children(node) {
            if is_name(name.kind()) {
                let text = self.text(name);
                self.type_use_name(name, cx, text);
            }
        }
    }

    // ---- members

    fn enum_case(&mut self, node: Node<'t>, cx: &Cx, doc: Option<&'t str>, depth: u16) {
        let Some(name_node) = node.child_by_field_name("name") else {
            return;
        };
        let name = self.text(name_node).to_string();
        let value = node.child_by_field_name("value");
        let signature = self.signature(node, &[]);
        let key = self.define(
            node,
            name_node,
            &name,
            cx,
            DefKind::EnumMember,
            Namespace::Value,
            Visibility::Public,
            signature,
            value,
            &[],
            doc,
        );
        if let Some(value) = value {
            let inner = self.inside(cx, &key);
            self.expr(value, &inner, depth + 1);
        }
    }

    fn constants(&mut self, node: Node<'t>, cx: &Cx, doc: Option<&'t str>, depth: u16) {
        let visibility = self.visibility(node);
        let elements: Vec<Node<'t>> = Self::named_children(node)
            .into_iter()
            .filter(|c| c.kind() == "const_element")
            .collect();
        let header = self.signature(
            node,
            &elements
                .iter()
                .copied()
                .chain(self.attribute_nodes(node))
                .collect::<Vec<_>>(),
        );
        for element in &elements {
            let children = Self::named_children(*element);
            let Some(name_node) = children.iter().find(|c| c.kind() == "name").copied() else {
                continue;
            };
            let value = children
                .iter()
                .rev()
                .find(|c| c.id() != name_node.id())
                .copied();
            let name = self.text(name_node).to_string();
            let signature = format!("{header} {name}");
            let key = self.define_element(
                *element,
                node,
                name_node,
                &name,
                cx,
                DefKind::Constant,
                visibility,
                signature,
                value,
                doc,
            );
            if let Some(value) = value {
                let inner = self.inside(cx, &key);
                self.expr(value, &inner, depth + 1);
            }
        }
    }

    fn attribute_nodes(&self, node: Node<'t>) -> Vec<Node<'t>> {
        Self::named_children(node)
            .into_iter()
            .filter(|c| c.kind() == "attribute_list")
            .collect()
    }

    /// A definition that is one element of a declaration listing several (`const
    /// A = 1, B = 2;`, `public $a, $b;`): its span and text are the element's, its
    /// attributes the declaration's.
    #[allow(clippy::too_many_arguments)]
    fn define_element(
        &mut self,
        element: Node<'t>,
        declaration: Node<'t>,
        name_node: Node<'t>,
        name: &str,
        cx: &Cx,
        kind: DefKind,
        visibility: Visibility,
        signature: String,
        value: Option<Node<'t>>,
        doc: Option<&'t str>,
    ) -> LocalKey {
        let key = self.key_for(cx, name, Namespace::Value);
        let decorators = self
            .attributes(declaration)
            .into_iter()
            .filter_map(|a| self.attribute_name(a))
            .map(|n| Name::new(self.text(n).trim_start_matches('\\')))
            .collect();
        let mut hasher = Hasher::new();
        hasher.token(signature.as_bytes());
        hasher.token(self.text(element).as_bytes());
        self.facts.push(Fact::Definition(Def {
            local: key.clone(),
            kind,
            span: span(element),
            name_span: span(name_node),
            signature: Some(signature),
            decorators,
            visibility,
            body_hash: value.map_or(0, |v| self.hash_tree(v, &[], None)),
            text_hash: hasher.finish().unwrap_or(0),
            doc_hash: self.doc_hash(doc),
        }));
        key
    }

    fn properties(&mut self, node: Node<'t>, cx: &Cx, doc: Option<&'t str>, depth: u16) {
        let visibility = self.visibility(node);
        let elements: Vec<Node<'t>> = Self::named_children(node)
            .into_iter()
            .filter(|c| c.kind() == "property_element")
            .collect();
        let header = self.signature(
            node,
            &elements
                .iter()
                .copied()
                .chain(self.attribute_nodes(node))
                .collect::<Vec<_>>(),
        );
        for element in &elements {
            let Some(name_node) = element.child_by_field_name("name") else {
                continue;
            };
            let name = self.text(name_node).trim_start_matches('$').to_string();
            let signature = format!("{header} ${name}");
            let value = element.child_by_field_name("default_value");
            let key = self.define_element(
                *element,
                node,
                name_node,
                &name,
                cx,
                DefKind::Property,
                visibility,
                signature,
                value,
                doc,
            );
            if let Some(ty) = node.child_by_field_name("type") {
                let inner = self.inside(cx, &key);
                self.type_uses(ty, &inner);
            }
            if let Some(value) = value {
                let inner = self.inside(cx, &key);
                self.expr(value, &inner, depth + 1);
            }
        }
    }

    // ---- functions

    fn function(&mut self, node: Node<'t>, cx: &Cx, doc: Option<&'t str>, depth: u16) {
        let Some(name_node) = node.child_by_field_name("name") else {
            return;
        };
        let name = self.text(name_node).to_string();
        let in_class = cx.class.is_some();
        let kind = match (in_class, name.eq_ignore_ascii_case("__construct")) {
            (true, true) => DefKind::Constructor,
            (true, false) => DefKind::Method,
            (false, _) => DefKind::Function,
        };
        let body = node.child_by_field_name("body");
        let signature = self.signature(node, &body.into_iter().collect::<Vec<_>>());
        let visibility = if node.kind() == "method_declaration" {
            self.visibility(node)
        } else {
            Visibility::Public
        };
        let key = self.define(
            node,
            name_node,
            &name,
            cx,
            kind,
            Namespace::Value,
            visibility,
            signature,
            body,
            &[],
            doc,
        );
        let mut inner = self.inside(cx, &key);
        let parameters = node.child_by_field_name("parameters");
        self.parameters(parameters, &mut inner, doc);
        inner.locals = Rc::new(self.scan_locals(body, parameters, &inner));
        if in_class && kind == DefKind::Method && cx.has_supers {
            self.facts.push(Fact::Reference(Ref {
                site: span(name_node),
                enclosing: Scope::Def(key.clone()),
                receiver: Receiver::None,
                use_kind: UseKind::Override,
                name: Name::new(name.as_str()),
                qualifier: Vec::new(),
            }));
        }
        if in_class && self.is_test(node, &name, cx) {
            self.facts.push(Fact::EntryPoint(EntryPoint {
                site: span(node),
                kind: EntryKind::Test,
                target: Scope::Def(key.clone()),
                framework: None,
            }));
        }
        for attribute in self.attributes(node) {
            self.attribute(attribute, &inner, depth);
        }
        if let Some(ret) = node.child_by_field_name("return_type") {
            self.type_uses(ret, &inner);
        }
        if let Some(parameters) = parameters {
            for parameter in Self::named_children(parameters) {
                if let Some(default) = parameter.child_by_field_name("default_value") {
                    self.expr(default, &inner, depth + 1);
                }
            }
        }
        if let Some(body) = body {
            self.statements(body, &inner, depth + 1);
        }
    }

    fn is_test(&self, node: Node<'t>, name: &str, cx: &Cx) -> bool {
        let attributed = self.attributes(node).into_iter().any(|a| {
            self.attribute_name(a)
                .is_some_and(|n| self.text(n).trim_start_matches('\\') == "Test")
        });
        let named = name.len() > 4
            && name[..4].eq_ignore_ascii_case("test")
            && cx
                .class
                .as_deref()
                .is_some_and(|c| c.ends_with("Test") || c.ends_with("TestCase"));
        attributed || named
    }

    /// The parameters of a function: each is a local, with its type a use; a
    /// promoted one is also a property of the class around.
    fn parameters(&mut self, parameters: Option<Node<'t>>, cx: &mut Cx, doc: Option<&'t str>) {
        let Some(parameters) = parameters else {
            return;
        };
        let class_cx = Cx {
            container: cx.container[..cx.container.len().saturating_sub(1)].to_vec(),
            ..cx.clone()
        };
        for parameter in Self::named_children(parameters) {
            let Some(name_node) = parameter.child_by_field_name("name") else {
                continue;
            };
            let name_node = Self::variable_of(name_node).unwrap_or(name_node);
            let variable = self.text(name_node).to_string();
            self.facts.push(Fact::LocalBinding(Local {
                name: Name::new(variable.as_str()),
                site: span(name_node),
                enclosing: cx.scope.clone(),
                kind: LocalKind::Parameter,
            }));
            if let Some(ty) = parameter.child_by_field_name("type") {
                self.type_uses(ty, cx);
            }
            for attribute in self.attributes(parameter) {
                self.attribute(attribute, cx, 0);
            }
            if parameter.kind() == "property_promotion_parameter" {
                let name = variable.trim_start_matches('$').to_string();
                let visibility =
                    parameter
                        .child_by_field_name("visibility")
                        .map_or(Visibility::Public, |v| match self.text(v) {
                            "private" => Visibility::Private,
                            "protected" => Visibility::Protected,
                            _ => Visibility::Public,
                        });
                let signature = format!(
                    "{} ${name}",
                    self.signature(
                        parameter,
                        &[name_node]
                            .into_iter()
                            .chain(parameter.child_by_field_name("default_value"))
                            .chain(self.attribute_nodes(parameter))
                            .collect::<Vec<_>>()
                    )
                );
                let key = self.key_for(&class_cx, &name, Namespace::Value);
                self.facts.push(Fact::Definition(Def {
                    local: key,
                    kind: DefKind::Property,
                    span: span(parameter),
                    name_span: span(name_node),
                    signature: Some(signature),
                    decorators: Vec::new(),
                    visibility,
                    body_hash: 0,
                    text_hash: hash_text(self.text(parameter).as_bytes()),
                    doc_hash: self.doc_hash(doc),
                }));
            }
        }
    }

    fn variable_of(node: Node<'t>) -> Option<Node<'t>> {
        match node.kind() {
            "variable_name" => Some(node),
            _ => Self::named_children(node)
                .into_iter()
                .find(|c| c.kind() == "variable_name"),
        }
    }

    /// The type of each variable of a function body: what its parameter states
    /// and every assignment agrees on; a variable assigned in two ways is untyped.
    fn scan_locals(&self, body: Option<Node<'t>>, parameters: Option<Node<'t>>, cx: &Cx) -> Locals {
        let mut seen: HashMap<String, Vec<Option<Ty>>> = HashMap::new();
        if let Some(parameters) = parameters {
            for parameter in Self::named_children(parameters) {
                let Some(name) = parameter.child_by_field_name("name") else {
                    continue;
                };
                let name = Self::variable_of(name).unwrap_or(name);
                let ty = parameter
                    .child_by_field_name("type")
                    .and_then(|t| self.written_type(t))
                    .map(Ty::Typed);
                seen.entry(self.text(name).to_string())
                    .or_default()
                    .push(ty);
            }
        }
        let mut current = cx.clone();
        let mut stack: Vec<Node<'t>> = body.into_iter().collect();
        let mut order: Vec<Node<'t>> = Vec::new();
        while let Some(node) = stack.pop() {
            if is_function_like(node.kind()) || is_type_declaration(node.kind()) {
                continue;
            }
            order.push(node);
            let mut cursor = node.walk();
            let kids: Vec<Node<'t>> = node.named_children(&mut cursor).collect();
            stack.extend(kids.into_iter().rev());
        }
        for node in order {
            match node.kind() {
                "assignment_expression" | "reference_assignment_expression" => {
                    let (Some(left), Some(right)) = (
                        node.child_by_field_name("left"),
                        node.child_by_field_name("right"),
                    ) else {
                        continue;
                    };
                    if left.kind() != "variable_name" {
                        for variable in self.variables_in(left) {
                            seen.entry(variable).or_default().push(None);
                        }
                        continue;
                    }
                    let ty = if node.kind() == "assignment_expression" {
                        self.assigned_type(right, &current)
                    } else {
                        None
                    };
                    let name = self.text(left).to_string();
                    seen.entry(name.clone()).or_default().push(ty);
                    current = Cx {
                        locals: Rc::new(Self::agree(&seen)),
                        ..current
                    };
                }
                "foreach_statement"
                | "global_declaration"
                | "static_variable_declaration"
                | "anonymous_function_use_clause"
                | "augmented_assignment_expression" => {
                    for child in Self::named_children(node) {
                        for variable in self.variables_in(child) {
                            seen.entry(variable).or_default().push(None);
                        }
                    }
                }
                "catch_clause" => {
                    let ty = node
                        .child_by_field_name("type")
                        .and_then(|t| {
                            let names: Vec<_> = Self::named_children(t);
                            (names.len() == 1).then(|| self.text(names[0]).to_string())
                        })
                        .map(Ty::Typed);
                    if let Some(name) = node.child_by_field_name("name") {
                        seen.entry(self.text(name).to_string())
                            .or_default()
                            .push(ty);
                    }
                }
                _ => {}
            }
        }
        Self::agree(&seen)
    }

    fn variables_in(&self, node: Node<'t>) -> Vec<String> {
        let mut found = Vec::new();
        let mut stack = vec![node];
        while let Some(n) = stack.pop() {
            if n.kind() == "variable_name" {
                found.push(self.text(n).to_string());
                continue;
            }
            if matches!(
                n.kind(),
                "member_access_expression" | "subscript_expression"
            ) {
                continue;
            }
            stack.extend(Self::named_children(n));
        }
        found
    }

    fn agree(seen: &HashMap<String, Vec<Option<Ty>>>) -> Locals {
        seen.iter()
            .map(|(name, types)| {
                let first = types.first().cloned().flatten();
                let same = first.is_some() && types.iter().all(|t| *t == first);
                (name.clone(), if same { first } else { None })
            })
            .collect()
    }

    /// What an assigned expression states about the variable's type.
    fn assigned_type(&self, node: Node<'t>, cx: &Cx) -> Option<Ty> {
        match node.kind() {
            "parenthesized_expression" => Self::named_children(node)
                .first()
                .and_then(|n| self.assigned_type(*n, cx)),
            "object_creation_expression" => self.created(node).map(Ty::New),
            "function_call_expression" | "scoped_call_expression" | "member_call_expression" => {
                match self.head(node, cx).receiver {
                    Receiver::CallResult(path) => Some(Ty::Call(
                        path.iter().map(|n| n.as_str().to_string()).collect(),
                    )),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// The class `new` names: as written, `None` for a dynamic or anonymous one.
    fn created(&self, node: Node<'t>) -> Option<String> {
        let target = Self::named_children(node)
            .into_iter()
            .find(|c| c.kind() != "arguments")?;
        match target.kind() {
            "name" | "qualified_name" | "relative_name" => Some(self.text(target).to_string()),
            _ => None,
        }
    }

    // ---- types as uses

    /// The class a written type names; `None` for a builtin, a union of classes
    /// or an intersection.
    fn written_type(&self, node: Node<'t>) -> Option<String> {
        match node.kind() {
            "named_type" => {
                let name = Self::named_children(node)
                    .into_iter()
                    .find(|c| is_name(c.kind()))?;
                Some(self.text(name).to_string())
            }
            "optional_type" => Self::named_children(node)
                .into_iter()
                .find_map(|c| self.written_type(c)),
            "union_type" => {
                let classes: Vec<String> = Self::named_children(node)
                    .into_iter()
                    .filter(|c| !(c.kind() == "primitive_type" && self.text(*c) == "null"))
                    .filter_map(|c| {
                        if c.kind() == "primitive_type" {
                            Some(String::new())
                        } else {
                            self.written_type(c)
                        }
                    })
                    .collect();
                (classes.len() == 1 && !classes[0].is_empty()).then(|| classes[0].clone())
            }
            _ => None,
        }
    }

    fn type_uses(&mut self, node: Node<'t>, cx: &Cx) {
        let mut stack = vec![node];
        while let Some(n) = stack.pop() {
            if n.kind() == "named_type" {
                if let Some(name) = Self::named_children(n)
                    .into_iter()
                    .find(|c| is_name(c.kind()))
                {
                    let text = self.text(name);
                    self.type_use_name(name, cx, text);
                }
                continue;
            }
            stack.extend(Self::named_children(n));
        }
    }

    fn type_use_name(&mut self, node: Node<'t>, cx: &Cx, text: &str) {
        let mut segments = split_path(text);
        let Some(name) = segments.pop() else { return };
        if segments.is_empty() && matches!(name.as_str(), "self" | "static" | "parent") {
            return;
        }
        self.emit(
            node,
            cx,
            Receiver::None,
            UseKind::TypeUse,
            &name,
            segments.iter().map(|s| Name::new(s.as_str())).collect(),
        );
    }

    fn attribute(&mut self, attribute: Node<'t>, cx: &Cx, depth: u16) {
        if let Some(name) = self.attribute_name(attribute) {
            let mut segments = split_path(self.text(name));
            if let Some(last) = segments.pop() {
                self.emit(
                    name,
                    cx,
                    Receiver::None,
                    UseKind::Decorate,
                    &last,
                    segments.iter().map(|s| Name::new(s.as_str())).collect(),
                );
            }
        }
        for arguments in Self::named_children(attribute)
            .into_iter()
            .filter(|c| c.kind() == "arguments")
        {
            self.arguments(arguments, cx, depth + 1);
        }
    }

    fn emit(
        &mut self,
        node: Node<'t>,
        cx: &Cx,
        receiver: Receiver,
        use_kind: UseKind,
        name: &str,
        qualifier: Vec<Name>,
    ) {
        self.facts.push(Fact::Reference(Ref {
            site: span(node),
            enclosing: cx.scope.clone(),
            receiver,
            use_kind,
            name: Name::new(name),
            qualifier,
        }));
    }

    // ---- expressions

    fn expr(&mut self, node: Node<'t>, cx: &Cx, depth: u16) {
        if !self.deeper(depth) {
            return;
        }
        match node.kind() {
            "function_call_expression" => self.function_call(node, cx, depth),
            "member_call_expression" | "nullsafe_member_call_expression" => {
                self.member(node, cx, UseKind::Call, depth)
            }
            "member_access_expression" | "nullsafe_member_access_expression" => {
                self.member(node, cx, UseKind::Read, depth)
            }
            "scoped_call_expression" => self.scoped(node, cx, UseKind::Call, depth),
            "scoped_property_access_expression" => self.scoped(node, cx, UseKind::Read, depth),
            "class_constant_access_expression" => self.class_constant(node, cx, depth),
            "object_creation_expression" => self.creation(node, cx, depth),
            "assignment_expression" | "reference_assignment_expression" => {
                self.assignment(node, cx, depth)
            }
            "augmented_assignment_expression" | "update_expression" => {
                self.write_target(node, cx, depth)
            }
            "binary_expression" => self.binary(node, cx, depth),
            "catch_clause" => {
                if let Some(ty) = node.child_by_field_name("type") {
                    self.type_uses(ty, cx);
                }
                if let Some(body) = node.child_by_field_name("body") {
                    self.statements(body, cx, depth + 1);
                }
            }
            "anonymous_function" | "arrow_function" => self.closure(node, cx, depth),
            "anonymous_class" => self.type_declaration(node, cx, None, depth),
            "array_creation_expression" => self.array(node, cx, depth),
            "compound_statement" | "declaration_list" => self.statements(node, cx, depth),
            "function_definition"
            | "class_declaration"
            | "interface_declaration"
            | "trait_declaration"
            | "enum_declaration"
            | "const_declaration"
            | "namespace_use_declaration" => self.visit(node, cx, None, depth),
            "require_expression"
            | "require_once_expression"
            | "include_expression"
            | "include_once_expression" => self.require(node, cx, depth),
            "comment" | "string" | "integer" | "float" | "boolean" | "null" | "name" => {}
            _ => {
                for child in Self::named_children(node) {
                    self.expr(child, cx, depth + 1);
                }
            }
        }
    }

    fn arguments(&mut self, node: Node<'t>, cx: &Cx, depth: u16) {
        for child in Self::named_children(node) {
            self.expr(child, cx, depth + 1);
        }
    }

    fn is_first_class_callable(node: Node<'t>) -> bool {
        node.child_by_field_name("arguments").is_some_and(|a| {
            Self::named_children(a)
                .iter()
                .any(|c| c.kind() == "variadic_placeholder")
        })
    }

    fn use_for_call(node: Node<'t>) -> UseKind {
        if Self::is_first_class_callable(node) {
            UseKind::PassAsValue
        } else {
            UseKind::Call
        }
    }

    fn function_call(&mut self, node: Node<'t>, cx: &Cx, depth: u16) {
        let Some(function) = node.child_by_field_name("function") else {
            return;
        };
        let kind = Self::use_for_call(node);
        if is_name(function.kind()) {
            let mut segments = split_path(self.text(function));
            if let Some(name) = segments.pop() {
                let receiver = if segments.is_empty() {
                    Receiver::None
                } else {
                    Receiver::Module(ModulePath::new(segments.join("\\").as_str()))
                };
                self.emit(
                    function,
                    cx,
                    receiver,
                    kind,
                    &name,
                    segments.iter().map(|s| Name::new(s.as_str())).collect(),
                );
            }
        } else {
            // `$f()`, `($this->handler)()`: the name is not written.
            let text = self.text(function).to_string();
            self.emit(
                function,
                cx,
                Receiver::Unknown,
                kind,
                &dynamic(&text),
                Vec::new(),
            );
            self.expr(function, cx, depth + 1);
        }
        if let Some(arguments) = node.child_by_field_name("arguments") {
            self.arguments(arguments, cx, depth + 1);
        }
    }

    /// `$o->m()`, `$o->p`: a call or a read through the chain written before it.
    fn member(&mut self, node: Node<'t>, cx: &Cx, use_kind: UseKind, depth: u16) {
        let (Some(object), Some(name)) = (
            node.child_by_field_name("object"),
            node.child_by_field_name("name"),
        ) else {
            return;
        };
        let use_kind = if use_kind == UseKind::Call {
            Self::use_for_call(node)
        } else {
            use_kind
        };
        let head = self.head(object, cx);
        let (written, name_text) = if name.kind() == "name" {
            (true, self.text(name).to_string())
        } else {
            (false, dynamic(self.text(name)))
        };
        self.emit(
            name,
            cx,
            head.receiver,
            use_kind,
            &name_text,
            head.qualifier,
        );
        self.expr(object, cx, depth + 1);
        if !written {
            self.expr(name, cx, depth + 1);
        }
        if let Some(arguments) = node.child_by_field_name("arguments") {
            self.arguments(arguments, cx, depth + 1);
        }
    }

    /// `A::m()`, `A::$p`: the scope is written as a class name, `self`, `parent`
    /// or `static`, or is an expression nothing types.
    fn scoped(&mut self, node: Node<'t>, cx: &Cx, use_kind: UseKind, depth: u16) {
        let (Some(scope), Some(name)) = (
            node.child_by_field_name("scope"),
            node.child_by_field_name("name"),
        ) else {
            return;
        };
        let use_kind = if use_kind == UseKind::Call {
            Self::use_for_call(node)
        } else {
            use_kind
        };
        let name_text = match name.kind() {
            "name" => self.text(name).to_string(),
            "variable_name" if use_kind != UseKind::Call && use_kind != UseKind::PassAsValue => {
                self.text(name).trim_start_matches('$').to_string()
            }
            _ => dynamic(self.text(name)),
        };
        let (receiver, qualifier) = self.scope_of(scope);
        self.emit(name, cx, receiver, use_kind, &name_text, qualifier);
        if !is_name(scope.kind()) && scope.kind() != "relative_scope" {
            self.expr(scope, cx, depth + 1);
        }
        if let Some(arguments) = node.child_by_field_name("arguments") {
            self.arguments(arguments, cx, depth + 1);
        }
    }

    fn scope_of(&self, scope: Node<'t>) -> (Receiver, Vec<Name>) {
        match scope.kind() {
            "name" | "qualified_name" | "relative_name" | "relative_scope" => (
                Receiver::None,
                split_path(self.text(scope))
                    .iter()
                    .map(|s| Name::new(s.as_str()))
                    .collect(),
            ),
            _ => (Receiver::Unknown, Vec::new()),
        }
    }

    /// `A::B`, `A::class`, `Status::Active`.
    fn class_constant(&mut self, node: Node<'t>, cx: &Cx, depth: u16) {
        let children = Self::named_children(node);
        let (Some(scope), Some(name)) = (children.first().copied(), children.last().copied())
        else {
            return;
        };
        if children.len() < 2 {
            return;
        }
        let (receiver, qualifier) = self.scope_of(scope);
        let text = self.text(name);
        if text.eq_ignore_ascii_case("class") {
            if is_name(scope.kind()) {
                self.type_use_name(scope, cx, self.text(scope));
            } else {
                self.expr(scope, cx, depth + 1);
            }
            return;
        }
        if name.kind() == "name" {
            self.emit(name, cx, receiver, UseKind::Read, text, qualifier);
        } else {
            self.expr(name, cx, depth + 1);
        }
        if !is_name(scope.kind()) && scope.kind() != "relative_scope" {
            self.expr(scope, cx, depth + 1);
        }
    }

    fn creation(&mut self, node: Node<'t>, cx: &Cx, depth: u16) {
        let children = Self::named_children(node);
        for child in &children {
            match child.kind() {
                "arguments" => self.arguments(*child, cx, depth + 1),
                "anonymous_class" => self.type_declaration(*child, cx, None, depth + 1),
                kind if is_name(kind) || kind == "relative_scope" => {
                    let mut segments = split_path(self.text(*child));
                    if let Some(name) = segments.pop() {
                        self.emit(
                            *child,
                            cx,
                            Receiver::None,
                            UseKind::Instantiate,
                            &name,
                            segments.iter().map(|s| Name::new(s.as_str())).collect(),
                        );
                    }
                }
                _ => {
                    let text = self.text(*child).to_string();
                    self.emit(
                        *child,
                        cx,
                        Receiver::Unknown,
                        UseKind::Instantiate,
                        &dynamic(&text),
                        Vec::new(),
                    );
                    self.expr(*child, cx, depth + 1);
                }
            }
        }
    }

    fn assignment(&mut self, node: Node<'t>, cx: &Cx, depth: u16) {
        if let Some(left) = node.child_by_field_name("left") {
            self.target(left, cx, depth + 1);
        }
        if let Some(right) = node.child_by_field_name("right") {
            self.expr(right, cx, depth + 1);
        }
    }

    /// `++$this->n`, `$this->n .= 'x'`: the operand is written and read.
    fn write_target(&mut self, node: Node<'t>, cx: &Cx, depth: u16) {
        let operand = node
            .child_by_field_name("left")
            .or_else(|| Self::named_children(node).into_iter().next());
        for child in Self::named_children(node) {
            if Some(child.id()) == operand.map(|o| o.id()) && child.kind() != "variable_name" {
                self.target(child, cx, depth + 1);
            } else {
                self.expr(child, cx, depth + 1);
            }
        }
    }

    /// The left side of an assignment: a property is written, a variable is a
    /// local, an element of a list is each of those.
    fn target(&mut self, node: Node<'t>, cx: &Cx, depth: u16) {
        if !self.deeper(depth) {
            return;
        }
        match node.kind() {
            "variable_name" => {
                self.facts.push(Fact::LocalBinding(Local {
                    name: Name::new(self.text(node)),
                    site: span(node),
                    enclosing: cx.scope.clone(),
                    kind: LocalKind::Variable,
                }));
            }
            "member_access_expression" | "nullsafe_member_access_expression" => {
                self.member(node, cx, UseKind::Write, depth)
            }
            "scoped_property_access_expression" => self.scoped(node, cx, UseKind::Write, depth),
            "subscript_expression" => {
                let children = Self::named_children(node);
                if let Some(base) = children.first() {
                    self.target(*base, cx, depth + 1);
                }
                for index in children.iter().skip(1) {
                    self.expr(*index, cx, depth + 1);
                }
            }
            "list_literal" | "array_creation_expression" => {
                for child in Self::named_children(node) {
                    self.target(child, cx, depth + 1);
                }
            }
            "array_element_initializer" => {
                for child in Self::named_children(node) {
                    self.target(child, cx, depth + 1);
                }
            }
            _ => self.expr(node, cx, depth + 1),
        }
    }

    fn binary(&mut self, node: Node<'t>, cx: &Cx, depth: u16) {
        let operator = node
            .child_by_field_name("operator")
            .map(|o| self.text(o).to_ascii_lowercase());
        if operator.as_deref() == Some("instanceof") {
            if let Some(left) = node.child_by_field_name("left") {
                self.expr(left, cx, depth + 1);
            }
            if let Some(right) = node.child_by_field_name("right") {
                if is_name(right.kind()) {
                    self.type_use_name(right, cx, self.text(right));
                } else {
                    self.expr(right, cx, depth + 1);
                }
            }
            return;
        }
        for child in Self::named_children(node) {
            self.expr(child, cx, depth + 1);
        }
    }

    fn closure(&mut self, node: Node<'t>, cx: &Cx, depth: u16) {
        let parameters = node.child_by_field_name("parameters");
        let mut inner = cx.clone();
        self.parameters(parameters, &mut inner, None);
        let body = node.child_by_field_name("body");
        let mut known: Locals = (*cx.locals).clone();
        known.extend(self.scan_locals(body, parameters, &inner));
        inner.locals = Rc::new(known);
        if let Some(ret) = node.child_by_field_name("return_type") {
            self.type_uses(ret, &inner);
        }
        if let Some(body) = body {
            if body.kind() == "compound_statement" {
                self.statements(body, &inner, depth + 1);
            } else {
                self.expr(body, &inner, depth + 1);
            }
        }
    }

    /// `[$this, 'm']` and `[A::class, 'm']` name the method they pass.
    fn array(&mut self, node: Node<'t>, cx: &Cx, depth: u16) {
        let elements: Vec<Node<'t>> = Self::named_children(node)
            .into_iter()
            .filter(|c| c.kind() == "array_element_initializer")
            .collect();
        if elements.len() == 2 {
            let value = |e: Node<'t>| Self::named_children(e).into_iter().next();
            if let (Some(owner), Some(method)) = (value(elements[0]), value(elements[1])) {
                if let Some(name) = self.string_literal(method) {
                    let head = match owner.kind() {
                        "variable_name" if self.text(owner) == "$this" => {
                            Some((Receiver::SelfType, Vec::new()))
                        }
                        "class_constant_access_expression" => {
                            let parts = Self::named_children(owner);
                            (parts.len() == 2
                                && self.text(parts[1]).eq_ignore_ascii_case("class")
                                && is_name(parts[0].kind()))
                            .then(|| self.scope_of(parts[0]))
                        }
                        _ => None,
                    };
                    if let Some((receiver, qualifier)) = head {
                        self.emit(method, cx, receiver, UseKind::PassAsValue, &name, qualifier);
                    }
                }
            }
        }
        for child in Self::named_children(node) {
            self.expr(child, cx, depth + 1);
        }
    }

    fn string_literal(&self, node: Node<'t>) -> Option<String> {
        let inner = match node.kind() {
            "string" => Self::named_children(node)
                .into_iter()
                .find(|c| c.kind() == "string_content")
                .map(|c| self.text(c).to_string())?,
            _ => return None,
        };
        (!inner.is_empty() && inner.chars().all(|c| c.is_alphanumeric() || c == '_'))
            .then_some(inner)
    }

    fn require(&mut self, node: Node<'t>, cx: &Cx, depth: u16) {
        let Some(argument) = Self::named_children(node).into_iter().next() else {
            return;
        };
        if let Some((module, relative)) = self.literal_path(argument) {
            self.facts.push(Fact::Import(Import {
                site: span(node),
                enclosing: cx.scope.clone(),
                module: ModulePath::new(module.as_str()),
                relative,
                kind: ImportKind::Require,
                names: Vec::new(),
                star: Star::None,
                default: None,
            }));
        }
        self.expr(argument, cx, depth + 1);
    }

    /// The path a `require` writes as a literal, with whether it starts at
    /// `__DIR__`.
    fn literal_path(&self, node: Node<'t>) -> Option<(String, u32)> {
        match node.kind() {
            "string" | "encapsed_string" => {
                let text = self.text(node);
                let inner = text.get(1..text.len().checked_sub(1)?)?;
                (!inner.contains('$')).then(|| (inner.to_string(), 0))
            }
            "parenthesized_expression" => Self::named_children(node)
                .into_iter()
                .next()
                .and_then(|n| self.literal_path(n)),
            "binary_expression" => {
                let left = node.child_by_field_name("left")?;
                let right = node.child_by_field_name("right")?;
                let rest = self.literal_path(right)?.0;
                (left.kind() == "name" && self.text(left) == "__DIR__").then(|| (rest, 1))
            }
            _ => None,
        }
    }

    // ---- chains

    /// What an expression written before `->` stands for.
    fn head(&self, node: Node<'t>, cx: &Cx) -> Head {
        match node.kind() {
            "parenthesized_expression" => Self::named_children(node)
                .into_iter()
                .next()
                .map_or_else(Head::unknown, |n| self.head(n, cx)),
            "variable_name" => {
                let text = self.text(node);
                if text == "$this" {
                    return Head {
                        receiver: Receiver::SelfType,
                        qualifier: Vec::new(),
                    };
                }
                let receiver = match cx.locals.get(text) {
                    Some(Some(Ty::Typed(t))) => Receiver::Typed(TypeName::new(t.as_str())),
                    Some(Some(Ty::New(t))) => Receiver::New(TypeName::new(t.as_str())),
                    Some(Some(Ty::Call(path))) => {
                        Receiver::CallResult(path.iter().map(|s| Name::new(s.as_str())).collect())
                    }
                    _ => Receiver::Unknown,
                };
                Head {
                    receiver,
                    qualifier: Vec::new(),
                }
            }
            "member_access_expression" | "nullsafe_member_access_expression" => {
                let (Some(object), Some(name)) = (
                    node.child_by_field_name("object"),
                    node.child_by_field_name("name"),
                ) else {
                    return Head::unknown();
                };
                let mut head = self.head(object, cx);
                if name.kind() != "name" || matches!(head.receiver, Receiver::Unknown) {
                    return Head::unknown();
                }
                head.qualifier.push(Name::new(self.text(name)));
                head
            }
            "member_call_expression" | "nullsafe_member_call_expression" => {
                let (Some(object), Some(name)) = (
                    node.child_by_field_name("object"),
                    node.child_by_field_name("name"),
                ) else {
                    return Head::unknown();
                };
                if name.kind() != "name" {
                    return Head::unknown();
                }
                let inner = self.head(object, cx);
                let owner: Vec<String> = match &inner.receiver {
                    Receiver::SelfType => vec!["self".to_string()],
                    Receiver::Typed(t) | Receiver::New(t) => split_path(t.as_str()),
                    _ => return Head::unknown(),
                };
                let mut path: Vec<Name> = owner.iter().map(|s| Name::new(s.as_str())).collect();
                if !inner.qualifier.is_empty() {
                    path.push(Name::new(PROPERTIES));
                    path.extend(inner.qualifier);
                }
                path.push(Name::new(self.text(name)));
                Head {
                    receiver: Receiver::CallResult(path),
                    qualifier: Vec::new(),
                }
            }
            "scoped_call_expression" => {
                let (Some(scope), Some(name)) = (
                    node.child_by_field_name("scope"),
                    node.child_by_field_name("name"),
                ) else {
                    return Head::unknown();
                };
                if name.kind() != "name"
                    || !(is_name(scope.kind()) || scope.kind() == "relative_scope")
                {
                    return Head::unknown();
                }
                let mut path: Vec<Name> = split_path(self.text(scope))
                    .iter()
                    .map(|s| Name::new(s.as_str()))
                    .collect();
                path.push(Name::new(self.text(name)));
                Head {
                    receiver: Receiver::CallResult(path),
                    qualifier: Vec::new(),
                }
            }
            "function_call_expression" => {
                let Some(function) = node.child_by_field_name("function") else {
                    return Head::unknown();
                };
                if !is_name(function.kind()) {
                    return Head::unknown();
                }
                Head {
                    receiver: Receiver::CallResult(
                        split_path(self.text(function))
                            .iter()
                            .map(|s| Name::new(s.as_str()))
                            .collect(),
                    ),
                    qualifier: Vec::new(),
                }
            }
            "object_creation_expression" => {
                self.created(node).map_or_else(Head::unknown, |t| Head {
                    receiver: Receiver::New(TypeName::new(t.as_str())),
                    qualifier: Vec::new(),
                })
            }
            _ => Head::unknown(),
        }
    }
}

/// Between the class and the properties of a call-result path written through
/// them: `$this->repo->find()` is `[self, ->, repo, find]`.
pub const PROPERTIES: &str = "->";

/// The name of a call or access that is not written: the variable's own text.
fn dynamic(text: &str) -> String {
    let text = text.trim();
    if text.starts_with('$') {
        text.to_string()
    } else {
        format!("${text}")
    }
}

/// Tokens of a declaration written for a reader: a space between two tokens
/// unless one hugs the other, so equal declarations print equal.
fn join_tokens(tokens: &[&str]) -> String {
    let word_end = |s: &str| s.ends_with(|c: char| c.is_alphanumeric() || c == '_');
    let mut out = String::new();
    let mut previous: Option<&str> = None;
    for &token in tokens {
        if let Some(a) = previous {
            let tight = matches!(token, ")" | "]" | "," | ":" | ";")
                || matches!(a, "(" | "[" | "?" | "\\" | "&" | "...")
                || token == "\\"
                || (token == "(" && (word_end(a) || a == ")"))
                || (token == "[" && (word_end(a) || a == "]"));
            if !tight {
                out.push(' ');
            }
        }
        out.push_str(token);
        previous = Some(token);
    }
    out
}
