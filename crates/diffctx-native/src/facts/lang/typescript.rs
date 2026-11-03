//! What a TypeScript or JavaScript file's syntax states: its definitions,
//! imports and exports, and every reference with the scope that owns it.
//! Facts never resolve a binding; a specifier is kept as written.
//!
//! Two passes over one tree. The declaring pass reads the statements of the
//! module (and of each namespace) and decides what is a definition; the
//! reading pass then walks everything with that answer in hand, so a
//! reference knows its owner and a parameter or a local is never taken for a
//! definition (#592, #706). Closures are transparent: a reference inside one
//! belongs to the nearest definition, wrapped in [`Scope::Callback`] when the
//! closure is an argument of a call.

use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};
use tree_sitter::{Language as Grammar, Node, ParseOptions, ParseState, Parser, Tree};

use crate::facts::{
    BlobFacts, Def, DefKind, Disambig, Export, Extractor, Fact, Import, ImportKind, Language,
    Local, LocalKey, LocalKind, ModulePath, Name, Namespace, Receiver, Ref, Scope, Span, TypeName,
    UseKind, Visibility, expired, hash_text, hash_tokens,
};
use crate::snapshot::{Loss, RepoPath, RunBudget};

const VERSION: u32 = 1;
/// A blob names no path: the path of a loss is bound by whoever holds the file.
const UNBOUND_PATH: &str = "<blob>";
const POLL_EVERY: u32 = 256;
const MAX_ERROR_RANGES: usize = 64;

const OPEN: &str = "\u{1}";
const CLOSE: &str = "\u{2}";

/// Words in front of a declaration that change what it is: a signature
/// carries them, a visibility does not.
const MODIFIERS: &[&str] = &[
    "static", "async", "abstract", "readonly", "get", "set", "declare", "override", "?", "*",
    "accessor",
];

pub struct TypeScript;
pub struct JavaScript;

/// One extractor per language this file reads.
pub fn extractors() -> Vec<Arc<dyn Extractor>> {
    vec![Arc::new(TypeScript), Arc::new(JavaScript)]
}

impl Extractor for TypeScript {
    fn language(&self) -> Language {
        Language::TypeScript
    }

    fn version(&self) -> u32 {
        VERSION
    }

    fn extract(&self, text: &[u8], budget: &RunBudget) -> BlobFacts {
        extract(Dialect::TypeScript, text, budget)
    }
}

impl Extractor for JavaScript {
    fn language(&self) -> Language {
        Language::JavaScript
    }

    fn version(&self) -> u32 {
        VERSION
    }

    fn extract(&self, text: &[u8], budget: &RunBudget) -> BlobFacts {
        extract(Dialect::JavaScript, text, budget)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Dialect {
    TypeScript,
    JavaScript,
}

#[derive(Clone, Copy)]
enum Syntax {
    TypeScript,
    Tsx,
    JavaScript,
}

enum Parsed {
    Tree(Tree),
    Missing,
    Aborted,
}

fn grammar(syntax: Syntax) -> Option<Grammar> {
    match syntax {
        #[cfg(feature = "tree-sitter-typescript")]
        Syntax::TypeScript => Some(Grammar::new(tree_sitter_typescript::LANGUAGE_TYPESCRIPT)),
        #[cfg(feature = "tree-sitter-typescript")]
        Syntax::Tsx => Some(Grammar::new(tree_sitter_typescript::LANGUAGE_TSX)),
        #[cfg(feature = "tree-sitter-javascript")]
        Syntax::JavaScript => Some(Grammar::new(tree_sitter_javascript::LANGUAGE)),
        #[allow(unreachable_patterns)]
        _ => None,
    }
}

fn parse(syntax: Syntax, source: &str, budget: &RunBudget) -> Parsed {
    let Some(grammar) = grammar(syntax) else {
        return Parsed::Missing;
    };
    let mut parser = Parser::new();
    if parser.set_language(&grammar).is_err() {
        return Parsed::Missing;
    }
    let bytes = source.as_bytes();
    let mut progress = |_state: &ParseState| -> bool { expired(budget) };
    let tree = parser.parse_with_options(
        &mut |at, _| bytes.get(at..).unwrap_or_default(),
        None,
        Some(ParseOptions::new().progress_callback(&mut progress)),
    );
    tree.map_or(Parsed::Aborted, Parsed::Tree)
}

/// A file's bytes do not say whether JSX is in it, so a `.ts` parse that
/// fails is tried as TSX and the reading with fewer errors is kept.
fn parse_dialect(dialect: Dialect, source: &str, budget: &RunBudget) -> Parsed {
    match dialect {
        Dialect::JavaScript => parse(Syntax::JavaScript, source, budget),
        Dialect::TypeScript => match parse(Syntax::TypeScript, source, budget) {
            Parsed::Tree(plain) if plain.root_node().has_error() => {
                match parse(Syntax::Tsx, source, budget) {
                    Parsed::Tree(jsx)
                        if error_lines(jsx.root_node()).len()
                            < error_lines(plain.root_node()).len() =>
                    {
                        Parsed::Tree(jsx)
                    }
                    _ => Parsed::Tree(plain),
                }
            }
            other => other,
        },
    }
}

fn unbound() -> RepoPath {
    RepoPath::new(UNBOUND_PATH).expect("a valid placeholder path")
}

fn error_lines(root: Node<'_>) -> Vec<(u32, u32)> {
    if !root.has_error() {
        return Vec::new();
    }
    let mut spans = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.is_error() || node.is_missing() {
            spans.push((
                node.start_position().row as u32 + 1,
                node.end_position().row as u32 + 1,
            ));
        } else if node.has_error() {
            stack.extend(kids(node));
        }
    }
    spans.sort_unstable();
    let mut merged: Vec<(u32, u32)> = Vec::new();
    for (from, to) in spans {
        match merged.last_mut() {
            Some(last) if from <= last.1 + 1 => last.1 = last.1.max(to),
            _ => merged.push((from, to)),
        }
    }
    merged.truncate(MAX_ERROR_RANGES);
    merged
}

#[derive(Default)]
struct Ticker {
    polls: u32,
    over: bool,
}

impl Ticker {
    fn over(&mut self, budget: &RunBudget) -> bool {
        if !self.over && self.polls % POLL_EVERY == 0 && expired(budget) {
            self.over = true;
        }
        self.polls = self.polls.wrapping_add(1);
        self.over
    }
}

fn extract(dialect: Dialect, text: &[u8], budget: &RunBudget) -> BlobFacts {
    let language = match dialect {
        Dialect::TypeScript => Language::TypeScript,
        Dialect::JavaScript => Language::JavaScript,
    };
    let source = String::from_utf8_lossy(text);
    let tree = match parse_dialect(dialect, &source, budget) {
        Parsed::Tree(tree) => tree,
        Parsed::Missing => {
            let mut blob = BlobFacts::new(language);
            blob.losses.push(Loss::Unsupported { path: unbound() });
            return blob;
        }
        Parsed::Aborted => {
            let mut blob = BlobFacts::new(language);
            blob.deadline();
            return blob;
        }
    };
    let root = tree.root_node();
    let mut blob = BlobFacts::new(language);
    let lines = error_lines(root);
    if !lines.is_empty() {
        blob.parse_error(lines);
    }
    let mut ticker = Ticker::default();
    let declared = Declarer::run(&source, root, budget, &mut ticker);
    let Declared {
        defs,
        owners,
        facts: static_facts,
        module_types,
        declarators,
    } = declared;
    let mut facts: Vec<Fact> = defs.into_iter().map(Fact::Definition).collect();
    facts.extend(static_facts);
    if !ticker.over {
        let walker = Walker {
            src: &source,
            owners: &owners,
            declarators: &declarators,
            facts: Vec::new(),
            scopes: vec![Scope::Module],
            types: vec![module_types],
            callees: Vec::new(),
            tasks: Vec::new(),
        };
        facts.extend(walker.run(root, budget, &mut ticker));
    }
    if ticker.over {
        blob.deadline();
    }
    blob.facts = facts;
    blob.in_source_order();
    blob
}

// ---- tree helpers ------------------------------------------------------

fn kids(node: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = node.walk();
    let found: Vec<Node<'_>> = node.children(&mut cursor).collect();
    found
}

fn field<'t>(node: Node<'t>, name: &str) -> Option<Node<'t>> {
    node.child_by_field_name(name)
}

fn text<'a>(src: &'a str, node: Node<'_>) -> &'a str {
    src.get(node.start_byte()..node.end_byte()).unwrap_or("")
}

