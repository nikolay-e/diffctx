//! Kotlin facts from the fwcd grammar (`brokk-tree-sitter-kotlin`), read once
//! per blob and shared by the fragmenter, the JVM edge builder, `--symbol`
//! and impact: the declared package, the imports with their aliases, every
//! top-level and member declaration with its owners, receiver, visibility,
//! declared type, header and annotations, the supertypes each type names,
//! every use site (call, property access, type use, callable reference) with
//! the receiver it is reached through, the local bindings that can shadow a
//! name, and the bytes that are text rather than code.
//!
//! The package is the one the file declares: Kotlin does not tie it to the
//! directory, so a path is never the namespace here. Names keep their case:
//! `UI.DP` and `ui.dp` are different names in Kotlin.

use std::ops::Range;
use std::sync::{Arc, Mutex};

use once_cell::sync::Lazy;
use rustc_hash::FxHashMap;
use tree_sitter::{Node, Tree};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Import {
    /// The dotted path as written: `com.acme.tags.tagFor`.
    pub path: String,
    /// `import … as local`: the name the file uses instead.
    pub alias: Option<String>,
    pub wildcard: bool,
    pub line: u32,
}

impl Import {
    /// The name this import brings into the file.
    pub fn local_name(&self) -> Option<&str> {
        if self.wildcard {
            return None;
        }
        self.alias
            .as_deref()
            .or_else(|| self.path.rsplit('.').next())
    }

