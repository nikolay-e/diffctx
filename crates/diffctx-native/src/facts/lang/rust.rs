//! What a Rust file's syntax states, as the facts contract names it.
//!
//! Conventions a reader of the facts cannot recover from the types:
//! - A module is a file or an inline `mod`; the file's own module path is the
//!   workspace's (`facts_of`). A definition inside an inline `mod m` has `m` at
//!   the head of its container, and `mod m` is a `Module` definition itself. A
//!   `mod m;` declaration is a `Module` definition too; its items are in the
//!   file it names.
//! - An `impl X` block is not a definition: its methods are `X`'s, with `X` (the
//!   last segment of the type as written) as their container. A method of
//!   `impl Tr for X` takes `ImplOf(Tr)` and is public; an inherent method keeps
//!   no disambiguator. A trait's members have the trait as container.
//! - A `use` is an `Import` whose `module` is the path as written up to the
//!   imported name (`crate`, `self` or `super` first, or a crate's name), `relative: 0`.
//!   `use a::b;` imports `b` from `a`; `use a::{self}` imports `a` from its
//!   parent. A glob is `star: Star::Glob`. A `pub use` is also an `Export`.
//! - A reference names the last segment and carries the written path before it
//!   in `qualifier` (`a::f()` is `f` with `[a]`; `Self::f()` is `f` with
//!   `[Self]`). A macro invocation `m!` is a `Call` of the name `m!`. A bare
//!   name that is a local of the function around it is no reference.
//! - A method call `x.m()` is `m` with the receiver the syntax states: `self` is
//!   `SelfType`; a local whose type its declaration or constructor states is
//!   `Typed` (a trait object, `impl Tr` or a generic with a bound is the type
//!   `dyn Tr`); a call result is `CallResult` (the callee's path, with a final
//!   `?` when the result was unwrapped); anything else is `Unknown`. The fields
//!   written after `self` or a typed local go to the `qualifier`; for an
//!   `Unknown` receiver the qualifier is the whole chain written.
//! - A trait-impl method has an `Override` reference to the trait's method of
//!   the same name (qualifier: the trait's path), and the impl header has one to
//!   the trait itself, from the implementing type.
//! - `#[test]`-like functions are `Test` entry points, a top-level `main` is
//!   `Main`.
//! - Hashes are FNV-1a over the definition's own parts: members that are
//!   definitions themselves (fields, variants, a trait's items, a module's
//!   items) are left out of their container's.

use std::collections::HashMap;
use std::rc::Rc;

use tree_sitter::Node;

use super::walker::{hash_outside, hash_tree, is_member};
use super::{ERROR_RANGES_KEPT, lines_of, merge_ranges, parse_tree, span, through_errors};
use crate::facts::{
    BlobFacts, Def, DefKind, Disambig, EntryKind, EntryPoint, Export, Extractor, Fact, Hasher,
    Import, ImportKind, Language, Local, LocalKey, LocalKind, ModulePath, Name, Namespace,
    Receiver, Ref, Scope, Star, TypeName, UseKind, Visibility, expired,
};
use crate::snapshot::{Loss, RepoPath, RunBudget};

const MAX_DEPTH: u16 = 256;
const POLL_EVERY: usize = 256;

pub struct RustExtractor;

impl Extractor for RustExtractor {
    fn language(&self) -> Language {
        Language::Rust
    }

    fn extract(&self, text: &[u8], budget: &RunBudget) -> BlobFacts {
        extract(text, budget)
    }
}

fn unbound() -> RepoPath {
    RepoPath::new("<blob>").expect("a valid path")
}

fn extract(text: &[u8], budget: &RunBudget) -> BlobFacts {
    let mut blob = BlobFacts::new(Language::Rust);
    let Some(tree) = parse_tree(Language::Rust, text, budget) else {
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
    blob.record_lexical(root);
    blob.in_source_order();
    blob
}

/// A construct the grammar read whole, whatever lies around it.
fn is_whole(kind: &str) -> bool {
    kind.ends_with("_item") || kind.ends_with("_declaration") || kind.ends_with("comment")
}

/// The line ranges the parse could not read, merged: the items an error node
/// holds are read by the walk (`items`) and left out, its stray tokens are
/// what stays unread.
fn error_lines(root: Node) -> Vec<(u32, u32)> {
    let mut ranges = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let mut cursor = node.walk();
        if node.is_error() {
            if node.child_count() == 0 {
                ranges.push(lines_of(node));
            }
            for child in node.children(&mut cursor) {
                if child.is_error() {
                    stack.push(child);
                } else if child.is_missing() || !is_whole(child.kind()) {
                    ranges.push(lines_of(child));
                } else if child.has_error() {
                    stack.push(child);
                }
            }
        } else if node.is_missing() {
            ranges.push(lines_of(node));
        } else if node.has_error() {
            stack.extend(node.children(&mut cursor));
        }
    }
    merge_ranges(ranges, ERROR_RANGES_KEPT)
}

/// Tokens of a declaration written for a reader: a space between two tokens
/// unless one hugs the other, so equal declarations print equal.
fn join_tokens(tokens: &[&str]) -> String {
    let word_end = |s: &str| s.ends_with(|c: char| c.is_alphanumeric() || c == '_');
    let mut out = String::new();
    let mut previous: Option<&str> = None;
    for &token in tokens {
        if let Some(a) = previous {
            let tight = matches!(token, ")" | "]" | ">" | "," | ":" | ";" | "::" | "?")
                || matches!(a, "(" | "[" | "<" | "::" | "&" | "!" | "'")
                || (matches!(token, "(" | "[" | "<") && (word_end(a) || a == ")" || a == "]"));
            if !tight {
                out.push(' ');
            }
        }
        out.push_str(token);
        previous = Some(token);
    }
    out
}

/// A type a local is known by: a named type (the path as written, `dyn Tr` for
/// a trait object), or the result of calling a path.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Ty {
    Named(String),
    Result { path: Vec<String>, unwrapped: bool },
}

#[derive(Clone)]
struct Cx {
    /// The enclosing definitions of what is defined here, outermost first.
    container: Vec<String>,
    /// Who owns the references made here.
    scope: Scope,
    /// The last segment of the type of the `impl` around, and its full path.
    self_name: Option<String>,
    self_path: Option<String>,
    /// The trait of the `impl Tr for X` around.
    impl_trait: Option<String>,
    /// Inside a trait: its members are public.
    in_trait: bool,
    /// Generic parameters in scope, with their first bound.
    generics: Rc<HashMap<String, Option<String>>>,
    /// Inside a function body.
    in_fn: bool,
}

impl Cx {
    fn root() -> Self {
        Self {
            container: Vec::new(),
            scope: Scope::Module,
            self_name: None,
            self_path: None,
            impl_trait: None,
            in_trait: false,
            generics: Rc::new(HashMap::new()),
            in_fn: false,
        }
    }
}