fn first_named(node: Node<'_>) -> Option<Node<'_>> {
    kids(node)
        .into_iter()
        .find(|child| child.is_named() && child.kind() != "comment")
}

fn has_token(node: Node<'_>, token: &str) -> bool {
    kids(node)
        .iter()
        .any(|child| !child.is_named() && child.kind() == token)
}

fn span(node: Node<'_>) -> Span {
    Span {
        bytes: (node.start_byte() as u32)..(node.end_byte() as u32),
        lines: (
            node.start_position().row as u32 + 1,
            node.end_position().row as u32 + 1,
        ),
    }
}

fn is_function_kind(kind: &str) -> bool {
    matches!(
        kind,
        "arrow_function" | "function_expression" | "generator_function"
    )
}

fn unwrapped(node: Node<'_>) -> Node<'_> {
    let mut current = node;
    while matches!(
        current.kind(),
        "parenthesized_expression"
            | "as_expression"
            | "satisfies_expression"
            | "non_null_expression"
    ) {
        match first_named(current) {
            Some(inner) => current = inner,
            None => break,
        }
    }
    current
}

fn function_value(node: Node<'_>) -> Option<Node<'_>> {
    let inner = unwrapped(node);
    is_function_kind(inner.kind()).then_some(inner)
}

fn is_const_declaration(node: Option<Node<'_>>) -> bool {
    node.is_some_and(|declaration| {
        declaration.kind() == "lexical_declaration" && has_token(declaration, "const")
    })
}

fn strip_quotes(raw: &str) -> &str {
    let trimmed = raw.trim();
    let mut chars = trimmed.chars();
    match (chars.next(), chars.next_back()) {
        (Some(open @ ('"' | '\'' | '`')), Some(close)) if open == close => {
            &trimmed[1..trimmed.len() - 1]
        }
        _ => trimmed,
    }
}

/// The text of a string literal or of a template with no substitution in it.
fn string_value(src: &str, node: Node<'_>) -> Option<String> {
    match node.kind() {
        "string" => Some(strip_quotes(text(src, node)).to_string()),
        "template_string"
            if !kids(node)
                .iter()
                .any(|c| c.kind() == "template_substitution") =>
        {
            Some(strip_quotes(text(src, node)).to_string())
        }
        _ => None,
    }
}

/// How many directories a relative specifier climbs, counting its own: `./a`
/// is 1, `../a` is 2, a bare or absolute one 0.
fn relative_of(specifier: &str) -> u32 {
    let mut rest = specifier;
    let mut relative = 0;
    loop {
        if let Some(tail) = rest.strip_prefix("./") {
            relative = relative.max(1);
            rest = tail;
        } else if let Some(tail) = rest.strip_prefix("../") {
            relative = relative.max(1) + 1;
            rest = tail;
        } else {
            break;
        }
    }
    match rest {
        "." => relative.max(1),
        ".." => relative.max(1) + 1,
        _ => relative,
    }
}

/// The names an expression is written with, outermost first; a call in the
/// middle is transparent (`a.b().c` is `a b c`).
fn path_of(src: &str, node: Node<'_>) -> Vec<Name> {
    let mut reversed = Vec::new();
    let mut current = node;
    loop {
        match current.kind() {
            "identifier" | "this" | "super" => {
                reversed.push(Name::from(text(src, current)));
                break;
            }
            "member_expression" => match (field(current, "object"), field(current, "property")) {
                (Some(object), Some(property)) => {
                    reversed.push(Name::from(text(src, property)));
                    current = object;
                }
                _ => break,
            },
            "call_expression" => match field(current, "function") {
                Some(function) => current = function,
                None => break,
            },
            "parenthesized_expression"
            | "non_null_expression"
            | "as_expression"
            | "satisfies_expression"
            | "await_expression" => match first_named(current) {
                Some(inner) => current = inner,
                None => break,
            },
            _ => break,
        }
    }
    reversed.reverse();
    reversed
}

/// A type written at a use site, as far as it names one thing: `Foo`,
/// `Foo<T>`, `ns.Foo`, `Foo | null`.
fn type_name(src: &str, node: Node<'_>) -> Option<TypeName> {
    let mut current = node;
    if current.kind() == "type_annotation" {
        current = first_named(current)?;
    }
    match current.kind() {
        "type_identifier" => Some(TypeName::from(text(src, current))),
        "nested_type_identifier" => Some(TypeName::from(squeeze(text(src, current)))),
        "generic_type" => type_name(src, field(current, "name")?),
        "parenthesized_type" => type_name(src, first_named(current)?),
        "union_type" => {
            let mut named = kids(current).into_iter().filter(|member| {
                member.is_named()
                    && member.kind() != "comment"
                    && !matches!(text(src, *member), "null" | "undefined")
            });
            let only = named.next()?;
            named
                .next()
                .is_none()
                .then(|| type_name(src, only))
                .flatten()
        }
        _ => None,
    }
}

fn squeeze(raw: &str) -> String {
    raw.chars().filter(|c| !c.is_whitespace()).collect()
}

/// The class a value is, when the syntax says so without inference: `new C()`
/// and `x as C` (#611).
fn value_type(src: &str, node: Node<'_>) -> Option<TypeName> {
    match unwrapped_once(node).kind() {
        "new_expression" => {
            let constructor = field(unwrapped_once(node), "constructor")?;
            match constructor.kind() {
                "identifier" | "member_expression" => {
                    Some(TypeName::from(squeeze(text(src, constructor))))
                }
                _ => None,
            }
        }
        _ if node.kind() == "as_expression" => {
            let annotated = kids(node)
                .into_iter()
                .rev()
                .find(|child| child.is_named() && child.kind() != "comment")?;
            type_name(src, annotated)
        }
        _ => None,
    }
}

fn unwrapped_once(node: Node<'_>) -> Node<'_> {
    if matches!(node.kind(), "parenthesized_expression" | "await_expression") {
        first_named(node).unwrap_or(node)
    } else {
        node
    }
}

fn declared_type(
    src: &str,
    annotation: Option<Node<'_>>,
    value: Option<Node<'_>>,
    is_const: bool,
) -> Option<TypeName> {
    annotation
        .and_then(|node| type_name(src, node))
        .or_else(|| {
            value
                .filter(|_| is_const)
                .and_then(|node| value_type(src, node))
        })
}

fn leaves<'a>(src: &'a str, node: Node<'_>, out: &mut Vec<&'a str>) {
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        if current.kind() == "comment" {
            continue;
        }
        if current.child_count() == 0 {
            let leaf = text(src, current);
            if !leaf.is_empty() {
                out.push(leaf);
            }
            continue;
        }
        stack.extend(kids(current).into_iter().rev());
    }
}

/// A syntax tree as a token stream: node kinds, nesting and leaf text, with
/// whitespace and comments left out (#442).
fn tokens<'a>(src: &'a str, node: Node<'_>, out: &mut Vec<&'a str>) {
    enum Step<'t> {
        Enter(Node<'t>),
        Exit,
    }
    let mut stack = vec![Step::Enter(node)];
    while let Some(step) = stack.pop() {
        let Step::Enter(current) = step else {
            out.push(CLOSE);
            continue;
        };
        if current.kind() == "comment" {
            continue;
        }
        if current.child_count() == 0 {
            if current.is_missing() {
                out.push(OPEN);
                out.push(current.kind());
            } else {
                out.push(text(src, current));
            }
            continue;
        }
        out.push(OPEN);
        out.push(current.kind());
        stack.push(Step::Exit);
        stack.extend(kids(current).into_iter().rev().map(Step::Enter));
    }
}

/// Words, parameters and heritage that make up how a declaration reads from
/// outside, normalised so that equal declarations compare equal.
fn signature(src: &str, nodes: &[Node<'_>]) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for node in nodes {
        for child in kids(*node) {
            if !child.is_named() && MODIFIERS.contains(&child.kind()) {
                parts.push(child.kind());
            } else if child.kind() == "override_modifier" {
                parts.push("override");
            } else if matches!(child.kind(), "class_heritage" | "extends_type_clause") {
                leaves(src, child, &mut parts);
            }
        }
        for name in [
            "type_parameters",
            "parameters",
            "parameter",
            "return_type",
            "type",
        ] {
            if let Some(child) = field(*node, name) {
                leaves(src, child, &mut parts);
            }
        }
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

fn is_doc_comment(src: &str, node: Node<'_>) -> bool {
    node.kind() == "comment" && text(src, node).starts_with("/**")
}

fn doc_hash(raw: &str) -> Option<u64> {
    let inner = raw.trim_start_matches("/**").trim_end_matches("*/");
    hash_tokens(
        inner
            .lines()
            .flat_map(|line| line.trim().trim_start_matches('*').split_whitespace())
            .map(str::as_bytes),
    )
}

fn decorator_name(src: &str, node: Node<'_>) -> Option<Name> {
    let target = first_named(node)?;
    path_of(src, target).pop()
}

struct Leading {
    start_byte: usize,
    start_row: usize,
    decorators: Vec<Name>,
    doc: Option<u64>,
}

/// What stands in front of a declaration: decorators written before it as
/// siblings (a method's, in a class body) and the doc comment above them.
fn leading(src: &str, outer: Node<'_>, node: Node<'_>) -> Leading {
    let mut start_byte = outer.start_byte();
    let mut start_row = outer.start_position().row;
    let mut own: Vec<Name> = Vec::new();
    let owners = if outer.id() == node.id() {
        vec![outer]
    } else {
        vec![outer, node]
    };
    for owner in owners {
        for child in kids(owner) {
            if child.kind() == "decorator" {
                own.extend(decorator_name(src, child));
            }
        }
    }
    let mut before: Vec<Name> = Vec::new();
    let mut doc = None;
    let mut next_row = start_row;
    let mut current = outer.prev_sibling();
    while let Some(sibling) = current {
        match sibling.kind() {
            "decorator" => {
                before.extend(decorator_name(src, sibling));
                start_byte = sibling.start_byte();
                start_row = sibling.start_position().row;
                next_row = start_row;
                current = sibling.prev_sibling();
            }
            "comment" => {
                if is_doc_comment(src, sibling) {
                    if sibling.end_position().row + 1 >= next_row {
                        doc = doc_hash(text(src, sibling));
                    }
                    break;
                }
                current = sibling.prev_sibling();
            }
            _ => break,
        }
    }
    before.reverse();
    before.extend(own);
    Leading {
        start_byte,
        start_row,
        decorators: before,
        doc,
    }
}

fn outer_of(node: Node<'_>) -> Node<'_> {
    match node.parent() {
        Some(parent) if parent.kind() == "export_statement" => parent,
        _ => node,
    }
}

fn visibility_of(src: &str, member: Node<'_>, name: &str) -> Visibility {
    if name.starts_with('#') {
        return Visibility::Private;
    }
    kids(member)
        .into_iter()
        .find(|child| child.kind() == "accessibility_modifier")
        .map_or(Visibility::Public, |modifier| match text(src, modifier) {
            "private" => Visibility::Private,
            "protected" => Visibility::Protected,
            _ => Visibility::Public,
        })
}

fn member_name<'t>(src: &str, member: Node<'t>) -> Option<(Node<'t>, String)> {
    let node = field(member, "name").or_else(|| field(member, "property"))?;
    let name = match node.kind() {
        "string" => strip_quotes(text(src, node)).to_string(),
        _ => text(src, node).trim().to_string(),
    };
    Some((node, name))
}

// ---- patterns ----------------------------------------------------------

enum Bound<'t> {
    /// A name the pattern binds or assigns.
    Ident(Node<'t>),
    /// An expression inside the pattern: a default, a computed key.
    Expr(Node<'t>),
    /// A member an assignment writes.
    Member(Node<'t>),
}

fn pattern_bounds(root: Node<'_>) -> Vec<Bound<'_>> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "identifier" | "shorthand_property_identifier_pattern" => out.push(Bound::Ident(node)),
            "object_pattern" | "array_pattern" | "rest_pattern" => stack.extend(
                kids(node)
                    .into_iter()
                    .rev()
                    .filter(|child| child.is_named() && child.kind() != "comment"),
            ),
            "pair_pattern" => {
                if let Some(key) =
                    field(node, "key").filter(|k| k.kind() == "computed_property_name")
                {
                    out.push(Bound::Expr(key));
                }
                stack.extend(field(node, "value"));
            }
            "object_assignment_pattern" | "assignment_pattern" => {
                out.extend(field(node, "right").map(Bound::Expr));
                stack.extend(field(node, "left"));
            }
            "member_expression" | "subscript_expression" | "non_null_expression" => {
                out.push(Bound::Member(node));
            }
            "this" | "comment" => {}
            _ => out.push(Bound::Expr(node)),
        }
    }
    out
}

// ---- the declaring pass ------------------------------------------------

#[derive(Clone, Copy)]
enum Body<'t> {
    Absent,
    /// A function's body, a variable's value, a type alias's type.
    Whole(Node<'t>),
    /// A class, interface, enum or namespace body: the members that are
    /// definitions themselves are left out of its hashes.
    Members(Node<'t>),
}

fn body_of(node: Node<'_>) -> Body<'_> {
    field(node, "body").map_or(Body::Absent, Body::Whole)
}

struct Spec<'t> {
    /// The owner of the references inside, and the node whose text is hashed.
    node: Node<'t>,
    /// The node whose range the definition's span covers.
    outer: Node<'t>,
    name_node: Node<'t>,
    name: String,
    container: Vec<Name>,
    kind: DefKind,
    type_namespace: bool,
    visibility: Visibility,
    signature: Option<String>,
    body: Body<'t>,
    /// Whether references inside `node` belong to this definition.
    owner: bool,
}

struct Pending<'t> {
    spec: Spec<'t>,
    start_byte: usize,
    start_row: usize,
    decorators: Vec<Name>,
    doc: Option<u64>,
}

struct Declared {
    defs: Vec<Def>,
    owners: FxHashMap<usize, LocalKey>,
    facts: Vec<Fact>,
    module_types: FxHashMap<String, Option<TypeName>>,
    declarators: FxHashSet<usize>,
}

struct Stmt<'t> {
    node: Node<'t>,
    container: Vec<Name>,
    exported: bool,
}

struct Declarer<'t> {
    src: &'t str,
    pending: Vec<Pending<'t>>,
    facts: Vec<Fact>,
    exported: FxHashSet<String>,
    module_types: FxHashMap<String, Option<TypeName>>,
    declarators: FxHashSet<usize>,
}

fn visibility_for(exported: bool) -> Visibility {
    if exported {
        Visibility::Public
    } else {
        Visibility::Private
    }
}

impl<'t> Declarer<'t> {
    fn run(src: &'t str, root: Node<'t>, budget: &RunBudget, ticker: &mut Ticker) -> Declared {
        let mut declarer = Self {
            src,
            pending: Vec::new(),
            facts: Vec::new(),
            exported: FxHashSet::default(),
            module_types: FxHashMap::default(),
            declarators: FxHashSet::default(),
        };
        let mut stack: Vec<Stmt<'t>> = Vec::new();
        Self::push_statements(&mut stack, kids(root), &[], false);
        while let Some(Stmt {
            node,
            container,
            exported,
        }) = stack.pop()
        {
            if ticker.over(budget) {
                break;
            }
            declarer.statement(node, container, exported, &mut stack);
        }
        declarer.finish()
    }

    fn push_statements(
        stack: &mut Vec<Stmt<'t>>,
        nodes: Vec<Node<'t>>,
        container: &[Name],
        exported: bool,
    ) {
        for node in nodes.into_iter().rev() {
            if node.is_named() && node.kind() != "comment" {
                stack.push(Stmt {
                    node,
                    container: container.to_vec(),
                    exported,
                });
            }
        }
    }

    fn add(&mut self, spec: Spec<'t>) {
        let lead = leading(self.src, spec.outer, spec.node);
        self.pending.push(Pending {
            spec,
            start_byte: lead.start_byte,
            start_row: lead.start_row,
            decorators: lead.decorators,
            doc: lead.doc,
        });
    }

    fn statement(
        &mut self,
        node: Node<'t>,
        container: Vec<Name>,
        exported: bool,
        stack: &mut Vec<Stmt<'t>>,
    ) {
        match node.kind() {
            "export_statement" => self.export_statement(node, container, stack),
            "import_statement" => self.import_statement(node),
            "ambient_declaration" => {
                let mut inner = Vec::new();
                for child in kids(node) {
                    if child.kind() == "statement_block" {
                        inner.extend(kids(child));
                    } else {
                        inner.push(child);
                    }
                }
                Self::push_statements(stack, inner, &container, exported);
            }
            "function_declaration" | "generator_function_declaration" | "function_signature" => {
                self.function(node, container, exported);
            }
            "class_declaration" | "abstract_class_declaration" => {
                self.class(node, container, exported);
            }
            "interface_declaration" => self.interface(node, container, exported),
            "type_alias_declaration" => self.type_alias(node, container, exported),
            "enum_declaration" => self.enumeration(node, container, exported),
            "internal_module" | "module" => self.namespace(node, container, exported, stack),
            "lexical_declaration" | "variable_declaration" => {
                self.declaration(node, container, exported);
            }
            "expression_statement" => match first_named(node) {
                // The grammar reads `namespace N { … }` as an expression too.
                Some(inner) if matches!(inner.kind(), "internal_module" | "module") => {
                    self.namespace(inner, container, exported, stack);
                }
                _ => self.commonjs(node, container),
            },
            _ => {}
        }
    }

    fn function(&mut self, node: Node<'t>, container: Vec<Name>, exported: bool) {
        let Some(name_node) = field(node, "name") else {
            return;
        };
        self.add(Spec {
            node,
            outer: outer_of(node),
            name_node,
            name: text(self.src, name_node).to_string(),
            container,
            kind: DefKind::Function,
            type_namespace: false,
            visibility: visibility_for(exported),
            signature: signature(self.src, &[node]),
            body: body_of(node),
            owner: true,
        });
    }

    fn class(&mut self, node: Node<'t>, container: Vec<Name>, exported: bool) {
        let Some(name_node) = field(node, "name") else {
            return;
        };
        let name = text(self.src, name_node).to_string();
        self.class_def(
            node,
            outer_of(node),
            name_node,
            name,
            container,
            visibility_for(exported),
            node,
        );
    }

    /// `node` owns the references, `class` holds the heritage and the body:
    /// they differ for `const C = class { … }` and `export default class { … }`.
    fn class_def(
        &mut self,
        node: Node<'t>,
        outer: Node<'t>,
        name_node: Node<'t>,
        name: String,
        container: Vec<Name>,
        visibility: Visibility,
        class: Node<'t>,
    ) {
        let body = field(class, "body");
        let mut inner = container.clone();
        inner.push(Name::from(name.as_str()));
        self.add(Spec {
            node,
            outer,
            name_node,
            name,
            container,
            kind: DefKind::Class,
            type_namespace: false,
            visibility,
            signature: signature(self.src, &[class]),
            body: body.map_or(Body::Absent, Body::Members),
            owner: true,
        });
        if let Some(body) = body {
            self.class_members(body, inner);
        }
    }

    fn class_members(&mut self, body: Node<'t>, container: Vec<Name>) {
        for member in kids(body) {
            match member.kind() {
                "method_definition" => self.method(member, &container, true),
                "method_signature" | "abstract_method_signature" => {
                    self.method(member, &container, false);
                }
                "public_field_definition" | "field_definition" => self.field(member, &container),
                _ => {}
            }
        }
    }

    fn method(&mut self, member: Node<'t>, container: &[Name], with_body: bool) {
        let Some((name_node, name)) = member_name(self.src, member) else {
            return;
        };
        let constructor = with_body && name == "constructor";
        let kind = if constructor {
            DefKind::Constructor
        } else if has_token(member, "get") || has_token(member, "set") {
            DefKind::Property
        } else {
            DefKind::Method
        };
        let visibility = visibility_of(self.src, member, &name);
        if constructor {
            self.parameter_properties(member, container);
        }
        self.add(Spec {
            node: member,
            outer: member,
            name_node,
            name,
            container: container.to_vec(),
            kind,
            type_namespace: false,
            visibility,
            signature: signature(self.src, &[member]),
            body: body_of(member),
            owner: true,
        });
    }

    /// `constructor(private readonly svc: Svc)` declares a field.
    fn parameter_properties(&mut self, constructor: Node<'t>, container: &[Name]) {
        let Some(parameters) = field(constructor, "parameters") else {
            return;
        };
        for parameter in kids(parameters) {
            if !matches!(
                parameter.kind(),
                "required_parameter" | "optional_parameter"
            ) {
                continue;
            }
            let promoted = has_token(parameter, "readonly")
                || kids(parameter)
                    .iter()
                    .any(|child| child.kind() == "accessibility_modifier");
            let Some(name_node) = field(parameter, "pattern")
                .or_else(|| field(parameter, "name"))
                .filter(|n| n.kind() == "identifier")
            else {
                continue;
            };
            if !promoted {
                continue;
            }
            let name = text(self.src, name_node).to_string();
            let visibility = visibility_of(self.src, parameter, &name);
            self.add(Spec {
                node: parameter,
                outer: parameter,
                name_node,
                name,
                container: container.to_vec(),
                kind: DefKind::Field,
                type_namespace: false,
                visibility,
                signature: signature(self.src, &[parameter]),
                body: Body::Absent,
                owner: false,
            });
        }
    }

    fn field(&mut self, member: Node<'t>, container: &[Name]) {
        let Some((name_node, name)) = member_name(self.src, member) else {
            return;
        };
        let value = field(member, "value");
        let visibility = visibility_of(self.src, member, &name);
        let (kind, signature, body) = match value.and_then(function_value) {
            Some(function) => (
                DefKind::Method,
                signature(self.src, &[member, function]),
                body_of(function),
            ),
            None => (
                DefKind::Field,
                signature(self.src, &[member]),
                value.map_or(Body::Absent, Body::Whole),
            ),
        };
        self.add(Spec {
            node: member,
            outer: member,
            name_node,
            name,
            container: container.to_vec(),
            kind,
            type_namespace: false,
            visibility,
            signature,
            body,
            owner: true,
        });
    }

    fn interface(&mut self, node: Node<'t>, container: Vec<Name>, exported: bool) {
        let Some(name_node) = field(node, "name") else {
            return;
        };
        let name = text(self.src, name_node).to_string();
        let body = field(node, "body");
        let mut inner = container.clone();
        inner.push(Name::from(name.as_str()));
        self.add(Spec {
            node,
            outer: outer_of(node),
            name_node,
            name,
            container,
            kind: DefKind::Interface,
            type_namespace: true,
            visibility: visibility_for(exported),
            signature: signature(self.src, &[node]),
            body: body.map_or(Body::Absent, Body::Members),
            owner: true,
        });
        for member in body.map(kids).unwrap_or_default() {
            let kind = match member.kind() {
                "method_signature" => DefKind::Method,
                "property_signature" => DefKind::Property,
                _ => continue,
            };
            let Some((name_node, name)) = member_name(self.src, member) else {
                continue;
            };
            self.add(Spec {
                node: member,
                outer: member,
                name_node,
                name,
                container: inner.clone(),
                kind,
                type_namespace: false,
                visibility: Visibility::Public,
                signature: signature(self.src, &[member]),
                body: Body::Absent,
                owner: true,
            });
        }
    }

    fn type_alias(&mut self, node: Node<'t>, container: Vec<Name>, exported: bool) {
        let Some(name_node) = field(node, "name") else {
            return;
        };
        self.add(Spec {
            node,
            outer: outer_of(node),
            name_node,
            name: text(self.src, name_node).to_string(),
            container,
            kind: DefKind::TypeAlias,
            type_namespace: true,
            visibility: visibility_for(exported),
            signature: signature(self.src, &[node]),
            body: field(node, "value").map_or(Body::Absent, Body::Whole),
            owner: true,
        });
    }

    fn enumeration(&mut self, node: Node<'t>, container: Vec<Name>, exported: bool) {
        let Some(name_node) = field(node, "name") else {
            return;
        };
        let name = text(self.src, name_node).to_string();
        let body = field(node, "body");
        let mut inner = container.clone();
        inner.push(Name::from(name.as_str()));
        self.add(Spec {
            node,
            outer: outer_of(node),
            name_node,
            name,
            container,
            kind: DefKind::Enum,
            type_namespace: false,
            visibility: visibility_for(exported),
            signature: None,
            body: body.map_or(Body::Absent, Body::Members),
            owner: true,
        });
        for member in body.map(kids).unwrap_or_default() {
            let (name_node, value) = match member.kind() {
                "enum_assignment" => match field(member, "name") {
                    Some(name_node) => (name_node, field(member, "value")),
                    None => continue,
                },
                "property_identifier" | "string" | "computed_property_name" | "number" => {
                    (member, None)
                }
                _ => continue,
            };
            let name = match name_node.kind() {
                "string" => strip_quotes(text(self.src, name_node)).to_string(),
                _ => text(self.src, name_node).trim().to_string(),
            };
            self.add(Spec {
                node: member,
                outer: member,
                name_node,
                name,
                container: inner.clone(),
                kind: DefKind::EnumMember,
                type_namespace: false,
                visibility: Visibility::Public,
                signature: None,
                body: value.map_or(Body::Absent, Body::Whole),
                owner: true,
            });
        }
    }

    fn namespace(
        &mut self,
        node: Node<'t>,
        container: Vec<Name>,
        exported: bool,
        stack: &mut Vec<Stmt<'t>>,
    ) {
        let Some(name_node) = field(node, "name") else {
            return;
        };
        let raw = text(self.src, name_node);
        let mut segments: Vec<String> = match name_node.kind() {
            "string" => vec![strip_quotes(raw).to_string()],
            _ => squeeze(raw).split('.').map(str::to_string).collect(),
        };
        let Some(name) = segments.pop() else {
            return;
        };
        let mut outer_container = container;
        outer_container.extend(segments.iter().map(|s| Name::from(s.as_str())));
        let mut inner = outer_container.clone();
        inner.push(Name::from(name.as_str()));
        let body = field(node, "body");
        self.add(Spec {
            node,
            outer: outer_of(node),
            name_node,
            name,
            container: outer_container,
            kind: DefKind::Module,
            type_namespace: false,
            visibility: visibility_for(exported),
            signature: None,
            body: body.map_or(Body::Absent, Body::Members),
            owner: true,
        });
        if let Some(body) = body {
            Self::push_statements(stack, kids(body), &inner, false);
        }
    }

    fn declaration(&mut self, node: Node<'t>, container: Vec<Name>, exported: bool) {
        let outer = outer_of(node);
        let is_const = is_const_declaration(Some(node));
        let declarators: Vec<Node<'t>> = kids(node)
            .into_iter()
            .filter(|child| child.kind() == "variable_declarator")
            .collect();
        let sole = declarators.len() == 1;
        for declarator in declarators {
            let span_node = if sole { outer } else { declarator };
            self.declarator(declarator, span_node, &container, exported, is_const);
        }
    }

    fn declarator(
        &mut self,
        declarator: Node<'t>,
        outer: Node<'t>,
        container: &[Name],
        exported: bool,
        is_const: bool,
    ) {
        let Some(name_node) = field(declarator, "name") else {
            return;
        };
        self.declarators.insert(declarator.id());
        let value = field(declarator, "value");
        let annotation = field(declarator, "type");
        let plain = if is_const {
            DefKind::Constant
        } else {
            DefKind::Variable
        };
        let visibility = visibility_for(exported);
        if name_node.kind() == "identifier" {
            let name = text(self.src, name_node).to_string();
            self.module_types.insert(
                name.clone(),
                declared_type(self.src, annotation, value, is_const),
            );
            self.value_def(
                declarator,
                outer,
                name_node,
                name,
                value,
                Some(declarator),
                plain,
                container.to_vec(),
                visibility,
            );
            return;
        }
        for bound in pattern_bounds(name_node) {
            let Bound::Ident(ident) = bound else {
                continue;
            };
            self.add(Spec {
                node: declarator,
                outer,
                name_node: ident,
                name: text(self.src, ident).to_string(),
                container: container.to_vec(),
                kind: plain,
                type_namespace: false,
                visibility,
                signature: None,
                body: value.map_or(Body::Absent, Body::Whole),
                owner: false,
            });
        }
    }

    /// A name bound to a value: a function, a class, or anything else.
    fn value_def(
        &mut self,
        node: Node<'t>,
        outer: Node<'t>,
        name_node: Node<'t>,
        name: String,
        value: Option<Node<'t>>,
        annotated: Option<Node<'t>>,
        plain: DefKind,
        container: Vec<Name>,
        visibility: Visibility,
    ) {
        if let Some(function) = value.and_then(function_value) {
            let nodes: Vec<Node<'t>> = annotated.into_iter().chain([function]).collect();
            self.add(Spec {
                node,
                outer,
                name_node,
                name,
                container,
                kind: DefKind::Function,
                type_namespace: false,
                visibility,
                signature: signature(self.src, &nodes),
                body: body_of(function),
                owner: true,
            });
            return;
        }
        if let Some(class) = value.map(unwrapped).filter(|inner| inner.kind() == "class") {
            self.class_def(node, outer, name_node, name, container, visibility, class);
            return;
        }
        let nodes: Vec<Node<'t>> = annotated.into_iter().collect();
        self.add(Spec {
            node,
            outer,
            name_node,
            name,
            container,
            kind: plain,
            type_namespace: false,
            visibility,
            signature: signature(self.src, &nodes),
            body: value.map_or(Body::Absent, Body::Whole),
            owner: true,
        });
    }

    fn import_statement(&mut self, node: Node<'t>) {
        let mut clause = None;
        let mut require = None;
        let mut type_only = false;
        for child in kids(node) {
            match child.kind() {
                "import_clause" => clause = Some(child),
                "import_require_clause" => require = Some(child),
                "type" if !child.is_named() => type_only = true,
                _ => {}
            }
        }
        if let Some(require) = require {
            let Some(module) = field(require, "source").and_then(|s| string_value(self.src, s))
            else {
                return;
            };
            let alias = first_named(require)
                .filter(|n| n.kind() == "identifier")
                .map(|n| Name::from(text(self.src, n)));
            self.push_import(
                node,
                module,
                ImportKind::Require,
                Vec::new(),
                Some(alias),
                None,
            );
            return;
        }
        let Some(module) = field(node, "source").and_then(|s| string_value(self.src, s)) else {
            return;
        };
        let mut names = Vec::new();
        let mut star = None;
        let mut default = None;
        if let Some(clause) = clause {
            for child in kids(clause) {
                match child.kind() {
                    "identifier" => default = Some(Name::from(text(self.src, child))),
                    "namespace_import" => {
                        star = Some(
                            first_named(child)
                                .filter(|n| n.kind() == "identifier")
                                .map(|n| Name::from(text(self.src, n))),
                        );
                    }
                    "named_imports" => {
                        for specifier in kids(child) {
                            if specifier.kind() != "import_specifier" {
                                continue;
                            }
                            let Some(name) = field(specifier, "name") else {
                                continue;
                            };
                            let alias =
                                field(specifier, "alias").map(|a| Name::from(text(self.src, a)));
                            names.push((Name::from(self.specifier_name(name)), alias));
                        }
                    }
                    _ => {}
                }
            }
        }
        let kind = if clause.is_none() {
            ImportKind::SideEffect
        } else if type_only {
            ImportKind::TypeOnly
        } else {
            ImportKind::Static
        };
        self.push_import(node, module, kind, names, star, default);
    }

    fn specifier_name(&self, node: Node<'t>) -> String {
        string_value(self.src, node).unwrap_or_else(|| text(self.src, node).to_string())
    }

    fn push_import(
        &mut self,
        node: Node<'t>,
        module: String,
        kind: ImportKind,
        names: Vec<(Name, Option<Name>)>,
        star: Option<Option<Name>>,
        default: Option<Name>,
    ) {
        self.facts.push(Fact::Import(Import {
            site: span(node),
            enclosing: Scope::Module,
            relative: relative_of(&module),
            module: ModulePath::from(module),
            kind,
            names,
            star,
            default,
        }));
    }

    fn export_statement(
        &mut self,
        node: Node<'t>,
        container: Vec<Name>,
        stack: &mut Vec<Stmt<'t>>,
    ) {
        let children = kids(node);
        let is_default = children.iter().any(|c| c.kind() == "default");
        let is_equals = children.iter().any(|c| !c.is_named() && c.kind() == "=");
        let type_only = children.iter().any(|c| !c.is_named() && c.kind() == "type");
        let from = field(node, "source")
            .and_then(|s| string_value(self.src, s))
            .map(ModulePath::from);
        if let Some(declaration) = field(node, "declaration") {
            if let Some(name) = field(declaration, "name").filter(|_| is_default) {
                let local = Name::from(text(self.src, name));
                self.exported.insert(local.as_str().to_string());
                self.facts.push(Fact::Export(Export::Default {
                    site: span(node),
                    local: Some(local),
                }));
            }
            stack.push(Stmt {
                node: declaration,
                container,
                exported: true,
            });
            return;
        }
        let value = field(node, "value").or_else(|| {
            is_equals
                .then(|| {
                    children.iter().copied().find(|c| {
                        c.is_named()
                            && !matches!(
                                c.kind(),
                                "comment" | "decorator" | "export_clause" | "namespace_export"
                            )
                    })
                })
                .flatten()
        });
        if let Some(value) = value {
            self.default_value(node, value, container);
            return;
        }
        for child in children {
            match child.kind() {
                "export_clause" => {
                    for specifier in kids(child) {
                        if specifier.kind() == "export_specifier" {
                            self.export_specifier(specifier, from.clone(), type_only);
                        }
                    }
                }
                "namespace_export" => {
                    if let Some(from) = from.clone() {
                        let alias = first_named(child).map(|n| Name::from(self.specifier_name(n)));
                        self.facts.push(Fact::Export(Export::Star {
                            site: span(node),
                            from,
                            alias,
                        }));
                    }
                }
                "*" if !child.is_named() => {
                    if let Some(from) = from.clone() {
                        self.facts.push(Fact::Export(Export::Star {
                            site: span(node),
                            from,
                            alias: None,
                        }));
                    }
                }
                _ => {}
            }
        }
    }

    fn export_specifier(
        &mut self,
        specifier: Node<'t>,
        from: Option<ModulePath>,
        clause_type_only: bool,
    ) {
        let Some(name) = field(specifier, "name") else {
            return;
        };
        let name = self.specifier_name(name);
        let alias = field(specifier, "alias").map(|a| Name::from(self.specifier_name(a)));
        if from.is_none() {
            self.exported.insert(name.clone());
        }
        self.facts.push(Fact::Export(Export::Named {
            site: span(specifier),
            name: Name::from(name),
            alias,
            from,
            type_only: clause_type_only || has_token(specifier, "type"),
        }));
    }

    /// `export default <expression>` and `export = <expression>`.
    fn default_value(&mut self, statement: Node<'t>, value: Node<'t>, container: Vec<Name>) {
        if value.kind() == "identifier" {
            let local = Name::from(text(self.src, value));
            self.exported.insert(local.as_str().to_string());
            self.facts.push(Fact::Export(Export::Default {
                site: span(statement),
                local: Some(local),
            }));
            return;
        }
        let named = matches!(
            value.kind(),
            "function_expression" | "generator_function" | "class"
        )
        .then(|| field(value, "name"))
        .flatten();
        let (name_node, name, local) = match named {
            Some(node) => {
                let name = text(self.src, node).to_string();
                (node, name.clone(), Some(Name::from(name)))
            }
            None => (value, "default".to_string(), None),
        };
        self.value_def(
            statement,
            statement,
            name_node,
            name,
            Some(value),
            None,
            DefKind::Variable,
            container,
            Visibility::Public,
        );
        self.facts.push(Fact::Export(Export::Default {
            site: span(statement),
            local,
        }));
    }

    /// `module.exports = …`, `exports.x = …`, `module.exports.x = …`.
    fn commonjs(&mut self, statement: Node<'t>, container: Vec<Name>) {
        let Some(assignment) =
            first_named(statement).filter(|n| n.kind() == "assignment_expression")
        else {
            return;
        };
        let (Some(left), Some(right)) = (field(assignment, "left"), field(assignment, "right"))
        else {
            return;
        };
        if left.kind() != "member_expression" {
            return;
        }
        let path = path_of(self.src, left);
        let names: Vec<&str> = path.iter().map(Name::as_str).collect();
        let exported_name = match names.as_slice() {
            ["module", "exports"] => None,
            ["module", "exports", name] | ["exports", name] => Some(Name::from(*name)),
            _ => return,
        };
        let site = span(statement);
        let right = unwrapped(right);
        match exported_name {
            None if right.kind() == "identifier" => {
                let local = Name::from(text(self.src, right));
                self.exported.insert(local.as_str().to_string());
                self.facts.push(Fact::Export(Export::Default {
                    site,
                    local: Some(local),
                }));
            }
            None if right.kind() == "object" => {
                for entry in kids(right) {
                    let (name, alias) = match entry.kind() {
                        "shorthand_property_identifier" => {
                            (Name::from(text(self.src, entry)), None)
                        }
                        "pair" => {
                            let (Some(key), Some(value)) =
                                (field(entry, "key"), field(entry, "value"))
                            else {
                                continue;
                            };
                            if value.kind() != "identifier" {
                                continue;
                            }
                            let key = Name::from(text(self.src, key));
                            let local = Name::from(text(self.src, value));
                            let alias = (key != local).then_some(key);
                            (local, alias)
                        }
                        _ => continue,
                    };
                    self.exported.insert(name.as_str().to_string());
                    self.facts.push(Fact::Export(Export::Named {
                        site: span(entry),
                        name,
                        alias,
                        from: None,
                        type_only: false,
                    }));
                }
            }
            None => self.default_value(statement, right, container),
            Some(exported) if right.kind() == "identifier" => {
                let local = Name::from(text(self.src, right));
                self.exported.insert(local.as_str().to_string());
                let alias = (local != exported).then_some(exported);
                self.facts.push(Fact::Export(Export::Named {
                    site,
                    name: local,
                    alias,
                    from: None,
                    type_only: false,
                }));
            }
            Some(exported) => {
                let name_node = field(left, "property").unwrap_or(left);
                self.value_def(
                    right,
                    statement,
                    name_node,
                    exported.as_str().to_string(),
                    Some(right),
                    None,
                    DefKind::Variable,
                    container,
                    Visibility::Public,
                );
                self.facts.push(Fact::Export(Export::Named {
                    site,
                    name: exported,
                    alias: None,
                    from: None,
                    type_only: false,
                }));
            }
        }
    }

    fn finish(self) -> Declared {
        let Self {
            src,
            pending,
            facts,
            exported,
            module_types,
            declarators,
        } = self;
        let group = |spec: &Spec<'_>| {
            (
                spec.container
                    .iter()
                    .map(Name::as_str)
                    .collect::<Vec<_>>()
                    .join("."),
                spec.name.clone(),
                spec.type_namespace,
            )
        };
        let mut counts: FxHashMap<(String, String, bool), u16> = FxHashMap::default();
        for entry in &pending {
            *counts.entry(group(&entry.spec)).or_default() += 1;
        }
        let mut seen: FxHashMap<(String, String, bool), u16> = FxHashMap::default();
        let mut locals: Vec<LocalKey> = Vec::with_capacity(pending.len());
        for entry in &pending {
            let key = group(&entry.spec);
            let disambig = if counts.get(&key).copied().unwrap_or(0) > 1 {
                let next = seen.entry(key).or_default();
                let found = Disambig::Overload(*next);
                *next += 1;
                Some(found)
            } else {
                None
            };
            locals.push(LocalKey {
                container: entry.spec.container.clone(),
                name: Name::from(entry.spec.name.as_str()),
                ns: if entry.spec.type_namespace {
                    Namespace::Type
                } else {
                    Namespace::Value
                },
                disambig,
            });
        }
        let mut owners: FxHashMap<usize, LocalKey> = FxHashMap::default();
        for (entry, local) in pending.iter().zip(&locals) {
            if entry.spec.owner {
                owners.insert(entry.spec.node.id(), local.clone());
                if entry.spec.outer.kind() == "export_statement" {
                    owners.insert(entry.spec.outer.id(), local.clone());
                }
            }
        }
        let mut defs = Vec::with_capacity(pending.len());
        for (entry, local) in pending.into_iter().zip(locals) {
            let (body_hash, text_hash) = hashes(src, &entry.spec, &owners, &declarators);
            let visibility =
                if entry.spec.container.is_empty() && exported.contains(&entry.spec.name) {
                    Visibility::Public
                } else {
                    entry.spec.visibility
                };
            defs.push(Def {
                local,
                kind: entry.spec.kind,
                span: Span {
                    bytes: (entry.start_byte as u32)..(entry.spec.outer.end_byte() as u32),
                    lines: (
                        entry.start_row as u32 + 1,
                        entry.spec.outer.end_position().row as u32 + 1,
                    ),
                },
                name_span: span(entry.spec.name_node),
                signature: entry.spec.signature,
                decorators: entry.decorators,
                visibility,
                body_hash,
                text_hash,
                doc_hash: entry.doc,
            });
        }
        Declared {
            defs,
            owners,
            facts,
            module_types,
            declarators,
        }
    }
}