    /// The imported declaration's own name.
    pub fn target_name(&self) -> &str {
        self.path.rsplit('.').next().unwrap_or(&self.path)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Visibility {
    Private,
    Protected,
    Internal,
    Public,
}

#[derive(Debug, Clone)]
pub struct Declaration {
    pub name: String,
    /// `class`, `interface`, `object`, `companion`, `fun`, `property`,
    /// `constructor` or `typealias`.
    pub kind: &'static str,
    /// Enclosing types, outermost first.
    pub owners: Vec<String>,
    /// `fun String.pad()`: the receiver type, which is not an owner.
    pub receiver: Option<String>,
    /// A property's or a function's declared type, outer name only.
    pub type_name: Option<String>,
    /// A property with no written type, initialized by a call.
    pub init_call: Option<InitCall>,
    /// What the declaration states itself.
    pub visibility: Option<Visibility>,
    /// What its owners allow on top: the narrowest of all. `None` for an
    /// `override` that states none — it inherits a visibility the file does
    /// not show.
    pub effective: Option<Visibility>,
    pub is_override: bool,
    /// An `inner class`, which sees the instance that holds it.
    pub inner: bool,
    /// A member with no body: an interface or `abstract` method, which every
    /// implementation has to provide.
    pub abstract_member: bool,
    pub annotations: Vec<String>,
    /// Parameters a call has to pass: those without a default. `None` for
    /// what takes no parameter list.
    pub required_params: Option<usize>,
    pub params: Option<usize>,
    pub start_line: u32,
    pub end_line: u32,
    pub span: Range<usize>,
    /// The declaration up to its body (`{` or `=`), parameter defaults and
    /// all; the whole declaration when it has no body.
    pub header: Range<usize>,
    /// The header's text with whitespace collapsed: what a caller compiles
    /// against, compared across revisions.
    pub signature: String,
}

impl Declaration {
    /// What identifies the declaration across revisions of its file.
    pub fn key(&self) -> (&[String], &str, &'static str, Option<&str>) {
        (
            &self.owners,
            &self.name,
            self.kind,
            self.receiver.as_deref(),
        )
    }

    pub fn is_type(&self) -> bool {
        matches!(
            self.kind,
            "class" | "interface" | "object" | "companion" | "typealias"
        )
    }

    pub fn is_member(&self) -> bool {
        !self.owners.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UseKind {
    Call,
    Access,
    TypeUse,
    /// `::f`, `x::f`, `Type::f`: the function named, not called.
    Reference,
}

/// What a use is reached through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Receiver {
    /// `x.f()`, `Type.f()`, `x::f`: a single name.
    Name(String),
    /// `this.f()`.
    This,
    /// `a.b.f()`: a dotted chain of names.
    Path(Vec<String>),
    /// `make(…).f()`: the result of a plain call.
    Call(String),
    /// Anything else: a call result, an index, a literal.
    Expr,
}

#[derive(Debug, Clone)]
pub struct Use {
    pub name: String,
    pub kind: UseKind,
    pub receiver: Option<Receiver>,
    pub at: usize,
    pub line: u32,
}

/// A local name — a parameter, a local `val`, a lambda or loop variable —
/// visible from `start` to the end of `scope`.
/// `val x = Make(…)`, `val x = make(…)` or `val x = repo.find(…)`: the call
/// a value with no written type takes its type from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitCall {
    /// A plain name the callee is called on.
    pub receiver: Option<String>,
    pub callee: String,
}

#[derive(Debug, Clone)]
pub struct Binding {
    pub name: String,
    pub type_name: Option<String>,
    /// The call that types it when no type is written.
    pub init_call: Option<InitCall>,
    pub start: usize,
    pub scope: Range<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct KotlinFacts {
    pub package: Option<String>,
    pub imports: Vec<Import>,
    pub declarations: Vec<Declaration>,
    /// Each type declared here with the supertypes it names, outer names
    /// only (`Repo<T>` is `Repo`, `a.b.Base` is `Base`).
    pub supertypes: Vec<(String, Vec<String>)>,
    pub uses: Vec<Use>,
    pub bindings: Vec<Binding>,
    /// Comments and string text; `${…}` and `$name` inside a string are code.
    pub text: Vec<Range<usize>>,
    /// Lines with an ERROR or MISSING node: what was read elsewhere is right,
    /// what sits there may be missing.
    pub error_lines: Vec<u32>,
}

impl KotlinFacts {
    pub fn partial(&self) -> bool {
        !self.error_lines.is_empty()
    }

    /// The innermost declaration holding a byte.
    pub fn declaration_at_byte(&self, at: usize) -> Option<&Declaration> {
        self.declarations
            .iter()
            .filter(|d| d.span.contains(&at))
            .min_by_key(|d| d.span.len())
    }

    /// The declaration a fragment starting at `line` stands for.
    pub fn declaration_named(&self, name: &str, line: u32) -> Option<&Declaration> {
        self.declarations
            .iter()
            .filter(|d| d.name == name && d.start_line <= line && d.end_line >= line)
            .min_by_key(|d| d.span.len())
    }

    /// The import that binds a local name, alias resolved.
    pub fn imported(&self, local: &str) -> Option<&Import> {
        self.imports.iter().find(|i| i.local_name() == Some(local))
    }

    /// The local binding of `name` visible at `at`: the innermost scope wins.
    pub fn binding_at(&self, name: &str, at: usize) -> Option<&Binding> {
        self.bindings
            .iter()
            .filter(|b| b.name == name && b.start <= at && b.scope.contains(&at))
            .min_by_key(|b| b.scope.len())
    }

    pub fn is_text(&self, byte: usize) -> bool {
        self.text.iter().any(|r| r.contains(&byte))
    }

    /// The type declared here, by its name and owners.
    pub fn type_named(&self, name: &str) -> Option<&Declaration> {
        self.declarations
            .iter()
            .find(|d| d.is_type() && d.name == name)
    }

    pub fn supertypes_of(&self, name: &str) -> &[String] {
        self.supertypes
            .iter()
            .find(|(n, _)| n == name)
            .map_or(&[][..], |(_, s)| s.as_slice())
    }
}

const CACHE_LIMIT: usize = 8192;

static CACHE: Lazy<Mutex<FxHashMap<u64, Arc<KotlinFacts>>>> =
    Lazy::new(|| Mutex::new(FxHashMap::default()));

fn key(content: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = rustc_hash::FxHasher::default();
    content.hash(&mut h);
    h.finish()
}

fn remember(content: &str, facts: Arc<KotlinFacts>) {
    if let Ok(mut cache) = CACHE.lock() {
        if cache.len() >= CACHE_LIMIT {
            cache.clear();
        }
        cache.insert(key(content), facts);
    }
}

/// The facts of one blob: from the parse the fragmenter already made of the
/// same bytes when there was one, else from a parse of their own. `None`
/// when the grammar is not compiled in or the parse timed out.
pub fn facts(content: &str) -> Option<Arc<KotlinFacts>> {
    if let Some(hit) = CACHE
        .lock()
        .ok()
        .and_then(|c| c.get(&key(content)).cloned())
    {
        return Some(hit);
    }
    let tree = crate::parsers::parse_tree("kotlin", content)?;
    Some(record(&tree, content))
}

/// Facts walked off a tree the caller parsed, kept for every later reader
/// of the same bytes.
pub fn record(tree: &Tree, content: &str) -> Arc<KotlinFacts> {
    let facts = Arc::new(extract(tree.root_node(), content.as_bytes()));
    remember(content, Arc::clone(&facts));
    facts
}

type Owners = Vec<(String, Option<Visibility>)>;

fn extract(root: Node, bytes: &[u8]) -> KotlinFacts {
    let mut facts = KotlinFacts::default();
    let mut stack: Vec<(Node, Owners)> = vec![(root, Vec::new())];
    while let Some((node, owners)) = stack.pop() {
        if node.is_error() || node.is_missing() {
            facts.error_lines.push(line_of(node));
        }
        let mut inner = owners.clone();
        match node.kind() {
            "package_header" => {
                facts.package = child(node, "identifier").map(|i| dotted(i, bytes));
                continue;
            }
            "import_header" => {
                if let Some(path) = child(node, "identifier").map(|i| dotted(i, bytes)) {
                    facts.imports.push(Import {
                        path,
                        alias: child(node, "import_alias").and_then(|a| name_of(&a, bytes)),
                        wildcard: child(node, "wildcard_import").is_some(),
                        line: line_of(node),
                    });
                }
                continue;
            }
            "line_comment" | "multiline_comment" => {
                facts.text.push(node.byte_range());
                continue;
            }
            "string_literal" | "character_literal" => string_text(node, &mut facts.text),
            _ => {}
        }
        if let Some(kind) = declaration_kind(&node) {
            if is_local(&node) {
                local_binding(&node, bytes, &mut facts.bindings);
            } else {
                let mut decl = declaration(&node, kind, &owners, bytes);
                decl.signature = squash(bytes, decl.header.clone());
                if decl.is_type() && decl.kind != "typealias" {
                    facts
                        .supertypes
                        .push((decl.name.clone(), supertypes(&node, bytes)));
                    inner.push((decl.name.clone(), decl.visibility));
                }
                facts.declarations.push(decl);
            }
        }
        match node.kind() {
            "class_parameter" => constructor_property(&node, &owners, bytes, &mut facts),
            "parameter" | "lambda_parameters" | "for_statement" | "catch_block" => {
                parameter_bindings(&node, bytes, &mut facts.bindings);
            }
            _ => {}
        }
        uses_at(&node, bytes, &mut facts.uses);
        let mut cursor = node.walk();
        let children: Vec<Node> = node.children(&mut cursor).collect();
        for c in children.into_iter().rev() {
            stack.push((c, inner.clone()));
        }
    }
    facts
        .declarations
        .sort_by_key(|d| (d.span.start, std::cmp::Reverse(d.span.end)));
    facts.uses.sort_by_key(|u| u.at);
    // A grammar can misread a declaration without an error node (a bodyless
    // annotated class swallowed the next one in tree-sitter-kotlin-sg): a
    // type the text declares at the start of a line that the tree lacks is
    // a line the parse did not read.
    facts.error_lines.extend(unread_type_lines(bytes, &facts));
    facts.error_lines.sort_unstable();
    facts.error_lines.dedup();
    facts
}

static TYPE_LINE: Lazy<regex::Regex> = Lazy::new(|| {
    regex::Regex::new(
        r"(?m)^[ \t]*(?:@\w+(?:\([^)\n]*\))?[ \t]+)*(?:(?:public|private|internal|protected|abstract|open|sealed|data|enum|annotation|inner|value|final|fun|expect|actual)[ \t]+)*(?:class|interface|object)[ \t]+([A-Za-z_]\w*)",
    )
    .expect("type line regex")
});

fn unread_type_lines(bytes: &[u8], facts: &KotlinFacts) -> Vec<u32> {
    let text = String::from_utf8_lossy(bytes);
    // Local classes are bindings, not exported declarations, yet read.
    let declared: std::collections::HashSet<&str> = facts
        .declarations
        .iter()
        .filter(|d| d.is_type())
        .map(|d| d.name.as_str())
        .chain(facts.bindings.iter().map(|b| b.name.as_str()))
        .collect();
    TYPE_LINE
        .captures_iter(&text)
        .filter_map(|c| {
            let name = c.get(1)?;
            (!facts.is_text(name.start()) && !declared.contains(name.as_str()))
                .then(|| text[..name.start()].matches('\n').count() as u32 + 1)
        })
        .collect()
}

/// The declaration's name: its first identifier child, backticks dropped.
/// The fwcd grammar names nothing by field.
pub fn name_of(node: &Node, bytes: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    let found = node
        .children(&mut cursor)
        .find(|c| matches!(c.kind(), "simple_identifier" | "type_identifier"))?;
    text(found, bytes).map(|t| t.trim_matches('`').to_string())
}

/// A declaration inside a function, a lambda, an `init` block or an
/// accessor is a local: nothing outside reaches it.
pub fn is_local(node: &Node) -> bool {
    std::iter::successors(node.parent(), Node::parent)
        .take_while(|p| !matches!(p.kind(), "class_body" | "enum_class_body" | "source_file"))
        .any(|p| {
            matches!(
                p.kind(),
                "function_body"
                    | "lambda_literal"
                    | "anonymous_initializer"
                    | "getter"
                    | "setter"
                    | "secondary_constructor"
            )
        })
}

/// The kind a declaration node stands for, `None` for anything else.
pub fn declaration_kind(node: &Node) -> Option<&'static str> {
    Some(match node.kind() {
        "class_declaration" if has_token(node, "interface") => "interface",
        "class_declaration" => "class",
        "object_declaration" => "object",
        "companion_object" => "companion",
        "function_declaration" => "fun",
        "property_declaration" => "property",
        "secondary_constructor" => "constructor",
        "type_alias" => "typealias",
        _ => return None,
    })
}

/// The name a declaration node goes by: a property its variable, an unnamed
/// companion `Companion`, a secondary constructor its class.
pub fn declared_name(node: &Node, bytes: &[u8]) -> Option<String> {
    match node.kind() {
        "property_declaration" => {
            child(*node, "variable_declaration").and_then(|v| name_of(&v, bytes))
        }
        "companion_object" => name_of(node, bytes).or_else(|| Some("Companion".to_string())),
        "secondary_constructor" => std::iter::successors(node.parent(), Node::parent)
            .find(|p| matches!(p.kind(), "class_declaration" | "object_declaration"))
            .and_then(|owner| name_of(&owner, bytes)),
        _ => name_of(node, bytes),
    }
}

/// A property spans its `get()`/`set()`, which the grammar leaves as its
/// next siblings.
pub fn end_with_accessors(node: &Node) -> (u32, usize) {
    let mut end = (node.end_position().row as u32 + 1, node.end_byte());
    if node.kind() == "property_declaration" {
        let mut next = node.next_named_sibling();
        while let Some(n) = next.filter(|n| matches!(n.kind(), "getter" | "setter")) {
            end = (n.end_position().row as u32 + 1, n.end_byte());
            next = n.next_named_sibling();
        }
    }
    end
}

fn declaration(node: &Node, kind: &'static str, owners: &Owners, bytes: &[u8]) -> Declaration {
    let name = declared_name(node, bytes).unwrap_or_default();
    let modifiers = child(*node, "modifiers");
    let visibility = modifiers.and_then(|m| visibility_of(m, bytes));
    let is_override = modifiers.is_some_and(|m| has_word(m, bytes, "override"));
    let effective = effective_visibility(visibility, is_override, owners);
    let body = body_of(node);
    let owner_is_interface = node
        .parent()
        .and_then(|p| p.parent())
        .is_some_and(|t| declaration_kind(&t) == Some("interface"));
    let abstract_member = kind == "fun"
        && body.is_none()
        && (owner_is_interface || modifiers.is_some_and(|m| has_word(m, bytes, "abstract")));
    let (end_line, end_byte) = end_with_accessors(node);
    let (params, required_params) = child(*node, "function_value_parameters")
        .map(|p| count_params(p))
        .map_or((None, None), |(a, r)| (Some(a), Some(r)));
    Declaration {
        name,
        kind,
        owners: owners.iter().map(|(n, _)| n.clone()).collect(),
        receiver: node
            .child_by_field_name("receiver")
            .and_then(|r| last_type_name(r, bytes)),
        type_name: declared_type(node, bytes),
        init_call: initializer_call(node, bytes),
        visibility,
        effective,
        is_override,
        inner: modifiers.is_some_and(|m| has_word(m, bytes, "inner")),
        abstract_member,
        annotations: modifiers.map(|m| annotations(m, bytes)).unwrap_or_default(),
        required_params,
        params,
        start_line: line_of(*node),
        end_line,
        span: node.start_byte()..end_byte,
        header: node.start_byte()..body.map_or_else(|| header_end(node), |b| b.start_byte()),
        signature: String::new(),
    }
}

fn squash(bytes: &[u8], range: Range<usize>) -> String {
    String::from_utf8_lossy(&bytes[range.start.min(bytes.len())..range.end.min(bytes.len())])
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn effective_visibility(
    own: Option<Visibility>,
    is_override: bool,
    owners: &Owners,
) -> Option<Visibility> {
    let bound = owners
        .iter()
        .map(|(_, v)| v.unwrap_or(Visibility::Public))
        .min()
        .unwrap_or(Visibility::Public);
    match (own, is_override) {
        // An override inherits a visibility the file does not spell, but
        // never reaches past its owners.
        (None, true) => (bound < Visibility::Public).then_some(bound),
        (own, _) => Some(own.unwrap_or(Visibility::Public).min(bound)),
    }
}

/// `class C(private val repo: Repo)`: a `val`/`var` parameter is a property
/// of `C`; a plain one only reaches the initializers.
fn constructor_property(node: &Node, owners: &Owners, bytes: &[u8], facts: &mut KotlinFacts) {
    if child(*node, "binding_pattern_kind").is_none() || is_local(node) {
        return;
    }
    let Some(name) = name_of(node, bytes) else {
        return;
    };
    let modifiers = child(*node, "modifiers");
    let visibility = modifiers.and_then(|m| visibility_of(m, bytes));
    let is_override = modifiers.is_some_and(|m| has_word(m, bytes, "override"));
    facts.declarations.push(Declaration {
        name,
        kind: "property",
        owners: owners.iter().map(|(n, _)| n.clone()).collect(),
        receiver: None,
        type_name: first_child_type(node, bytes),
        init_call: None,
        visibility,
        effective: effective_visibility(visibility, is_override, owners),
        is_override,
        inner: false,
        abstract_member: false,
        annotations: modifiers.map(|m| annotations(m, bytes)).unwrap_or_default(),
        required_params: None,
        params: None,
        start_line: line_of(*node),
        end_line: node.end_position().row as u32 + 1,
        span: node.byte_range(),
        header: node.byte_range(),
        signature: squash(bytes, node.byte_range()),
    });
}

/// A local `val`/`var` (or a local `fun`, which shadows by name too).
fn local_binding(node: &Node, bytes: &[u8], bindings: &mut Vec<Binding>) {
    let Some(name) = declared_name(node, bytes) else {
        return;
    };
    let Some(scope) = enclosing_scope(node) else {
        return;
    };
    // A local `val` is not in scope in its own initializer, so
    // `val p = p(…)` calls the outer `p`; a local `fun` is, for recursion.
    let start = if node.kind() == "property_declaration" {
        node.end_byte()
    } else {
        node.start_byte()
    };
    bindings.push(Binding {
        name,
        type_name: declared_type(node, bytes),
        init_call: initializer_call(node, bytes),
        start,
        scope,
    });
}

/// Parameters, lambda parameters, loop and `catch` variables.
fn parameter_bindings(node: &Node, bytes: &[u8], bindings: &mut Vec<Binding>) {
    let scope = match node.kind() {
        "parameter" => std::iter::successors(node.parent(), Node::parent)
            .find(|p| {
                matches!(
                    p.kind(),
                    "function_declaration" | "secondary_constructor" | "anonymous_function"
                )
            })
            .map(|f| f.byte_range()),
        "lambda_parameters" => node.parent().map(|l| l.byte_range()),
        _ => Some(node.byte_range()),
    };
    let Some(scope) = scope else {
        return;
    };
    let mut push = |n: Node| {
        if let Some(name) = name_of(&n, bytes) {
            bindings.push(Binding {
                name,
                type_name: first_child_type(&n, bytes),
                init_call: None,
                start: scope.start,
                scope: scope.clone(),
            });
        }
    };
    match node.kind() {
        "parameter" => push(*node),
        _ => {
            let mut cursor = node.walk();
            let vars: Vec<Node> = node
                .children(&mut cursor)
                .filter(|c| c.kind() == "variable_declaration")
                .collect();
            for v in vars {
                push(v);
            }
            if node.kind() == "catch_block" {
                let mut cursor = node.walk();
                if let Some(id) = node
                    .children(&mut cursor)
                    .find(|c| c.kind() == "simple_identifier")
                {
                    if let Some(name) = text(id, bytes) {
                        bindings.push(Binding {
                            name: name.to_string(),
                            type_name: first_child_type(node, bytes),
                            init_call: None,
                            start: scope.start,
                            scope: scope.clone(),
                        });
                    }
                }
            }
        }
    }
}

fn enclosing_scope(node: &Node) -> Option<Range<usize>> {
    std::iter::successors(node.parent(), Node::parent)
        .find(|p| {
            matches!(
                p.kind(),
                "statements"
                    | "function_body"
                    | "lambda_literal"
                    | "control_structure_body"
                    | "anonymous_initializer"
            )
        })
        .map(|s| s.byte_range())
}

/// The uses a node is the head of. Each use is recorded once, at the node
/// that decides its kind.
fn uses_at(node: &Node, bytes: &[u8], uses: &mut Vec<Use>) {
    let mut push = |name: Option<String>, kind: UseKind, receiver: Option<Receiver>, at: Node| {
        if let Some(name) = name.filter(|n| !n.is_empty()) {
            uses.push(Use {
                name,
                kind,
                receiver,
                at: at.start_byte(),
                line: line_of(at),
            });
        }
    };
    match node.kind() {
        "navigation_expression" | "directly_assignable_expression" => {
            let Some(suffix) = child(*node, "navigation_suffix") else {
                return;
            };
            let Some(name_node) = child(suffix, "simple_identifier") else {
                return;
            };
            let called = node.parent().is_some_and(|p| {
                p.kind() == "call_expression"
                    && p.named_child(0).is_some_and(|c| c.id() == node.id())
            });
            let receiver = node.named_child(0).map(|r| receiver_of(r, bytes));
            push(
                text(name_node, bytes).map(str::to_string),
                if called {
                    UseKind::Call
                } else {
                    UseKind::Access
                },
                receiver,
                name_node,
            );
        }
        "call_expression" => {
            if let Some(callee) = node
                .named_child(0)
                .filter(|c| c.kind() == "simple_identifier")
            {
                push(
                    text(callee, bytes).map(str::to_string),
                    UseKind::Call,
                    None,
                    callee,
                );
            }
        }
        "callable_reference" => {
            let mut cursor = node.walk();
            let named: Vec<Node> = node.named_children(&mut cursor).collect();
            let (receiver, target) = match named.as_slice() {
                [target] => (None, *target),
                [recv, .., target] => {
                    // `Obj::f` reads `Obj` as much as it names `f`.
                    push(
                        text(*recv, bytes).map(str::to_string),
                        UseKind::Access,
                        None,
                        *recv,
                    );
                    (
                        text(*recv, bytes).map(|t| Receiver::Name(t.to_string())),
                        *target,
                    )
                }
                [] => return,
            };
            push(
                text(target, bytes).map(str::to_string),
                UseKind::Reference,
                receiver,
                target,
            );
        }
        "user_type" => {
            let mut cursor = node.walk();
            let path: Vec<Node> = node
                .children(&mut cursor)
                .filter(|c| c.kind() == "type_identifier")
                .collect();
            if let Some(last) = path.last() {
                let receiver = (path.len() > 1).then(|| {
                    Receiver::Path(
                        path[..path.len() - 1]
                            .iter()
                            .filter_map(|p| text(*p, bytes).map(str::to_string))
                            .collect(),
                    )
                });
                push(
                    text(*last, bytes).map(str::to_string),
                    UseKind::TypeUse,
                    receiver,
                    *last,
                );
            }
        }
        "interpolated_identifier" => {
            push(
                text(*node, bytes).map(str::to_string),
                UseKind::Access,
                None,
                *node,
            );
        }
        "simple_identifier" if is_bare_reference(node) => {
            push(
                text(*node, bytes).map(str::to_string),
                UseKind::Access,
                None,
                *node,
            );
        }
        _ => {}
    }
}

/// A plain name read as a value: not a declaration's name, not a member
/// after a dot, not a callee (a call records it), not a named argument.
fn is_bare_reference(node: &Node) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    let first = parent.named_child(0).is_some_and(|c| c.id() == node.id());
    match parent.kind() {
        "call_expression" => false,
        // `x.f`: `x` is a receiver, read as a value.
        "navigation_expression" | "directly_assignable_expression" => first,
        "value_argument" => !(first && parent.named_child_count() > 1),
        "statements"
        | "jump_expression"
        | "assignment"
        | "parenthesized_expression"
        | "additive_expression"
        | "multiplicative_expression"
        | "comparison_expression"
        | "equality_expression"
        | "conjunction_expression"
        | "disjunction_expression"
        | "elvis_expression"
        | "infix_expression"
        | "range_expression"
        | "prefix_expression"
        | "postfix_expression"
        | "as_expression"
        | "check_expression"
        | "if_expression"
        | "when_subject"
        | "when_condition"
        | "control_structure_body"
        | "indexing_suffix"
        | "spread_operator"
        | "interpolated_expression"
        | "collection_literal"
        | "property_declaration"
        | "function_body"
        | "range_test"
        | "type_test"
        | "indexing_expression"
        | "property_delegate"
        | "explicit_delegation" => true,
        _ => false,
    }
}

fn receiver_of(node: Node, bytes: &[u8]) -> Receiver {
    match node.kind() {
        "simple_identifier" | "type_identifier" => text(node, bytes)
            .map(|t| Receiver::Name(t.to_string()))
            .unwrap_or(Receiver::Expr),
        "this_expression" => Receiver::This,
        "call_expression" => node
            .named_child(0)
            .filter(|c| c.kind() == "simple_identifier")
            .and_then(|c| text(c, bytes))
            .map_or(Receiver::Expr, |callee| Receiver::Call(callee.to_string())),
        "navigation_expression" => {
            let mut parts = Vec::new();
            let mut current = node;
            loop {
                let Some(name) = child(current, "navigation_suffix")
                    .and_then(|s| child(s, "simple_identifier"))
                    .and_then(|n| text(n, bytes))
                else {
                    return Receiver::Expr;
                };
                parts.push(name.to_string());
                match current.named_child(0) {
                    Some(inner) if inner.kind() == "navigation_expression" => current = inner,
                    Some(inner) if inner.kind() == "simple_identifier" => {
                        if let Some(head) = text(inner, bytes) {
                            parts.push(head.to_string());
                        }
                        break;
                    }
                    Some(inner) if inner.kind() == "this_expression" => {
                        parts.push("this".to_string());
                        break;
                    }
                    _ => return Receiver::Expr,
                }
            }
            parts.reverse();
            Receiver::Path(parts)
        }
        _ => Receiver::Expr,
    }
}

fn body_of<'a>(node: &Node<'a>) -> Option<Node<'a>> {
    let mut cursor = node.walk();
    let found = node.children(&mut cursor).find(|c| {
        matches!(
            c.kind(),
            "function_body" | "class_body" | "enum_class_body" | "statements"
        )
    });
    found
}

