//! What a Terraform file's syntax states, as the facts contract names it.
//!
//! Conventions a reader of the facts cannot recover from the types:
//! - A module is a directory: the file's module is the workspace's (`facts_of`)
//!   for it, and every address of the module is a definition whose container
//!   is the address without its last segment, so a definition's display is its
//!   Terraform address: `var.region`, `local.tags`, `output.id`, `module.net`,
//!   `aws_s3_bucket.logs`, `data.aws_ami.base`.
//! - A `variable` and an `output` are `Variable` and `Public`: they are the
//!   module's interface. A `locals` entry is a `Constant`; a `resource`, a
//!   `data` source (its container is `data.<type>`) and a `module` call are
//!   `Variable`s, a name that holds an object; all `Private`. A display of
//!   the definition is then always its address.
//! - A signature is the declaration that is not its body: `variable region type
//!   string`, `module net source ./net version 1.2`, `resource aws_s3_bucket
//!   logs`. A change of a variable's type or of a module's source is a change of
//!   signature; a changed default, expression or argument is a change of body.
//! - A traversal that starts in a name the module defines is a `Read`:
//!   `var.x` is `x` with qualifier `[var]`, `local.x` `[local]`, `data.t.n` is
//!   `n` with `[data, t]`, `module.m.o` is `o` with `[module, m]` (`module.m`
//!   alone is `m` with `[module]`), and `t.n` of a resource is `n` with `[t]`.
//!   `path`, `terraform`, `count`, `each`, `self` and the names a `for`
//!   expression or a `dynamic` block binds are no reference.
//! - An argument of a `module` call other than its meta-arguments sets a
//!   variable of the called module: a `Write` of `name` with `[module, m]`,
//!   owned by the call. A top-level assignment of a variable file (`x = 1` in a
//!   `.tfvars`) is a `Write` of `x` with `[var]`.
//! - A key of an object constructor that is a bare name is a string, not a
//!   reference.

use std::collections::HashMap;

use tree_sitter::Node;

use super::{children, lines_of, parse_tree, span};
use crate::facts::{
    BlobFacts, Def, DefKind, Disambig, Extractor, Fact, Hasher, Language, LocalKey, Name,
    Namespace, Receiver, Ref, Scope, Span, UseKind, Visibility, expired, hash_text,
};
use crate::snapshot::{Loss, RepoPath, RunBudget};

const MAX_DEPTH: u16 = 256;
const ERROR_RANGES_KEPT: usize = 32;
const POLL_EVERY: usize = 256;

/// The arguments of a `module` call that configure the call, not the module.
const MODULE_META: &[&str] = &[
    "source",
    "version",
    "count",
    "for_each",
    "depends_on",
    "providers",
];

/// Roots of a traversal that name no definition of the module.
const BUILTIN_ROOTS: &[&str] = &["path", "terraform", "count", "each", "self"];

pub struct TerraformExtractor;

impl Extractor for TerraformExtractor {
    fn language(&self) -> Language {
        Language::Terraform
    }

    fn extract(&self, text: &[u8], budget: &RunBudget) -> BlobFacts {
        extract(text, budget)
    }
}

fn unbound() -> RepoPath {
    RepoPath::new("<blob>").expect("a valid path")
}