/// Whether a child of a container body is a definition of its own.
fn declares(
    node: Node<'_>,
    owners: &FxHashMap<usize, LocalKey>,
    declarators: &FxHashSet<usize>,
) -> bool {
    if node.kind() == "decorator" {
        return true;
    }
    let mut current = node;
    loop {
        let next = match current.kind() {
            "export_statement" => field(current, "declaration"),
            "ambient_declaration" => first_named(current),
            _ => break,
        };
        match next {
            Some(inner) => current = inner,
            None => return false,
        }
    }
    owners.contains_key(&current.id())
        || kids(current)
            .iter()
            .any(|child| declarators.contains(&child.id()))
}

fn hashes(
    src: &str,
    spec: &Spec<'_>,
    owners: &FxHashMap<usize, LocalKey>,
    declarators: &FxHashSet<usize>,
) -> (u64, u64) {
    match spec.body {
        Body::Absent => (0, hash_text(text(src, spec.node).as_bytes())),
        Body::Whole(body) => {
            let mut stream = Vec::new();
            tokens(src, body, &mut stream);
            (
                hash_tokens(stream.iter().map(|t| t.as_bytes())).unwrap_or(0),
                hash_text(text(src, spec.node).as_bytes()),
            )
        }
        Body::Members(body) => {
            let mut stream = Vec::new();
            let mut layout = src
                .get(spec.node.start_byte()..body.start_byte())
                .unwrap_or("")
                .to_string();
            for child in kids(body) {
                if declares(child, owners, declarators) {
                    continue;
                }
                tokens(src, child, &mut stream);
                if child.kind() != "comment" {
                    layout.push_str(text(src, child));
                    layout.push('\u{1}');
                }
            }
            (
                hash_tokens(stream.iter().map(|t| t.as_bytes())).unwrap_or(0),
                hash_text(layout.as_bytes()),
            )
        }
    }
}