/// Without a body the header ends where the declaration does, short of a
/// property's initializer or delegate.
fn header_end(node: &Node) -> usize {
    let mut cursor = node.walk();
    let found = node
        .children(&mut cursor)
        .find(|c| matches!(c.kind(), "=" | "property_delegate"))
        .map(|c| c.start_byte());
    found.unwrap_or_else(|| node.end_byte())
}

/// A property's or function's declared type: the type after `:` (a
/// function's return type, a property's variable type).
fn declared_type(node: &Node, bytes: &[u8]) -> Option<String> {
    match node.kind() {
        "property_declaration" => {
            child(*node, "variable_declaration").and_then(|v| first_child_type(&v, bytes))
        }
        "function_declaration" => first_child_type(node, bytes),
        _ => None,
    }
}

/// `val x = Make(…)`: the callee of an initializer that is a plain call.
fn initializer_call(node: &Node, bytes: &[u8]) -> Option<InitCall> {
    if node.kind() != "property_declaration" {
        return None;
    }
    let mut cursor = node.walk();
    let children: Vec<Node> = node.children(&mut cursor).collect();
    let after_eq = children
        .iter()
        .position(|c| c.kind() == "=")
        .and_then(|i| children.get(i + 1))?;
    let call = if after_eq.kind() == "call_expression" {
        *after_eq
    } else {
        return None;
    };
    let head = call.named_child(0)?;
    match head.kind() {
        "simple_identifier" => Some(InitCall {
            receiver: None,
            callee: text(head, bytes)?.to_string(),
        }),
        "navigation_expression" => {
            let receiver = head
                .named_child(0)
                .filter(|r| r.kind() == "simple_identifier")?;
            let callee =
                child(head, "navigation_suffix").and_then(|s| child(s, "simple_identifier"))?;
            Some(InitCall {
                receiver: Some(text(receiver, bytes)?.to_string()),
                callee: text(callee, bytes)?.to_string(),
            })
        }
        _ => None,
    }
}