fn extract(text: &[u8], budget: &RunBudget) -> BlobFacts {
    let mut blob = BlobFacts::new(Language::Terraform);
    let Some(tree) = parse_tree(Language::Terraform, text, budget) else {
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

/// A construct the walk reads, wherever the grammar put it.
fn is_whole(kind: &str) -> bool {
    matches!(kind, "block" | "attribute" | "comment")
}

/// The line ranges the parse could not read, merged. An error node among a
/// file's items is read by the walk (`through_errors`): the blocks and
/// attributes it holds whole are left out, and its stray tokens, with
/// whatever else sits among the items, are what stays unread. An error node
/// inside a block is unread whole.
fn error_lines(root: Node) -> Vec<(u32, u32)> {
    let mut ranges = Vec::new();
    let mut stack = vec![(root, true)];
    while let Some((node, among_items)) = stack.pop() {
        let mut cursor = node.walk();
        if node.is_error() && among_items {
            if node.child_count() == 0 {
                ranges.push(lines_of(node));
            }
            for child in node.children(&mut cursor) {
                if child.is_error() {
                    stack.push((child, true));
                } else if child.is_missing() || !is_whole(child.kind()) {
                    ranges.push(lines_of(child));
                } else if child.has_error() {
                    stack.push((child, false));
                }
            }
        } else if node.is_error() || node.is_missing() {
            ranges.push(lines_of(node));
        } else if node.has_error() {
            // A file holds bodies and a body holds blocks and attributes: what
            // else sits there was not read.
            let level = among_items && matches!(node.kind(), "config_file" | "body");
            for child in node.children(&mut cursor) {
                let stray = level
                    && child.is_named()
                    && !child.is_error()
                    && !child.is_missing()
                    && child.kind() != "body"
                    && !is_whole(child.kind());
                if stray {
                    ranges.push(lines_of(child));
                } else {
                    stack.push((child, level));
                }
            }
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

/// The named children of a file or body in source order, those the grammar
/// parked under an error node included: a stray token leaves the blocks
/// around it whole inside the error, and their definitions and uses are
/// still the file's.
fn through_errors<'t>(parent: Node<'t>) -> Vec<Node<'t>> {
    let mut out = Vec::new();
    let mut stack: Vec<Node<'t>> = children(parent).into_iter().rev().collect();
    while let Some(node) = stack.pop() {
        if node.is_error() {
            stack.extend(children(node).into_iter().rev());
        } else {
            out.push(node);
        }
    }
    out
}

/// What a walk of an expression carries: who owns the references and which names
/// the expressions around it bind.
#[derive(Clone)]
struct Cx {
    scope: Scope,
    bound: Vec<String>,
}

struct Walker<'t> {
    bytes: &'t [u8],
    budget: &'t RunBudget,
    ticks: usize,
    halted: bool,
    too_deep: bool,
    facts: Vec<Fact>,
    /// How many definitions each key has had, for twins in different files.
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

    walker_methods!();
    walker_depth!();

    // ---- the file

    fn run(&mut self, root: Node<'t>) {
        let module = Cx {
            scope: Scope::Module,
            bound: Vec::new(),
        };
        for part in through_errors(root) {
            let items = if part.kind() == "body" {
                through_errors(part)
            } else {
                vec![part]
            };
            for item in items {
                if !self.deeper(1) {
                    return;
                }
                match item.kind() {
                    "block" => self.block(item),
                    // `x = 1` outside any block is a variable file's assignment.
                    "attribute" => self.assignment(item, &module),
                    _ => {}
                }
            }
        }
    }

    /// `x = value` of a variable file: a value given to the variable `x`.
    fn assignment(&mut self, attribute: Node<'t>, cx: &Cx) {
        let Some(name) = children(attribute)
            .into_iter()
            .find(|n| n.kind() == "identifier")
        else {
            return;
        };
        self.facts.push(Fact::Reference(Ref {
            site: span(name),
            enclosing: cx.scope.clone(),
            receiver: Receiver::None,
            use_kind: UseKind::Write,
            name: Name::new(self.text(name)),
            qualifier: vec![Name::new("var")],
        }));
        if let Some(value) = Self::value_of(attribute) {
            self.walk(value, cx, 2);
        }
    }

    // ---- blocks

    /// A block's type, its labels as written, and its body.
    fn shape(
        &self,
        block: Node<'t>,
    ) -> Option<(&'t str, Vec<(Node<'t>, String)>, Option<Node<'t>>)> {
        let mut kids = children(block).into_iter();
        let kind = kids.next().filter(|n| n.kind() == "identifier")?;
        let mut labels = Vec::new();
        let mut body = None;
        for child in kids {
            match child.kind() {
                // The name is the text inside the quotes, where a scan finds it.
                "string_lit" => {
                    let inner = children(child)
                        .into_iter()
                        .find(|n| n.kind() == "template_literal");
                    labels.push((inner.unwrap_or(child), self.string_text(child)));
                }
                "identifier" => labels.push((child, self.text(child).to_string())),
                "body" => body = Some(child),
                _ => {}
            }
        }
        Some((self.text(kind), labels, body))
    }

    /// The text of a string literal, without its quotes.
    fn string_text(&self, node: Node<'t>) -> String {
        children(node)
            .into_iter()
            .filter(|n| n.kind() == "template_literal")
            .map(|n| self.text(n))
            .collect()
    }

    fn block(&mut self, block: Node<'t>) {
        let Some((kind, labels, body)) = self.shape(block) else {
            return;
        };
        match (kind, labels.as_slice()) {
            ("variable", [(name_node, name)]) => {
                let mut signature = format!("variable {name}");
                if let Some(ty) = self.attribute(body, "type") {
                    signature.push_str(" type ");
                    signature.push_str(&self.tokens(ty));
                }
                let key = self.define(
                    block,
                    *name_node,
                    &["var"],
                    name,
                    DefKind::Variable,
                    body,
                    signature,
                    Visibility::Public,
                );
                self.walk_body(body, &Cx::of(key), 1);
            }
            ("output", [(name_node, name)]) => {
                let key = self.define(
                    block,
                    *name_node,
                    &["output"],
                    name,
                    DefKind::Variable,
                    body,
                    format!("output {name}"),
                    Visibility::Public,
                );
                self.walk_body(body, &Cx::of(key), 1);
            }
            ("locals", []) => {
                for entry in body.into_iter().flat_map(children) {
                    if entry.kind() != "attribute" {
                        continue;
                    }
                    let Some(name_node) = children(entry)
                        .into_iter()
                        .find(|n| n.kind() == "identifier")
                    else {
                        continue;
                    };
                    let name = self.text(name_node).to_string();
                    let value = Self::value_of(entry);
                    let key = self.define(
                        entry,
                        name_node,
                        &["local"],
                        &name,
                        DefKind::Constant,
                        value,
                        format!("local {name}"),
                        Visibility::Private,
                    );
                    if let Some(value) = value {
                        self.walk(value, &Cx::of(key), 2);
                    }
                }
            }
            ("resource", [(_, ty), (name_node, name)]) => {
                let key = self.define(
                    block,
                    *name_node,
                    &[ty.as_str()],
                    name,
                    DefKind::Variable,
                    body,
                    format!("resource {ty} {name}"),
                    Visibility::Private,
                );
                self.walk_body(body, &Cx::of(key), 1);
            }
            ("data", [(_, ty), (name_node, name)]) => {
                let key = self.define(
                    block,
                    *name_node,
                    &[format!("data.{ty}").as_str()],
                    name,
                    DefKind::Variable,
                    body,
                    format!("data {ty} {name}"),
                    Visibility::Private,
                );
                self.walk_body(body, &Cx::of(key), 1);
            }
            ("module", [(name_node, name)]) => self.module(block, *name_node, name, body),
            // `provider`, `terraform`, `moved`, `import`, `check` and the rest
            // define nothing the module's own code names; what they read is the
            // module's.
            _ => self.walk_body(
                body,
                &Cx {
                    scope: Scope::Module,
                    bound: Vec::new(),
                },
                1,
            ),
        }
    }

    /// A `module` call: a definition that states where the module is, and a
    /// `Write` of each argument the called module could take as a variable.
    fn module(&mut self, block: Node<'t>, name_node: Node<'t>, name: &str, body: Option<Node<'t>>) {
        let mut signature = format!("module {name}");
        for meta in ["source", "version"] {
            if let Some(expression) = self.attribute(body, meta) {
                signature.push(' ');
                signature.push_str(meta);
                signature.push(' ');
                match self.literal(expression) {
                    Some(text) => signature.push_str(&text),
                    None => signature.push_str("<computed>"),
                }
            }
        }
        let key = self.define(
            block,
            name_node,
            &["module"],
            name,
            DefKind::Variable,
            body,
            signature,
            Visibility::Private,
        );
        let cx = Cx::of(key);
        for entry in body.into_iter().flat_map(children) {
            if entry.kind() != "attribute" {
                self.walk(entry, &cx, 1);
                continue;
            }
            let Some(argument) = children(entry)
                .into_iter()
                .find(|n| n.kind() == "identifier")
            else {
                continue;
            };
            let written = self.text(argument);
            if !MODULE_META.contains(&written) {
                self.facts.push(Fact::Reference(Ref {
                    site: span(argument),
                    enclosing: cx.scope.clone(),
                    receiver: Receiver::None,
                    use_kind: UseKind::Write,
                    name: Name::new(written),
                    qualifier: vec![Name::new("module"), Name::new(name)],
                }));
            }
            if let Some(value) = Self::value_of(entry) {
                self.walk(value, &cx, 2);
            }
        }
    }

    // ---- definitions

    fn key_for(&mut self, container: &[&str], name: &str) -> LocalKey {
        let key = LocalKey::new(
            container.iter().map(|c| Name::new(*c)).collect(),
            name,
            Namespace::Value,
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

    /// Emits one definition; `body` is what its body hash reads.
    #[allow(clippy::too_many_arguments)]
    fn define(
        &mut self,
        span_node: Node<'t>,
        name_node: Node<'t>,
        container: &[&str],
        name: &str,
        kind: DefKind,
        body: Option<Node<'t>>,
        signature: String,
        visibility: Visibility,
    ) -> LocalKey {
        let key = self.key_for(container, name);
        let mut body_hash = Hasher::new();
        body_hash.bytes(&[0]);
        if let Some(body) = body {
            self.hash_tree(body, &mut body_hash);
        }
        self.facts.push(Fact::Definition(Def {
            local: key.clone(),
            kind,
            span: span(span_node),
            name_span: span(name_node),
            signature: Some(signature),
            decorators: Vec::new(),
            visibility,
            body_hash: body_hash.finish().unwrap_or(0),
            text_hash: hash_text(
                self.bytes[span_node.start_byte()..span_node.end_byte()].trim_ascii(),
            ),
            doc_hash: None,
        }));
        key
    }

    /// The tree as node kinds, nesting and leaf text, comments left out.
    fn hash_tree(&self, root: Node<'t>, hash: &mut Hasher) {
        let mut stack = vec![(root, false)];
        while let Some((node, closing)) = stack.pop() {
            if closing {
                hash.bytes(&[2]);
                continue;
            }
            if node.kind() == "comment" {
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
    }

    /// The leaves of a node, a space between every two.
    fn tokens(&self, node: Node<'t>) -> String {
        let mut tokens: Vec<&'t str> = Vec::new();
        let mut stack = vec![node];
        while let Some(n) = stack.pop() {
            if n.kind() == "comment" {
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
        tokens.join(" ")
    }

    // ---- reading a body

    /// The expression of the attribute `name` of a body.
    fn attribute(&self, body: Option<Node<'t>>, name: &str) -> Option<Node<'t>> {
        body.into_iter().flat_map(children).find_map(|item| {
            let mut kids = children(item).into_iter();
            let first = kids.next()?;
            (item.kind() == "attribute" && first.kind() == "identifier" && self.text(first) == name)
                .then(|| kids.find(|n| n.kind() == "expression"))
                .flatten()
        })
    }

    /// The expression of an attribute node.
    fn value_of(attribute: Node<'t>) -> Option<Node<'t>> {
        children(attribute)
            .into_iter()
            .find(|n| n.kind() == "expression")
    }

    /// A string written without interpolation or directive, else `None`.
    fn literal(&self, expression: Node<'t>) -> Option<String> {
        let mut text = String::new();
        let mut stack = vec![expression];
        let mut strings = 0;
        while let Some(node) = stack.pop() {
            match node.kind() {
                "template_interpolation" | "template_directive" | "heredoc_template" => {
                    return None;
                }
                "string_lit" => {
                    strings += 1;
                    text.push_str(&self.string_text(node));
                }
                "expression" | "literal_value" | "template_expr" | "quoted_template" => {
                    stack.extend(children(node).into_iter().rev());
                }
                "template_literal" => text.push_str(self.text(node)),
                _ => return None,
            }
        }
        (strings > 0 || !text.is_empty()).then_some(text)
    }

    fn walk_body(&mut self, body: Option<Node<'t>>, cx: &Cx, depth: u16) {
        for item in body.into_iter().flat_map(children) {
            self.walk(item, cx, depth);
        }
    }

    // ---- expressions

    fn walk(&mut self, node: Node<'t>, cx: &Cx, depth: u16) {
        if !self.deeper(depth) {
            return;
        }
        match node.kind() {
            "comment" | "template_literal" | "numeric_lit" | "bool_lit" | "null_lit" => {}
            // A nested block, `dynamic` among them, reads like its parent.
            "block" => self.nested(node, cx, depth),
            "attribute" => {
                if let Some(value) = Self::value_of(node) {
                    self.walk(value, cx, depth + 1);
                }
            }
            "for_tuple_expr" | "for_object_expr" => {
                let mut inner = cx.clone();
                for intro in children(node)
                    .into_iter()
                    .filter(|n| n.kind() == "for_intro")
                {
                    for name in children(intro)
                        .into_iter()
                        .filter(|n| n.kind() == "identifier")
                    {
                        inner.bound.push(self.text(name).to_string());
                    }
                }
                self.sequence(node, &inner, depth);
            }
            "object_elem" => self.object_elem(node, cx, depth),
            _ => self.sequence(node, cx, depth),
        }
    }

    /// A block inside a block: `dynamic "x"` binds the name of its label (or its
    /// `iterator`) for what it contains.
    fn nested(&mut self, block: Node<'t>, cx: &Cx, depth: u16) {
        let Some((kind, labels, body)) = self.shape(block) else {
            return;
        };
        let mut inner = cx.clone();
        if kind == "dynamic" {
            if let Some((_, label)) = labels.first() {
                inner.bound.push(label.clone());
            }
            if let Some(iterator) = self.attribute(body, "iterator") {
                let name = self.tokens(iterator);
                inner.bound.push(name);
            }
        }
        self.walk_body(body, &inner, depth + 1);
    }

    /// `key = value` of an object constructor: a bare name as key is a string.
    fn object_elem(&mut self, elem: Node<'t>, cx: &Cx, depth: u16) {
        let mut parts = children(elem).into_iter();
        let (Some(key), Some(value)) = (parts.next(), parts.next()) else {
            for part in children(elem) {
                self.walk(part, cx, depth + 1);
            }
            return;
        };
        let bare = {
            let inner = children(key);
            key.kind() == "expression" && inner.len() == 1 && inner[0].kind() == "variable_expr"
        };
        if !bare {
            self.walk(key, cx, depth + 1);
        }
        self.walk(value, cx, depth + 1);
    }

    /// The children of a node in order, reading each run that starts at a name
    /// and goes on in attributes and indexes as one traversal.
    fn sequence(&mut self, node: Node<'t>, cx: &Cx, depth: u16) {
        let kids = children(node);
        let mut at = 0;
        while at < kids.len() {
            let child = kids[at];
            at += 1;
            if child.kind() != "variable_expr" {
                self.walk(child, cx, depth + 1);
                continue;
            }
            let from = at;
            while at < kids.len() && matches!(kids[at].kind(), "get_attr" | "index" | "splat") {
                at += 1;
            }
            let rest = &kids[from..at];
            self.traversal(child, rest, cx);
            // What an index or a splat holds is read as well: `a[var.k]`.
            for held in rest.iter().filter(|n| n.kind() != "get_attr") {
                self.walk(*held, cx, depth + 1);
            }
        }
    }

    fn traversal(&mut self, root: Node<'t>, rest: &[Node<'t>], cx: &Cx) {
        let root_name = self.text(root);
        if cx.bound.iter().any(|name| name == root_name) || BUILTIN_ROOTS.contains(&root_name) {
            return;
        }
        let mut attributes: Vec<Node<'t>> = Vec::new();
        for part in rest {
            match part.kind() {
                "get_attr" => {
                    if let Some(name) = children(*part)
                        .into_iter()
                        .find(|n| n.kind() == "identifier")
                    {
                        attributes.push(name);
                    }
                }
                "index" => {}
                _ => break,
            }
        }
        let name_at = |node: Node<'t>| Name::new(self.text(node));
        let (qualifier, name) = match (root_name, attributes.as_slice()) {
            ("var" | "local", [name, ..]) => (vec![Name::new(root_name)], *name),
            ("module", [call, output, ..]) => (vec![Name::new("module"), name_at(*call)], *output),
            ("module", [call]) => (vec![Name::new("module")], *call),
            ("data", [ty, name, ..]) => (vec![Name::new("data"), name_at(*ty)], *name),
            ("var" | "local" | "module" | "data", _) => return,
            (ty, [name, ..]) => (vec![Name::new(ty)], *name),
            _ => return,
        };
        let written = name_at(name);
        self.facts.push(Fact::Reference(Ref {
            site: Span {
                bytes: root.start_byte() as u32..name.end_byte() as u32,
                lines: (lines_of(root).0, lines_of(name).1),
            },
            enclosing: cx.scope.clone(),
            receiver: Receiver::None,
            use_kind: UseKind::Read,
            name: written,
            qualifier,
        }));
    }
}

impl Cx {
    fn of(key: LocalKey) -> Self {
        Self {
            scope: Scope::Def(key),
            bound: Vec::new(),
        }
    }
}
