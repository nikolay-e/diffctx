//! What a Go file's syntax states, as the facts contract names it.
//!
//! Conventions a reader of the facts cannot recover from the types:
//! - A package is a directory, so the file's module is the workspace's
//!   (`facts_of`): the directory. Every top-level name of the package is a
//!   definition with an empty container: a function (`Function`), a type
//!   (`Struct`, `Interface` or `TypeAlias`, in the type namespace), a `const`
//!   (`Constant`) or a `var` (`Variable`). A method has its receiver's type as
//!   container (`*Cart` and `Cart[T]` are `Cart`); an interface's methods and a
//!   struct's fields have the type as container. An embedded field is a `Field`
//!   named after its type, whose signature is the type alone.
//! - A signature is the declaration's tokens, a space between every two and the
//!   body, the value of a `const` or `var` and the members of a type left out:
//!   `func ( c * Cart ) Add ( k int )`. The resolver reads receiver, result and
//!   declared types back off it.
//! - The package clause is an `Export::Default` whose `local` is the package's
//!   name, which a plain import binds and `main` makes a program.
//! - A name written with a capital is `Public`; any other is `Package`.
//! - An import's `module` is the import path as written. A name given with it
//!   (`import p "x/y"`) is `default`; a dot import is `Star::Glob`; a blank
//!   import is `ImportKind::SideEffect`. A plain import binds the package's own
//!   name, which only the package's files say.
//! - A reference names the last segment and carries the path written before it
//!   in `qualifier`: `pricing.Total(x)` is `Total` with `[pricing]`. A name that
//!   is a local of the function around it is no reference. A call `x.M()` on a
//!   local whose type its declaration states is `M` with `Receiver::Typed`
//!   (`*T` is `T`, `pkg.T` is `pkg.T`; fields written after the local go to the
//!   `qualifier`); on a call's result it is `CallResult` (the callee's path);
//!   on any other local it is `Unknown` with the whole chain as `qualifier`.
//! - A composite literal `T{..}` is an `Instantiate` of `T`; a keyed field of a
//!   struct literal is a `Write` of that field on `T`. A name handed to a call
//!   as an argument is a `PassAsValue`; a name elsewhere a `Read`; the target of
//!   an assignment, `++` or `--` a `Write`; a name in a type position a `TypeUse`.
//! - A function literal that is an argument of a call is a `Scope::Callback`
//!   wrapped around the definition that holds it; any other is part of it.
//! - `func main()` of a package `main` is `Main`; `TestX(t *testing.T)` and its
//!   `Benchmark`, `Fuzz` and `Example` kin are `Test` entry points.
//! - Hashes are FNV-1a over the definition's own parts: members that are
//!   definitions themselves (fields, interface methods) are left out of their
//!   type's.

use std::cell::RefCell;
use std::collections::HashMap;

use tree_sitter::{Language as TsLanguage, Node, ParseOptions, ParseState, Parser, Tree};

use crate::facts::{
    BlobFacts, Def, DefKind, EntryKind, EntryPoint, Export, Extractor, Fact, Hasher, Import,
    ImportKind, Language, Local, LocalKey, LocalKind, ModulePath, Name, Namespace, Receiver, Ref,
    Scope, Span, Star, TypeName, UseKind, Visibility, expired, hash_text,
};
use crate::snapshot::{Loss, RepoPath, RunBudget};

const MAX_DEPTH: u16 = 256;
const ERROR_RANGES_KEPT: usize = 32;
const POLL_EVERY: usize = 256;

/// The types the language declares: a name in a type position that is one of
/// these is no reference.
const PREDECLARED: &[&str] = &[
    "any",
    "bool",
    "byte",
    "comparable",
    "complex128",
    "complex64",
    "error",
    "float32",
    "float64",
    "int",
    "int16",
    "int32",
    "int64",
    "int8",
    "rune",
    "string",
    "uint",
    "uint16",
    "uint32",
    "uint64",
    "uint8",
    "uintptr",
];

pub struct GoExtractor;

impl Extractor for GoExtractor {
    fn language(&self) -> Language {
        Language::Go
    }

    fn extract(&self, text: &[u8], budget: &RunBudget) -> BlobFacts {
        extract(text, budget)
    }
}

fn unbound() -> RepoPath {
    RepoPath::new("<blob>").expect("a valid path")
}

#[cfg(feature = "tree-sitter-go")]
fn grammar() -> Option<TsLanguage> {
    Some(TsLanguage::new(tree_sitter_go::LANGUAGE))
}

#[cfg(not(feature = "tree-sitter-go"))]
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
    let mut blob = BlobFacts::new(Language::Go);
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

/// A construct the grammar read whole, whatever lies around it.
fn is_whole(kind: &str) -> bool {
    kind.ends_with("_declaration") || kind == "comment"
}

/// The line ranges the parse could not read, merged; an error node that
/// swallows a whole file is narrowed to its stray tokens.
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

fn children<'t>(node: Node<'t>) -> Vec<Node<'t>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

/// A type a local is known by: a named type (the path as written, no `*`, no
/// type arguments), or the result of calling a path.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Ty {
    Named(String),
    Result(Vec<String>),
}

#[derive(Clone)]
struct Cx {
    /// The enclosing definitions of what is defined here, outermost first.
    container: Vec<String>,
    /// Who owns the references made here.
    scope: Scope,
}