fn first_child_type(node: &Node, bytes: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    let found = node
        .children(&mut cursor)
        .find(|c| matches!(c.kind(), "user_type" | "nullable_type"))?;
    last_type_name(found, bytes)
}

fn count_params(params: Node) -> (usize, usize) {
    let mut cursor = params.walk();
    let all: Vec<Node> = params
        .named_children(&mut cursor)
        .filter(|c| c.kind() == "parameter")
        .collect();
    // A default value follows its parameter as a sibling expression.
    let mut defaults = 0;
    let mut cursor = params.walk();
    let children: Vec<Node> = params.children(&mut cursor).collect();
    for w in children.windows(2) {
        if w[0].kind() == "parameter" && w[1].kind() == "=" {
            defaults += 1;
        }
    }
    (all.len(), all.len().saturating_sub(defaults))
}

fn visibility_of(modifiers: Node, bytes: &[u8]) -> Option<Visibility> {
    let mut cursor = modifiers.walk();
    let found = modifiers
        .children(&mut cursor)
        .find(|c| c.kind() == "visibility_modifier")?;
    Some(match text(found, bytes)?.trim() {
        "private" => Visibility::Private,
        "protected" => Visibility::Protected,
        "internal" => Visibility::Internal,
        _ => Visibility::Public,
    })
}

