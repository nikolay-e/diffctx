//! What a Python file's syntax states, as the facts contract names it.
//!
//! Conventions a reader of the facts cannot recover from the types:
//! - `import a.b as c` is an `Import` of module `a.b` with `default: Some(c)`
//!   (the module bound as a name); `import a.b` has `default: None` and binds
//!   `a`. `from . import x` is module `""` with `relative: 1`.
//! - A reference names the last attribute and carries the written chain in
//!   `qualifier`; the chain's head is never a reference of its own. `self.x.f()`
//!   is `f` with qualifier `[self, x]`; only `self.f()` is `Receiver::SelfType`.
//!   A name read as a call argument is `PassAsValue` — a position, not a
//!   binding: resolving it may turn it into a plain read.
//! - A twin of a key (property setter, `@overload`, redefinition) takes
//!   `Overload(n)` in source order, the primary one — the first that is no
//!   overload stub and no accessor — keeping `None`; the one twin under
//!   `if TYPE_CHECKING:` takes `Cfg("TYPE_CHECKING")`. A def's text excludes
//!   its nested definitions and its module- or class-level assignments, which
//!   are definitions of their own.
//! - A `def` or `class` inside a function is a definition only when it leaves
//!   the function (see `escaping_names`); any other is a `Closure` local of the
//!   function, its lines and the uses inside it the function's own.
//! - A module-level statement that changes what a module variable holds
//!   (`X[k] = v`, `X.attr = v`, `del X[k]`, `X += v`, `X.pop(..)` on a dict, list
//!   or set) is part of that variable's hashes (`scan_mutations`).
//! - Hashes are FNV-1a over the definition's own parts: `body_hash` over the
//!   body's tree (node kinds, nesting, leaf text; no comments, no docstring),
//!   `text_hash` over its bytes, `doc_hash` over the docstring's words.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use tree_sitter::{Language as TsLanguage, Node, ParseOptions, ParseState, Parser, Tree};

use crate::facts::framework;
use crate::facts::{
    BlobFacts, ComputedImport, Def, DefKind, Disambig, EntryKind, EntryPoint, Export, Extractor,
    Fact, Hasher, Import, ImportKind, Language, Local, LocalKey, LocalKind, ModulePath, Name,
    Namespace, PathMention, Receiver, Ref, Scope, Span, Star, TypeName, UseKind, Visibility,
    expired, hash_text,
};
use crate::snapshot::{Loss, RepoPath, RunBudget};

const MAX_DEPTH: u16 = 256;
const ERROR_RANGES_KEPT: usize = 32;
const POLL_EVERY: usize = 256;

pub struct PythonExtractor;

impl Extractor for PythonExtractor {
    fn language(&self) -> Language {
        Language::Python
    }

    fn extract(&self, text: &[u8], budget: &RunBudget) -> BlobFacts {
        extract(text, budget)
    }
}

/// A blob names no path, so a loss it raises about itself carries this one
/// for `facts_of` to replace with the file's own.
fn unbound() -> RepoPath {
    RepoPath::new("<blob>").expect("a valid path")
}

#[cfg(feature = "tree-sitter-python")]
fn grammar() -> Option<TsLanguage> {
    Some(TsLanguage::new(tree_sitter_python::LANGUAGE))
}

#[cfg(not(feature = "tree-sitter-python"))]
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
    let mut blob = BlobFacts::new(Language::Python);
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
    if walker.too_deep.get() {
        blob.losses.push(Loss::ExtractorFailed { path: unbound() });
    }
    blob.facts = std::mem::take(&mut walker.facts);
    framework::derive(&mut blob);
    blob.in_source_order();
    blob
}

/// A construct the grammar read whole, whatever lies around it.
fn is_whole(kind: &str) -> bool {
    kind.ends_with("_statement") || kind.ends_with("_definition") || kind == "comment"
}