// ---- the reading pass --------------------------------------------------

#[derive(Clone, Copy, Default)]
struct Ctx {
    /// Set on an argument of a call (or a JSX attribute value): the callee's
    /// path, interned.
    arg: Option<u32>,
}

enum Task<'t> {
    Visit(Node<'t>, Ctx),
    Leave { scope: bool, types: bool },
}

enum Root {
    Identifier,
    This,
    Super,
    CallResult(Vec<Name>),
    New(TypeName),
    Other,
}

struct Chain<'t> {
    qualifier: Vec<Name>,
    receiver: Receiver,
    inner: Option<Node<'t>>,
}

#[derive(Default)]
struct Bindings {
    names: Vec<(Name, Option<Name>)>,
    star: Option<Option<Name>>,
    default: Option<Name>,
}

type TypeFrame = FxHashMap<String, Option<TypeName>>;

struct Walker<'t> {
    src: &'t str,
    owners: &'t FxHashMap<usize, LocalKey>,
    declarators: &'t FxHashSet<usize>,
    facts: Vec<Fact>,
    scopes: Vec<Scope>,
    types: Vec<TypeFrame>,
    callees: Vec<Vec<Name>>,
    tasks: Vec<Task<'t>>,
}

impl<'t> Walker<'t> {
    fn run(mut self, root: Node<'t>, budget: &RunBudget, ticker: &mut Ticker) -> Vec<Fact> {
        self.tasks.push(Task::Visit(root, Ctx::default()));
        while let Some(task) = self.tasks.pop() {
            if ticker.over(budget) {
                break;
            }
            match task {
                Task::Leave { scope, types } => {
                    if scope {
                        self.scopes.pop();
                    }
                    if types {
                        self.types.pop();
                    }
                }
                Task::Visit(node, ctx) => self.visit(node, ctx),
            }
        }
        self.facts
    }