fn has_word(node: Node, bytes: &[u8], word: &str) -> bool {
    text(node, bytes).is_some_and(|t| t.split(|c: char| !c.is_alphanumeric()).any(|w| w == word))
}

fn has_token(node: &Node, token: &str) -> bool {
    let mut cursor = node.walk();
    let found = node.children(&mut cursor).any(|c| c.kind() == token);
    found
}

fn annotations(modifiers: Node, bytes: &[u8]) -> Vec<String> {
    let mut cursor = modifiers.walk();
    modifiers
        .children(&mut cursor)
        .filter(|c| c.kind() == "annotation")
        .filter_map(|a| last_type_name(a, bytes))
        .collect()
}

fn supertypes(node: &Node, bytes: &[u8]) -> Vec<String> {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .filter(|c| c.kind() == "delegation_specifier")
        .filter_map(|d| last_type_name(d, bytes))
        .collect()
}

/// The outer type a subtree names: the first `user_type`'s last segment
/// (`a.b.Base<T>` is `Base`).
fn last_type_name(node: Node, bytes: &[u8]) -> Option<String> {
    let mut stack = vec![node];
    let mut user_type = None;
    while let Some(n) = stack.pop() {
        if n.kind() == "user_type" {
            user_type = Some(n);
            break;
        }
        let mut cursor = n.walk();
        let children: Vec<Node> = n.children(&mut cursor).collect();
        stack.extend(children.into_iter().rev());
    }
    let user_type = user_type?;
    let mut cursor = user_type.walk();
    let last = user_type
        .children(&mut cursor)
        .filter(|c| c.kind() == "type_identifier")
        .last()?;
    text(last, bytes).map(str::to_string)
}