/// The line ranges the parse could not read, merged. An error node that
/// swallows a whole file is narrowed to its stray tokens: the definitions the
/// grammar read inside it are facts, not errors.
fn error_lines(root: Node) -> Vec<(u32, u32)> {
    let mut ranges = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let mut cursor = node.walk();
        if node.is_error() {
            for child in node.children(&mut cursor) {
                if child.is_error() || child.is_missing() || !is_whole(child.kind()) {
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

/// Tokens of a declaration written for a reader: a space between two tokens
/// unless one hugs the other, so equal declarations print equal.
fn join_tokens(tokens: &[&str]) -> String {
    let word_end = |s: &str| s.ends_with(|c: char| c.is_alphanumeric() || c == '_');
    let mut out = String::new();
    let mut previous: Option<&str> = None;
    for &token in tokens {
        if let Some(a) = previous {
            let tight = matches!(token, ")" | "]" | "}" | "," | ":" | "." | ";")
                || matches!(a, "(" | "[" | "{" | "." | "*" | "**" | "@" | "~")
                || (matches!(token, "(" | "[") && (word_end(a) || a == ")" || a == "]"));
            if !tight {
                out.push(' ');
            }
        }
        out.push_str(token);
        previous = Some(token);
    }
    out
}

fn is_dotted_name(text: &str) -> bool {
    !text.is_empty()
        && text.split('.').all(|part| {
            let mut chars = part.chars();
            chars.next().is_some_and(|c| c.is_alphabetic() || c == '_')
                && chars.all(|c| c.is_alphanumeric() || c == '_')
        })
}

fn is_constant(name: &str) -> bool {
    name.chars().any(|c| c.is_ascii_uppercase())
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

fn visibility(name: &str, in_class: bool, local: bool) -> Visibility {
    let dunder = name.len() > 4 && name.starts_with("__") && name.ends_with("__");
    if local {
        Visibility::Private
    } else if dunder {
        Visibility::Public
    } else if name.starts_with("__") {
        Visibility::Private
    } else if name.starts_with('_') {
        if in_class {
            Visibility::Protected
        } else {
            Visibility::Private
        }
    } else {
        Visibility::Public
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    Plain,
    Overload,
    Accessor,
}

fn decorator_head(decorator: &str) -> &str {
    decorator.split('(').next().unwrap_or(decorator)
}

fn shape_of(decorators: &[String], name: &str) -> Shape {
    for decorator in decorators {
        let head = decorator_head(decorator);
        if head.rsplit('.').next() == Some("overload") {
            return Shape::Overload;
        }
        let accessor = [".setter", ".getter", ".deleter"]
            .iter()
            .any(|suffix| head.strip_suffix(suffix) == Some(name));
        if accessor {
            return Shape::Accessor;
        }
    }
    Shape::Plain
}

fn is_property(decorators: &[String]) -> bool {
    decorators.iter().any(|d| {
        matches!(
            decorator_head(d).rsplit('.').next(),
            Some("property" | "cached_property" | "abstractproperty")
        )
    })
}

fn first_identifier(node: Node) -> Option<Node> {
    if node.kind() == "identifier" {
        return Some(node);
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|c| c.kind() == "identifier")
}

fn first_identifier_deep(node: Node) -> Option<Node> {
    let mut current = node;
    loop {
        if current.kind() == "identifier" {
            return Some(current);
        }
        current = current.named_child(0)?;
    }
}

fn has_child_of_kind(node: Node, kind: &str) -> bool {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).any(|c| c.kind() == kind)
}

/// The methods of a dict, list or set that change it in place.
const MUTATING_METHODS: [&str; 12] = [
    "pop",
    "update",
    "append",
    "extend",
    "insert",
    "remove",
    "clear",
    "setdefault",
    "add",
    "discard",
    "sort",
    "reverse",
];

/// What builds a dict, a list or a set.
const CONTAINER_CONSTRUCTORS: [&str; 7] = [
    "dict",
    "list",
    "set",
    "defaultdict",
    "OrderedDict",
    "Counter",
    "deque",
];

/// Statements and blocks a definition can sit in.
fn is_container(kind: &str) -> bool {
    matches!(
        kind,
        "module"
            | "ERROR"
            | "block"
            | "if_statement"
            | "elif_clause"
            | "else_clause"
            | "for_statement"
            | "while_statement"
            | "try_statement"
            | "except_clause"
            | "except_group_clause"
            | "finally_clause"
            | "with_statement"
            | "match_statement"
            | "case_clause"
    )
}

/// Builtins and library calls that use a callable argument to the end inside
/// the call: a function handed to one goes nowhere.
const RUNS_CALLABLE: [&str; 27] = [
    "map",
    "filter",
    "sorted",
    "min",
    "max",
    "sum",
    "any",
    "all",
    "list",
    "tuple",
    "set",
    "frozenset",
    "zip",
    "enumerate",
    "reversed",
    "next",
    "iter",
    "callable",
    "isinstance",
    "issubclass",
    "print",
    "len",
    "repr",
    "str",
    "bool",
    "type",
    "id",
];
const RUNS_CALLABLE_METHODS: [&str; 8] = [
    "reduce",
    "starmap",
    "takewhile",
    "dropwhile",
    "filterfalse",
    "groupby",
    "accumulate",
    "sort",
];
/// Decorators that return the function they wrap, changed, to the name it was
/// bound to: they hand it nowhere.
const WRAPPING_DECORATORS: [&str; 14] = [
    "wraps",
    "lru_cache",
    "cache",
    "cached_property",
    "partial",
    "contextmanager",
    "asynccontextmanager",
    "staticmethod",
    "classmethod",
    "property",
    "abstractmethod",
    "overload",
    "final",
    "total_ordering",
];
/// Calls that keep their argument in the object they are called on.
const STORES_ARGUMENT: [&str; 7] = [
    "append",
    "extend",
    "add",
    "insert",
    "appendleft",
    "setdefault",
    "update",
];

enum Flow<'t> {
    /// A value a function hands back: out of the definition when that function
    /// is the definition's own or a lambda, else to whoever the named one is
    /// handed to.
    Returned {
        value: Node<'t>,
        from: Option<&'t str>,
    },
    Assigned {
        target: Node<'t>,
        value: Node<'t>,
    },
    Called(Node<'t>),
    Decorated {
        name: &'t str,
        decorator: Node<'t>,
    },
}

/// What a definition is known by once the file is indexed.
#[derive(Clone)]
struct Known {
    key: LocalKey,
    kind: DefKind,
    vis: Visibility,
    decorators: Vec<String>,
    receiver: Option<String>,
}

#[derive(Clone, Default)]
struct Ix {
    container: Vec<String>,
    in_class: bool,
    in_fn: bool,
    type_checking: bool,
    /// Inside a function's body: the names declared there that leave it. A
    /// `def` or `class` among them is a definition, any other a local.
    leaving: Option<Rc<HashSet<String>>>,
}

struct Cand {
    id: usize,
    container: Vec<String>,
    name: String,
    kind: DefKind,
    shape: Shape,
    type_checking: bool,
    statement: Option<usize>,
    variable: bool,
    vis: Visibility,
    decorators: Vec<String>,
    receiver: Option<String>,
}

impl Cand {
    fn group(&self) -> String {
        format!("{}\u{0}{}", self.container.join("\u{1}"), self.name)
    }
}

#[derive(Clone)]
struct Cx {
    scope: Scope,
    in_fn: bool,
    type_only: bool,
    receiver: Option<String>,
    read: UseKind,
}

struct Walker<'t> {
    bytes: &'t [u8],
    budget: &'t RunBudget,
    ticks: usize,
    halted: bool,
    /// A cut of depth; a `Cell` so that the path evaluator, which only reads
    /// the walker, can state its own.
    too_deep: Cell<bool>,
    facts: Vec<Fact>,
    keys: HashMap<usize, Known>,
    vars: HashMap<usize, Known>,
    owned: HashSet<usize>,
    module_values: HashMap<String, Node<'t>>,
    /// The module-level statements that change what a module variable holds,
    /// by the variable (see `scan_mutations`).
    mutations: HashMap<String, Vec<Node<'t>>>,
    types: HashMap<String, TypeName>,
}

impl<'t> Walker<'t> {
    fn new(bytes: &'t [u8], budget: &'t RunBudget) -> Self {
        Self {
            bytes,
            budget,
            ticks: 0,
            halted: false,
            too_deep: Cell::new(false),
            facts: Vec::new(),
            keys: HashMap::new(),
            vars: HashMap::new(),
            owned: HashSet::new(),
            module_values: HashMap::new(),
            mutations: HashMap::new(),
            types: HashMap::new(),
        }
    }

    fn run(&mut self, root: Node<'t>) {
        let mut cands = Vec::new();
        self.collect(root, &Ix::default(), &mut cands, 0);
        self.settle(cands);
        self.scan_mutations(root, 0);
        let cx = Cx {
            scope: Scope::Module,
            in_fn: false,
            type_only: false,
            receiver: None,
            read: UseKind::Read,
        };
        self.visit(root, &cx, 0);
    }

    /// Polls the budget on a cadence; `false` once the run is over.
    fn tick(&mut self) -> bool {
        if self.ticks % POLL_EVERY == 0 && expired(self.budget) {
            self.halted = true;
        }
        self.ticks += 1;
        !self.halted
    }

    fn text(&self, node: Node<'_>) -> &'t str {
        let bytes: &'t [u8] = self.bytes;
        std::str::from_utf8(&bytes[node.start_byte()..node.end_byte()]).unwrap_or("")
    }

    fn is_type_checking(&self, condition: Node<'_>) -> bool {
        let compact: String = self
            .text(condition)
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        compact == "TYPE_CHECKING" || compact.ends_with(".TYPE_CHECKING")
    }

    fn is_main_guard(&self, condition: Node<'_>) -> bool {
        let compact: String = self
            .text(condition)
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        matches!(
            compact.as_str(),
            "__name__==\"__main__\""
                | "__name__=='__main__'"
                | "\"__main__\"==__name__"
                | "'__main__'==__name__"
        )
    }

    fn decorators(&self, wrapper: Option<Node<'t>>) -> Vec<String> {
        let Some(wrapper) = wrapper else {
            return Vec::new();
        };
        let mut cursor = wrapper.walk();
        let mut out = Vec::new();
        for decorator in wrapper
            .named_children(&mut cursor)
            .filter(|c| c.kind() == "decorator")
        {
            let mut tokens = Vec::new();
            self.leaf_tokens(decorator, &mut tokens);
            if tokens.first() == Some(&"@") {
                tokens.remove(0);
            }
            out.push(join_tokens(&tokens));
        }
        out
    }

    /// The leaf tokens of a subtree, comments left out, a string kept whole.
    fn leaf_tokens(&self, node: Node<'t>, out: &mut Vec<&'t str>) {
        let mut stack = vec![node];
        while let Some(n) = stack.pop() {
            if n.kind() == "comment" {
                continue;
            }
            if n.child_count() == 0 || n.kind() == "string" {
                let text = self.text(n);
                if !text.is_empty() {
                    out.push(text);
                }
                continue;
            }
            let mut cursor = n.walk();
            let kids: Vec<Node<'t>> = n.children(&mut cursor).collect();
            stack.extend(kids.into_iter().rev());
        }
    }

    fn simple_chain(&self, node: Node<'t>) -> Option<Vec<&'t str>> {
        let mut parts = Vec::new();
        let mut current = node;
        while current.kind() == "attribute" {
            parts.push(self.text(current.child_by_field_name("attribute")?));
            current = current.child_by_field_name("object")?;
        }
        if current.kind() != "identifier" {
            return None;
        }
        parts.push(self.text(current));
        parts.reverse();
        Some(parts)
    }

    fn plain_string(&self, node: Node<'t>) -> Option<String> {
        if node.kind() != "string" || has_child_of_kind(node, "interpolation") {
            return None;
        }
        let (mut from, mut to) = (node.start_byte(), node.end_byte());
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                "string_start" => from = child.end_byte(),
                "string_end" => to = child.start_byte(),
                _ => {}
            }
        }
        let inner = self.bytes.get(from..to.max(from))?;
        Some(String::from_utf8_lossy(inner).into_owned())
    }

    fn receiver_name(&self, def: Node<'t>, decorators: &[String]) -> Option<String> {
        if decorators.iter().any(|d| d == "staticmethod") {
            return None;
        }
        let first = def.child_by_field_name("parameters")?.named_child(0)?;
        let name = match first.kind() {
            "identifier" => first,
            "typed_parameter" => first_identifier(first)?,
            _ => return None,
        };
        Some(self.text(name).to_string())
    }

    fn docstring(&self, body: Node<'t>) -> Option<Node<'t>> {
        let mut cursor = body.walk();
        let first = body
            .named_children(&mut cursor)
            .find(|c| c.kind() != "comment")?;
        if first.kind() != "expression_statement" || first.named_child_count() != 1 {
            return None;
        }
        let string = first.named_child(0)?;
        (string.kind() == "string" && !has_child_of_kind(string, "interpolation")).then_some(first)
    }

    // ---- what leaves a function

    /// The names a value carries out of an expression: a name, the members of
    /// a container literal, the branches of a conditional, the class an
    /// instance is made from. A call's result carries nothing: the call is a
    /// flow of its own.
    fn carried(&self, node: Node<'t>, classes: &HashSet<&'t str>, out: &mut Vec<&'t str>) {
        let mut cursor = node.walk();
        let parts: Vec<Node<'t>> = match node.kind() {
            "identifier" => {
                out.push(self.text(node));
                return;
            }
            "call" => {
                let class = node
                    .child_by_field_name("function")
                    .filter(|f| f.kind() == "identifier")
                    .map(|f| self.text(f))
                    .filter(|name| classes.contains(name));
                out.extend(class);
                return;
            }
            "dictionary" => node
                .named_children(&mut cursor)
                .filter_map(|member| match member.kind() {
                    "pair" => member.child_by_field_name("value"),
                    "dictionary_splat" => member.named_child(0),
                    _ => None,
                })
                .collect(),
            "list"
            | "tuple"
            | "set"
            | "expression_list"
            | "parenthesized_expression"
            | "list_splat"
            | "await"
            | "yield" => node.named_children(&mut cursor).collect(),
            "keyword_argument" => node.child_by_field_name("value").into_iter().collect(),
            "assignment" => node.child_by_field_name("right").into_iter().collect(),
            "conditional_expression" => [0, 2]
                .into_iter()
                .filter_map(|at| node.named_child(at))
                .collect(),
            "boolean_operator" => ["left", "right"]
                .into_iter()
                .filter_map(|field| node.child_by_field_name(field))
                .collect(),
            _ => return,
        };
        for part in parts {
            self.carried(part, classes, out);
        }
    }

    /// The names a statement's target binds or writes: identifiers, and the
    /// attributes and items written.
    fn leaf_targets(&self, node: Node<'t>, out: &mut Vec<Node<'t>>) {
        match node.kind() {
            "identifier" | "attribute" | "subscript" => out.push(node),
            "pattern_list"
            | "tuple_pattern"
            | "list_pattern"
            | "list_splat_pattern"
            | "as_pattern_target"
            | "parenthesized_expression"
            | "expression_list"
            | "tuple"
            | "list" => {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    self.leaf_targets(child, out);
                }
            }
            _ => {}
        }
    }

    /// The name an attribute or item write is rooted in: `a` for `a.b[c]`.
    fn target_root(&self, node: Node<'t>) -> Option<&'t str> {
        let mut current = node;
        loop {
            match current.kind() {
                "identifier" => return Some(self.text(current)),
                "attribute" => current = current.child_by_field_name("object")?,
                "subscript" => current = current.child_by_field_name("value")?,
                _ => return None,
            }
        }
    }

    fn bind_parameters(
        &self,
        list: Node<'t>,
        declared: &mut HashSet<&'t str>,
        params: &mut HashSet<&'t str>,
    ) {
        let mut cursor = list.walk();
        for parameter in list.named_children(&mut cursor) {
            let bound = match parameter.kind() {
                "identifier" => Some(parameter),
                "typed_parameter" | "list_splat_pattern" | "dictionary_splat_pattern" => {
                    first_identifier(parameter)
                }
                "default_parameter" | "typed_default_parameter" => {
                    parameter.child_by_field_name("name")
                }
                _ => None,
            };
            if let Some(bound) = bound.filter(|b| b.kind() == "identifier") {
                declared.insert(self.text(bound));
                params.insert(self.text(bound));
            }
        }
    }

    /// The nested `def` a `return` or `yield` hands its value back from, when
    /// it is not the definition whose body is `body`.
    fn returning_function(&self, node: Node<'t>, body: Node<'t>) -> Option<&'t str> {
        let mut current = node.parent();
        while let Some(parent) = current {
            if parent.id() == body.id() {
                return None;
            }
            match parent.kind() {
                "function_definition" => {
                    return parent
                        .child_by_field_name("name")
                        .map(|name| self.text(name));
                }
                "lambda" => return None,
                _ => current = parent.parent(),
            }
        }
        None
    }

    /// Whether `name` read at `node` is a parameter of a `def` or lambda
    /// between `node` and the definition's body: it names that parameter, not
    /// a nested function of the same name beside it.
    fn is_parameter_there(&self, node: Node<'t>, body: Node<'t>, name: &str) -> bool {
        let mut current = node.parent();
        while let Some(parent) = current {
            if parent.id() == body.id() {
                return false;
            }
            if matches!(parent.kind(), "function_definition" | "lambda") {
                if let Some(list) = parent.child_by_field_name("parameters") {
                    let mut declared: HashSet<&'t str> = HashSet::new();
                    let mut params: HashSet<&'t str> = HashSet::new();
                    self.bind_parameters(list, &mut declared, &mut params);
                    if params.contains(name) {
                        return true;
                    }
                }
            }
            current = parent.parent();
        }
        false
    }

    /// The names declared in a function's body that leave the function: one it
    /// returns or yields (itself, in a container it returns, or as the class
    /// of a returned instance), writes to something it did not declare (a
    /// `global` or `nonlocal` name, an attribute of `self` or another
    /// parameter, a module's item), hands to a call outside it, or decorates
    /// with something that registers it. A call that runs its callable to the
    /// end (`map`, `sorted(key=…)`, `reduce`) and a decorator that returns the
    /// function it wraps hand it nowhere. A name stored in a local variable
    /// leaves when that variable does.
    fn escaping_names(&self, def: Node<'t>, body: Node<'t>) -> HashSet<String> {
        let mut declared: HashSet<&'t str> = HashSet::new();
        let mut params: HashSet<&'t str> = HashSet::new();
        let mut outward: HashSet<&'t str> = HashSet::new();
        let mut classes: HashSet<&'t str> = HashSet::new();
        let mut flows: Vec<Flow<'t>> = Vec::new();
        if let Some(list) = def.child_by_field_name("parameters") {
            self.bind_parameters(list, &mut declared, &mut params);
        }
        let mut stack = vec![body];
        while let Some(node) = stack.pop() {
            let field = |name: &str| node.child_by_field_name(name);
            match node.kind() {
                "function_definition" | "class_definition" => {
                    if let Some(name) = field("name") {
                        declared.insert(self.text(name));
                        if node.kind() == "class_definition" {
                            classes.insert(self.text(name));
                        }
                    }
                    if let Some(list) = field("parameters") {
                        self.bind_parameters(list, &mut declared, &mut params);
                    }
                }
                "lambda" => {
                    if let Some(list) = field("parameters") {
                        self.bind_parameters(list, &mut declared, &mut params);
                    }
                }
                "decorated_definition" => {
                    let name = field("definition")
                        .and_then(|d| d.child_by_field_name("name"))
                        .map(|n| self.text(n));
                    let mut cursor = node.walk();
                    for decorator in node
                        .named_children(&mut cursor)
                        .filter(|c| c.kind() == "decorator")
                    {
                        if let Some(name) = name {
                            flows.push(Flow::Decorated { name, decorator });
                        }
                    }
                }
                "assignment" | "augmented_assignment" => {
                    if let (Some(target), Some(value)) = (field("left"), field("right")) {
                        let mut leaves = Vec::new();
                        self.leaf_targets(target, &mut leaves);
                        declared.extend(
                            leaves
                                .iter()
                                .filter(|l| l.kind() == "identifier")
                                .map(|l| self.text(*l)),
                        );
                        flows.push(Flow::Assigned { target, value });
                    }
                }
                "named_expression" => {
                    if let (Some(target), Some(value)) = (field("name"), field("value")) {
                        declared.insert(self.text(target));
                        flows.push(Flow::Assigned { target, value });
                    }
                }
                "for_statement" | "for_in_clause" => {
                    let mut leaves = Vec::new();
                    if let Some(left) = field("left") {
                        self.leaf_targets(left, &mut leaves);
                    }
                    declared.extend(
                        leaves
                            .iter()
                            .filter(|l| l.kind() == "identifier")
                            .map(|l| self.text(*l)),
                    );
                }
                "as_pattern" => {
                    let mut leaves = Vec::new();
                    if let Some(alias) = field("alias") {
                        self.leaf_targets(alias, &mut leaves);
                    }
                    declared.extend(leaves.iter().map(|l| self.text(*l)));
                }
                "import_statement" | "import_from_statement" => {
                    let mut cursor = node.walk();
                    for name in node.named_children(&mut cursor) {
                        let bound = match name.kind() {
                            "aliased_import" => name.child_by_field_name("alias"),
                            "dotted_name" => name.named_child(0),
                            _ => None,
                        };
                        declared.extend(bound.map(|b| self.text(b)));
                    }
                }
                "global_statement" | "nonlocal_statement" => {
                    let mut cursor = node.walk();
                    outward.extend(
                        node.named_children(&mut cursor)
                            .filter(|c| c.kind() == "identifier")
                            .map(|c| self.text(c)),
                    );
                }
                "return_statement" | "yield" => {
                    flows.extend(node.named_child(0).map(|value| Flow::Returned {
                        value,
                        from: self.returning_function(node, body),
                    }));
                }
                "call" => flows.push(Flow::Called(node)),
                _ => {}
            }
            let mut cursor = node.walk();
            stack.extend(
                node.named_children(&mut cursor)
                    .filter(|c| c.kind() != "comment"),
            );
        }
        let local = |root: &str| {
            declared.contains(root) && !params.contains(root) && !outward.contains(root)
        };
        let mut leaving: HashSet<String> = HashSet::new();
        let mut stored: Vec<(String, String)> = Vec::new();
        let mut names: Vec<&'t str> = Vec::new();
        for flow in &flows {
            names.clear();
            match flow {
                Flow::Returned { value, from } => {
                    self.carried(*value, &classes, &mut names);
                    names.retain(|n| !self.is_parameter_there(*value, body, n));
                    match from {
                        Some(function) => stored
                            .extend(names.iter().map(|n| (n.to_string(), function.to_string()))),
                        None => leaving.extend(names.iter().map(|n| n.to_string())),
                    }
                }
                Flow::Assigned { target, value } => {
                    self.carried(*value, &classes, &mut names);
                    names.retain(|n| !self.is_parameter_there(*value, body, n));
                    let mut leaves = Vec::new();
                    self.leaf_targets(*target, &mut leaves);
                    for leaf in leaves {
                        let into = if leaf.kind() == "identifier" {
                            Some(self.text(leaf)).filter(|n| local(*n))
                        } else {
                            self.target_root(leaf).filter(|root| local(*root))
                        };
                        match into {
                            Some(root) => stored
                                .extend(names.iter().map(|n| (n.to_string(), root.to_string()))),
                            None => leaving.extend(names.iter().map(|n| n.to_string())),
                        }
                    }
                }
                Flow::Called(call) => {
                    if let Some(list) = call
                        .child_by_field_name("arguments")
                        .filter(|a| a.kind() == "argument_list")
                    {
                        let mut cursor = list.walk();
                        for argument in list.named_children(&mut cursor) {
                            self.carried(argument, &classes, &mut names);
                        }
                    }
                    names.retain(|n| !self.is_parameter_there(*call, body, n));
                    if names.is_empty() {
                        continue;
                    }
                    let path = call
                        .child_by_field_name("function")
                        .and_then(|f| self.simple_chain(f));
                    let Some(path) = path else {
                        leaving.extend(names.iter().map(|n| n.to_string()));
                        continue;
                    };
                    let (root, last) = (path[0], path[path.len() - 1]);
                    if local(root) {
                        if path.len() > 1 && STORES_ARGUMENT.contains(&last) {
                            stored.extend(names.iter().map(|n| (n.to_string(), root.to_string())));
                        }
                    } else if !(path.len() == 1 && RUNS_CALLABLE.contains(&root))
                        && !(path.len() > 1 && RUNS_CALLABLE_METHODS.contains(&last))
                    {
                        leaving.extend(names.iter().map(|n| n.to_string()));
                    }
                }
                Flow::Decorated { name, decorator } => {
                    let expression = decorator.named_child(0);
                    let callee = expression
                        .map(|e| {
                            if e.kind() == "call" {
                                e.child_by_field_name("function").unwrap_or(e)
                            } else {
                                e
                            }
                        })
                        .and_then(|e| self.simple_chain(e));
                    let wraps = callee.as_ref().is_some_and(|path| {
                        local(path[0]) || WRAPPING_DECORATORS.contains(&path[path.len() - 1])
                    });
                    if !wraps {
                        leaving.insert((*name).to_string());
                    }
                }
            }
        }
        loop {
            let known = leaving.len();
            for (name, into) in &stored {
                if leaving.contains(into) {
                    leaving.insert(name.clone());
                }
            }
            if leaving.len() == known {
                return leaving;
            }
        }
    }

    // ---- index: which definitions the file holds, and under which keys

    fn collect(&mut self, node: Node<'t>, ix: &Ix, out: &mut Vec<Cand>, depth: u16) {
        if !self.tick() {
            return;
        }
        if depth > MAX_DEPTH {
            self.too_deep.set(true);
            return;
        }
        match node.kind() {
            "function_definition" | "class_definition" => {
                self.collect_def(node, None, ix, out, depth);
            }
            "decorated_definition" => {
                if let Some(def) = node.child_by_field_name("definition") {
                    self.collect_def(def, Some(node), ix, out, depth);
                }
            }
            "expression_statement" => self.collect_assignment(node, ix, out),
            "type_alias_statement" => self.collect_alias(node, ix, out),
            "if_statement" => {
                let condition = node.child_by_field_name("condition");
                let consequence = node.child_by_field_name("consequence");
                let type_checking = condition.is_some_and(|c| self.is_type_checking(c));
                let branch = Ix {
                    type_checking: ix.type_checking || type_checking,
                    ..ix.clone()
                };
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    let inner = if consequence.is_some_and(|c| c.id() == child.id()) {
                        &branch
                    } else {
                        ix
                    };
                    self.collect(child, inner, out, depth + 1);
                }
            }
            kind if is_container(kind) => {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    self.collect(child, ix, out, depth + 1);
                }
            }
            _ => {}
        }
    }

    fn collect_def(
        &mut self,
        def: Node<'t>,
        wrapper: Option<Node<'t>>,
        ix: &Ix,
        out: &mut Vec<Cand>,
        depth: u16,
    ) {
        let Some(name_node) = def.child_by_field_name("name") else {
            return;
        };
        let name = self.text(name_node).to_string();
        let is_class = def.kind() == "class_definition";
        let body = def.child_by_field_name("body");
        if ix
            .leaving
            .as_ref()
            .is_some_and(|leaving| !leaving.contains(&name))
        {
            // A local function: what leaves the outer one from inside it is
            // still a definition; a local class holds none.
            if let (false, Some(body)) = (is_class, body) {
                self.collect(body, ix, out, depth + 1);
            }
            return;
        }
        let decorators = self.decorators(wrapper);
        let shape = shape_of(&decorators, &name);
        let kind = if is_class {
            DefKind::Class
        } else if ix.in_class {
            if name == "__init__" {
                DefKind::Constructor
            } else if shape == Shape::Accessor || is_property(&decorators) {
                DefKind::Property
            } else {
                DefKind::Method
            }
        } else {
            DefKind::Function
        };
        let receiver = if !is_class && ix.in_class {
            self.receiver_name(def, &decorators)
        } else {
            None
        };
        self.owned.insert(def.id());
        if let Some(wrapper) = wrapper {
            self.owned.insert(wrapper.id());
        }
        out.push(Cand {
            id: def.id(),
            container: ix.container.clone(),
            name: name.clone(),
            kind,
            shape,
            type_checking: ix.type_checking,
            statement: None,
            variable: false,
            vis: visibility(&name, ix.in_class, ix.in_fn),
            decorators,
            receiver: receiver.clone(),
        });
        if let (Some(receiver), Some(body)) = (receiver.as_deref(), body) {
            self.collect_self_fields(body, receiver, &ix.container, out, 0);
        }
        let leaving = match (is_class, body) {
            (false, Some(body)) => Some(Rc::new(self.escaping_names(def, body))),
            _ => None,
        };
        let mut inner = Ix {
            container: ix.container.clone(),
            in_class: is_class,
            in_fn: ix.in_fn || !is_class,
            type_checking: false,
            leaving,
        };
        inner.container.push(name);
        if let Some(body) = body {
            self.collect(body, &inner, out, depth + 1);
        }
    }

    /// `self.x = …` in a method declares the field `x` of its class.
    fn collect_self_fields(
        &mut self,
        node: Node<'t>,
        receiver: &str,
        class: &[String],
        out: &mut Vec<Cand>,
        depth: u16,
    ) {
        if !self.tick() {
            return;
        }
        if depth > MAX_DEPTH {
            self.too_deep.set(true);
            return;
        }
        match node.kind() {
            "expression_statement" => {
                let Some(assignment) = node.named_child(0).filter(|a| a.kind() == "assignment")
                else {
                    return;
                };
                let Some(left) = assignment
                    .child_by_field_name("left")
                    .filter(|l| l.kind() == "attribute")
                else {
                    return;
                };
                let object = left.child_by_field_name("object");
                let attribute = left.child_by_field_name("attribute");
                if let (Some(object), Some(attribute)) = (object, attribute) {
                    if object.kind() == "identifier" && self.text(object) == receiver {
                        let name = self.text(attribute).to_string();
                        out.push(Cand {
                            id: attribute.id(),
                            container: class.to_vec(),
                            kind: DefKind::Field,
                            shape: Shape::Plain,
                            type_checking: false,
                            statement: None,
                            variable: true,
                            vis: visibility(&name, true, false),
                            decorators: Vec::new(),
                            receiver: None,
                            name,
                        });
                    }
                }
            }
            kind if is_container(kind) => {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    self.collect_self_fields(child, receiver, class, out, depth + 1);
                }
            }
            _ => {}
        }
    }

    fn collect_assignment(&mut self, statement: Node<'t>, ix: &Ix, out: &mut Vec<Cand>) {
        if ix.in_fn {
            return;
        }
        let Some(assignment) = statement
            .named_child(0)
            .filter(|a| a.kind() == "assignment")
        else {
            return;
        };
        if ix.container.is_empty() {
            if let (Some(left), Some(right)) = (
                assignment
                    .child_by_field_name("left")
                    .filter(|l| l.kind() == "identifier"),
                assignment.child_by_field_name("right"),
            ) {
                self.module_values
                    .entry(self.text(left).to_string())
                    .or_insert(right);
            }
        }
        self.collect_targets(assignment, statement.id(), ix, out);
    }

    fn collect_targets(
        &mut self,
        assignment: Node<'t>,
        statement: usize,
        ix: &Ix,
        out: &mut Vec<Cand>,
    ) {
        if let Some(left) = assignment.child_by_field_name("left") {
            self.collect_names(left, statement, ix, out);
        }
        if let Some(right) = assignment
            .child_by_field_name("right")
            .filter(|r| r.kind() == "assignment")
        {
            self.collect_targets(right, statement, ix, out);
        }
    }

    fn collect_names(&mut self, node: Node<'t>, statement: usize, ix: &Ix, out: &mut Vec<Cand>) {
        match node.kind() {
            "identifier" => {
                let name = self.text(node).to_string();
                let kind = if ix.in_class {
                    DefKind::Field
                } else if is_constant(&name) {
                    DefKind::Constant
                } else {
                    DefKind::Variable
                };
                out.push(Cand {
                    id: node.id(),
                    container: ix.container.clone(),
                    kind,
                    shape: Shape::Plain,
                    type_checking: ix.type_checking,
                    statement: Some(statement),
                    variable: true,
                    vis: visibility(&name, ix.in_class, false),
                    decorators: Vec::new(),
                    receiver: None,
                    name,
                });
            }
            "pattern_list" | "tuple_pattern" | "list_pattern" => {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    self.collect_names(child, statement, ix, out);
                }
            }
            _ => {}
        }
    }

    fn collect_alias(&mut self, statement: Node<'t>, ix: &Ix, out: &mut Vec<Cand>) {
        if ix.in_fn {
            return;
        }
        let Some(ident) = statement.named_child(0).and_then(first_identifier_deep) else {
            return;
        };
        let name = self.text(ident).to_string();
        out.push(Cand {
            id: ident.id(),
            container: ix.container.clone(),
            kind: DefKind::TypeAlias,
            shape: Shape::Plain,
            type_checking: ix.type_checking,
            statement: Some(statement.id()),
            variable: true,
            vis: visibility(&name, ix.in_class, false),
            decorators: Vec::new(),
            receiver: None,
            name,
        });
    }

    /// The module-level statements (in any block of the module's own code except
    /// a script's main guard, in none of a function or a class) that change what
    /// a module variable `X` holds: `X[k] = v`, `X.attr = v`, `del X[k]`,
    /// `X += v`, and a call of a mutating method (`pop`, `update`, `append`, …)
    /// on an `X` bound to a dict, list or set. Such a statement is no
    /// definition: it is kept against `X`, and its tree is part of the hashes of
    /// `X`'s definition, so editing it is a change of `X` and its readers are
    /// material. Any other module-level statement stays code outside every
    /// definition.
    fn scan_mutations(&mut self, node: Node<'t>, depth: u16) {
        if !self.tick() {
            return;
        }
        if depth > MAX_DEPTH {
            self.too_deep.set(true);
            return;
        }
        match node.kind() {
            "expression_statement" | "delete_statement" => {
                if let Some(name) = self.mutated(node) {
                    self.mutations.entry(name).or_default().push(node);
                }
            }
            // The run of a script is no part of the module's value.
            "if_statement"
                if node
                    .child_by_field_name("condition")
                    .is_some_and(|c| self.is_main_guard(c)) => {}
            kind if is_container(kind) => {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    self.scan_mutations(child, depth + 1);
                }
            }
            _ => {}
        }
    }

    /// The module variable the statement changes the value of.
    fn mutated(&self, statement: Node<'t>) -> Option<String> {
        let held = |name: &str| self.module_values.contains_key(name);
        let mut targets: Vec<Node<'t>> = Vec::new();
        if statement.kind() == "delete_statement" {
            let mut cursor = statement.walk();
            for child in statement.named_children(&mut cursor) {
                if child.kind() == "expression_list" {
                    let mut inner = child.walk();
                    targets.extend(child.named_children(&mut inner));
                } else {
                    targets.push(child);
                }
            }
        } else {
            let inner = statement.named_child(0)?;
            match inner.kind() {
                "assignment" => targets.extend(inner.child_by_field_name("left")),
                "augmented_assignment" => {
                    let left = inner.child_by_field_name("left")?;
                    if left.kind() == "identifier" {
                        let name = self.text(left);
                        return held(name).then(|| name.to_string());
                    }
                    targets.push(left);
                }
                "call" => {
                    let function = inner.child_by_field_name("function")?;
                    let object = function.child_by_field_name("object")?;
                    let method = function.child_by_field_name("attribute")?;
                    if function.kind() != "attribute"
                        || object.kind() != "identifier"
                        || !MUTATING_METHODS.contains(&self.text(method))
                    {
                        return None;
                    }
                    let name = self.text(object);
                    return (held(name) && self.holds_a_container(name)).then(|| name.to_string());
                }
                _ => {}
            }
        }
        targets
            .into_iter()
            .filter(|target| matches!(target.kind(), "subscript" | "attribute"))
            .find_map(|target| {
                let mut current = target;
                loop {
                    current = match current.kind() {
                        "subscript" => current.child_by_field_name("value")?,
                        "attribute" => current.child_by_field_name("object")?,
                        "identifier" => break,
                        _ => return None,
                    };
                }
                let name = self.text(current);
                held(name).then(|| name.to_string())
            })
    }

    /// Whether the module variable is first bound to a dict, a list or a set: a
    /// literal, a comprehension, or the constructor of one.
    fn holds_a_container(&self, name: &str) -> bool {
        let Some(value) = self.module_values.get(name) else {
            return false;
        };
        match value.kind() {
            "dictionary"
            | "list"
            | "set"
            | "dictionary_comprehension"
            | "list_comprehension"
            | "set_comprehension" => true,
            "call" => value
                .child_by_field_name("function")
                .is_some_and(|function| {
                    let callee = self.text(function);
                    CONTAINER_CONSTRUCTORS.contains(&callee.rsplit('.').next().unwrap_or(callee))
                }),
            _ => false,
        }
    }

    /// Gives every candidate its key: a variable assigned twice is one
    /// definition, and the twins of a function or class are told apart.
    fn settle(&mut self, cands: Vec<Cand>) {
        let mut taken = HashSet::new();
        let mut kept: Vec<Cand> = Vec::new();
        for cand in cands {
            if !taken.insert(cand.group()) && cand.variable {
                continue;
            }
            kept.push(cand);
        }
        let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, cand) in kept.iter().enumerate() {
            groups.entry(cand.group()).or_default().push(i);
        }
        let mut disambig: Vec<Option<Disambig>> = vec![None; kept.len()];
        for members in groups.values().filter(|m| m.len() > 1) {
            let tagged: Vec<usize> = members
                .iter()
                .copied()
                .filter(|&i| kept[i].type_checking)
                .collect();
            let cfg = (tagged.len() == 1).then(|| tagged[0]);
            if let Some(i) = cfg {
                disambig[i] = Some(Disambig::Cfg("TYPE_CHECKING".to_string()));
            }
            let rest: Vec<usize> = members
                .iter()
                .copied()
                .filter(|&i| Some(i) != cfg)
                .collect();
            let primary = rest
                .iter()
                .copied()
                .find(|&i| kept[i].shape == Shape::Plain)
                .or_else(|| rest.first().copied());
            let mut n = 0u16;
            for i in rest {
                if Some(i) != primary {
                    n = n.saturating_add(1);
                    disambig[i] = Some(Disambig::Overload(n));
                }
            }
        }
        for (cand, disambig) in kept.into_iter().zip(disambig) {
            let known = Known {
                key: LocalKey {
                    container: cand
                        .container
                        .iter()
                        .map(|c| Name::new(c.as_str()))
                        .collect(),
                    name: Name::new(cand.name.as_str()),
                    ns: Namespace::Value,
                    disambig,
                },
                kind: cand.kind,
                vis: cand.vis,
                decorators: cand.decorators,
                receiver: cand.receiver,
            };
            if cand.variable {
                if let Some(statement) = cand.statement {
                    self.owned.insert(statement);
                }
                self.vars.insert(cand.id, known);
            } else {
                self.keys.insert(cand.id, known);
            }
        }
    }

    // ---- hashes

    /// A tree as node kinds, nesting and leaf text: no comment, no skipped
    /// subtree. `0` never comes back, so `0` stays a definition's "no body".
    fn hash_tree(&self, root: Node<'t>, skip: impl Fn(Node<'t>) -> bool) -> u64 {
        let mut hash = Hasher::new();
        let mut stack = vec![(root, false)];
        while let Some((node, closing)) = stack.pop() {
            if closing {
                hash.bytes(&[2]);
                continue;
            }
            if node.kind() == "comment" || (node.id() != root.id() && skip(node)) {
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

    fn body_hash(&self, body: Node<'t>, doc: Option<usize>) -> u64 {
        self.hash_tree(body, |n| {
            Some(n.id()) == doc || self.owned.contains(&n.id())
        })
    }

    /// The bytes of a definition less those of the definitions and
    /// assignments inside it. Each piece between them is trimmed: the blank
    /// space beside a nested definition goes with it, so adding a method
    /// leaves its class's text as it was.
    fn text_hash(&self, outer: Node<'t>, def: Node<'t>) -> u64 {
        let mut cuts: Vec<(usize, usize)> = Vec::new();
        let mut stack = vec![outer];
        while let Some(node) = stack.pop() {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.id() != def.id() && self.owned.contains(&child.id()) {
                    cuts.push((child.start_byte(), child.end_byte()));
                } else {
                    stack.push(child);
                }
            }
        }
        cuts.sort_unstable();
        let mut hash = Hasher::new();
        let mut at = outer.start_byte();
        let mut piece = |bytes: &[u8]| {
            let bytes = bytes.trim_ascii();
            if !bytes.is_empty() {
                hash.token(bytes);
            }
        };
        for (from, to) in cuts {
            piece(&self.bytes[at..from.max(at)]);
            at = to.max(at);
        }
        piece(&self.bytes[at..outer.end_byte().max(at)]);
        hash.finish().unwrap_or(0)
    }

    fn doc_hash(&self, statement: Node<'t>) -> u64 {
        let mut hash = Hasher::new();
        if let Some(string) = statement.named_child(0) {
            if let Some(text) = self.plain_string(string) {
                for word in text.split_whitespace() {
                    hash.token(word.as_bytes());
                }
            }
        }
        hash.finish().unwrap_or_else(|| hash_text(b""))
    }

    fn definition_signature(&self, def: Node<'t>) -> String {
        let mut tokens: Vec<&str> = Vec::new();
        let mut cursor = def.walk();
        if def.children(&mut cursor).any(|c| c.kind() == "async") {
            tokens.push("async");
        }
        let is_class = def.kind() == "class_definition";
        tokens.push(if is_class { "class" } else { "def" });
        if let Some(name) = def.child_by_field_name("name") {
            tokens.push(self.text(name));
        }
        if let Some(parameters) = def.child_by_field_name("type_parameters") {
            self.leaf_tokens(parameters, &mut tokens);
        }
        let list = if is_class {
            "superclasses"
        } else {
            "parameters"
        };
        if let Some(list) = def.child_by_field_name(list) {
            self.leaf_tokens(list, &mut tokens);
        }
        if let Some(returns) = def.child_by_field_name("return_type") {
            tokens.push("->");
            self.leaf_tokens(returns, &mut tokens);
        }
        join_tokens(&tokens)
    }

    // ---- emission

    fn local(&mut self, node: Node<'t>, kind: LocalKind, cx: &Cx) {
        self.facts.push(Fact::LocalBinding(Local {
            name: Name::new(self.text(node)),
            site: span(node),
            enclosing: cx.scope.clone(),
            kind,
        }));
    }

    fn emit_ref(
        &mut self,
        site: Node<'t>,
        cx: &Cx,
        receiver: Receiver,
        use_kind: UseKind,
        qualifier: &[&str],
    ) {
        let name = Name::new(self.text(site));
        self.facts.push(Fact::Reference(Ref {
            site: span(site),
            enclosing: cx.scope.clone(),
            receiver,
            use_kind,
            name,
            qualifier: qualifier.iter().map(|q| Name::new(*q)).collect(),
        }));
    }

    fn emit_binding(
        &mut self,
        ident: Node<'t>,
        statement: Node<'t>,
        value: Option<Node<'t>>,
        annotation: Option<Node<'t>>,
        known: &Known,
    ) {
        let name = self.text(ident);
        let signature = annotation.map(|t| {
            let mut tokens = vec![name, ":"];
            self.leaf_tokens(t, &mut tokens);
            join_tokens(&tokens)
        });
        let mut body_hash = value.map_or(0, |v| self.hash_tree(v, |_| false));
        let mut text_hash = hash_text(&self.bytes[statement.start_byte()..statement.end_byte()]);
        if let Some(statements) = known
            .key
            .container
            .is_empty()
            .then(|| self.mutations.get(name))
            .flatten()
        {
            // What a module-level statement does to the variable is part of its
            // value: editing it is a change of the variable.
            let mut body = Hasher::new();
            let mut text = Hasher::new();
            body.bytes(&body_hash.to_le_bytes());
            text.bytes(&text_hash.to_le_bytes());
            for statement in statements {
                body.bytes(&self.hash_tree(*statement, |_| false).to_le_bytes());
                text.token(self.bytes[statement.start_byte()..statement.end_byte()].trim_ascii());
            }
            body_hash = body.finish().unwrap_or(body_hash);
            text_hash = text.finish().unwrap_or(text_hash);
        }
        self.facts.push(Fact::Definition(Def {
            local: known.key.clone(),
            kind: known.kind,
            span: span(statement),
            name_span: span(ident),
            signature,
            decorators: Vec::new(),
            visibility: known.vis,
            body_hash,
            text_hash,
            doc_hash: None,
        }));
    }

    // ---- walk

    fn children(&mut self, node: Node<'t>, cx: &Cx, depth: u16) {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            self.visit(child, cx, depth);
        }
    }

    fn children_but(&mut self, node: Node<'t>, skip: Option<Node<'t>>, cx: &Cx, depth: u16) {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if skip.is_none_or(|s| s.id() != child.id()) {
                self.visit(child, cx, depth);
            }
        }
    }

    fn visit_field(&mut self, node: Node<'t>, field: &str, cx: &Cx, depth: u16) {
        if let Some(child) = node.child_by_field_name(field) {
            self.visit(child, cx, depth);
        }
    }

    fn visit(&mut self, node: Node<'t>, cx: &Cx, depth: u16) {
        if !self.tick() {
            return;
        }
        if depth > MAX_DEPTH {
            self.too_deep.set(true);
            return;
        }
        let d = depth + 1;
        match node.kind() {
            "comment" | "case_pattern" | "global_statement" | "nonlocal_statement" => {}
            "ERROR" => {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    if is_whole(child.kind()) {
                        self.visit(child, cx, d);
                    }
                }
            }
            "decorated_definition" => {
                if let Some(def) = node.child_by_field_name("definition") {
                    self.def(def, Some(node), cx, d);
                }
            }
            "function_definition" | "class_definition" => self.def(node, None, cx, d),
            "import_statement" => self.import(node, cx),
            "import_from_statement" | "future_import_statement" => self.import_from(node, cx),
            "assignment" => self.assignment(node, cx, d),
            "augmented_assignment" => self.augmented(node, cx, d),
            "type_alias_statement" => self.type_alias(node, cx, d),
            "if_statement" => self.if_statement(node, cx, d),
            "call" => self.call(node, cx, UseKind::Call, d),
            "identifier" | "attribute" => self.chain(node, cx, cx.read, d),
            "keyword_argument" => self.visit_field(node, "value", cx, d),
            "type" => self.type_expr(node, cx, d),
            "lambda" => {
                self.params(node.child_by_field_name("parameters"), cx, d);
                self.visit_field(node, "body", cx, d);
            }
            "for_statement" | "for_in_clause" => {
                let left = node.child_by_field_name("left");
                if let Some(left) = left {
                    self.bind_targets(left, cx, d);
                }
                self.children_but(node, left, cx, d);
            }
            "as_pattern" => {
                let alias = node.child_by_field_name("alias");
                self.children_but(node, alias, cx, d);
                if let Some(alias) = alias {
                    self.bind_targets(alias, cx, d);
                }
            }
            "named_expression" => {
                if let Some(name) = node.child_by_field_name("name") {
                    self.local(name, LocalKind::Variable, cx);
                }
                self.visit_field(node, "value", cx, d);
            }
            "binary_operator" | "boolean_operator" => {
                let mut current = node;
                loop {
                    self.visit_field(current, "right", cx, d);
                    match current.child_by_field_name("left") {
                        Some(left)
                            if matches!(left.kind(), "binary_operator" | "boolean_operator") =>
                        {
                            current = left;
                        }
                        Some(left) => {
                            self.visit(left, cx, d);
                            break;
                        }
                        None => break,
                    }
                }
            }
            _ => self.children(node, cx, d),
        }
    }

    fn def(&mut self, def: Node<'t>, wrapper: Option<Node<'t>>, cx: &Cx, depth: u16) {
        let Some(known) = self.keys.get(&def.id()).cloned() else {
            self.local_def(def, wrapper, cx, depth);
            return;
        };
        let outer = wrapper.unwrap_or(def);
        let is_class = def.kind() == "class_definition";
        let body = def.child_by_field_name("body");
        let doc = body.and_then(|b| self.docstring(b));
        let body_hash = body.map_or(0, |b| self.body_hash(b, doc.map(|d| d.id())));
        self.facts.push(Fact::Definition(Def {
            local: known.key.clone(),
            kind: known.kind,
            span: span(outer),
            name_span: span(def.child_by_field_name("name").unwrap_or(def)),
            signature: Some(self.definition_signature(def)),
            decorators: known
                .decorators
                .iter()
                .map(|d| Name::new(d.as_str()))
                .collect(),
            visibility: known.vis,
            body_hash,
            text_hash: self.text_hash(outer, def),
            doc_hash: doc.map(|d| self.doc_hash(d)),
        }));
        let inner = Cx {
            scope: Scope::Def(known.key.clone()),
            in_fn: cx.in_fn || !is_class,
            type_only: cx.type_only,
            receiver: known.receiver.clone().or_else(|| cx.receiver.clone()),
            read: UseKind::Read,
        };
        if let Some(wrapper) = wrapper {
            let mut cursor = wrapper.walk();
            for decorator in wrapper
                .named_children(&mut cursor)
                .filter(|c| c.kind() == "decorator")
            {
                self.decorator(decorator, &inner, depth);
            }
        }
        if is_class {
            if let Some(bases) = def.child_by_field_name("superclasses") {
                let mut cursor = bases.walk();
                for base in bases.named_children(&mut cursor) {
                    let value = if base.kind() == "keyword_argument" {
                        base.child_by_field_name("value").unwrap_or(base)
                    } else {
                        base
                    };
                    self.type_node(
                        value,
                        &Cx {
                            read: UseKind::TypeUse,
                            ..inner.clone()
                        },
                        depth,
                    );
                }
            }
            if let Some(body) = body {
                self.visit(body, &inner, depth);
            }
            return;
        }
        let saved = self.types.clone();
        self.params(def.child_by_field_name("parameters"), &inner, depth);
        if let Some(returns) = def.child_by_field_name("return_type") {
            self.type_expr(returns, &inner, depth);
        }
        if let Some(body) = body {
            self.visit(body, &inner, depth);
        }
        self.types = saved;
    }

    /// A `def` or `class` inside a function that does not leave it: a local
    /// name of the enclosing definition, whose parameters and body are read as
    /// part of it.
    fn local_def(&mut self, def: Node<'t>, wrapper: Option<Node<'t>>, cx: &Cx, depth: u16) {
        if let Some(name) = def.child_by_field_name("name") {
            self.local(name, LocalKind::Closure, cx);
        }
        if let Some(wrapper) = wrapper {
            let mut cursor = wrapper.walk();
            for decorator in wrapper
                .named_children(&mut cursor)
                .filter(|c| c.kind() == "decorator")
            {
                self.decorator(decorator, cx, depth);
            }
        }
        let body = def.child_by_field_name("body");
        if def.kind() == "class_definition" {
            if let Some(bases) = def.child_by_field_name("superclasses") {
                let mut cursor = bases.walk();
                for base in bases.named_children(&mut cursor) {
                    let value = if base.kind() == "keyword_argument" {
                        base.child_by_field_name("value").unwrap_or(base)
                    } else {
                        base
                    };
                    let read = Cx {
                        read: UseKind::TypeUse,
                        ..cx.clone()
                    };
                    self.type_node(value, &read, depth);
                }
            }
            if let Some(body) = body {
                self.visit(body, cx, depth);
            }
            return;
        }
        let saved = self.types.clone();
        self.params(def.child_by_field_name("parameters"), cx, depth);
        if let Some(returns) = def.child_by_field_name("return_type") {
            self.type_expr(returns, cx, depth);
        }
        if let Some(body) = body {
            self.visit(body, cx, depth);
        }
        self.types = saved;
    }

    fn decorator(&mut self, decorator: Node<'t>, cx: &Cx, depth: u16) {
        let mut cursor = decorator.walk();
        let Some(expression) = decorator
            .named_children(&mut cursor)
            .find(|c| c.kind() != "comment")
        else {
            return;
        };
        match expression.kind() {
            "call" => self.call(expression, cx, UseKind::Decorate, depth),
            "identifier" | "attribute" => self.chain(expression, cx, UseKind::Decorate, depth),
            _ => self.visit(expression, cx, depth),
        }
    }

    fn params(&mut self, parameters: Option<Node<'t>>, cx: &Cx, depth: u16) {
        let Some(parameters) = parameters else {
            return;
        };
        let mut cursor = parameters.walk();
        for parameter in parameters.named_children(&mut cursor) {
            let annotation = parameter.child_by_field_name("type");
            let bound = match parameter.kind() {
                "identifier" => Some(parameter),
                "typed_parameter" => first_identifier(parameter),
                "default_parameter" | "typed_default_parameter" => {
                    parameter.child_by_field_name("name")
                }
                "list_splat_pattern" | "dictionary_splat_pattern" => first_identifier(parameter),
                _ => None,
            };
            let Some(bound) = bound.filter(|b| b.kind() == "identifier") else {
                continue;
            };
            self.local(bound, LocalKind::Parameter, cx);
            if let Some(annotation) = annotation {
                self.type_expr(annotation, cx, depth);
                if let Some(type_name) = self.type_name_of(annotation) {
                    self.types.insert(self.text(bound).to_string(), type_name);
                }
            }
            self.visit_field(parameter, "value", cx, depth);
        }
    }

    fn bind_targets(&mut self, node: Node<'t>, cx: &Cx, depth: u16) {
        match node.kind() {
            "identifier" => self.local(node, LocalKind::Variable, cx),
            "attribute" => self.chain(node, cx, UseKind::Write, depth),
            "pattern_list"
            | "tuple_pattern"
            | "list_pattern"
            | "list_splat_pattern"
            | "as_pattern_target"
            | "parenthesized_expression"
            | "expression_list"
            | "tuple"
            | "list" => {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    self.bind_targets(child, cx, depth);
                }
            }
            _ => self.visit(node, cx, depth),
        }
    }

    fn assignment(&mut self, node: Node<'t>, cx: &Cx, depth: u16) {
        let left = node.child_by_field_name("left");
        let right = node.child_by_field_name("right");
        if let Some(annotation) = node.child_by_field_name("type") {
            self.type_expr(annotation, cx, depth);
            let ident = left.filter(|l| l.kind() == "identifier");
            if let (Some(ident), Some(type_name)) = (ident, self.type_name_of(annotation)) {
                self.types.insert(self.text(ident).to_string(), type_name);
            }
        }
        if let Some(right) = right {
            self.visit(right, cx, depth);
        }
        if let Some(left) = left {
            self.targets(left, node, cx, depth);
            let module_level = matches!(cx.scope, Scope::Module) && !cx.in_fn;
            if let (true, true, Some(right)) = (module_level, self.text(left) == "__all__", right) {
                self.exports(right);
            }
        }
    }

    fn targets(&mut self, node: Node<'t>, assignment: Node<'t>, cx: &Cx, depth: u16) {
        let statement = assignment
            .parent()
            .filter(|p| p.kind() == "expression_statement")
            .unwrap_or(assignment);
        let value = assignment.child_by_field_name("right");
        let annotation = assignment.child_by_field_name("type");
        match node.kind() {
            "identifier" => {
                if let Some(known) = self.vars.get(&node.id()).cloned() {
                    self.emit_binding(node, statement, value, annotation, &known);
                } else if cx.in_fn {
                    self.local(node, LocalKind::Variable, cx);
                } else {
                    self.emit_ref(node, cx, Receiver::None, UseKind::Write, &[]);
                }
            }
            "attribute" => {
                let attribute = node.child_by_field_name("attribute");
                let known = attribute.and_then(|a| self.vars.get(&a.id()).cloned().map(|k| (a, k)));
                match known {
                    Some((attribute, known)) => {
                        self.emit_binding(attribute, statement, value, annotation, &known);
                    }
                    None => self.chain(node, cx, UseKind::Write, depth),
                }
            }
            "pattern_list"
            | "tuple_pattern"
            | "list_pattern"
            | "list_splat_pattern"
            | "parenthesized_expression" => {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    self.targets(child, assignment, cx, depth);
                }
            }
            _ => self.visit(node, cx, depth),
        }
    }

    fn augmented(&mut self, node: Node<'t>, cx: &Cx, depth: u16) {
        let right = node.child_by_field_name("right");
        if let Some(left) = node.child_by_field_name("left") {
            match left.kind() {
                "identifier" | "attribute" => self.chain(left, cx, UseKind::Write, depth),
                _ => self.visit(left, cx, depth),
            }
            let module_level = matches!(cx.scope, Scope::Module) && !cx.in_fn;
            if let (true, true, Some(right)) = (module_level, self.text(left) == "__all__", right) {
                self.exports(right);
            }
        }
        if let Some(right) = right {
            self.visit(right, cx, depth);
        }
    }

    fn type_alias(&mut self, node: Node<'t>, cx: &Cx, depth: u16) {
        let mut cursor = node.walk();
        let parts: Vec<Node<'t>> = node
            .named_children(&mut cursor)
            .filter(|c| c.kind() != "comment")
            .collect();
        let (Some(&left), Some(&right)) = (parts.first(), parts.last()) else {
            return;
        };
        if let Some(ident) = first_identifier_deep(left) {
            if let Some(known) = self.vars.get(&ident.id()).cloned() {
                self.emit_binding(ident, node, Some(right), None, &known);
            }
        }
        self.type_expr(right, cx, depth);
    }

    fn exports(&mut self, value: Node<'t>) {
        let mut stack = vec![value];
        while let Some(node) = stack.pop() {
            match node.kind() {
                "string" => {
                    if let Some(name) = self.plain_string(node) {
                        self.facts.push(Fact::Export(Export::Named {
                            site: span(node),
                            name: Name::new(name.as_str()),
                            alias: None,
                            from: None,
                            type_only: false,
                        }));
                    }
                }
                "list"
                | "tuple"
                | "set"
                | "parenthesized_expression"
                | "expression_list"
                | "binary_operator" => {
                    let mut cursor = node.walk();
                    stack.extend(node.named_children(&mut cursor));
                }
                _ => {}
            }
        }
    }

    fn if_statement(&mut self, node: Node<'t>, cx: &Cx, depth: u16) {
        let condition = node.child_by_field_name("condition");
        let guard = matches!(cx.scope, Scope::Module)
            && !cx.in_fn
            && condition.is_some_and(|c| self.is_main_guard(c));
        if guard {
            self.facts.push(Fact::EntryPoint(EntryPoint {
                site: span(node),
                kind: EntryKind::ScriptGuard,
                target: Scope::Module,
                framework: None,
            }));
        } else if let Some(condition) = condition {
            self.visit(condition, cx, depth);
        }
        let mut branch = cx.clone();
        branch.type_only = cx.type_only || condition.is_some_and(|c| self.is_type_checking(c));
        self.visit_field(node, "consequence", &branch, depth);
        let mut cursor = node.walk();
        for alternative in node.children_by_field_name("alternative", &mut cursor) {
            self.visit(alternative, cx, depth);
        }
    }

    // ---- imports

    fn dotted(&self, node: Option<Node<'t>>) -> String {
        let Some(node) = node else {
            return String::new();
        };
        let mut cursor = node.walk();
        let parts: Vec<&str> = node
            .named_children(&mut cursor)
            .filter(|c| c.kind() == "identifier")
            .map(|c| self.text(c))
            .collect();
        if parts.is_empty() && node.kind() == "identifier" {
            return self.text(node).to_string();
        }
        parts.join(".")
    }

    fn import_kind(cx: &Cx) -> ImportKind {
        if cx.type_only {
            ImportKind::TypeOnly
        } else {
            ImportKind::Static
        }
    }

    fn import(&mut self, node: Node<'t>, cx: &Cx) {
        let mut cursor = node.walk();
        for clause in node.children_by_field_name("name", &mut cursor) {
            let (module, default) = match clause.kind() {
                "aliased_import" => (
                    self.dotted(clause.child_by_field_name("name")),
                    clause
                        .child_by_field_name("alias")
                        .map(|a| Name::new(self.text(a))),
                ),
                "dotted_name" => (self.dotted(Some(clause)), None),
                _ => continue,
            };
            self.facts.push(Fact::Import(Import {
                site: span(clause),
                enclosing: cx.scope.clone(),
                module: ModulePath::new(module.as_str()),
                relative: 0,
                kind: Self::import_kind(cx),
                names: Vec::new(),
                star: Star::None,
                default,
            }));
        }
    }

    fn import_from(&mut self, node: Node<'t>, cx: &Cx) {
        let (module, relative) = match node.child_by_field_name("module_name") {
            Some(m) if m.kind() == "relative_import" => {
                let mut cursor = m.walk();
                let mut dots = 0;
                let mut dotted = None;
                for part in m.named_children(&mut cursor) {
                    match part.kind() {
                        "import_prefix" => dots = self.text(part).matches('.').count() as u32,
                        "dotted_name" => dotted = Some(part),
                        _ => {}
                    }
                }
                (self.dotted(dotted), dots)
            }
            Some(m) => (self.dotted(Some(m)), 0),
            None => ("__future__".to_string(), 0),
        };
        let mut names = Vec::new();
        let mut cursor = node.walk();
        for clause in node.children_by_field_name("name", &mut cursor) {
            match clause.kind() {
                "aliased_import" => names.push((
                    Name::new(self.dotted(clause.child_by_field_name("name")).as_str()),
                    clause
                        .child_by_field_name("alias")
                        .map(|a| Name::new(self.text(a))),
                )),
                "dotted_name" => names.push((Name::new(self.dotted(Some(clause)).as_str()), None)),
                _ => {}
            }
        }
        let star = if has_child_of_kind(node, "wildcard_import") {
            Star::Glob
        } else {
            Star::None
        };
        self.facts.push(Fact::Import(Import {
            site: span(node),
            enclosing: cx.scope.clone(),
            module: ModulePath::new(module.as_str()),
            relative,
            kind: Self::import_kind(cx),
            names,
            star,
            default: None,
        }));
    }

    fn dynamic_import(&mut self, argument: Option<&Node<'t>>, cx: &Cx) {
        let Some(&argument) = argument else {
            return;
        };
        let Some(text) = self.plain_string(argument) else {
            self.facts.push(Fact::ComputedImport(ComputedImport {
                site: span(argument),
                enclosing: cx.scope.clone(),
                kind: ImportKind::Dynamic,
            }));
            return;
        };
        let relative = text.chars().take_while(|c| *c == '.').count();
        self.facts.push(Fact::Import(Import {
            site: span(argument),
            enclosing: cx.scope.clone(),
            module: ModulePath::new(&text[relative..]),
            relative: relative as u32,
            kind: ImportKind::Dynamic,
            names: Vec::new(),
            star: Star::None,
            default: None,
        }));
    }

    // ---- references

    fn receiver_of(&self, cx: &Cx, qualifier: &[&str]) -> Receiver {
        match qualifier {
            [] => Receiver::None,
            [only] if cx.receiver.as_deref() == Some(*only) => Receiver::SelfType,
            [only] => self
                .types
                .get(*only)
                .map_or(Receiver::Unknown, |t| Receiver::Typed(t.clone())),
            _ => Receiver::Unknown,
        }
    }

    fn call_result(&self, head: Node<'t>) -> Receiver {
        let callee = (head.kind() == "call")
            .then(|| head.child_by_field_name("function"))
            .flatten()
            .and_then(|f| self.simple_chain(f));
        match callee {
            Some(parts) => Receiver::CallResult(parts.into_iter().map(Name::new).collect()),
            None => Receiver::Unknown,
        }
    }

    /// A name or attribute chain as one reference: the last name, the written
    /// chain before it as its qualifier.
    fn chain(&mut self, node: Node<'t>, cx: &Cx, kind: UseKind, depth: u16) {
        let mut names: Vec<Node<'t>> = Vec::new();
        let mut head = node;
        while head.kind() == "attribute" {
            let (Some(attribute), Some(object)) = (
                head.child_by_field_name("attribute"),
                head.child_by_field_name("object"),
            ) else {
                self.children(head, cx, depth);
                return;
            };
            names.push(attribute);
            head = object;
        }
        names.reverse();
        let named_head = head.kind() == "identifier";
        if named_head {
            names.insert(0, head);
        } else {
            self.visit(head, cx, depth);
        }
        let Some((&last, before)) = names.split_last() else {
            return;
        };
        let qualifier: Vec<&str> = before.iter().map(|q| self.text(*q)).collect();
        let receiver = if named_head {
            self.receiver_of(cx, &qualifier)
        } else if qualifier.is_empty() {
            self.call_result(head)
        } else {
            Receiver::Unknown
        };
        self.emit_ref(last, cx, receiver, kind, &qualifier);
    }

    fn call(&mut self, node: Node<'t>, cx: &Cx, callee_kind: UseKind, depth: u16) {
        let mut callee = None;
        if let Some(function) = node.child_by_field_name("function") {
            match function.kind() {
                "identifier" | "attribute" => {
                    callee = self.simple_chain(function);
                    self.chain(function, cx, callee_kind, depth);
                }
                _ => self.visit(function, cx, depth),
            }
        }
        let mut arguments: Vec<Node<'t>> = Vec::new();
        if let Some(list) = node.child_by_field_name("arguments") {
            if list.kind() == "argument_list" {
                let mut cursor = list.walk();
                arguments.extend(
                    list.named_children(&mut cursor)
                        .filter(|c| c.kind() != "comment"),
                );
            } else {
                self.visit(list, cx, depth);
            }
        }
        for argument in &arguments {
            self.argument(*argument, cx, depth);
        }
        if let (UseKind::Call, Some(path)) = (callee_kind, callee) {
            match path.as_slice() {
                ["sys", "path", "insert"] => self.path_mention(arguments.get(1), cx),
                ["sys", "path", "append"] => self.path_mention(arguments.first(), cx),
                ["importlib", "import_module"] | ["import_module"] | ["__import__"] => {
                    self.dynamic_import(arguments.first(), cx);
                }
                _ => {}
            }
        }
    }

    fn argument(&mut self, argument: Node<'t>, cx: &Cx, depth: u16) {
        let value = match argument.kind() {
            "keyword_argument" => argument.child_by_field_name("value"),
            "list_splat" | "dictionary_splat" => argument.named_child(0),
            _ => Some(argument),
        };
        let Some(value) = value else {
            return;
        };
        let passed = matches!(value.kind(), "identifier" | "attribute")
            && !matches!(cx.read, UseKind::TypeUse);
        if passed {
            self.chain(value, cx, UseKind::PassAsValue, depth);
        } else {
            self.visit(value, cx, depth);
        }
    }

    // ---- types

    fn type_name_of(&self, annotation: Node<'t>) -> Option<TypeName> {
        let inner = if annotation.kind() == "type" {
            annotation.named_child(0)?
        } else {
            annotation
        };
        match inner.kind() {
            "identifier" | "attribute" => {
                let name = self.simple_chain(inner)?.join(".");
                Some(TypeName::new(name.as_str()))
            }
            "string" => {
                let name = self.plain_string(inner)?;
                is_dotted_name(&name).then(|| TypeName::new(name.as_str()))
            }
            "union_type" | "binary_operator" | "subscript" | "generic_type" => {
                let mut alternatives = Vec::new();
                self.alternatives(inner, &mut alternatives)?;
                let mut named = alternatives
                    .into_iter()
                    .filter(|a| self.text(*a).trim() != "None");
                let only = named.next()?;
                if named.next().is_some() {
                    return None;
                }
                self.type_name_of(only)
            }
            _ => None,
        }
    }

    /// The members of `X | Y`, `Optional[X]` and `Union[X, Y]` written as the
    /// annotation; `None` for any other shape (`list[X]` is no choice among
    /// types). A member that is itself such a choice is flattened.
    fn alternatives(&self, node: Node<'t>, out: &mut Vec<Node<'t>>) -> Option<()> {
        match node.kind() {
            "type" => self.alternatives(node.named_child(0)?, out),
            "union_type" => {
                let mut cursor = node.walk();
                for member in node.named_children(&mut cursor) {
                    self.alternatives(member, out)?;
                }
                Some(())
            }
            "binary_operator" => {
                (self.text(node.child_by_field_name("operator")?) == "|").then_some(())?;
                self.alternatives(node.child_by_field_name("left")?, out)?;
                self.alternatives(node.child_by_field_name("right")?, out)
            }
            // `Optional[X]` in an annotation is a generic type: a name and its
            // parameters.
            "generic_type" => {
                let head = self.text(node.named_child(0)?);
                let head = head.rsplit('.').next().unwrap_or(head);
                if !matches!(head, "Optional" | "Union") {
                    return None;
                }
                let mut cursor = node.walk();
                let parameters = node
                    .named_children(&mut cursor)
                    .find(|c| c.kind() == "type_parameter")?;
                let mut inner = parameters.walk();
                for member in parameters.named_children(&mut inner) {
                    self.alternatives(member, out)?;
                }
                Some(())
            }
            "subscript" => {
                let head = self.text(node.child_by_field_name("value")?);
                let head = head.rsplit('.').next().unwrap_or(head);
                if !matches!(head, "Optional" | "Union") {
                    return None;
                }
                let mut cursor = node.walk();
                for member in node.children_by_field_name("subscript", &mut cursor) {
                    self.alternatives(member, out)?;
                }
                Some(())
            }
            _ => {
                out.push(node);
                Some(())
            }
        }
    }

    fn type_expr(&mut self, node: Node<'t>, cx: &Cx, depth: u16) {
        let types = Cx {
            read: UseKind::TypeUse,
            ..cx.clone()
        };
        self.type_node(node, &types, depth);
    }

    fn type_node(&mut self, node: Node<'t>, cx: &Cx, depth: u16) {
        if !self.tick() {
            return;
        }
        if depth > MAX_DEPTH {
            self.too_deep.set(true);
            return;
        }
        let d = depth + 1;
        match node.kind() {
            "string" => {
                if let Some(name) = self.plain_string(node).filter(|s| is_dotted_name(s)) {
                    let parts: Vec<&str> = name.split('.').collect();
                    if let Some((last, before)) = parts.split_last() {
                        let receiver = if before.is_empty() {
                            Receiver::None
                        } else {
                            Receiver::Unknown
                        };
                        self.facts.push(Fact::Reference(Ref {
                            site: span(node),
                            enclosing: cx.scope.clone(),
                            receiver,
                            use_kind: UseKind::TypeUse,
                            name: Name::new(*last),
                            qualifier: before.iter().map(|q| Name::new(*q)).collect(),
                        }));
                    }
                }
            }
            "subscript" => {
                let value = node.child_by_field_name("value");
                let literal = value.is_some_and(|v| self.text(v).ends_with("Literal"));
                if let Some(value) = value {
                    self.type_node(value, cx, d);
                }
                if !literal {
                    let mut cursor = node.walk();
                    for index in node.children_by_field_name("subscript", &mut cursor) {
                        self.type_node(index, cx, d);
                    }
                }
            }
            "type" | "generic_type" | "union_type" | "member_type" | "splat_type"
            | "constrained_type" | "type_parameter" | "list" | "tuple" => {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    self.type_node(child, cx, d);
                }
            }
            _ => self.visit(node, cx, d),
        }
    }

    // ---- paths

    fn path_mention(&mut self, argument: Option<&Node<'t>>, cx: &Cx) {
        let Some(&argument) = argument else {
            return;
        };
        if let Some(text) = self.path_text(argument, 0, &[]) {
            self.facts.push(Fact::PathMention(PathMention {
                site: span(argument),
                enclosing: cx.scope.clone(),
                text,
            }));
        }
    }

    /// The path an expression builds, `__file__` standing for the file: a
    /// `Path(__file__).parent / "src"` is `__file__/../src`. `None` for
    /// anything the syntax cannot evaluate.
    fn path_text(&self, node: Node<'t>, depth: u8, via: &[&'t str]) -> Option<String> {
        if depth > 8 {
            self.too_deep.set(true);
            return None;
        }
        let go = |n: Node<'t>| self.path_text(n, depth + 1, via);
        let join = |base: String, segment: &str| {
            if segment.starts_with('/') {
                segment.to_string()
            } else {
                format!("{base}/{}", segment.trim_start_matches("./"))
            }
        };
        match node.kind() {
            "identifier" => match self.text(node) {
                "__file__" => Some("__file__".to_string()),
                // A name that holds an expression of itself is no path, not a
                // chain too deep to follow.
                name if via.contains(&name) => None,
                name => {
                    let through: Vec<&'t str> = via.iter().copied().chain([name]).collect();
                    self.path_text(*self.module_values.get(name)?, depth + 1, &through)
                }
            },
            "parenthesized_expression" => go(node.named_child(0)?),
            "string" => self.plain_string(node),
            "attribute" => {
                (self.text(node.child_by_field_name("attribute")?) == "parent").then_some(())?;
                Some(format!("{}/..", go(node.child_by_field_name("object")?)?))
            }
            "subscript" => {
                let value = node.child_by_field_name("value")?;
                #[allow(
                    clippy::disallowed_methods,
                    reason = "a subscript that is not an integer is not `parents[n]`, so the expression is no parent path"
                )]
                let up: usize = self
                    .text(node.child_by_field_name("subscript")?)
                    .parse()
                    .ok()?;
                (value.kind() == "attribute"
                    && self.text(value.child_by_field_name("attribute")?) == "parents")
                    .then_some(())?;
                let base = go(value.child_by_field_name("object")?)?;
                Some(format!("{base}{}", "/..".repeat(up + 1)))
            }
            "binary_operator" => {
                (self.text(node.child_by_field_name("operator")?) == "/").then_some(())?;
                let base = go(node.child_by_field_name("left")?)?;
                Some(join(base, &go(node.child_by_field_name("right")?)?))
            }
            "call" => {
                let function = node.child_by_field_name("function")?;
                let mut cursor = node.child_by_field_name("arguments")?.walk();
                let args: Vec<Node<'t>> = node
                    .child_by_field_name("arguments")?
                    .named_children(&mut cursor)
                    .filter(|c| c.kind() != "comment")
                    .collect();
                match self.simple_chain(function).as_deref() {
                    Some(
                        ["Path"]
                        | ["pathlib", "Path"]
                        | ["str"]
                        | ["os", "fspath"]
                        | ["os", "path", "abspath"]
                        | ["os", "path", "realpath"]
                        | ["os", "path", "normpath"],
                    ) => go(*args.first()?),
                    Some(["os", "path", "dirname"]) => Some(format!("{}/..", go(*args.first()?)?)),
                    Some(["os", "path", "join"]) => {
                        let (first, rest) = args.split_first()?;
                        let mut path = go(*first)?;
                        for segment in rest {
                            path = join(path, &go(*segment)?);
                        }
                        Some(path)
                    }
                    _ => {
                        let method = self.text(function.child_by_field_name("attribute")?);
                        (matches!(method, "resolve" | "absolute") && args.is_empty())
                            .then_some(())?;
                        go(function.child_by_field_name("object")?)
                    }
                }
            }
            _ => None,
        }
    }
}