    fn t(&self, node: Node<'_>) -> &'t str {
        text(self.src, node)
    }

    fn current(&self) -> Scope {
        self.scopes.last().cloned().unwrap_or(Scope::Module)
    }

    /// Children go on the task stack reversed, so they come off in source
    /// order. A handler that leaves a scope or a type frame pushes its
    /// `Leave` first and its children after.
    fn push_children(&mut self, items: Vec<(Node<'t>, Ctx)>) {
        for (node, ctx) in items.into_iter().rev() {
            self.tasks.push(Task::Visit(node, ctx));
        }
    }

    fn callee_id(&mut self, path: Vec<Name>) -> u32 {
        self.callees.push(path);
        (self.callees.len() - 1) as u32
    }

    fn lookup(&self, key: &str) -> Option<TypeName> {
        self.types
            .iter()
            .rev()
            .find_map(|frame| frame.get(key))
            .and_then(Clone::clone)
    }

    fn reference(
        &mut self,
        at: Node<'t>,
        name: &str,
        qualifier: Vec<Name>,
        receiver: Receiver,
        use_kind: UseKind,
    ) {
        if name.is_empty() {
            return;
        }
        self.facts.push(Fact::Reference(Ref {
            site: span(at),
            enclosing: self.current(),
            receiver,
            use_kind,
            name: Name::from(name),
            qualifier,
        }));
    }

    fn local(&mut self, ident: Node<'t>, kind: LocalKind) {
        self.facts.push(Fact::LocalBinding(Local {
            name: Name::from(self.t(ident)),
            site: span(ident),
            enclosing: self.current(),
            kind,
        }));
    }

    fn visit(&mut self, node: Node<'t>, ctx: Ctx) {
        if let Some(key) = self.owners.get(&node.id()) {
            self.scopes.push(Scope::Def(key.clone()));
            self.tasks.push(Task::Leave {
                scope: true,
                types: false,
            });
        }
        match node.kind() {
            "comment"
            | "string"
            | "regex"
            | "number"
            | "import_statement"
            | "jsx_closing_element"
            | "jsx_text"
            | "hash_bang_line" => {}
            "identifier" | "shorthand_property_identifier" => self.identifier(node, ctx),
            "type_identifier" => {
                let name = self.t(node);
                self.reference(node, name, Vec::new(), Receiver::None, UseKind::TypeUse);
            }
            "nested_type_identifier" => self.nested_type(node),
            "member_expression" => self.member(node, ctx),
            "call_expression" => self.call(node),
            "new_expression" => self.new_expression(node),
            "assignment_expression" | "augmented_assignment_expression" => self.assignment(node),
            "update_expression" => {
                let mut children = Vec::new();
                if let Some(argument) = field(node, "argument") {
                    self.target(argument, &mut children);
                }
                self.push_children(children);
            }
            "arrow_function"
            | "function_expression"
            | "generator_function"
            | "function_declaration"
            | "generator_function_declaration"
            | "method_definition"
            | "function_signature"
            | "method_signature"
            | "abstract_method_signature" => {
                self.function(node, ctx);
            }
            "class" | "class_declaration" | "abstract_class_declaration" => self.class(node),
            "public_field_definition" | "field_definition" => self.field_definition(node),
            "variable_declarator" => self.declarator(node),
            "formal_parameters" => {
                let mut frame = TypeFrame::default();
                let mut children = Vec::new();
                self.parameters(node, false, &mut frame, &mut children);
                self.push_children(children);
            }
            "decorator" => self.decorator(node),
            "jsx_self_closing_element" | "jsx_opening_element" => self.jsx(node),
            "pair" => {
                let mut children = Vec::new();
                if let Some(key) =
                    field(node, "key").filter(|k| k.kind() == "computed_property_name")
                {
                    children.push((key, Ctx::default()));
                }
                children.extend(field(node, "value").map(|value| (value, ctx)));
                self.push_children(children);
            }
            "object"
            | "array"
            | "spread_element"
            | "parenthesized_expression"
            | "await_expression"
            | "non_null_expression" => self.passthrough(node, ctx, false),
            "as_expression" | "satisfies_expression" => self.passthrough(node, ctx, true),
            "ternary_expression" => {
                let mut children = Vec::new();
                for child in kids(node) {
                    if !child.is_named() || child.kind() == "comment" {
                        continue;
                    }
                    let inherits = [field(node, "consequence"), field(node, "alternative")]
                        .iter()
                        .flatten()
                        .any(|branch| branch.id() == child.id());
                    children.push((child, if inherits { ctx } else { Ctx::default() }));
                }
                self.push_children(children);
            }
            "binary_expression" => {
                let passes =
                    field(node, "operator").is_some_and(|op| matches!(self.t(op), "||" | "??"));
                if passes {
                    self.passthrough(node, ctx, false);
                } else {
                    self.generic(node);
                }
            }
            "export_statement" => {
                let children = kids(node)
                    .into_iter()
                    .filter(|c| {
                        c.is_named()
                            && !matches!(c.kind(), "export_clause" | "namespace_export" | "comment")
                    })
                    .map(|c| (c, Ctx::default()))
                    .collect();
                self.push_children(children);
            }
            "class_heritage" => self.heritage(node),
            "extends_clause" => self.extends_clause(node),
            "catch_clause" => self.catch_clause(node),
            "for_in_statement" => self.for_in(node),
            "interface_declaration"
            | "enum_declaration"
            | "type_alias_declaration"
            | "internal_module"
            | "module"
            | "enum_assignment"
            | "type_parameter"
            | "index_signature"
            | "mapped_type_clause"
            | "property_signature" => {
                self.generic_except_name(node);
            }
            _ => self.generic(node),
        }
    }

    fn generic(&mut self, node: Node<'t>) {
        let children = kids(node)
            .into_iter()
            .filter(|c| c.is_named())
            .map(|c| (c, Ctx::default()))
            .collect();
        self.push_children(children);
    }

    fn generic_except_name(&mut self, node: Node<'t>) {
        let name = field(node, "name").map(|n| n.id());
        let children = kids(node)
            .into_iter()
            .filter(|c| c.is_named() && Some(c.id()) != name)
            .map(|c| (c, Ctx::default()))
            .collect();
        self.push_children(children);
    }

    fn passthrough(&mut self, node: Node<'t>, ctx: Ctx, first_only: bool) {
        let mut first = true;
        let mut children = Vec::new();
        for child in kids(node) {
            if !child.is_named() || child.kind() == "comment" {
                continue;
            }
            let inherit = !first_only || first;
            first = false;
            children.push((child, if inherit { ctx } else { Ctx::default() }));
        }
        self.push_children(children);
    }

    fn identifier(&mut self, node: Node<'t>, ctx: Ctx) {
        let name = self.t(node);
        if name == "undefined" {
            return;
        }
        let use_kind = if ctx.arg.is_some() {
            UseKind::PassAsValue
        } else {
            UseKind::Read
        };
        self.reference(node, name, Vec::new(), Receiver::None, use_kind);
    }

    fn nested_type(&mut self, node: Node<'t>) {
        let (Some(module), Some(name)) = (field(node, "module"), field(node, "name")) else {
            return self.generic(node);
        };
        let qualifier = squeeze(self.t(module)).split('.').map(Name::from).collect();
        let written = self.t(name);
        self.reference(
            name,
            written,
            qualifier,
            Receiver::Unknown,
            UseKind::TypeUse,
        );
    }

    fn chain(&self, object: Node<'t>) -> Chain<'t> {
        let mut path: Vec<Name> = Vec::new();
        let mut inner = None;
        let mut current = object;
        let root = loop {
            match current.kind() {
                "identifier" => {
                    path.push(Name::from(self.t(current)));
                    break Root::Identifier;
                }
                "this" => break Root::This,
                "super" => break Root::Super,
                "member_expression" => {
                    match (field(current, "object"), field(current, "property")) {
                        (Some(next), Some(property)) => {
                            path.push(Name::from(self.t(property)));
                            current = next;
                        }
                        _ => {
                            inner = Some(current);
                            break Root::Other;
                        }
                    }
                }
                "parenthesized_expression"
                | "non_null_expression"
                | "as_expression"
                | "satisfies_expression" => match first_named(current) {
                    Some(next) => current = next,
                    None => {
                        inner = Some(current);
                        break Root::Other;
                    }
                },
                "call_expression" => {
                    inner = Some(current);
                    break Root::CallResult(path_of(self.src, current));
                }
                "new_expression" => {
                    inner = Some(current);
                    break match value_type(self.src, current) {
                        Some(class) => Root::New(class),
                        None => Root::Other,
                    };
                }
                _ => {
                    inner = Some(current);
                    break Root::Other;
                }
            }
        };
        path.reverse();
        let (qualifier, receiver) = match root {
            Root::Identifier => {
                let receiver = match path.as_slice() {
                    [only] => self
                        .lookup(only.as_str())
                        .map_or(Receiver::Unknown, Receiver::Typed),
                    _ => Receiver::Unknown,
                };
                (path, receiver)
            }
            Root::This => {
                let receiver = match path.as_slice() {
                    [] => Receiver::SelfType,
                    [member] => self
                        .lookup(&format!("this.{member}"))
                        .map_or(Receiver::Unknown, Receiver::Typed),
                    _ => Receiver::Unknown,
                };
                if path.is_empty() {
                    (path, receiver)
                } else {
                    let mut qualifier = vec![Name::from("this")];
                    qualifier.extend(path);
                    (qualifier, receiver)
                }
            }
            Root::Super => {
                let mut qualifier = vec![Name::from("super")];
                qualifier.extend(path);
                (qualifier, Receiver::SelfType)
            }
            Root::CallResult(callee) => (path, Receiver::CallResult(callee)),
            Root::New(class) => (path, Receiver::New(class)),
            Root::Other => (path, Receiver::Unknown),
        };
        Chain {
            qualifier,
            receiver,
            inner,
        }
    }

    fn emit_member(&mut self, property: Node<'t>, chain: Chain<'t>, use_kind: UseKind) {
        let name = self.t(property);
        let Chain {
            qualifier,
            receiver,
            inner,
        } = chain;
        self.reference(property, name, qualifier, receiver, use_kind);
        if let Some(inner) = inner {
            self.tasks.push(Task::Visit(inner, Ctx::default()));
        }
    }

    fn member(&mut self, node: Node<'t>, ctx: Ctx) {
        let (Some(object), Some(property)) = (field(node, "object"), field(node, "property"))
        else {
            return self.generic(node);
        };
        let chain = self.chain(object);
        let use_kind = if ctx.arg.is_some() {
            UseKind::PassAsValue
        } else {
            UseKind::Read
        };
        self.emit_member(property, chain, use_kind);
    }

    fn call(&mut self, node: Node<'t>) {
        let (Some(function), Some(arguments)) = (field(node, "function"), field(node, "arguments"))
        else {
            return self.generic(node);
        };
        let mut children: Vec<(Node<'t>, Ctx)> = Vec::new();
        let mut arguments_done = false;
        match function.kind() {
            "import" if arguments.kind() == "arguments" => {
                arguments_done = self.import_call(node, arguments, true);
            }
            "identifier" if self.t(function) == "require" && arguments.kind() == "arguments" => {
                arguments_done = self.import_call(node, arguments, false);
            }
            "identifier" => {
                let name = self.t(function);
                self.reference(function, name, Vec::new(), Receiver::None, UseKind::Call);
            }
            "member_expression" => {
                if let (Some(object), Some(property)) =
                    (field(function, "object"), field(function, "property"))
                {
                    let chain = self.chain(object);
                    self.emit_member(property, chain, UseKind::Call);
                } else {
                    children.push((function, Ctx::default()));
                }
            }
            "super" => self.reference(function, "super", Vec::new(), Receiver::None, UseKind::Call),
            _ => children.push((function, Ctx::default())),
        }
        if arguments.kind() == "arguments" {
            if !arguments_done {
                let id = self.callee_id(path_of(self.src, function));
                for argument in kids(arguments) {
                    if argument.is_named() && argument.kind() != "comment" {
                        children.push((argument, Ctx { arg: Some(id) }));
                    }
                }
            }
        } else {
            children.push((arguments, Ctx::default()));
        }
        children.extend(field(node, "type_arguments").map(|t| (t, Ctx::default())));
        self.push_children(children);
    }

    /// A `require('m')` or `import('m')` call with a literal specifier; false
    /// when the specifier is not one, so that the arguments are read as code.
    fn import_call(&mut self, call: Node<'t>, arguments: Node<'t>, dynamic: bool) -> bool {
        let first = kids(arguments)
            .into_iter()
            .find(|c| c.is_named() && c.kind() != "comment");
        let Some(module) = first.and_then(|n| string_value(self.src, n)) else {
            return false;
        };
        let Bindings {
            names,
            star,
            default,
        } = self.import_bindings(call, dynamic);
        self.facts.push(Fact::Import(Import {
            site: span(call),
            enclosing: self.written_scope(call),
            relative: relative_of(&module),
            module: ModulePath::from(module),
            kind: if dynamic {
                ImportKind::Dynamic
            } else {
                ImportKind::Require
            },
            names,
            star,
            default,
        }));
        true
    }

    /// Where the import is written: a module-level `const x = require(…)` is
    /// the module's own code, though the constant owns what its value holds.
    fn written_scope(&self, call: Node<'t>) -> Scope {
        let mut current = call;
        while let Some(parent) = current.parent() {
            match parent.kind() {
                "await_expression"
                | "parenthesized_expression"
                | "as_expression"
                | "non_null_expression"
                | "satisfies_expression" => current = parent,
                "member_expression"
                    if field(parent, "object").is_some_and(|o| o.id() == current.id()) =>
                {
                    current = parent;
                }
                "variable_declarator" if self.declarators.contains(&parent.id()) => {
                    return Scope::Module;
                }
                _ => break,
            }
        }
        self.current()
    }

    /// What the result of the import call is bound to: a name, a pattern, a
    /// member of it, or the default of a `lazy(() => import(…))` constant.
    fn import_bindings(&self, call: Node<'t>, dynamic: bool) -> Bindings {
        let mut current = call;
        let mut member: Option<String> = None;
        loop {
            let Some(parent) = current.parent() else {
                return Bindings::default();
            };
            match parent.kind() {
                "await_expression"
                | "parenthesized_expression"
                | "as_expression"
                | "non_null_expression"
                | "satisfies_expression" => current = parent,
                "member_expression"
                    if member.is_none()
                        && field(parent, "object").is_some_and(|o| o.id() == current.id()) =>
                {
                    member = field(parent, "property").map(|p| self.t(p).to_string());
                    current = parent;
                }
                "variable_declarator"
                    if field(parent, "value").is_some_and(|v| v.id() == current.id()) =>
                {
                    return self.declarator_bindings(parent, member);
                }
                "arrow_function"
                    if dynamic
                        && member.is_none()
                        && field(parent, "body").is_some_and(|b| b.id() == current.id()) =>
                {
                    return self.lazy_bindings(parent);
                }
                _ => return Bindings::default(),
            }
        }
    }

    fn declarator_bindings(&self, declarator: Node<'t>, member: Option<String>) -> Bindings {
        let Some(name) = field(declarator, "name") else {
            return Bindings::default();
        };
        let mut bindings = Bindings::default();
        match (name.kind(), member) {
            ("identifier", Some(member)) => {
                let local = Name::from(self.t(name));
                if member == "default" {
                    bindings.default = Some(local);
                } else {
                    let alias = (local.as_str() != member).then_some(local);
                    bindings.names.push((Name::from(member), alias));
                }
            }
            ("identifier", None) => bindings.star = Some(Some(Name::from(self.t(name)))),
            ("object_pattern", None) => {
                for entry in kids(name) {
                    match entry.kind() {
                        "shorthand_property_identifier_pattern" => {
                            bindings.names.push((Name::from(self.t(entry)), None));
                        }
                        "object_assignment_pattern" => {
                            if let Some(left) = field(entry, "left") {
                                bindings.names.push((Name::from(self.t(left)), None));
                            }
                        }
                        "pair_pattern" => {
                            let (Some(key), Some(value)) =
                                (field(entry, "key"), field(entry, "value"))
                            else {
                                continue;
                            };
                            let target = if value.kind() == "assignment_pattern" {
                                field(value, "left")
                            } else {
                                Some(value)
                            };
                            let alias = target
                                .filter(|t| t.kind() == "identifier")
                                .map(|t| Name::from(self.t(t)));
                            bindings.names.push((Name::from(self.t(key)), alias));
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        bindings
    }

    /// `const Card = lazy(() => import('./Card'))`: `lazy` resolves the
    /// module's default export, so the constant is that export.
    fn lazy_bindings(&self, arrow: Node<'t>) -> Bindings {
        let call = arrow
            .parent()
            .filter(|p| p.kind() == "arguments")
            .and_then(|a| a.parent())
            .filter(|c| c.kind() == "call_expression");
        let Some(call) = call else {
            return Bindings::default();
        };
        let lazy = field(call, "function")
            .map(|f| path_of(self.src, f))
            .and_then(|mut path| path.pop())
            .is_some_and(|last| last.as_str() == "lazy");
        let declarator = call
            .parent()
            .filter(|p| p.kind() == "variable_declarator")
            .and_then(|d| field(d, "name"))
            .filter(|n| n.kind() == "identifier");
        match (lazy, declarator) {
            (true, Some(name)) => Bindings {
                default: Some(Name::from(self.t(name))),
                ..Bindings::default()
            },
            _ => Bindings::default(),
        }
    }

    fn new_expression(&mut self, node: Node<'t>) {
        let Some(constructor) = field(node, "constructor") else {
            return self.generic(node);
        };
        let mut children: Vec<(Node<'t>, Ctx)> = Vec::new();
        match constructor.kind() {
            "identifier" => {
                let name = self.t(constructor);
                self.reference(
                    constructor,
                    name,
                    Vec::new(),
                    Receiver::None,
                    UseKind::Instantiate,
                );
            }
            "member_expression" => {
                if let (Some(object), Some(property)) =
                    (field(constructor, "object"), field(constructor, "property"))
                {
                    let chain = self.chain(object);
                    self.emit_member(property, chain, UseKind::Instantiate);
                } else {
                    children.push((constructor, Ctx::default()));
                }
            }
            _ => children.push((constructor, Ctx::default())),
        }
        if let Some(arguments) = field(node, "arguments") {
            let id = self.callee_id(path_of(self.src, constructor));
            for argument in kids(arguments) {
                if argument.is_named() && argument.kind() != "comment" {
                    children.push((argument, Ctx { arg: Some(id) }));
                }
            }
        }
        children.extend(field(node, "type_arguments").map(|t| (t, Ctx::default())));
        self.push_children(children);
    }

    fn assignment(&mut self, node: Node<'t>) {
        let (Some(left), Some(right)) = (field(node, "left"), field(node, "right")) else {
            return self.generic(node);
        };
        let mut children = Vec::new();
        self.target(left, &mut children);
        children.push((right, Ctx::default()));
        self.push_children(children);
    }

    /// The left side of an assignment: names it writes, and the expressions
    /// inside it that are read.
    fn target(&mut self, left: Node<'t>, children: &mut Vec<(Node<'t>, Ctx)>) {
        match left.kind() {
            "identifier" => {
                let name = self.t(left);
                self.reference(left, name, Vec::new(), Receiver::None, UseKind::Write);
            }
            "member_expression" => {
                let (Some(object), Some(property)) =
                    (field(left, "object"), field(left, "property"))
                else {
                    return;
                };
                let path = path_of(self.src, left);
                let names: Vec<&str> = path.iter().map(Name::as_str).collect();
                if matches!(
                    names.as_slice(),
                    ["module", "exports", ..] | ["exports", ..]
                ) {
                    return;
                }
                let chain = self.chain(object);
                self.emit_member(property, chain, UseKind::Write);
            }
            "object_pattern" | "array_pattern" => {
                for bound in pattern_bounds(left) {
                    match bound {
                        Bound::Ident(ident) => {
                            let name = self.t(ident);
                            self.reference(ident, name, Vec::new(), Receiver::None, UseKind::Write);
                        }
                        Bound::Expr(expr) => children.push((expr, Ctx::default())),
                        Bound::Member(member) => self.target(member, children),
                    }
                }
            }
            "parenthesized_expression" | "non_null_expression" => {
                if let Some(inner) = first_named(left) {
                    self.target(inner, children);
                }
            }
            _ => children.push((left, Ctx::default())),
        }
    }

    fn function(&mut self, node: Node<'t>, ctx: Ctx) {
        let kind = node.kind();
        let owner = self.owners.contains_key(&node.id());
        let declaration = matches!(
            kind,
            "function_declaration" | "generator_function_declaration"
        );
        let signature_only = matches!(
            kind,
            "function_signature" | "method_signature" | "abstract_method_signature"
        );
        let mut callback = false;
        let passed = ctx
            .arg
            .filter(|_| !owner && !declaration && !signature_only);
        if let Some(id) = passed {
            let callee = self.callees.get(id as usize).cloned().unwrap_or_default();
            let parent = Box::new(self.current());
            self.scopes.push(Scope::Callback {
                callee,
                site: span(node),
                parent,
            });
            callback = true;
        }
        if let Some(name) = field(node, "name").filter(|_| !owner && declaration) {
            self.local(name, LocalKind::Closure);
        }
        let has_body = field(node, "body").is_some();
        let mut frame = TypeFrame::default();
        let mut children: Vec<(Node<'t>, Ctx)> = Vec::new();
        if let Some(parameters) = field(node, "parameters") {
            self.parameters(parameters, has_body, &mut frame, &mut children);
        } else if let Some(parameter) = field(node, "parameter").filter(|_| has_body) {
            self.local(parameter, LocalKind::Parameter);
            frame.insert(self.t(parameter).to_string(), None);
        }
        for name in ["type_parameters", "return_type", "body"] {
            children.extend(field(node, name).map(|c| (c, Ctx::default())));
        }
        for child in kids(node) {
            if child.kind() == "decorator" {
                children.push((child, Ctx::default()));
            }
        }
        if let Some(name) = field(node, "name").filter(|n| n.kind() == "computed_property_name") {
            children.push((name, Ctx::default()));
        }
        self.types.push(frame);
        self.tasks.push(Task::Leave {
            scope: callback,
            types: true,
        });
        self.push_children(children);
    }

    fn parameters(
        &mut self,
        list: Node<'t>,
        bind: bool,
        frame: &mut TypeFrame,
        children: &mut Vec<(Node<'t>, Ctx)>,
    ) {
        for parameter in kids(list) {
            if !parameter.is_named() || parameter.kind() == "comment" {
                continue;
            }
            if matches!(
                parameter.kind(),
                "required_parameter" | "optional_parameter"
            ) {
                let pattern = field(parameter, "pattern")
                    .or_else(|| field(parameter, "name"))
                    .filter(|_| bind);
                if let Some(pattern) = pattern {
                    let annotated = (pattern.kind() == "identifier")
                        .then(|| field(parameter, "type"))
                        .flatten()
                        .and_then(|t| type_name(self.src, t));
                    self.bind_parameter(pattern, annotated, frame, children);
                }
                for name in ["type", "value"] {
                    children.extend(field(parameter, name).map(|c| (c, Ctx::default())));
                }
                for child in kids(parameter) {
                    if child.kind() == "decorator" {
                        children.push((child, Ctx::default()));
                    }
                }
            } else if bind {
                self.bind_parameter(parameter, None, frame, children);
            }
        }
    }

    fn bind_parameter(
        &mut self,
        pattern: Node<'t>,
        annotated: Option<TypeName>,
        frame: &mut TypeFrame,
        children: &mut Vec<(Node<'t>, Ctx)>,
    ) {
        let mut annotated = annotated;
        for bound in pattern_bounds(pattern) {
            match bound {
                Bound::Ident(ident) => {
                    self.local(ident, LocalKind::Parameter);
                    frame.insert(self.t(ident).to_string(), annotated.take());
                }
                Bound::Expr(expr) => children.push((expr, Ctx::default())),
                Bound::Member(_) => {}
            }
        }
    }

    fn class(&mut self, node: Node<'t>) {
        let owner = self.owners.contains_key(&node.id());
        let local_class = !owner && node.kind() != "class";
        if let Some(name) = field(node, "name").filter(|_| local_class) {
            self.local(name, LocalKind::Closure);
        }
        let mut children: Vec<(Node<'t>, Ctx)> = Vec::new();
        for child in kids(node) {
            if matches!(child.kind(), "decorator" | "class_heritage") {
                children.push((child, Ctx::default()));
            }
        }
        for name in ["type_parameters", "body"] {
            children.extend(field(node, name).map(|c| (c, Ctx::default())));
        }
        let frame =
            field(node, "body").map_or_else(TypeFrame::default, |b| class_frame(self.src, b));
        self.types.push(frame);
        self.tasks.push(Task::Leave {
            scope: false,
            types: true,
        });
        self.push_children(children);
    }

    fn field_definition(&mut self, node: Node<'t>) {
        let mut children: Vec<(Node<'t>, Ctx)> = Vec::new();
        for child in kids(node) {
            if child.kind() == "decorator" {
                children.push((child, Ctx::default()));
            }
        }
        for name in ["name", "property"] {
            children.extend(
                field(node, name)
                    .filter(|n| n.kind() == "computed_property_name")
                    .map(|n| (n, Ctx::default())),
            );
        }
        for name in ["type", "value"] {
            children.extend(field(node, name).map(|c| (c, Ctx::default())));
        }
        self.push_children(children);
    }

    fn declarator(&mut self, node: Node<'t>) {
        let name = field(node, "name");
        let value = field(node, "value");
        let annotation = field(node, "type");
        let mut children: Vec<(Node<'t>, Ctx)> = Vec::new();
        if let Some(name) = name {
            let module_level = self.declarators.contains(&node.id());
            let simple = name.kind() == "identifier";
            let closure = value.and_then(function_value).is_some();
            let tname = declared_type(
                self.src,
                annotation,
                value,
                is_const_declaration(node.parent()),
            );
            for bound in pattern_bounds(name) {
                match bound {
                    Bound::Ident(ident) if !module_level => {
                        let kind = if simple && closure {
                            LocalKind::Closure
                        } else {
                            LocalKind::Variable
                        };
                        self.local(ident, kind);
                        let typed = if simple { tname.clone() } else { None };
                        let key = self.t(ident).to_string();
                        if let Some(frame) = self.types.last_mut() {
                            frame.insert(key, typed);
                        }
                    }
                    Bound::Expr(expr) => children.push((expr, Ctx::default())),
                    _ => {}
                }
            }
        }
        children.extend(annotation.map(|a| (a, Ctx::default())));
        children.extend(value.map(|v| (v, Ctx::default())));
        self.push_children(children);
    }

    fn decorator(&mut self, node: Node<'t>) {
        let Some(target) = first_named(node) else {
            return;
        };
        let mut children: Vec<(Node<'t>, Ctx)> = Vec::new();
        let callee = match target.kind() {
            "call_expression" => field(target, "function"),
            _ => Some(target),
        };
        if let Some(callee) = callee {
            match callee.kind() {
                "identifier" => {
                    let name = self.t(callee);
                    self.reference(callee, name, Vec::new(), Receiver::None, UseKind::Decorate);
                }
                "member_expression" => {
                    if let (Some(object), Some(property)) =
                        (field(callee, "object"), field(callee, "property"))
                    {
                        let chain = self.chain(object);
                        self.emit_member(property, chain, UseKind::Decorate);
                    }
                }
                _ => children.push((callee, Ctx::default())),
            }
        }
        let call_arguments =
            field(target, "arguments").filter(|_| target.kind() == "call_expression");
        if let Some(arguments) = call_arguments {
            let id = self.callee_id(path_of(self.src, callee.unwrap_or(target)));
            for argument in kids(arguments) {
                if argument.is_named() && argument.kind() != "comment" {
                    children.push((argument, Ctx { arg: Some(id) }));
                }
            }
        }
        self.push_children(children);
    }

    /// `class A extends B` in a JavaScript grammar, where the heritage holds
    /// the expression itself; in TypeScript it holds clauses.
    fn heritage(&mut self, node: Node<'t>) {
        let mut children: Vec<(Node<'t>, Ctx)> = Vec::new();
        for child in kids(node) {
            if !child.is_named() || child.kind() == "comment" {
                continue;
            }
            self.type_use(child, &mut children);
        }
        self.push_children(children);
    }

    fn extends_clause(&mut self, node: Node<'t>) {
        let values: Vec<usize> = {
            let mut cursor = node.walk();
            node.children_by_field_name("value", &mut cursor)
                .map(|n| n.id())
                .collect()
        };
        let mut children: Vec<(Node<'t>, Ctx)> = Vec::new();
        for child in kids(node) {
            if !child.is_named() || child.kind() == "comment" {
                continue;
            }
            if values.contains(&child.id()) {
                self.type_use(child, &mut children);
            } else {
                children.push((child, Ctx::default()));
            }
        }
        self.push_children(children);
    }

    /// A name written where a class is extended: a type use, not a read.
    fn type_use(&mut self, node: Node<'t>, children: &mut Vec<(Node<'t>, Ctx)>) {
        match node.kind() {
            "identifier" => {
                let name = self.t(node);
                self.reference(node, name, Vec::new(), Receiver::None, UseKind::TypeUse);
            }
            "member_expression" => {
                if let (Some(object), Some(property)) =
                    (field(node, "object"), field(node, "property"))
                {
                    let chain = self.chain(object);
                    self.emit_member(property, chain, UseKind::TypeUse);
                }
            }
            _ => children.push((node, Ctx::default())),
        }
    }

    fn jsx(&mut self, node: Node<'t>) {
        let mut callee = Vec::new();
        if let Some(name) = field(node, "name") {
            callee = path_of(self.src, name);
            match name.kind() {
                "identifier" => {
                    let written = self.t(name);
                    if written.starts_with(|c: char| c.is_ascii_uppercase()) {
                        self.reference(name, written, Vec::new(), Receiver::None, UseKind::Call);
                    }
                }
                "member_expression" => {
                    if let (Some(object), Some(property)) =
                        (field(name, "object"), field(name, "property"))
                    {
                        let chain = self.chain(object);
                        self.emit_member(property, chain, UseKind::Call);
                    }
                }
                _ => {}
            }
        }
        let passed = Ctx {
            arg: Some(self.callee_id(callee)),
        };
        let mut children: Vec<(Node<'t>, Ctx)> = Vec::new();
        for child in kids(node) {
            match child.kind() {
                "jsx_attribute" => {
                    for value in kids(child) {
                        match value.kind() {
                            "jsx_expression" => children.extend(
                                kids(value)
                                    .into_iter()
                                    .filter(|e| e.is_named() && e.kind() != "comment")
                                    .map(|e| (e, passed)),
                            ),
                            "jsx_element" | "jsx_self_closing_element" => {
                                children.push((value, Ctx::default()));
                            }
                            _ => {}
                        }
                    }
                }
                "jsx_expression" => children.extend(
                    kids(child)
                        .into_iter()
                        .filter(|e| e.is_named() && e.kind() != "comment")
                        .map(|e| (e, passed)),
                ),
                "type_arguments" => children.push((child, Ctx::default())),
                _ => {}
            }
        }
        self.push_children(children);
    }

    fn catch_clause(&mut self, node: Node<'t>) {
        let mut children: Vec<(Node<'t>, Ctx)> = Vec::new();
        if let Some(parameter) = field(node, "parameter") {
            for bound in pattern_bounds(parameter) {
                match bound {
                    Bound::Ident(ident) => self.local(ident, LocalKind::Variable),
                    Bound::Expr(expr) => children.push((expr, Ctx::default())),
                    Bound::Member(_) => {}
                }
            }
        }
        for name in ["type", "body"] {
            children.extend(field(node, name).map(|c| (c, Ctx::default())));
        }
        self.push_children(children);
    }

    fn for_in(&mut self, node: Node<'t>) {
        let mut children: Vec<(Node<'t>, Ctx)> = Vec::new();
        if let Some(left) = field(node, "left") {
            if field(node, "kind").is_some() {
                for bound in pattern_bounds(left) {
                    match bound {
                        Bound::Ident(ident) => self.local(ident, LocalKind::Variable),
                        Bound::Expr(expr) => children.push((expr, Ctx::default())),
                        Bound::Member(_) => {}
                    }
                }
            } else {
                self.target(left, &mut children);
            }
        }
        for name in ["right", "body"] {
            children.extend(field(node, name).map(|c| (c, Ctx::default())));
        }
        self.push_children(children);
    }
}

/// The types `this.<field>` has in a class, as far as the fields and the
/// constructor's promoted parameters say (#611).
fn class_frame(src: &str, body: Node<'_>) -> TypeFrame {
    let mut frame = TypeFrame::default();
    for member in kids(body) {
        match member.kind() {
            "public_field_definition" | "field_definition" => {
                let Some((_, name)) = member_name(src, member) else {
                    continue;
                };
                let found = field(member, "type")
                    .and_then(|t| type_name(src, t))
                    .or_else(|| field(member, "value").and_then(|v| value_type(src, v)));
                if let Some(found) = found {
                    frame.insert(format!("this.{name}"), Some(found));
                }
            }
            "method_definition" => {
                let is_constructor =
                    member_name(src, member).is_some_and(|(_, name)| name == "constructor");
                let Some(parameters) = field(member, "parameters").filter(|_| is_constructor)
                else {
                    continue;
                };
                for parameter in kids(parameters) {
                    let promoted = has_token(parameter, "readonly")
                        || kids(parameter)
                            .iter()
                            .any(|c| c.kind() == "accessibility_modifier");
                    let name = field(parameter, "pattern")
                        .or_else(|| field(parameter, "name"))
                        .filter(|n| promoted && n.kind() == "identifier");
                    let found = field(parameter, "type").and_then(|t| type_name(src, t));
                    if let (Some(name), Some(found)) = (name, found) {
                        frame.insert(format!("this.{}", text(src, name)), Some(found));
                    }
                }
            }
            _ => {}
        }
    }
    frame
}