struct Walker<'t> {
    bytes: &'t [u8],
    budget: &'t RunBudget,
    ticks: usize,
    halted: bool,
    too_deep: bool,
    facts: Vec<Fact>,
    /// The package's name, which makes a `main` the program's.
    package: String,
    /// How many definitions each key has had, for twins (`init`).
    twins: HashMap<LocalKey, u16>,
    /// The locals of the function, block and literal scopes around, innermost
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
            package: String::new(),
            twins: HashMap::new(),
            locals: Vec::new(),
        }
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

    // ---- locals

    fn lookup(&self, name: &str) -> Option<&Option<Ty>> {
        self.locals.iter().rev().find_map(|scope| scope.get(name))
    }

    fn is_local(&self, name: &str) -> bool {
        self.lookup(name).is_some()
    }

    fn declare(&mut self, node: Node<'t>, ty: Option<Ty>, kind: LocalKind, cx: &Cx) {
        let name = self.text(node);
        if name == "_" || name.is_empty() {
            return;
        }
        self.facts.push(Fact::LocalBinding(Local {
            name: Name::new(name),
            site: span(node),
            enclosing: cx.scope.clone(),
            kind,
        }));
        if let Some(scope) = self.locals.last_mut() {
            scope.insert(name.to_string(), ty);
        }
    }

    // ---- the file

    fn run(&mut self, root: Node<'t>) {
        let cx = Cx {
            container: Vec::new(),
            scope: Scope::Module,
        };
        let mut docs: Vec<&'t str> = Vec::new();
        for child in children(root) {
            if !self.deeper(1) {
                return;
            }
            match child.kind() {
                "comment" => {
                    docs.push(self.text(child));
                    continue;
                }
                "package_clause" => {
                    if let Some(name) = children(child).first() {
                        self.package = self.text(*name).to_string();
                        self.facts.push(Fact::Export(Export::Default {
                            site: span(child),
                            local: Some(Name::new(self.package.as_str())),
                        }));
                    }
                }
                "import_declaration" => self.imports(child),
                "function_declaration" => self.function(child, &docs),
                "method_declaration" => self.method(child, &docs),
                "type_declaration" => self.type_declaration(child, &docs),
                "const_declaration" => self.values(child, DefKind::Constant, &cx, &docs),
                "var_declaration" => self.values(child, DefKind::Variable, &cx, &docs),
                _ => {}
            }
            docs.clear();
        }
    }

    // ---- imports

    fn imports(&mut self, node: Node<'t>) {
        for child in children(node) {
            match child.kind() {
                "import_spec" => self.import(child),
                "import_spec_list" => {
                    for spec in children(child) {
                        if spec.kind() == "import_spec" {
                            self.import(spec);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn import(&mut self, spec: Node<'t>) {
        let Some(path) = spec.child_by_field_name("path") else {
            return;
        };
        let written = self.text(path);
        let module = written.trim_matches(|c| c == '"' || c == '`');
        let mut kind = ImportKind::Static;
        let mut star = Star::None;
        let mut default = None;
        if let Some(name) = spec.child_by_field_name("name") {
            match name.kind() {
                "blank_identifier" => kind = ImportKind::SideEffect,
                "dot" => star = Star::Glob,
                _ => default = Some(Name::new(self.text(name))),
            }
        }
        self.facts.push(Fact::Import(Import {
            site: span(spec),
            enclosing: Scope::Module,
            module: ModulePath::new(module),
            relative: 0,
            kind,
            names: Vec::new(),
            star,
            default,
        }));
    }

    // ---- definitions

    fn visibility(name: &str) -> Visibility {
        if name.starts_with(char::is_uppercase) {
            Visibility::Public
        } else {
            Visibility::Package
        }
    }

    /// The key a definition takes: twins of one key are told apart in source
    /// order.
    fn key_for(&mut self, container: &[String], name: &str, ns: Namespace) -> LocalKey {
        let key = LocalKey::new(
            container.iter().map(|c| Name::new(c.as_str())).collect(),
            name,
            ns,
        );
        let seen = self.twins.entry(key.clone()).or_insert(0);
        let nth = *seen;
        *seen += 1;
        if nth == 0 {
            key
        } else {
            key.with_disambig(crate::facts::Disambig::Overload(nth))
        }
    }

    /// Emits one definition. `span_node` is its lines, `root` what its hashes
    /// read, less `members`; `body` is the part that makes the body hash.
    #[allow(clippy::too_many_arguments)]
    fn define(
        &mut self,
        span_node: Node<'t>,
        name_node: Node<'t>,
        container: &[String],
        kind: DefKind,
        root: Node<'t>,
        members: &[Node<'t>],
        body: Option<Node<'t>>,
        signature: String,
        docs: &[&'t str],
    ) -> LocalKey {
        let name = self.text(name_node);
        let ns = match kind {
            DefKind::Struct | DefKind::Interface | DefKind::TypeAlias => Namespace::Type,
            _ => Namespace::Value,
        };
        let key = self.key_for(container, name, ns);
        let body_hash = match body {
            Some(body) => self.hash_tree(body, &[], None),
            None if matches!(
                kind,
                DefKind::Struct | DefKind::Interface | DefKind::TypeAlias
            ) =>
            {
                self.hash_tree(root, members, Some(name_node.id()))
            }
            None => 0,
        };
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
            kind,
            span: span(span_node),
            name_span: span(name_node),
            signature: Some(signature),
            decorators: Vec::new(),
            visibility: Self::visibility(name),
            body_hash,
            text_hash: self.text_hash(span_node, members),
            doc_hash,
        }));
        key
    }

    /// The declaration's tokens without its body, comments and members.
    fn signature(&self, node: Node<'t>, members: &[Node<'t>], body: Option<Node<'t>>) -> String {
        let mut tokens: Vec<&'t str> = Vec::new();
        let mut stack = vec![node];
        let body_id = body.map(|b| b.id());
        let named_body = node.child_by_field_name("body").map(|b| b.id());
        let value = node.child_by_field_name("value").map(|v| v.id());
        while let Some(n) = stack.pop() {
            let held = n.kind() == "comment"
                || Some(n.id()) == body_id
                || Some(n.id()) == named_body
                || Some(n.id()) == value
                || members.iter().any(|m| m.id() == n.id());
            if held && n.id() != node.id() {
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

    fn hash_tree(&self, root: Node<'t>, members: &[Node<'t>], skip: Option<usize>) -> u64 {
        let mut hash = Hasher::new();
        let mut stack = vec![(root, false)];
        while let Some((node, closing)) = stack.pop() {
            if closing {
                hash.bytes(&[2]);
                continue;
            }
            let held = node.id() != root.id()
                && (members.iter().any(|m| m.id() == node.id()) || Some(node.id()) == skip);
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
        let joined: Vec<u8> = pieces.join(&b'\n');
        hash_text(&joined)
    }

    // ---- functions and methods

    fn function(&mut self, node: Node<'t>, docs: &[&'t str]) {
        let Some(name_node) = node.child_by_field_name("name") else {
            return;
        };
        let body = node.child_by_field_name("body");
        let key = self.define(
            node,
            name_node,
            &[],
            DefKind::Function,
            node,
            &[],
            body,
            self.signature(node, &[], body),
            docs,
        );
        let name = self.text(name_node);
        let parameters = node.child_by_field_name("parameters");
        if self.is_test(name, parameters) {
            self.entry(node, EntryKind::Test, &key);
        } else if name == "main"
            && self.package == "main"
            && parameters.is_none_or(|p| children(p).is_empty())
        {
            self.entry(node, EntryKind::Main, &key);
        }
        let cx = Cx {
            container: Vec::new(),
            scope: Scope::Def(key),
        };
        self.callable(node, None, &cx);
    }

    fn method(&mut self, node: Node<'t>, docs: &[&'t str]) {
        let Some(name_node) = node.child_by_field_name("name") else {
            return;
        };
        let receiver = node.child_by_field_name("receiver");
        let owner = receiver
            .and_then(|r| children(r).into_iter().next())
            .and_then(|p| p.child_by_field_name("type"))
            .and_then(|t| self.type_text(t))
            .map(|t| t.rsplit('.').next().unwrap_or(&t).to_string());
        let Some(owner) = owner else {
            return;
        };
        let container = [owner];
        let body = node.child_by_field_name("body");
        let key = self.define(
            node,
            name_node,
            &container,
            DefKind::Method,
            node,
            &[],
            body,
            self.signature(node, &[], body),
            docs,
        );
        let cx = Cx {
            container: container.to_vec(),
            scope: Scope::Def(key),
        };
        self.callable(node, receiver, &cx);
    }

    fn entry(&mut self, node: Node<'t>, kind: EntryKind, key: &LocalKey) {
        self.facts.push(Fact::EntryPoint(EntryPoint {
            site: span(node),
            kind,
            target: Scope::Def(key.clone()),
            framework: None,
        }));
    }

    /// `TestX(t *testing.T)`, `BenchmarkX(b *testing.B)`, `FuzzX(f *testing.F)`
    /// and `ExampleX()`.
    fn is_test(&self, name: &str, parameters: Option<Node<'t>>) -> bool {
        let tested = |prefix: &str| {
            name.strip_prefix(prefix)
                .is_some_and(|rest| !rest.starts_with(char::is_lowercase))
        };
        let takes_testing = parameters.is_some_and(|p| self.text(p).contains("testing."));
        ((tested("Test") || tested("Benchmark") || tested("Fuzz")) && takes_testing)
            || (tested("Example") && parameters.is_none_or(|p| children(p).is_empty()))
    }

    /// The parameters, results and body of a function, method or literal, in a
    /// scope of their own.
    fn callable(&mut self, node: Node<'t>, receiver: Option<Node<'t>>, cx: &Cx) {
        self.locals.push(HashMap::new());
        if let Some(tparams) = node.child_by_field_name("type_parameters") {
            self.type_refs(tparams, cx, 2);
        }
        if let Some(receiver) = receiver {
            self.parameters(receiver, cx, 2);
        }
        if let Some(parameters) = node.child_by_field_name("parameters") {
            self.parameters(parameters, cx, 2);
        }
        if let Some(result) = node.child_by_field_name("result") {
            if result.kind() == "parameter_list" {
                self.parameters(result, cx, 2);
            } else {
                self.type_refs(result, cx, 2);
            }
        }
        if let Some(body) = node.child_by_field_name("body") {
            self.walk(body, cx, 2);
        }
        self.locals.pop();
    }

    fn parameters(&mut self, list: Node<'t>, cx: &Cx, depth: u16) {
        for parameter in children(list) {
            let Some(ty) = parameter.child_by_field_name("type") else {
                continue;
            };
            self.type_refs(ty, cx, depth + 1);
            let known = self.type_text(ty).map(Ty::Named);
            let mut cursor = parameter.walk();
            let names: Vec<Node<'t>> = parameter
                .children_by_field_name("name", &mut cursor)
                .filter(|n| n.kind() == "identifier")
                .collect();
            for name in names {
                // A variadic parameter is a slice of its type.
                let ty = if parameter.kind() == "variadic_parameter_declaration" {
                    None
                } else {
                    known.clone()
                };
                self.declare(name, ty, LocalKind::Parameter, cx);
            }
        }
    }

    // ---- types

    /// The type a type node names, as written without `*` and type arguments;
    /// `None` for a type with no name (a slice, a map, a function type).
    fn type_text(&self, node: Node<'t>) -> Option<String> {
        match node.kind() {
            "type_identifier" => {
                let name = self.text(node);
                (!PREDECLARED.contains(&name)).then(|| name.to_string())
            }
            "qualified_type" => Some(format!(
                "{}.{}",
                self.text(node.child_by_field_name("package")?),
                self.text(node.child_by_field_name("name")?)
            )),
            "pointer_type" | "parenthesized_type" => {
                self.type_text(children(node).into_iter().next()?)
            }
            "generic_type" => self.type_text(node.child_by_field_name("type")?),
            _ => None,
        }
    }

    fn type_declaration(&mut self, node: Node<'t>, docs: &[&'t str]) {
        let specs: Vec<Node<'t>> = children(node)
            .into_iter()
            .filter(|c| matches!(c.kind(), "type_spec" | "type_alias"))
            .collect();
        let lone = specs.len() == 1;
        for spec in specs {
            let Some(name_node) = spec.child_by_field_name("name") else {
                continue;
            };
            let Some(ty) = spec.child_by_field_name("type") else {
                continue;
            };
            let members: Vec<Node<'t>> = match ty.kind() {
                "struct_type" => children(ty)
                    .into_iter()
                    .filter(|c| c.kind() == "field_declaration_list")
                    .flat_map(children)
                    .filter(|c| c.kind() == "field_declaration")
                    .collect(),
                "interface_type" => children(ty)
                    .into_iter()
                    .filter(|c| c.kind() == "method_elem")
                    .collect(),
                _ => Vec::new(),
            };
            let kind = match (spec.kind(), ty.kind()) {
                ("type_alias", _) => DefKind::TypeAlias,
                (_, "struct_type") => DefKind::Struct,
                (_, "interface_type") => DefKind::Interface,
                _ => DefKind::TypeAlias,
            };
            let span_node = if lone { node } else { spec };
            let signature = self.signature(spec, &members, None);
            let key = self.define(
                span_node,
                name_node,
                &[],
                kind,
                spec,
                &members,
                None,
                signature,
                docs,
            );
            let inside = Cx {
                container: vec![self.text(name_node).to_string()],
                scope: Scope::Def(key),
            };
            if let Some(tparams) = spec.child_by_field_name("type_parameters") {
                self.type_refs(tparams, &inside, 2);
            }
            match ty.kind() {
                "struct_type" => {
                    for field in &members {
                        self.field(*field, &inside, docs);
                    }
                }
                "interface_type" => self.interface(ty, &inside, docs),
                _ => self.type_refs(ty, &inside, 2),
            }
        }
    }

    fn field(&mut self, node: Node<'t>, cx: &Cx, docs: &[&'t str]) {
        let Some(ty) = node.child_by_field_name("type") else {
            return;
        };
        self.type_refs(ty, cx, 3);
        let mut cursor = node.walk();
        let names: Vec<Node<'t>> = node
            .children_by_field_name("name", &mut cursor)
            .filter(|n| n.kind() == "field_identifier")
            .collect();
        let mut written = self.signature(ty, &[], None);
        if names.is_empty() {
            // `*Base` embeds a pointer: the `*` belongs to the field, not to the
            // type node, and the two embeddings promote different method sets.
            let mut walker = node.walk();
            if node
                .children(&mut walker)
                .any(|c| c.kind() == "*" && c.end_byte() <= ty.start_byte())
            {
                written = format!("* {written}");
            }
            // An embedded field is named after its type, and is that type alone.
            if let Some(name_node) = embedded_name_node(ty) {
                self.define(
                    node,
                    name_node,
                    &cx.container,
                    DefKind::Field,
                    node,
                    &[],
                    None,
                    written,
                    docs,
                );
            }
            return;
        }
        for name in names {
            let signature = format!("{} {written}", self.text(name));
            self.define(
                node,
                name,
                &cx.container,
                DefKind::Field,
                node,
                &[],
                None,
                signature,
                docs,
            );
        }
    }

    fn interface(&mut self, node: Node<'t>, cx: &Cx, docs: &[&'t str]) {
        for member in children(node) {
            match member.kind() {
                "method_elem" => {
                    let Some(name) = member.child_by_field_name("name") else {
                        continue;
                    };
                    let key = self.define(
                        member,
                        name,
                        &cx.container,
                        DefKind::Method,
                        member,
                        &[],
                        None,
                        self.signature(member, &[], None),
                        docs,
                    );
                    let inside = Cx {
                        container: cx.container.clone(),
                        scope: Scope::Def(key),
                    };
                    for part in ["parameters", "result"] {
                        if let Some(part) = member.child_by_field_name(part) {
                            self.type_refs(part, &inside, 3);
                        }
                    }
                }
                _ => self.type_refs(member, cx, 3),
            }
        }
    }

    /// The names in a type position, each a `TypeUse`.
    fn type_refs(&mut self, node: Node<'t>, cx: &Cx, depth: u16) {
        if !self.deeper(depth) {
            return;
        }
        match node.kind() {
            "type_identifier" => {
                let name = self.text(node);
                if !PREDECLARED.contains(&name) {
                    self.emit(node, cx, Receiver::None, UseKind::TypeUse, name, Vec::new());
                }
            }
            "qualified_type" => {
                if let (Some(package), Some(name)) = (
                    node.child_by_field_name("package"),
                    node.child_by_field_name("name"),
                ) {
                    self.emit(
                        name,
                        cx,
                        Receiver::None,
                        UseKind::TypeUse,
                        self.text(name),
                        vec![Name::new(self.text(package))],
                    );
                }
            }
            "array_type" => {
                if let Some(length) = node.child_by_field_name("length") {
                    self.walk(length, cx, depth + 1);
                }
                if let Some(element) = node.child_by_field_name("element") {
                    self.type_refs(element, cx, depth + 1);
                }
            }
            "func_literal" => self.walk(node, cx, depth + 1),
            _ => {
                for child in children(node) {
                    if child.kind() == "comment" {
                        continue;
                    }
                    self.type_refs(child, cx, depth + 1);
                }
            }
        }
    }

    // ---- constants and variables

    fn values(&mut self, node: Node<'t>, kind: DefKind, cx: &Cx, docs: &[&'t str]) {
        let mut specs: Vec<Node<'t>> = Vec::new();
        for child in children(node) {
            match child.kind() {
                "const_spec" | "var_spec" => specs.push(child),
                "var_spec_list" => specs.extend(
                    children(child)
                        .into_iter()
                        .filter(|c| c.kind() == "var_spec"),
                ),
                _ => {}
            }
        }
        let lone = specs.len() == 1;
        for spec in specs {
            let mut cursor = spec.walk();
            let names: Vec<Node<'t>> = spec
                .children_by_field_name("name", &mut cursor)
                .filter(|n| n.kind() == "identifier")
                .collect();
            let value = spec.child_by_field_name("value");
            let mut keys: Vec<LocalKey> = Vec::new();
            for name in &names {
                if self.text(*name) == "_" {
                    continue;
                }
                let span_node = if lone { node } else { spec };
                let signature = self.signature(spec, &[], value);
                keys.push(self.define(
                    span_node,
                    *name,
                    &cx.container,
                    kind,
                    spec,
                    &[],
                    value,
                    signature,
                    docs,
                ));
            }
            let scope = keys
                .first()
                .map_or_else(|| cx.scope.clone(), |k| Scope::Def(k.clone()));
            let inside = Cx {
                container: cx.container.clone(),
                scope,
            };
            if let Some(ty) = spec.child_by_field_name("type") {
                self.type_refs(ty, &inside, 2);
            }
            if let Some(value) = value {
                self.walk(value, &inside, 2);
            }
        }
    }

    // ---- statements and expressions

    fn emit(
        &mut self,
        site: Node<'t>,
        cx: &Cx,
        receiver: Receiver,
        use_kind: UseKind,
        name: &str,
        qualifier: Vec<Name>,
    ) {
        self.facts.push(Fact::Reference(Ref {
            site: span(site),
            enclosing: cx.scope.clone(),
            receiver,
            use_kind,
            name: Name::new(name),
            qualifier,
        }));
    }

    /// `a`, `a.b`, `a.b.c` as `[a, b, c]` when it is made of identifiers only.
    fn chain(&self, node: Node<'t>) -> Option<Vec<String>> {
        match node.kind() {
            "identifier" => Some(vec![self.text(node).to_string()]),
            "selector_expression" => {
                let mut head = self.chain(node.child_by_field_name("operand")?)?;
                head.push(self.text(node.child_by_field_name("field")?).to_string());
                Some(head)
            }
            "parenthesized_expression" => self.chain(children(node).into_iter().next()?),
            _ => None,
        }
    }

    fn names(path: &[String]) -> Vec<Name> {
        path.iter().map(|s| Name::new(s.as_str())).collect()
    }

    /// What an expression is the value of, when the syntax says.
    fn ty_of(&self, node: Node<'t>) -> Option<Ty> {
        match node.kind() {
            "composite_literal" => self
                .type_text(node.child_by_field_name("type")?)
                .map(Ty::Named),
            "unary_expression" => {
                if node.child_by_field_name("operator").map(|o| self.text(o)) == Some("&") {
                    self.ty_of(node.child_by_field_name("operand")?)
                } else {
                    None
                }
            }
            "parenthesized_expression" => self.ty_of(children(node).into_iter().next()?),
            "type_assertion_expression" => self
                .type_text(node.child_by_field_name("type")?)
                .map(Ty::Named),
            "identifier" => match self.lookup(self.text(node)) {
                Some(known) => known.clone(),
                None => None,
            },
            "call_expression" => {
                let function = node.child_by_field_name("function")?;
                let path = self.chain(function)?;
                if path.as_slice() == ["new"] {
                    let argument = children(node.child_by_field_name("arguments")?)
                        .into_iter()
                        .next()?;
                    return self
                        .chain(argument)
                        .or_else(|| {
                            self.type_text(argument)
                                .map(|t| t.split('.').map(str::to_string).collect())
                        })
                        .map(|p| Ty::Named(p.join(".")));
                }
                // A local function value and `make` (a slice, a map, a channel)
                // have no type to name.
                if self.is_local(&path[0]) || path == ["make"] {
                    return None;
                }
                Some(Ty::Result(path))
            }
            _ => None,
        }
    }

    /// The receiver and qualifier of what is selected from `operand`.
    fn receiver_of(&mut self, operand: Node<'t>, cx: &Cx, depth: u16) -> (Receiver, Vec<Name>) {
        if let Some(chain) = self.chain(operand) {
            let head = chain[0].as_str();
            if !self.is_local(head) {
                return (Receiver::None, Self::names(&chain));
            }
            let rest = Self::names(&chain[1..]);
            return match self.lookup(head).cloned().flatten() {
                Some(Ty::Named(t)) => (Receiver::Typed(TypeName::new(t.as_str())), rest),
                Some(Ty::Result(path)) => (Receiver::CallResult(Self::names(&path)), rest),
                None => (Receiver::Unknown, Self::names(&chain)),
            };
        }
        // Whatever else is written is read for what it uses, and its type is
        // what the syntax of it says.
        self.walk(operand, cx, depth + 1);
        let mut at = operand;
        while matches!(at.kind(), "parenthesized_expression")
            || (at.kind() == "unary_expression"
                && at.child_by_field_name("operator").map(|o| self.text(o)) == Some("&"))
        {
            match children(at).into_iter().next() {
                Some(inner) => at = inner,
                None => break,
            }
        }
        match self.ty_of(at) {
            Some(Ty::Named(t)) => (Receiver::Typed(TypeName::new(t.as_str())), Vec::new()),
            Some(Ty::Result(path)) => (Receiver::CallResult(Self::names(&path)), Vec::new()),
            None => (Receiver::Unknown, Vec::new()),
        }
    }

    fn selector(&mut self, node: Node<'t>, cx: &Cx, kind: UseKind, depth: u16) {
        let (Some(operand), Some(field)) = (
            node.child_by_field_name("operand"),
            node.child_by_field_name("field"),
        ) else {
            return;
        };
        let (receiver, qualifier) = self.receiver_of(operand, cx, depth);
        self.emit(field, cx, receiver, kind, self.text(field), qualifier);
    }

    fn block(&mut self, node: Node<'t>, cx: &Cx, depth: u16) {
        self.locals.push(HashMap::new());
        for child in children(node) {
            self.walk(child, cx, depth + 1);
        }
        self.locals.pop();
    }

    fn walk(&mut self, node: Node<'t>, cx: &Cx, depth: u16) {
        if !self.deeper(depth) {
            return;
        }
        match node.kind() {
            "comment"
            | "interpreted_string_literal"
            | "raw_string_literal"
            | "rune_literal"
            | "int_literal"
            | "float_literal"
            | "imaginary_literal"
            | "nil"
            | "true"
            | "false"
            | "iota"
            | "field_identifier"
            | "label_name"
            | "package_identifier"
            | "blank_identifier" => {}
            "identifier" => {
                let name = self.text(node);
                if !self.is_local(name) {
                    self.emit(node, cx, Receiver::None, UseKind::Read, name, Vec::new());
                }
            }
            "selector_expression" => self.selector(node, cx, UseKind::Read, depth),
            "call_expression" => self.call(node, cx, depth),
            "composite_literal" => self.composite(node, cx, depth),
            "func_literal" => self.closure(node, cx, cx.scope.clone(), depth),
            "type_conversion_expression" | "type_assertion_expression" => {
                if let Some(ty) = node.child_by_field_name("type") {
                    self.type_refs(ty, cx, depth + 1);
                }
                if let Some(operand) = node.child_by_field_name("operand") {
                    self.walk(operand, cx, depth + 1);
                }
            }
            "type_identifier" | "qualified_type" | "generic_type" | "slice_type" | "array_type"
            | "map_type" | "pointer_type" | "channel_type" | "function_type" | "struct_type"
            | "interface_type" | "type_arguments" | "type_elem" => {
                self.type_refs(node, cx, depth + 1);
            }
            "short_var_declaration" => self.short_declaration(node, cx, depth),
            "assignment_statement" => {
                if let Some(right) = node.child_by_field_name("right") {
                    self.walk(right, cx, depth + 1);
                }
                if let Some(left) = node.child_by_field_name("left") {
                    for target in children(left) {
                        self.write(target, cx, depth + 1);
                    }
                }
            }
            "inc_statement" | "dec_statement" => {
                for target in children(node) {
                    self.write(target, cx, depth + 1);
                }
            }
            "range_clause" => {
                if let Some(right) = node.child_by_field_name("right") {
                    self.walk(right, cx, depth + 1);
                }
                let defines = node.children(&mut node.walk()).any(|c| c.kind() == ":=");
                if let Some(left) = node.child_by_field_name("left") {
                    for target in children(left) {
                        if defines && target.kind() == "identifier" {
                            self.declare(target, None, LocalKind::Variable, cx);
                        } else {
                            self.write(target, cx, depth + 1);
                        }
                    }
                }
            }
            "var_declaration" | "const_declaration" => self.local_values(node, cx, depth),
            "type_declaration" => {
                for spec in children(node) {
                    if let Some(ty) = spec.child_by_field_name("type") {
                        self.type_refs(ty, cx, depth + 1);
                    }
                }
            }
            "type_switch_statement" => {
                self.locals.push(HashMap::new());
                for part in ["initializer", "value"] {
                    if let Some(part) = node.child_by_field_name(part) {
                        self.walk(part, cx, depth + 1);
                    }
                }
                if let Some(alias) = node.child_by_field_name("alias") {
                    for name in children(alias) {
                        if name.kind() == "identifier" {
                            self.declare(name, None, LocalKind::Variable, cx);
                        }
                    }
                }
                for case in children(node) {
                    self.walk(case, cx, depth + 1);
                }
                self.locals.pop();
            }
            "type_case" => {
                self.locals.push(HashMap::new());
                let mut cursor = node.walk();
                let types: Vec<Node<'t>> =
                    node.children_by_field_name("type", &mut cursor).collect();
                for ty in types {
                    self.type_refs(ty, cx, depth + 1);
                }
                for child in children(node) {
                    if child.kind() == "statement_list" {
                        self.walk(child, cx, depth + 1);
                    }
                }
                self.locals.pop();
            }
            "block"
            | "for_statement"
            | "if_statement"
            | "expression_switch_statement"
            | "select_statement"
            | "expression_case"
            | "default_case"
            | "communication_case" => {
                self.block(node, cx, depth);
            }
            _ => {
                for child in children(node) {
                    self.walk(child, cx, depth + 1);
                }
            }
        }
    }

    /// The target of an assignment: a field or a package variable is written, a
    /// local is nobody's.
    fn write(&mut self, target: Node<'t>, cx: &Cx, depth: u16) {
        match target.kind() {
            "identifier" => {
                let name = self.text(target);
                if name != "_" && !self.is_local(name) {
                    self.emit(target, cx, Receiver::None, UseKind::Write, name, Vec::new());
                }
            }
            "selector_expression" => self.selector(target, cx, UseKind::Write, depth),
            "parenthesized_expression" => {
                if let Some(inner) = children(target).into_iter().next() {
                    self.write(inner, cx, depth + 1);
                }
            }
            _ => self.walk(target, cx, depth + 1),
        }
    }

    fn short_declaration(&mut self, node: Node<'t>, cx: &Cx, depth: u16) {
        let (Some(left), Some(right)) = (
            node.child_by_field_name("left"),
            node.child_by_field_name("right"),
        ) else {
            return;
        };
        self.walk(right, cx, depth + 1);
        let targets: Vec<Node<'t>> = children(left);
        let values: Vec<Node<'t>> = children(right);
        for (at, target) in targets.iter().enumerate() {
            let ty = if values.len() == targets.len() {
                self.ty_of(values[at])
            } else if at == 0 && values.len() == 1 {
                // `x, err := f()` types x by the call; the rest is unsaid.
                self.ty_of(values[0])
            } else {
                None
            };
            if target.kind() == "identifier" {
                self.declare(*target, ty, LocalKind::Variable, cx);
            }
        }
    }

    fn local_values(&mut self, node: Node<'t>, cx: &Cx, depth: u16) {
        let mut specs: Vec<Node<'t>> = Vec::new();
        for child in children(node) {
            match child.kind() {
                "const_spec" | "var_spec" => specs.push(child),
                "var_spec_list" => specs.extend(children(child)),
                _ => {}
            }
        }
        for spec in specs {
            let declared = spec.child_by_field_name("type");
            if let Some(ty) = declared {
                self.type_refs(ty, cx, depth + 1);
            }
            let value = spec.child_by_field_name("value");
            if let Some(value) = value {
                self.walk(value, cx, depth + 1);
            }
            let values: Vec<Node<'t>> = value.map_or_else(Vec::new, children);
            let mut cursor = spec.walk();
            let names: Vec<Node<'t>> = spec
                .children_by_field_name("name", &mut cursor)
                .filter(|n| n.kind() == "identifier")
                .collect();
            for (at, name) in names.iter().enumerate() {
                let ty = declared
                    .and_then(|t| self.type_text(t))
                    .map(Ty::Named)
                    .or_else(|| {
                        (values.len() == names.len())
                            .then(|| self.ty_of(values[at]))
                            .flatten()
                    });
                self.declare(*name, ty, LocalKind::Variable, cx);
            }
        }
    }

    fn call(&mut self, node: Node<'t>, cx: &Cx, depth: u16) {
        let Some(function) = node.child_by_field_name("function") else {
            return;
        };
        if let Some(arguments) = node.child_by_field_name("type_arguments") {
            self.type_refs(arguments, cx, depth + 1);
        }
        let callee_names = self
            .chain(function)
            .map_or_else(Vec::new, |path| Self::names(&path));
        let arguments = node.child_by_field_name("arguments").map(children);
        // `new(T)` and `make(T, …)` name their first argument as a type, said once.
        let mut typed_first = false;
        match function.kind() {
            "identifier" => {
                let name = self.text(function);
                if !self.is_local(name) {
                    match (name, arguments.as_ref().and_then(|a| a.first())) {
                        ("new", Some(argument)) => {
                            typed_first = true;
                            self.instantiate(*argument, cx);
                        }
                        ("make", Some(argument)) => {
                            typed_first = true;
                            self.type_refs(*argument, cx, depth + 1);
                        }
                        _ => self.emit(
                            function,
                            cx,
                            Receiver::None,
                            UseKind::Call,
                            name,
                            Vec::new(),
                        ),
                    }
                }
            }
            "selector_expression" => self.selector(function, cx, UseKind::Call, depth),
            "slice_type" | "array_type" | "map_type" | "pointer_type" | "channel_type"
            | "function_type" | "struct_type" | "interface_type" => {
                self.type_refs(function, cx, depth + 1);
            }
            _ => self.walk(function, cx, depth + 1),
        }
        for argument in arguments
            .into_iter()
            .flatten()
            .skip(usize::from(typed_first))
        {
            match argument.kind() {
                "identifier" => {
                    let name = self.text(argument);
                    if !self.is_local(name) {
                        self.emit(
                            argument,
                            cx,
                            Receiver::None,
                            UseKind::PassAsValue,
                            name,
                            Vec::new(),
                        );
                    }
                }
                "selector_expression" => {
                    self.selector(argument, cx, UseKind::PassAsValue, depth + 1);
                }
                "func_literal" => {
                    let scope = Scope::Callback {
                        callee: callee_names.clone(),
                        site: span(argument),
                        parent: Box::new(cx.scope.clone()),
                    };
                    self.closure(argument, cx, scope, depth + 1);
                }
                "variadic_argument" => {
                    for inner in children(argument) {
                        self.walk(inner, cx, depth + 1);
                    }
                }
                _ => self.walk(argument, cx, depth + 1),
            }
        }
    }

    /// The type `new(T)` builds.
    fn instantiate(&mut self, node: Node<'t>, cx: &Cx) {
        let (site, name, qualifier) = match node.kind() {
            "identifier" | "type_identifier" => (node, self.text(node).to_string(), Vec::new()),
            "qualified_type" => match (
                node.child_by_field_name("package"),
                node.child_by_field_name("name"),
            ) {
                (Some(package), Some(name)) => (
                    name,
                    self.text(name).to_string(),
                    vec![Name::new(self.text(package))],
                ),
                _ => return,
            },
            "selector_expression" => match (
                node.child_by_field_name("operand")
                    .and_then(|o| self.chain(o)),
                node.child_by_field_name("field"),
            ) {
                (Some(path), Some(field)) => {
                    (field, self.text(field).to_string(), Self::names(&path))
                }
                _ => return,
            },
            _ => return,
        };
        if PREDECLARED.contains(&name.as_str()) {
            return;
        }
        self.emit(
            site,
            cx,
            Receiver::None,
            UseKind::Instantiate,
            &name,
            qualifier,
        );
    }

    fn composite(&mut self, node: Node<'t>, cx: &Cx, depth: u16) {
        let ty = node.child_by_field_name("type");
        let named = ty.and_then(|t| self.type_text(t));
        if let Some(ty) = ty {
            match ty.kind() {
                "type_identifier" | "qualified_type" => self.instantiate(ty, cx),
                "generic_type" => {
                    if let Some(base) = ty.child_by_field_name("type") {
                        self.instantiate(base, cx);
                    }
                    if let Some(arguments) = ty.child_by_field_name("type_arguments") {
                        self.type_refs(arguments, cx, depth + 1);
                    }
                }
                _ => self.type_refs(ty, cx, depth + 1),
            }
        }
        let Some(body) = node.child_by_field_name("body") else {
            return;
        };
        self.elements(body, named.as_deref(), cx, depth + 1);
    }

    /// The elements of a literal; a key of a literal of a named type is the
    /// name of one of its fields.
    fn elements(&mut self, body: Node<'t>, named: Option<&str>, cx: &Cx, depth: u16) {
        if !self.deeper(depth) {
            return;
        }
        for element in children(body) {
            match element.kind() {
                "keyed_element" => {
                    let key = element.child_by_field_name("key").map(children);
                    let value = element.child_by_field_name("value");
                    let field = key
                        .as_ref()
                        .and_then(|k| k.first())
                        .filter(|k| k.kind() == "identifier");
                    match (field, named) {
                        (Some(field), Some(ty)) => self.emit(
                            *field,
                            cx,
                            Receiver::Typed(TypeName::new(ty)),
                            UseKind::Write,
                            self.text(*field),
                            Vec::new(),
                        ),
                        _ => {
                            for part in key.into_iter().flatten() {
                                self.walk(part, cx, depth + 1);
                            }
                        }
                    }
                    if let Some(value) = value {
                        self.element(value, cx, depth + 1);
                    }
                }
                _ => self.element(element, cx, depth + 1),
            }
        }
    }

    fn element(&mut self, node: Node<'t>, cx: &Cx, depth: u16) {
        for child in children(node) {
            if child.kind() == "literal_value" {
                self.elements(child, None, cx, depth + 1);
            } else {
                self.walk(child, cx, depth + 1);
            }
        }
    }

    /// A function literal: its parameters and body in a scope of their own.
    fn closure(&mut self, node: Node<'t>, cx: &Cx, scope: Scope, _depth: u16) {
        let inner = Cx {
            container: cx.container.clone(),
            scope,
        };
        self.callable(node, None, &inner);
    }
}

/// The identifier a type names as an embedded field's own name.
fn embedded_name_node(ty: Node<'_>) -> Option<Node<'_>> {
    match ty.kind() {
        "type_identifier" => Some(ty),
        "qualified_type" => ty.child_by_field_name("name"),
        "pointer_type" | "parenthesized_type" => {
            embedded_name_node(children(ty).into_iter().next()?)
        }
        "generic_type" => embedded_name_node(ty.child_by_field_name("type")?),
        _ => None,
    }
}