struct Walker<'t> {
    bytes: &'t [u8],
    budget: &'t RunBudget,
    ticks: usize,
    halted: bool,
    too_deep: bool,
    facts: Vec<Fact>,
    /// How many definitions each key has had, for twins.
    twins: HashMap<LocalKey, u16>,
    /// The locals of the function, block and closure scopes around, innermost
    /// last, with the type their declaration states.
    locals: Vec<HashMap<String, Option<Ty>>>,
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
            locals: Vec::new(),
        }
    }

    fn run(&mut self, root: Node<'t>) {
        self.items(root, &Cx::root(), 0);
    }

    walker_methods!();
    walker_depth!();
    walker_emit!();

    /// The name an identifier node writes: `r#match` is `match`.
    fn ident(&self, node: Node<'_>) -> &'t str {
        unraw(self.text(node))
    }

    // ---- locals

    fn lookup(&self, name: &str) -> Option<&Option<Ty>> {
        self.locals.iter().rev().find_map(|scope| scope.get(name))
    }

    fn bind(&mut self, name: &str, ty: Option<Ty>) {
        if let Some(scope) = self.locals.last_mut() {
            scope.insert(name.to_string(), ty);
        }
    }

    // ---- items

    /// The children of an item container (file, module body, impl or trait
    /// body, block), each with the attributes and doc comments before it.
    fn items(&mut self, parent: Node<'t>, cx: &Cx, depth: u16) {
        if !self.deeper(depth) {
            return;
        }
        let mut attrs: Vec<String> = Vec::new();
        let mut docs: Vec<&'t str> = Vec::new();
        for child in through_errors(parent) {
            match child.kind() {
                "attribute_item" => {
                    attrs.push(self.text(child).to_string());
                    continue;
                }
                "inner_attribute_item" => continue,
                "line_comment" | "block_comment" => {
                    let doc = self.text(child);
                    if doc.starts_with("///") || doc.starts_with("/**") {
                        docs.push(doc);
                    }
                    continue;
                }
                _ => {}
            }
            self.item(child, cx, &attrs, &docs, depth + 1);
            attrs.clear();
            docs.clear();
        }
    }

    fn item(&mut self, node: Node<'t>, cx: &Cx, attrs: &[String], docs: &[&'t str], depth: u16) {
        if !self.deeper(depth) {
            return;
        }
        match node.kind() {
            "function_item" | "function_signature_item" => {
                self.function(node, cx, attrs, docs, depth)
            }
            "impl_item" => self.implementation(node, cx, depth),
            "trait_item" => self.trait_item(node, cx, attrs, docs, depth),
            "struct_item" | "union_item" => self.structure(node, cx, attrs, docs, depth),
            "enum_item" => self.enumeration(node, cx, attrs, docs, depth),
            "type_item" | "associated_type" => {
                let Some(key) =
                    self.define(node, cx, attrs, docs, Shape::of(node, DefKind::TypeAlias))
                else {
                    return;
                };
                let inner = self.inside(cx, &key);
                self.type_refs_in(node, &inner, depth, &["name"]);
            }
            "const_item" | "static_item" => {
                let kind = if node.kind() == "const_item" {
                    DefKind::Constant
                } else {
                    DefKind::Variable
                };
                let Some(key) = self.define(node, cx, attrs, docs, Shape::of(node, kind)) else {
                    return;
                };
                let inner = self.inside(cx, &key);
                if let Some(ty) = node.child_by_field_name("type") {
                    self.type_refs(ty, &inner, depth);
                }
                if let Some(value) = node.child_by_field_name("value") {
                    self.locals.push(HashMap::new());
                    self.expr(value, &inner, depth + 1);
                    self.locals.pop();
                }
            }
            "mod_item" => self.module(node, cx, attrs, docs, depth),
            "macro_definition" => {
                let shape = Shape::of(node, DefKind::Macro);
                self.define(node, cx, attrs, docs, shape);
            }
            "use_declaration" => self.import(node, cx),
            "extern_crate_declaration" | "foreign_mod_item" | "empty_statement" => {}
            "let_declaration" => self.let_statement(node, cx, depth),
            _ => self.expr(node, cx, depth),
        }
    }

    /// The context of what is defined inside the definition `key`.
    fn inside(&self, cx: &Cx, key: &LocalKey) -> Cx {
        let mut container = cx.container.clone();
        container.push(key.name.as_str().to_string());
        Cx {
            container,
            scope: Scope::Def(key.clone()),
            ..cx.clone()
        }
    }

    fn visibility(&self, node: Node<'t>, cx: &Cx) -> Visibility {
        let mut cursor = node.walk();
        let written = node
            .named_children(&mut cursor)
            .find(|c| c.kind() == "visibility_modifier")
            .map(|v| self.text(v).split_whitespace().collect::<String>());
        match written.as_deref() {
            Some("pub") => Visibility::Public,
            Some("pub(crate)") => Visibility::Crate,
            Some("pub(self)") => Visibility::Private,
            Some(_) => Visibility::Package,
            None if cx.in_trait || cx.impl_trait.is_some() => Visibility::Public,
            None => Visibility::Private,
        }
    }

    /// The key a definition of `name` takes in `cx`: twins of one key are
    /// told apart by their configuration, else in source order.
    fn key_for(&mut self, cx: &Cx, name: &str, ns: Namespace, attrs: &[String]) -> LocalKey {
        let mut key = LocalKey::new(
            cx.container.iter().map(|c| Name::new(c.as_str())).collect(),
            name,
            ns,
        );
        if let Some(tr) = &cx.impl_trait {
            key = key.with_disambig(Disambig::ImplOf(TypeName::new(tr.as_str())));
        }
        let seen = self.twins.entry(key.clone()).or_insert(0);
        let nth = *seen;
        *seen += 1;
        if nth == 0 {
            return key;
        }
        let cfg = attrs
            .iter()
            .find(|a| a.contains("cfg("))
            .map(|a| a.split_whitespace().collect::<String>());
        match cfg {
            Some(cfg) => key.with_disambig(Disambig::Cfg(cfg)),
            None => key.with_disambig(Disambig::Overload(nth)),
        }
    }

    /// Emits the definition of a named item and returns its key.
    fn define(
        &mut self,
        node: Node<'t>,
        cx: &Cx,
        attrs: &[String],
        docs: &[&'t str],
        shape: Shape,
    ) -> Option<LocalKey> {
        let name_node = node.child_by_field_name("name")?;
        let name = self.ident(name_node);
        let key = self.key_for(cx, name, shape.ns, attrs);
        let members = members_of(node);
        let signature = self.signature(node, &members);
        let decorators = attrs
            .iter()
            .filter_map(|a| attribute_name(a))
            .map(Name::new)
            .collect();
        let doc_hash = {
            let mut hasher = Hasher::new();
            for doc in docs {
                for word in doc.split_whitespace() {
                    hasher.token(word.trim_start_matches('/').as_bytes());
                }
            }
            hasher.finish()
        };
        self.facts.push(Fact::Definition(Def {
            local: key.clone(),
            kind: shape.kind,
            span: span(node),
            name_span: span(name_node),
            signature: Some(signature),
            decorators,
            visibility: self.visibility(node, cx),
            body_hash: self.body_hash(node, &members),
            text_hash: hash_outside(self.bytes, node, &members),
            doc_hash,
        }));
        Some(key)
    }

    /// The declaration without its body, comments and attributes.
    fn signature(&self, node: Node<'t>, members: &[Node<'t>]) -> String {
        let mut tokens: Vec<&'t str> = Vec::new();
        let mut stack = vec![node];
        let body = node.child_by_field_name("body");
        let value = node.child_by_field_name("value");
        while let Some(n) = stack.pop() {
            if n.kind().ends_with("comment") || n.kind() == "attribute_item" {
                continue;
            }
            let held = Some(n.id()) == body.map(|b| b.id())
                || Some(n.id()) == value.map(|v| v.id())
                || members.iter().any(|m| m.id() == n.id());
            if held {
                continue;
            }
            if n.child_count() == 0 {
                tokens.push(self.text(n));
            } else {
                let mut cursor = n.walk();
                let kids: Vec<Node<'t>> = n.children(&mut cursor).collect();
                stack.extend(kids.into_iter().rev());
            }
        }
        join_tokens(&tokens)
    }

    /// The body's tree, or for a declaration with none (a struct, a type
    /// alias, a trait method signature) nothing: `0` is never paired by.
    fn body_hash(&self, node: Node<'t>, members: &[Node<'t>]) -> u64 {
        match node.kind() {
            "function_signature_item" | "associated_type" => 0,
            "struct_item" | "union_item" | "enum_item" | "trait_item" | "type_item"
            | "mod_item" => {
                let name = node.child_by_field_name("name").map(|n| n.id());
                hash_tree(self.bytes, node, |n| {
                    is_member(members, n) || Some(n.id()) == name
                })
            }
            _ => node
                .child_by_field_name("body")
                .or_else(|| node.child_by_field_name("value"))
                .map_or(0, |b| hash_tree(self.bytes, b, |n| is_member(members, n))),
        }
    }

    // ---- functions

    fn function(
        &mut self,
        node: Node<'t>,
        cx: &Cx,
        attrs: &[String],
        docs: &[&'t str],
        depth: u16,
    ) {
        let member = cx.self_name.is_some() || cx.in_trait;
        let kind = if member && !cx.in_fn {
            DefKind::Method
        } else {
            DefKind::Function
        };
        let Some(key) = self.define(node, cx, attrs, docs, Shape::of(node, kind)) else {
            return;
        };
        let name = key.name.as_str().to_string();
        if attrs.iter().any(|a| is_test_attribute(a)) {
            self.facts.push(Fact::EntryPoint(EntryPoint {
                site: span(node),
                kind: EntryKind::Test,
                target: Scope::Def(key.clone()),
                framework: None,
            }));
        } else if name == "main" && cx.container.is_empty() {
            self.facts.push(Fact::EntryPoint(EntryPoint {
                site: span(node),
                kind: EntryKind::Main,
                target: Scope::Def(key.clone()),
                framework: None,
            }));
        }
        if let Some(tr) = &cx.impl_trait {
            let path = path_names(tr);
            let name_node = node.child_by_field_name("name").unwrap_or(node);
            self.facts.push(Fact::Reference(Ref {
                site: span(name_node),
                enclosing: Scope::Def(key.clone()),
                receiver: Receiver::None,
                use_kind: UseKind::Override,
                name: Name::new(name.as_str()),
                qualifier: path.into_iter().map(Name::new).collect(),
            }));
        }
        let mut inner = self.inside(cx, &key);
        inner.in_fn = true;
        inner.generics = Rc::new(self.generics_of(node, &cx.generics));
        self.locals.push(HashMap::new());
        if let Some(parameters) = node.child_by_field_name("parameters") {
            self.parameters(parameters, &inner, depth + 1);
        }
        if let Some(ret) = node.child_by_field_name("return_type") {
            self.type_refs(ret, &inner, depth + 1);
        }
        self.where_refs(node, &inner, depth + 1);
        if let Some(body) = node.child_by_field_name("body") {
            self.block(body, &inner, depth + 1);
        }
        self.locals.pop();
    }

    fn parameters(&mut self, parameters: Node<'t>, cx: &Cx, depth: u16) {
        let mut cursor = parameters.walk();
        let list: Vec<Node<'t>> = parameters.named_children(&mut cursor).collect();
        for parameter in list {
            match parameter.kind() {
                "parameter" => {
                    let ty = parameter.child_by_field_name("type");
                    if let Some(ty) = ty {
                        self.type_refs(ty, cx, depth);
                    }
                    let known = ty.and_then(|t| self.ty_of(t, cx));
                    if let Some(pattern) = parameter.child_by_field_name("pattern") {
                        self.pattern(pattern, known, cx, LocalKind::Parameter, depth);
                    }
                }
                "self_parameter" => {}
                _ => {}
            }
        }
    }

    fn block(&mut self, body: Node<'t>, cx: &Cx, depth: u16) {
        self.locals.push(HashMap::new());
        self.items(body, cx, depth);
        self.locals.pop();
    }

    fn generics_of(
        &self,
        node: Node<'t>,
        outer: &Rc<HashMap<String, Option<String>>>,
    ) -> HashMap<String, Option<String>> {
        let mut found: HashMap<String, Option<String>> = (**outer).clone();
        let bound = |bounds: Node<'t>| -> Option<String> {
            let mut cursor = bounds.walk();
            let list: Vec<Node<'t>> = bounds.named_children(&mut cursor).collect();
            list.into_iter().find_map(|b| match b.kind() {
                "type_identifier" | "scoped_type_identifier" | "generic_type" => {
                    path_of(self.bytes, b).map(|p| p.join("::"))
                }
                _ => None,
            })
        };
        if let Some(parameters) = node.child_by_field_name("type_parameters") {
            let mut cursor = parameters.walk();
            let list: Vec<Node<'t>> = parameters.named_children(&mut cursor).collect();
            for parameter in list {
                if parameter.kind() != "type_parameter" {
                    continue;
                }
                if let Some(name) = parameter.child_by_field_name("name") {
                    let held = parameter.child_by_field_name("bounds").and_then(bound);
                    found.insert(self.ident(name).to_string(), held);
                }
            }
        }
        let mut cursor = node.walk();
        let clauses: Vec<Node<'t>> = node
            .named_children(&mut cursor)
            .filter(|c| c.kind() == "where_clause")
            .collect();
        for clause in clauses {
            let mut inner = clause.walk();
            let predicates: Vec<Node<'t>> = clause.named_children(&mut inner).collect();
            for predicate in predicates {
                let (Some(left), Some(bounds)) = (
                    predicate.child_by_field_name("left"),
                    predicate.child_by_field_name("bounds"),
                ) else {
                    continue;
                };
                if left.kind() == "type_identifier" {
                    if let Some(held) = bound(bounds) {
                        found.insert(self.ident(left).to_string(), Some(held));
                    }
                }
            }
        }
        found
    }

    /// The type references of a `where` clause.
    fn where_refs(&mut self, node: Node<'t>, cx: &Cx, depth: u16) {
        let mut cursor = node.walk();
        let clauses: Vec<Node<'t>> = node
            .named_children(&mut cursor)
            .filter(|c| c.kind() == "where_clause" || c.kind() == "type_parameters")
            .collect();
        for clause in clauses {
            self.type_refs(clause, cx, depth);
        }
    }

    // ---- types

    /// The type a declaration states, for the receivers of values of it.
    fn ty_of(&self, node: Node<'t>, cx: &Cx) -> Option<Ty> {
        match node.kind() {
            "type_identifier" | "scoped_type_identifier" | "identifier" | "scoped_identifier" => {
                let path = path_of(self.bytes, node)?;
                if let [only] = path.as_slice() {
                    if only == "Self" {
                        return cx.self_path.clone().map(Ty::Named);
                    }
                    if let Some(bound) = cx.generics.get(only) {
                        return bound.as_ref().map(|b| Ty::Named(format!("dyn {b}")));
                    }
                }
                Some(Ty::Named(path.join("::")))
            }
            "generic_type" => {
                let head = path_of(self.bytes, node.child_by_field_name("type")?)?;
                // A smart pointer derefs to what it holds.
                if matches!(head.as_slice(), [p] if matches!(p.as_str(), "Box" | "Rc" | "Arc")) {
                    let args = node.child_by_field_name("type_arguments")?;
                    let mut cursor = args.walk();
                    let list: Vec<Node<'t>> = args.named_children(&mut cursor).collect();
                    return list.into_iter().find_map(|a| self.ty_of(a, cx));
                }
                if let [only] = head.as_slice() {
                    if let Some(bound) = cx.generics.get(only) {
                        return bound.as_ref().map(|b| Ty::Named(format!("dyn {b}")));
                    }
                }
                Some(Ty::Named(head.join("::")))
            }
            "reference_type" | "pointer_type" => self.ty_of(node.child_by_field_name("type")?, cx),
            "dynamic_type" | "abstract_type" => {
                let tr = node.child_by_field_name("trait")?;
                let inner = match tr.kind() {
                    "generic_type" => tr.child_by_field_name("type")?,
                    _ => tr,
                };
                Some(Ty::Named(format!(
                    "dyn {}",
                    path_of(self.bytes, inner)?.join("::")
                )))
            }
            "bounded_type" => {
                let mut cursor = node.walk();
                let list: Vec<Node<'t>> = node.named_children(&mut cursor).collect();
                list.into_iter().find_map(|c| self.ty_of(c, cx))
            }
            _ => None,
        }
    }

    /// Every type name written in a type (or in anything made of types) is a
    /// `TypeUse`; the head of a trait object or `impl Tr` is one too.
    fn type_refs(&mut self, node: Node<'t>, cx: &Cx, depth: u16) {
        self.type_refs_in(node, cx, depth, &[]);
    }

    fn type_refs_in(&mut self, node: Node<'t>, cx: &Cx, depth: u16, skip: &[&str]) {
        if !self.deeper(depth) {
            return;
        }
        let skipped: Vec<usize> = skip
            .iter()
            .filter_map(|f| node.child_by_field_name(f))
            .map(|n| n.id())
            .collect();
        match node.kind() {
            "type_identifier" | "scoped_type_identifier" => {
                self.path_ref(node, cx, UseKind::TypeUse, depth);
                return;
            }
            "primitive_type"
            | "lifetime"
            | "string_literal"
            | "line_comment"
            | "block_comment"
            | "visibility_modifier"
            | "attribute_item" => return,
            "generic_type" => {
                if let Some(head) = node.child_by_field_name("type") {
                    self.path_ref(head, cx, UseKind::TypeUse, depth);
                }
                if let Some(args) = node.child_by_field_name("type_arguments") {
                    self.type_refs(args, cx, depth + 1);
                }
                return;
            }
            // A generic parameter is declared here, not used: its bounds are.
            "type_parameter" | "const_parameter" => {
                for field in ["bounds", "default_type", "type", "value"] {
                    if let Some(inner) = node.child_by_field_name(field) {
                        self.type_refs(inner, cx, depth + 1);
                    }
                }
                return;
            }
            "scoped_identifier" | "identifier"
                if node
                    .parent()
                    .is_some_and(|p| matches!(p.kind(), "trait_bounds" | "type_binding")) =>
            {
                self.path_ref(node, cx, UseKind::TypeUse, depth);
                return;
            }
            _ => {}
        }
        let mut cursor = node.walk();
        let kids: Vec<Node<'t>> = node
            .named_children(&mut cursor)
            .filter(|k| !skipped.contains(&k.id()))
            .collect();
        for kid in kids {
            self.type_refs(kid, cx, depth + 1);
        }
    }

    // ---- impls, traits, structs, enums, modules

    fn implementation(&mut self, node: Node<'t>, cx: &Cx, depth: u16) {
        let Some(ty) = node.child_by_field_name("type") else {
            return;
        };
        let mut ty = ty;
        while matches!(ty.kind(), "reference_type" | "pointer_type") {
            match ty.child_by_field_name("type") {
                Some(inner) => ty = inner,
                None => return,
            }
        }
        let head = match ty.kind() {
            "generic_type" => ty.child_by_field_name("type").unwrap_or(ty),
            _ => ty,
        };
        let Some(path) = path_of(self.bytes, head) else {
            return;
        };
        let Some(self_name) = path.last().cloned() else {
            return;
        };
        let generics = Rc::new(self.generics_of(node, &cx.generics));
        let outer = Cx {
            generics: Rc::clone(&generics),
            ..cx.clone()
        };
        // The type's own arguments are uses; its name is the impl's subject.
        if ty.kind() == "generic_type" {
            if let Some(args) = ty.child_by_field_name("type_arguments") {
                self.type_refs(args, &outer, depth + 1);
            }
        }
        self.where_refs(node, &outer, depth + 1);
        let mut impl_trait = None;
        if let Some(tr) = node.child_by_field_name("trait") {
            let tr_head = match tr.kind() {
                "generic_type" => tr.child_by_field_name("type").unwrap_or(tr),
                _ => tr,
            };
            if tr.kind() == "generic_type" {
                if let Some(args) = tr.child_by_field_name("type_arguments") {
                    self.type_refs(args, &outer, depth + 1);
                }
            }
            if let Some(tr_path) = path_of(self.bytes, tr_head) {
                impl_trait = Some(tr_path.join("::"));
                let mut qualifier = tr_path.clone();
                let Some(name) = qualifier.pop() else {
                    return;
                };
                let subject = LocalKey::new(
                    cx.container.iter().map(|c| Name::new(c.as_str())).collect(),
                    self_name.as_str(),
                    Namespace::Type,
                );
                self.facts.push(Fact::Reference(Ref {
                    site: span(tr_head),
                    enclosing: Scope::Def(subject),
                    receiver: Receiver::None,
                    use_kind: UseKind::Override,
                    name: Name::new(name),
                    qualifier: qualifier.into_iter().map(Name::new).collect(),
                }));
            }
        }
        let Some(body) = node.child_by_field_name("body") else {
            return;
        };
        let mut container = cx.container.clone();
        container.push(self_name.clone());
        let inner = Cx {
            container,
            self_name: Some(self_name),
            self_path: Some(path.join("::")),
            impl_trait,
            in_trait: false,
            generics,
            ..cx.clone()
        };
        self.items(body, &inner, depth + 1);
    }

    fn trait_item(
        &mut self,
        node: Node<'t>,
        cx: &Cx,
        attrs: &[String],
        docs: &[&'t str],
        depth: u16,
    ) {
        let Some(key) = self.define(node, cx, attrs, docs, Shape::of(node, DefKind::Trait)) else {
            return;
        };
        let mut inner = self.inside(cx, &key);
        inner.in_trait = true;
        inner.self_name = None;
        inner.impl_trait = None;
        inner.generics = Rc::new(self.generics_of(node, &cx.generics));
        if let Some(bounds) = node.child_by_field_name("bounds") {
            self.type_refs(bounds, &inner, depth + 1);
        }
        self.where_refs(node, &inner, depth + 1);
        if let Some(body) = node.child_by_field_name("body") {
            self.items(body, &inner, depth + 1);
        }
    }

    fn structure(
        &mut self,
        node: Node<'t>,
        cx: &Cx,
        attrs: &[String],
        docs: &[&'t str],
        depth: u16,
    ) {
        let Some(key) = self.define(node, cx, attrs, docs, Shape::of(node, DefKind::Struct)) else {
            return;
        };
        let mut inner = self.inside(cx, &key);
        inner.generics = Rc::new(self.generics_of(node, &cx.generics));
        self.where_refs(node, &inner, depth + 1);
        if let Some(body) = node.child_by_field_name("body") {
            self.fields(body, &inner, depth + 1);
        }
    }

    /// The fields of a struct or of a struct-like variant: named ones are
    /// definitions, positional ones only name types.
    fn fields(&mut self, body: Node<'t>, cx: &Cx, depth: u16) {
        let mut cursor = body.walk();
        let list: Vec<Node<'t>> = body.named_children(&mut cursor).collect();
        let mut attrs: Vec<String> = Vec::new();
        for field in list {
            match field.kind() {
                "attribute_item" => {
                    attrs.push(self.text(field).to_string());
                    continue;
                }
                "field_declaration" => {
                    let ty = field.child_by_field_name("type");
                    let shape = Shape {
                        kind: DefKind::Field,
                        ns: Namespace::Value,
                    };
                    self.define(field, cx, &attrs, &[], shape);
                    if let Some(ty) = ty {
                        self.type_refs(ty, cx, depth + 1);
                    }
                }
                _ => self.type_refs(field, cx, depth + 1),
            }
            attrs.clear();
        }
    }

    fn enumeration(
        &mut self,
        node: Node<'t>,
        cx: &Cx,
        attrs: &[String],
        docs: &[&'t str],
        depth: u16,
    ) {
        let Some(key) = self.define(node, cx, attrs, docs, Shape::of(node, DefKind::Enum)) else {
            return;
        };
        let mut inner = self.inside(cx, &key);
        inner.generics = Rc::new(self.generics_of(node, &cx.generics));
        self.where_refs(node, &inner, depth + 1);
        let Some(body) = node.child_by_field_name("body") else {
            return;
        };
        let mut cursor = body.walk();
        let list: Vec<Node<'t>> = body.named_children(&mut cursor).collect();
        let mut variant_attrs: Vec<String> = Vec::new();
        for variant in list {
            if variant.kind() == "attribute_item" {
                variant_attrs.push(self.text(variant).to_string());
                continue;
            }
            if variant.kind() == "enum_variant" {
                let shape = Shape {
                    kind: DefKind::EnumMember,
                    ns: Namespace::Value,
                };
                let public = Cx {
                    in_trait: true,
                    ..inner.clone()
                };
                let made = self.define(variant, &public, &variant_attrs, &[], shape);
                if let (Some(_), Some(fields)) = (made, variant.child_by_field_name("body")) {
                    self.fields_as_types(fields, &inner, depth + 1);
                }
            }
            variant_attrs.clear();
        }
    }

    /// The types of a variant's fields: uses, never definitions of their own.
    fn fields_as_types(&mut self, body: Node<'t>, cx: &Cx, depth: u16) {
        let mut cursor = body.walk();
        let list: Vec<Node<'t>> = body.named_children(&mut cursor).collect();
        for field in list {
            match field.kind() {
                "field_declaration" => {
                    if let Some(ty) = field.child_by_field_name("type") {
                        self.type_refs(ty, cx, depth + 1);
                    }
                }
                "attribute_item" => {}
                _ => self.type_refs(field, cx, depth + 1),
            }
        }
    }

    fn module(&mut self, node: Node<'t>, cx: &Cx, attrs: &[String], docs: &[&'t str], depth: u16) {
        let Some(key) = self.define(node, cx, attrs, docs, Shape::of(node, DefKind::Module)) else {
            return;
        };
        let Some(body) = node.child_by_field_name("body") else {
            return;
        };
        let inner = Cx {
            in_fn: false,
            self_name: None,
            self_path: None,
            impl_trait: None,
            in_trait: false,
            ..self.inside(cx, &key)
        };
        let saved = std::mem::take(&mut self.locals);
        self.items(body, &inner, depth + 1);
        self.locals = saved;
    }

    // ---- imports

    fn import(&mut self, node: Node<'t>, cx: &Cx) {
        let Some(argument) = node.child_by_field_name("argument") else {
            return;
        };
        let mut cursor = node.walk();
        let exported = node
            .named_children(&mut cursor)
            .any(|c| c.kind() == "visibility_modifier");
        let mut leaves: Vec<Leaf> = Vec::new();
        self.use_leaves(argument, &mut Vec::new(), &mut leaves);
        let site = span(node);
        for leaf in leaves {
            let (module, names, star) = match leaf {
                Leaf::Glob(path) => (path, Vec::new(), Star::Glob),
                Leaf::Name { mut path, alias } => {
                    let Some(name) = path.pop() else { continue };
                    (
                        path,
                        vec![(Name::new(name), alias.map(Name::new))],
                        Star::None,
                    )
                }
            };
            let joined = module.join("::");
            self.facts.push(Fact::Import(Import {
                site: site.clone(),
                enclosing: cx.scope.clone(),
                module: ModulePath::new(joined.as_str()),
                relative: 0,
                kind: ImportKind::Static,
                names: names.clone(),
                star: star.clone(),
                default: None,
            }));
            if exported {
                match star {
                    Star::Glob | Star::Alias(_) => self.facts.push(Fact::Export(Export::Star {
                        site: site.clone(),
                        from: ModulePath::new(joined.as_str()),
                        alias: None,
                    })),
                    Star::None => {
                        for (name, alias) in names {
                            self.facts.push(Fact::Export(Export::Named {
                                site: site.clone(),
                                name,
                                alias,
                                from: Some(ModulePath::new(joined.as_str())),
                                type_only: false,
                            }));
                        }
                    }
                }
            }
        }
    }

    fn use_leaves(&self, node: Node<'t>, prefix: &mut Vec<String>, out: &mut Vec<Leaf>) {
        match node.kind() {
            "use_as_clause" => {
                let (Some(path), Some(alias)) = (
                    node.child_by_field_name("path")
                        .and_then(|p| path_of(self.bytes, p)),
                    node.child_by_field_name("alias"),
                ) else {
                    return;
                };
                let alias = self.ident(alias).to_string();
                if alias == "_" {
                    return;
                }
                let mut full = prefix.clone();
                full.extend(path);
                out.push(Leaf::Name {
                    path: full,
                    alias: Some(alias),
                });
            }
            "use_wildcard" => {
                let Some(path) = use_prefix(self.bytes, node.named_child(0)) else {
                    return;
                };
                let mut full = prefix.clone();
                full.extend(path);
                out.push(Leaf::Glob(full));
            }
            "scoped_use_list" => {
                let Some(path) = use_prefix(self.bytes, node.child_by_field_name("path")) else {
                    return;
                };
                let mut full = prefix.clone();
                full.extend(path);
                if let Some(list) = node.child_by_field_name("list") {
                    let mut cursor = list.walk();
                    let items: Vec<Node<'t>> = list.named_children(&mut cursor).collect();
                    for item in items {
                        self.use_leaves(item, &mut full.clone(), out);
                    }
                }
            }
            "use_list" => {
                let mut cursor = node.walk();
                let items: Vec<Node<'t>> = node.named_children(&mut cursor).collect();
                for item in items {
                    self.use_leaves(item, &mut prefix.clone(), out);
                }
            }
            _ => {
                let Some(path) = path_of(self.bytes, node) else {
                    return;
                };
                let mut full = prefix.clone();
                full.extend(path);
                // `use a::{self}` binds `a`.
                if full.last().is_some_and(|s| s == "self") && full.len() > 1 {
                    full.pop();
                }
                out.push(Leaf::Name {
                    path: full,
                    alias: None,
                });
            }
        }
    }

    // ---- statements and expressions

    fn let_statement(&mut self, node: Node<'t>, cx: &Cx, depth: u16) {
        let declared = node.child_by_field_name("type");
        if let Some(ty) = declared {
            self.type_refs(ty, cx, depth + 1);
        }
        let value = node.child_by_field_name("value");
        if let Some(value) = value {
            self.expr(value, cx, depth + 1);
        }
        if let Some(alternative) = node.child_by_field_name("alternative") {
            self.expr(alternative, cx, depth + 1);
        }
        let known = declared
            .and_then(|t| self.ty_of(t, cx))
            .or_else(|| value.and_then(|v| self.built_ty(v, cx)));
        if let Some(pattern) = node.child_by_field_name("pattern") {
            self.pattern(pattern, known, cx, LocalKind::Variable, depth + 1);
        }
    }

    /// The type a value expression builds: `T { .. }`, `T::new(..)` by the
    /// constructor convention, and what a call of a path returns.
    fn built_ty(&self, node: Node<'t>, cx: &Cx) -> Option<Ty> {
        let unwrap = |ty: Ty| match ty {
            Ty::Result { path, .. } => Ty::Result {
                path,
                unwrapped: true,
            },
            other => other,
        };
        match node.kind() {
            "try_expression" => self.built_ty(node.named_child(0)?, cx).map(unwrap),
            "parenthesized_expression" | "await_expression" => {
                self.built_ty(node.named_child(0)?, cx)
            }
            "reference_expression" => self.built_ty(node.child_by_field_name("value")?, cx),
            "struct_expression" => {
                let path = path_of(self.bytes, node.child_by_field_name("name")?)?;
                Some(Ty::Named(self.self_resolved(path, cx)))
            }
            "call_expression" => {
                let mut function = node.child_by_field_name("function")?;
                if function.kind() == "generic_function" {
                    function = function.child_by_field_name("function")?;
                }
                if function.kind() == "field_expression" {
                    let field = function.child_by_field_name("field").map(|n| self.ident(n));
                    return matches!(field, Some("unwrap" | "expect"))
                        .then(|| {
                            self.built_ty(function.child_by_field_name("value")?, cx)
                                .map(unwrap)
                        })
                        .flatten();
                }
                if !matches!(function.kind(), "identifier" | "scoped_identifier") {
                    return None;
                }
                let path = path_of(self.bytes, function)?;
                match path.as_slice() {
                    [.., ty, _] if ty.starts_with(char::is_uppercase) => {
                        let owner = path[..path.len() - 1].to_vec();
                        Some(Ty::Named(self.self_resolved(owner, cx)))
                    }
                    _ => Some(Ty::Result {
                        path,
                        unwrapped: false,
                    }),
                }
            }
            _ => None,
        }
    }

    /// `Self` and a generic name as a type path.
    fn self_resolved(&self, path: Vec<String>, cx: &Cx) -> String {
        if let [only] = path.as_slice() {
            if only == "Self" {
                if let Some(own) = &cx.self_path {
                    return own.clone();
                }
            }
            if let Some(bound) = cx.generics.get(only) {
                return bound
                    .as_ref()
                    .map_or_else(|| path.join("::"), |b| format!("dyn {b}"));
            }
        }
        path.join("::")
    }

    /// The names a pattern binds, with the type the declaration states for the
    /// whole pattern. A capitalised name is a constant, a unit struct or a
    /// variant: a use, not a binding.
    fn pattern(&mut self, node: Node<'t>, ty: Option<Ty>, cx: &Cx, kind: LocalKind, depth: u16) {
        if !self.deeper(depth) {
            return;
        }
        match node.kind() {
            "identifier" => {
                let name = self.ident(node);
                if name.starts_with(char::is_uppercase) {
                    self.emit(node, cx, Receiver::None, UseKind::Read, name, Vec::new());
                    return;
                }
                self.facts.push(Fact::LocalBinding(Local {
                    name: Name::new(name),
                    site: span(node),
                    enclosing: cx.scope.clone(),
                    kind,
                }));
                self.bind(name, ty);
            }
            "mut_pattern" | "reference_pattern" | "ref_pattern" | "captured_pattern" => {
                let mut cursor = node.walk();
                let kids: Vec<Node<'t>> = node.named_children(&mut cursor).collect();
                for kid in kids {
                    self.pattern(kid, ty.clone(), cx, kind, depth + 1);
                }
            }
            "tuple_struct_pattern" | "struct_pattern" => {
                if let Some(path) = node.child_by_field_name("type") {
                    self.path_ref(path, cx, UseKind::Read, depth + 1);
                }
                let mut cursor = node.walk();
                let kids: Vec<Node<'t>> = node.named_children(&mut cursor).collect();
                let head = node.child_by_field_name("type").map(|t| t.id());
                for kid in kids {
                    if Some(kid.id()) == head {
                        continue;
                    }
                    self.pattern(kid, None, cx, kind, depth + 1);
                }
            }
            "field_pattern" => {
                if let Some(inner) = node.child_by_field_name("pattern") {
                    self.pattern(inner, None, cx, kind, depth + 1);
                } else if let Some(name) = node.child_by_field_name("name") {
                    let written = self.ident(name);
                    self.facts.push(Fact::LocalBinding(Local {
                        name: Name::new(written),
                        site: span(name),
                        enclosing: cx.scope.clone(),
                        kind,
                    }));
                    self.bind(written, None);
                }
            }
            "scoped_identifier" => self.path_ref(node, cx, UseKind::Read, depth + 1),
            "_" | "string_literal" | "integer_literal" | "float_literal" | "boolean_literal"
            | "char_literal" | "negative_literal" | "raw_string_literal" | "line_comment"
            | "block_comment" => {}
            _ => {
                let mut cursor = node.walk();
                let kids: Vec<Node<'t>> = node.named_children(&mut cursor).collect();
                for kid in kids {
                    self.pattern(kid, None, cx, kind, depth + 1);
                }
            }
        }
    }

    fn expr(&mut self, node: Node<'t>, cx: &Cx, depth: u16) {
        if !self.deeper(depth) {
            return;
        }
        match node.kind() {
            "line_comment"
            | "block_comment"
            | "string_literal"
            | "raw_string_literal"
            | "char_literal"
            | "integer_literal"
            | "float_literal"
            | "boolean_literal"
            | "lifetime"
            | "label"
            | "self"
            | "metavariable"
            | "attribute_item"
            | "inner_attribute_item"
            | "primitive_type"
            | "field_identifier"
            | "shorthand_field_identifier" => {}
            "identifier" => self.path_ref(node, cx, UseKind::Read, depth),
            "scoped_identifier" => self.path_ref(node, cx, UseKind::Read, depth),
            "generic_function" => self.path_ref(node, cx, UseKind::Read, depth),
            "type_identifier" | "scoped_type_identifier" | "generic_type" => {
                self.type_refs(node, cx, depth)
            }
            "call_expression" => self.call(node, cx, depth),
            "field_expression" => self.field(node, cx, UseKind::Read, depth),
            "assignment_expression" | "compound_assignment_expr" => {
                if let Some(left) = node.child_by_field_name("left") {
                    if left.kind() == "field_expression" {
                        self.field(left, cx, UseKind::Write, depth + 1);
                    } else {
                        self.expr(left, cx, depth + 1);
                    }
                }
                if let Some(right) = node.child_by_field_name("right") {
                    self.expr(right, cx, depth + 1);
                }
            }
            "struct_expression" => {
                if let Some(name) = node.child_by_field_name("name") {
                    self.path_ref(name, cx, UseKind::Instantiate, depth + 1);
                }
                if let Some(body) = node.child_by_field_name("body") {
                    let mut cursor = body.walk();
                    let list: Vec<Node<'t>> = body.named_children(&mut cursor).collect();
                    for init in list {
                        // `a: 1` and the shorthand `a` write the field `a` of the struct built.
                        let label = match init.kind() {
                            "field_initializer" => init
                                .child_by_field_name("field")
                                .filter(|label| label.kind() == "field_identifier"),
                            "shorthand_field_initializer" => {
                                let mut cursor = init.walk();
                                init.named_children(&mut cursor)
                                    .find(|kid| kid.kind() == "identifier")
                            }
                            _ => None,
                        };
                        if let (Some(label), Some(Ty::Named(owner))) =
                            (label, self.built_ty(node, cx))
                        {
                            let receiver = Receiver::Typed(TypeName::new(owner.as_str()));
                            let name = self.ident(label);
                            self.emit(label, cx, receiver, UseKind::Write, name, Vec::new());
                        }
                        match init.child_by_field_name("value") {
                            Some(value) => self.expr(value, cx, depth + 1),
                            None => self.expr(init, cx, depth + 1),
                        }
                    }
                }
            }
            "macro_invocation" => self.macro_call(node, cx, depth),
            "token_tree" => self.tokens(node, cx, depth),
            "let_declaration" => self.let_statement(node, cx, depth),
            "block" | "unsafe_block" | "async_block" | "const_block" => self.block(node, cx, depth),
            "closure_expression" => {
                self.locals.push(HashMap::new());
                if let Some(parameters) = node.child_by_field_name("parameters") {
                    let mut cursor = parameters.walk();
                    let list: Vec<Node<'t>> = parameters.named_children(&mut cursor).collect();
                    for parameter in list {
                        if parameter.kind() == "parameter" {
                            let ty = parameter.child_by_field_name("type");
                            if let Some(ty) = ty {
                                self.type_refs(ty, cx, depth + 1);
                            }
                            let known = ty.and_then(|t| self.ty_of(t, cx));
                            if let Some(pattern) = parameter.child_by_field_name("pattern") {
                                self.pattern(pattern, known, cx, LocalKind::Parameter, depth + 1);
                            }
                        } else {
                            self.pattern(parameter, None, cx, LocalKind::Parameter, depth + 1);
                        }
                    }
                }
                if let Some(ret) = node.child_by_field_name("return_type") {
                    self.type_refs(ret, cx, depth + 1);
                }
                if let Some(body) = node.child_by_field_name("body") {
                    self.expr(body, cx, depth + 1);
                }
                self.locals.pop();
            }
            "match_expression" => {
                if let Some(value) = node.child_by_field_name("value") {
                    self.expr(value, cx, depth + 1);
                }
                if let Some(body) = node.child_by_field_name("body") {
                    let mut cursor = body.walk();
                    let arms: Vec<Node<'t>> = body.named_children(&mut cursor).collect();
                    for arm in arms {
                        self.locals.push(HashMap::new());
                        if let Some(pattern) = arm.child_by_field_name("pattern") {
                            self.match_pattern(pattern, cx, depth + 1);
                        }
                        if let Some(value) = arm.child_by_field_name("value") {
                            self.expr(value, cx, depth + 1);
                        }
                        self.locals.pop();
                    }
                }
            }
            "if_expression" | "while_expression" => {
                self.locals.push(HashMap::new());
                if let Some(condition) = node.child_by_field_name("condition") {
                    self.condition(condition, cx, depth + 1);
                }
                let mut cursor = node.walk();
                let rest: Vec<Node<'t>> = node
                    .named_children(&mut cursor)
                    .filter(|c| {
                        Some(c.id()) != node.child_by_field_name("condition").map(|n| n.id())
                    })
                    .collect();
                for kid in rest {
                    if kid.kind() == "else_clause" {
                        self.locals.pop();
                        self.expr(kid, cx, depth + 1);
                        self.locals.push(HashMap::new());
                    } else {
                        self.expr(kid, cx, depth + 1);
                    }
                }
                self.locals.pop();
            }
            "for_expression" => {
                if let Some(value) = node.child_by_field_name("value") {
                    self.expr(value, cx, depth + 1);
                }
                self.locals.push(HashMap::new());
                if let Some(pattern) = node.child_by_field_name("pattern") {
                    self.pattern(pattern, None, cx, LocalKind::Variable, depth + 1);
                }
                if let Some(body) = node.child_by_field_name("body") {
                    self.expr(body, cx, depth + 1);
                }
                self.locals.pop();
            }
            "let_condition" => self.condition(node, cx, depth),
            "arguments" => self.arguments(node, cx, depth),
            "type_arguments" => self.type_refs(node, cx, depth),
            // Items inside a block.
            "function_item" | "impl_item" | "struct_item" | "enum_item" | "trait_item"
            | "mod_item" | "const_item" | "static_item" | "type_item" | "union_item"
            | "use_declaration" | "macro_definition" => self.item(node, cx, &[], &[], depth + 1),
            _ => {
                let mut cursor = node.walk();
                let kids: Vec<Node<'t>> = node.named_children(&mut cursor).collect();
                for kid in kids {
                    self.expr(kid, cx, depth + 1);
                }
            }
        }
    }

    /// An `if` or `while` condition: `if let P = v` binds in what follows.
    fn condition(&mut self, node: Node<'t>, cx: &Cx, depth: u16) {
        if node.kind() == "let_condition" {
            if let Some(value) = node.child_by_field_name("value") {
                self.expr(value, cx, depth + 1);
            }
            if let Some(pattern) = node.child_by_field_name("pattern") {
                self.pattern(pattern, None, cx, LocalKind::Variable, depth + 1);
            }
            return;
        }
        let mut cursor = node.walk();
        let kids: Vec<Node<'t>> = node.named_children(&mut cursor).collect();
        if kids.iter().any(|k| k.kind() == "let_condition") {
            for kid in kids {
                self.condition(kid, cx, depth + 1);
            }
        } else {
            self.expr(node, cx, depth + 1);
        }
    }

    fn match_pattern(&mut self, node: Node<'t>, cx: &Cx, depth: u16) {
        let mut cursor = node.walk();
        let kids: Vec<Node<'t>> = node.named_children(&mut cursor).collect();
        let condition = node.child_by_field_name("condition").map(|c| c.id());
        for kid in kids {
            if Some(kid.id()) == condition {
                self.expr(kid, cx, depth + 1);
            } else {
                self.pattern(kid, None, cx, LocalKind::Variable, depth + 1);
            }
        }
    }

    /// The arguments of a call: a bare path is a value passed on (a function
    /// handed by name), anything else is read as code.
    fn arguments(&mut self, node: Node<'t>, cx: &Cx, depth: u16) {
        let mut cursor = node.walk();
        let list: Vec<Node<'t>> = node.named_children(&mut cursor).collect();
        for argument in list {
            match argument.kind() {
                "identifier" | "scoped_identifier" => {
                    self.path_ref(argument, cx, UseKind::PassAsValue, depth + 1);
                }
                _ => self.expr(argument, cx, depth + 1),
            }
        }
    }

    fn call(&mut self, node: Node<'t>, cx: &Cx, depth: u16) {
        if let Some(function) = node.child_by_field_name("function") {
            match function.kind() {
                "identifier" | "scoped_identifier" | "generic_function" => {
                    self.path_ref(function, cx, UseKind::Call, depth + 1);
                }
                "field_expression" => self.field(function, cx, UseKind::Call, depth + 1),
                _ => self.expr(function, cx, depth + 1),
            }
        }
        if let Some(arguments) = node.child_by_field_name("arguments") {
            self.arguments(arguments, cx, depth + 1);
        }
    }

    /// `value.field` or `value.method(..)`: the receiver is what the syntax
    /// states of `value`.
    fn field(&mut self, node: Node<'t>, cx: &Cx, kind: UseKind, depth: u16) {
        let (Some(value), Some(field)) = (
            node.child_by_field_name("value"),
            node.child_by_field_name("field"),
        ) else {
            return;
        };
        if field.kind() == "field_identifier" {
            let (receiver, qualifier) = self.receiver(value, cx);
            self.emit(field, cx, receiver, kind, self.ident(field), qualifier);
            // The head of a chain that is no local and no `self` is a name read
            // (`LIMIT.len()`, `COUNTER.load()`).
            if let (Some(chain), Some(head)) = (self.field_chain(value), self.chain_head(value)) {
                if chain[0] != "self" && self.lookup(&chain[0]).is_none() {
                    self.emit(
                        head,
                        cx,
                        Receiver::None,
                        UseKind::Read,
                        &chain[0],
                        Vec::new(),
                    );
                }
            }
        }
        // The value is read as code when it is more than a path of fields.
        if self.field_chain(value).is_none() {
            self.expr(value, cx, depth + 1);
        }
    }

    /// The leftmost identifier of a chain of fields.
    fn chain_head(&self, node: Node<'t>) -> Option<Node<'t>> {
        let mut at = node;
        while at.kind() == "field_expression" {
            at = at.child_by_field_name("value")?;
        }
        (at.kind() == "identifier").then_some(at)
    }

    /// `a.b.c` as `[a, b, c]` when it is made of identifiers and `self` only.
    fn field_chain(&self, node: Node<'t>) -> Option<Vec<String>> {
        match node.kind() {
            "identifier" | "self" => Some(vec![self.ident(node).to_string()]),
            "field_expression" => {
                let field = node.child_by_field_name("field")?;
                if field.kind() != "field_identifier" && field.kind() != "integer_literal" {
                    return None;
                }
                let mut head = self.field_chain(node.child_by_field_name("value")?)?;
                head.push(self.ident(field).to_string());
                Some(head)
            }
            _ => None,
        }
    }

    fn receiver(&self, value: Node<'t>, cx: &Cx) -> (Receiver, Vec<Name>) {
        if let Some(chain) = self.field_chain(value) {
            let head = chain[0].as_str();
            let rest: Vec<Name> = chain[1..].iter().map(|s| Name::new(s.as_str())).collect();
            if head == "self" {
                return (Receiver::SelfType, rest);
            }
            if let Some(known) = self.lookup(head) {
                return match known {
                    Some(Ty::Named(t)) => (Receiver::Typed(TypeName::new(t.as_str())), rest),
                    Some(Ty::Result { path, unwrapped }) => (call_result(path, *unwrapped), rest),
                    None => (
                        Receiver::Unknown,
                        chain.iter().map(|s| Name::new(s.as_str())).collect(),
                    ),
                };
            }
            return (
                Receiver::Unknown,
                chain.iter().map(|s| Name::new(s.as_str())).collect(),
            );
        }
        match self.built_ty(value, cx) {
            Some(Ty::Named(t)) => (Receiver::Typed(TypeName::new(t.as_str())), Vec::new()),
            Some(Ty::Result { path, unwrapped }) => (call_result(&path, unwrapped), Vec::new()),
            None => (Receiver::Unknown, Vec::new()),
        }
    }

    /// A path in expression or type position: the last segment is the
    /// reference, the ones before it its qualifier. A bare name that is a local
    /// is no reference.
    fn path_ref(&mut self, node: Node<'t>, cx: &Cx, kind: UseKind, depth: u16) {
        if !self.deeper(depth) {
            return;
        }
        // Generic arguments are uses of their own.
        let mut head = node;
        loop {
            match head.kind() {
                "generic_function" => {
                    if let Some(args) = head.child_by_field_name("type_arguments") {
                        self.type_refs(args, cx, depth + 1);
                    }
                    match head.child_by_field_name("function") {
                        Some(inner) => head = inner,
                        None => return,
                    }
                }
                "generic_type" | "generic_type_with_turbofish" => {
                    if let Some(args) = head.child_by_field_name("type_arguments") {
                        self.type_refs(args, cx, depth + 1);
                    }
                    match head.child_by_field_name("type") {
                        Some(inner) => head = inner,
                        None => return,
                    }
                }
                _ => break,
            }
        }
        // `<T as Tr>::f` and `<T>::f` call through the type `T`.
        if head.kind() == "scoped_identifier" {
            if let (Some(bracket), Some(name)) = (
                head.child_by_field_name("path")
                    .filter(|p| p.kind() == "bracketed_type"),
                head.child_by_field_name("name"),
            ) {
                let inner = bracket.named_child(0).map(|q| match q.kind() {
                    "qualified_type" => q.child_by_field_name("type").unwrap_or(q),
                    _ => q,
                });
                if let Some(Ty::Named(t)) = inner.and_then(|t| self.ty_of(t, cx)) {
                    self.emit(
                        name,
                        cx,
                        Receiver::Typed(TypeName::new(t.as_str())),
                        kind,
                        self.ident(name),
                        Vec::new(),
                    );
                }
                return;
            }
        }
        let Some(mut path) = path_of(self.bytes, head) else {
            return;
        };
        let Some(name) = path.pop() else { return };
        if path.is_empty() {
            if matches!(name.as_str(), "self" | "Self" | "super" | "crate") {
                return;
            }
            if cx.generics.contains_key(&name) {
                return;
            }
            if kind != UseKind::TypeUse && self.lookup(&name).is_some() {
                return;
            }
        }
        let site = match head.kind() {
            "scoped_identifier" | "scoped_type_identifier" => {
                head.child_by_field_name("name").unwrap_or(head)
            }
            _ => head,
        };
        let name_text = name.clone();
        self.emit(
            site,
            cx,
            Receiver::None,
            kind,
            &name_text,
            path.into_iter().map(Name::new).collect(),
        );
    }

    // ---- macros

    fn macro_call(&mut self, node: Node<'t>, cx: &Cx, depth: u16) {
        if let Some(name) = node.child_by_field_name("macro") {
            if let Some(mut path) = path_of(self.bytes, name) {
                if let Some(last) = path.pop() {
                    let site = match name.kind() {
                        "scoped_identifier" => name.child_by_field_name("name").unwrap_or(name),
                        _ => name,
                    };
                    let called = format!("{last}!");
                    self.emit(
                        site,
                        cx,
                        Receiver::None,
                        UseKind::Call,
                        &called,
                        path.into_iter().map(Name::new).collect(),
                    );
                }
            }
        }
        let mut cursor = node.walk();
        let trees: Vec<Node<'t>> = node
            .named_children(&mut cursor)
            .filter(|c| c.kind() == "token_tree")
            .collect();
        for tree in trees {
            self.tokens(tree, cx, depth + 1);
        }
    }

    /// A macro's tokens read as the code they usually expand to: `a::b(..)`,
    /// `x.m(..)` and `name!`.
    fn tokens(&mut self, tree: Node<'t>, cx: &Cx, depth: u16) {
        if !self.deeper(depth) {
            return;
        }
        let mut cursor = tree.walk();
        let toks: Vec<Node<'t>> = tree.children(&mut cursor).collect();
        let ident = |n: Node<'t>| {
            matches!(
                n.kind(),
                "identifier" | "self" | "type_identifier" | "crate" | "super"
            )
        };
        let mut i = 0;
        while i < toks.len() {
            let tok = toks[i];
            if tok.kind() == "token_tree" {
                self.tokens(tok, cx, depth + 1);
                i += 1;
                continue;
            }
            if !ident(tok) {
                i += 1;
                continue;
            }
            // A run `a::b::c`.
            let mut run = vec![tok];
            let mut j = i;
            while toks.get(j + 1).is_some_and(|t| t.kind() == "::")
                && toks.get(j + 2).is_some_and(|t| ident(*t))
            {
                run.push(toks[j + 2]);
                j += 2;
            }
            let end = j;
            let next = toks.get(end + 1);
            let call = next.is_some_and(|n| {
                n.kind() == "token_tree" && n.child(0).is_some_and(|c| c.kind() == "(")
            });
            let names: Vec<String> = run.iter().map(|n| self.ident(*n).to_string()).collect();
            let previous = i.checked_sub(1).and_then(|p| toks.get(p));
            let last = run[run.len() - 1];
            if next.is_some_and(|n| n.kind() == "!") {
                let called = format!("{}!", names[names.len() - 1]);
                let qualifier = names[..names.len() - 1]
                    .iter()
                    .map(|n| Name::new(n.as_str()))
                    .collect();
                self.emit(last, cx, Receiver::None, UseKind::Call, &called, qualifier);
            } else if previous.is_some_and(|p| p.kind() == ".") && run.len() == 1 {
                if call {
                    let before = i.checked_sub(2).and_then(|p| toks.get(p)).copied();
                    let (receiver, qualifier) = match before {
                        Some(b) if b.kind() == "self" => (Receiver::SelfType, Vec::new()),
                        Some(b) if b.kind() == "identifier" => {
                            let local = self.ident(b).to_string();
                            match self.lookup(&local).cloned() {
                                Some(Some(Ty::Named(t))) => {
                                    (Receiver::Typed(TypeName::new(t.as_str())), Vec::new())
                                }
                                _ => (Receiver::Unknown, vec![Name::new(local)]),
                            }
                        }
                        _ => (Receiver::Unknown, Vec::new()),
                    };
                    self.emit(last, cx, receiver, UseKind::Call, &names[0], qualifier);
                }
            } else if previous.is_none_or(|p| p.kind() != "::" && p.kind() != ".") {
                let keyword = |n: &str| matches!(n, "self" | "Self" | "super" | "crate");
                let name = names[names.len() - 1].clone();
                if !keyword(&name) && !(run.len() == 1 && self.lookup(&name).is_some()) {
                    let qualifier = names[..names.len() - 1]
                        .iter()
                        .map(|n| Name::new(n.as_str()))
                        .collect();
                    let kind = if call { UseKind::Call } else { UseKind::Read };
                    self.emit(last, cx, Receiver::None, kind, &name, qualifier);
                }
            }
            i = end + 1;
        }
    }
}

fn call_result(path: &[String], unwrapped: bool) -> Receiver {
    let mut names: Vec<Name> = path.iter().map(|s| Name::new(s.as_str())).collect();
    if unwrapped {
        names.push(Name::new("?"));
    }
    Receiver::CallResult(names)
}

enum Leaf {
    Glob(Vec<String>),
    Name {
        path: Vec<String>,
        alias: Option<String>,
    },
}

/// What a definition is: the kind it takes and the namespace it lives in.
#[derive(Clone, Copy)]
struct Shape {
    kind: DefKind,
    ns: Namespace,
}

impl Shape {
    fn of(_node: Node<'_>, kind: DefKind) -> Self {
        let ns = match kind {
            DefKind::Macro => Namespace::Macro,
            DefKind::Struct
            | DefKind::Enum
            | DefKind::Trait
            | DefKind::TypeAlias
            | DefKind::Module
            | DefKind::Class
            | DefKind::Interface => Namespace::Type,
            _ => Namespace::Value,
        };
        Self { kind, ns }
    }
}

/// The children of an item that are definitions of their own.
fn members_of<'t>(node: Node<'t>) -> Vec<Node<'t>> {
    let Some(body) = node.child_by_field_name("body") else {
        return Vec::new();
    };
    let take = |kinds: &[&str]| -> Vec<Node<'t>> {
        let mut cursor = body.walk();
        body.named_children(&mut cursor)
            .filter(|c| kinds.contains(&c.kind()))
            .collect()
    };
    match node.kind() {
        "struct_item" | "union_item" => take(&["field_declaration"]),
        "enum_item" => take(&["enum_variant"]),
        "trait_item" => take(&[
            "function_item",
            "function_signature_item",
            "associated_type",
            "const_item",
        ]),
        "mod_item" => {
            let mut cursor = body.walk();
            body.named_children(&mut cursor)
                .filter(|c| !c.kind().ends_with("comment") && c.kind() != "attribute_item")
                .collect()
        }
        _ => Vec::new(),
    }
}