fn string_text(node: Node, out: &mut Vec<Range<usize>>) {
    let mut start = node.start_byte();
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        if matches!(
            c.kind(),
            "interpolated_expression" | "interpolated_identifier"
        ) {
            if c.start_byte() > start {
                out.push(start..c.start_byte());
            }
            start = c.end_byte();
        }
    }
    if node.end_byte() > start {
        out.push(start..node.end_byte());
    }
}

fn child<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    let mut cursor = node.walk();
    let found = node.children(&mut cursor).find(|c| c.kind() == kind);
    found
}

fn dotted(identifier: Node, bytes: &[u8]) -> String {
    let mut cursor = identifier.walk();
    identifier
        .children(&mut cursor)
        .filter(|c| c.kind() == "simple_identifier")
        .filter_map(|c| text(c, bytes))
        .map(|t| t.trim_matches('`'))
        .collect::<Vec<_>>()
        .join(".")
}

fn text<'a>(node: Node, bytes: &'a [u8]) -> Option<&'a str> {
    node.utf8_text(bytes).ok()
}

fn line_of(node: Node) -> u32 {
    node.start_position().row as u32 + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = "package com.acme.matrix\n\
\n\
import com.acme.tags.tagFor as resolveTag\n\
import com.acme.util.*\n\
import com.acme.core.Outer.Inner\n\
\n\
@Service\n\
internal class Worker(private val repo: Port, plain: Int) : Base(), Port by repo, Runnable {\n\
    fun execute(limit: Int = 10, mode: Mode = Mode.FAST): List<Int> = load(limit, mode)\n\
    fun fetch(factory: () -> Item = { Item() }): Item = factory()\n\
    override fun run() {\n\
        val local: Port = repo\n\
        val s = \"a ${resolveTag(1)} $name\"\n\
        local.save(1, Item())\n\
        repo?.save(2, Item())\n\
        listOf(1).forEach { it -> it.inc() }\n\
        run(::fetch, repo::save, Port::save)\n\
    }\n\
    private fun String.pad(): String = \"\"\"raw $x\"\"\"\n\
    companion object {\n\
        const val MAX = 3\n\
    }\n\
}\n\
\n\
interface Port {\n\
    @Throws(IllegalStateException::class)\n\
    fun save(expected: Long, next: Item)\n\
}\n\
\n\
fun `weird name`() = 1 // tagFor in a comment\n";

    fn facts_of() -> Arc<KotlinFacts> {
        facts(SOURCE).expect("kotlin grammar")
    }

    fn decl<'a>(f: &'a KotlinFacts, name: &str) -> &'a Declaration {
        f.declarations
            .iter()
            .find(|d| d.name == name)
            .unwrap_or_else(|| panic!("{name}: {:?}", f.declarations))
    }

    #[test]
    fn package_and_imports_are_the_declared_ones_aliases_included() {
        let f = facts_of();
        assert!(!f.partial());
        assert_eq!(f.package.as_deref(), Some("com.acme.matrix"));
        assert_eq!(
            f.imported("resolveTag").map(|i| i.path.as_str()),
            Some("com.acme.tags.tagFor")
        );
        assert!(
            f.imported("tagFor").is_none(),
            "an alias hides the original"
        );
        assert!(
            f.imports
                .iter()
                .any(|i| i.wildcard && i.path == "com.acme.util")
        );
        assert_eq!(
            f.imported("Inner").map(|i| i.path.as_str()),
            Some("com.acme.core.Outer.Inner")
        );
    }

    #[test]
    fn declarations_carry_owner_receiver_type_and_effective_visibility() {
        let f = facts_of();
        let execute = decl(&f, "execute");
        assert_eq!(execute.owners, ["Worker"]);
        assert_eq!(execute.visibility, None);
        assert_eq!(
            execute.effective,
            Some(Visibility::Internal),
            "a public member of an internal class"
        );
        assert_eq!(execute.required_params, Some(0));
        assert_eq!(execute.params, Some(2));
        assert_eq!(
            decl(&f, "run").effective,
            Some(Visibility::Internal),
            "an override inherits, but not past its internal owner"
        );
        let pad = decl(&f, "pad");
        assert_eq!(pad.receiver.as_deref(), Some("String"));
        assert_eq!(pad.owners, ["Worker"]);
        assert_eq!(decl(&f, "MAX").owners, ["Worker", "Companion"]);
        let repo = decl(&f, "repo");
        assert_eq!(
            (repo.kind, repo.type_name.as_deref()),
            ("property", Some("Port"))
        );
        assert!(
            f.declarations.iter().all(|d| d.name != "plain"),
            "not a property"
        );
        assert!(f.declarations.iter().all(|d| d.name != "local"), "a local");
        let save = decl(&f, "save");
        assert!(save.abstract_member);
        assert_eq!(save.annotations, ["Throws"]);
        assert_eq!(decl(&f, "Worker").annotations, ["Service"]);
        assert_eq!(decl(&f, "weird name").effective, Some(Visibility::Public));
        assert_eq!(
            f.supertypes_of("Worker"),
            [
                "Base".to_string(),
                "Port".to_string(),
                "Runnable".to_string()
            ]
        );
    }

    #[test]
    fn a_header_keeps_its_defaults_and_stops_at_the_body() {
        let f = facts_of();
        let head = |name: &str| SOURCE[decl(&f, name).header.clone()].trim().to_string();
        assert_eq!(
            head("execute"),
            "fun execute(limit: Int = 10, mode: Mode = Mode.FAST): List<Int>"
        );
        assert_eq!(
            head("fetch"),
            "fun fetch(factory: () -> Item = { Item() }): Item"
        );
        assert_eq!(
            head("save"),
            "@Throws(IllegalStateException::class)\nfun save(expected: Long, next: Item)"
        );
    }

    #[test]
    fn uses_carry_kind_and_receiver_and_locals_bind_with_their_type() {
        let f = facts_of();
        let uses = |name: &str| -> Vec<(UseKind, Option<Receiver>)> {
            f.uses
                .iter()
                .filter(|u| u.name == name)
                .map(|u| (u.kind, u.receiver.clone()))
                .collect()
        };
        let save = uses("save");
        assert!(save.contains(&(UseKind::Call, Some(Receiver::Name("local".into())))));
        assert!(save.contains(&(UseKind::Call, Some(Receiver::Name("repo".into())))));
        assert!(save.contains(&(UseKind::Reference, Some(Receiver::Name("repo".into())))));
        assert!(save.contains(&(UseKind::Reference, Some(Receiver::Name("Port".into())))));
        assert!(uses("fetch").contains(&(UseKind::Reference, None)));
        assert!(
            uses("resolveTag").contains(&(UseKind::Call, None)),
            "inside ${{…}}"
        );
        assert!(uses("Item").iter().any(|u| u.0 == UseKind::Call));
        assert!(uses("Port").iter().any(|u| u.0 == UseKind::TypeUse));
        let at = SOURCE.find("local.save").unwrap();
        let local = f.binding_at("local", at).expect("local binding");
        assert_eq!(local.type_name.as_deref(), Some("Port"));
        assert!(
            f.binding_at("local", SOURCE.find("private fun String").unwrap())
                .is_none()
        );
        assert!(
            f.binding_at("limit", SOURCE.find("load(limit").unwrap())
                .is_some()
        );
    }

    #[test]
    fn string_text_and_comments_are_text_and_interpolations_are_code() {
        let f = facts_of();
        let at = |needle: &str| SOURCE.find(needle).expect(needle);
        assert!(!f.is_text(at("resolveTag(1)")), "inside ${{…}}");
        assert!(f.is_text(at("raw ")));
        assert!(f.is_text(at("tagFor in a comment")));
        assert!(!f.is_text(at("`weird name`")), "a backticked name is code");
        assert!(
            f.uses.iter().all(|u| u.name != "tagFor"),
            "a comment is no use"
        );
    }

    #[test]
    fn a_type_the_tree_did_not_read_is_a_parse_gap_and_a_clean_file_has_none() {
        let clean = facts("package a\n\n// class Ghost in a comment\nval s = \"class Phantom\"\n\n@JvmInline\nvalue class Meters(val v: Double)\n\nclass A {\n    companion object Factory\n}\n")
            .expect("kotlin grammar");
        assert!(!clean.partial(), "{:?}", clean.error_lines);
        let mut gap = (*clean).clone();
        gap.declarations.retain(|d| d.name != "A");
        let source = "package a\n\n// class Ghost in a comment\nval s = \"class Phantom\"\n\n@JvmInline\nvalue class Meters(val v: Double)\n\nclass A {\n    companion object Factory\n}\n";
        assert_eq!(unread_type_lines(source.as_bytes(), &gap), [9]);
    }

    #[test]
    fn a_broken_declaration_is_reported_and_the_rest_survives() {
        let f = facts("class A {\n    fun ok() = 1\n}\nclass B {\n    fun broken(: Int {\n}\n")
            .expect("kotlin grammar");
        assert!(f.partial());
        assert!(f.declarations.iter().any(|d| d.name == "ok"));
    }
}