/// The identifier a raw identifier writes: `r#match` is `match`, and `r#name`
/// is `name`, one name to the language wherever the word is no keyword.
fn unraw(written: &str) -> &str {
    written.strip_prefix("r#").unwrap_or(written)
}

/// A path as the segments written: `a::b::C`, `Vec::<T>` is `Vec`. `$crate` is
/// `crate`.
fn path_of(bytes: &[u8], node: Node<'_>) -> Option<Vec<String>> {
    let text = |n: Node<'_>| {
        unraw(std::str::from_utf8(&bytes[n.start_byte()..n.end_byte()]).unwrap_or("")).to_string()
    };
    match node.kind() {
        "identifier" | "type_identifier" | "crate" | "self" | "super" => Some(vec![text(node)]),
        "metavariable" if text(node) == "$crate" => Some(vec!["crate".to_string()]),
        "scoped_identifier" | "scoped_type_identifier" => {
            let mut path = match node.child_by_field_name("path") {
                Some(p) => path_of(bytes, p)?,
                None => Vec::new(),
            };
            path.extend(path_of(bytes, node.child_by_field_name("name")?)?);
            Some(path)
        }
        "generic_type" | "generic_type_with_turbofish" => {
            path_of(bytes, node.child_by_field_name("type")?)
        }
        _ => None,
    }
}

/// The path before a `*` or a list: empty for `use *;` and `use {a, b};`, and
/// none when it is a form `path_of` does not read, which leaves the `use` out.
fn use_prefix(bytes: &[u8], node: Option<Node<'_>>) -> Option<Vec<String>> {
    node.map_or_else(|| Some(Vec::new()), |n| path_of(bytes, n))
}

fn path_names(text: &str) -> Vec<String> {
    text.split("::").map(str::to_string).collect()
}

/// The head of an attribute: `#[tokio::test(flavor = "x")]` is `tokio::test`.
fn attribute_name(attribute: &str) -> Option<&str> {
    let inner = attribute.trim().strip_prefix("#[")?.strip_suffix(']')?;
    let head = inner.split(['(', '=']).next()?.trim();
    (!head.is_empty()).then_some(head)
}

/// `#[test]`, `#[tokio::test]`, `#[rstest]`, `#[test_case(..)]`.
fn is_test_attribute(attribute: &str) -> bool {
    attribute_name(attribute).is_some_and(|head| {
        let last = head.rsplit("::").next().unwrap_or(head);
        matches!(
            last,
            "test" | "rstest" | "test_case" | "quickcheck" | "proptest"
        )
    })
}
