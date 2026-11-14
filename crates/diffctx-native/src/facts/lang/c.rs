//! What a C or C++ file's syntax states, as the facts contract names it. A `.h`
//! file that reads as C++ (a class, a namespace, a template, an access specifier)
//! is read with the C++ grammar and stays a `C` file; `.cc`, `.cpp`, `.hpp` and
//! the like are `Cpp`. One walker reads both: what is C++ only is below.
//!
//! Conventions a reader of the facts cannot recover from the types:
//! - A key's module is the file's path with its extension, so a header and its
//!   source (one stem) are two modules, each with its own `f`.
//! - `#include "a/b.h"` is an [`Import`] of kind `Require` with `relative: 1`, module
//!   `a/b.h` and `star: Glob` (the whole file); `#include <a/b.h>` is the same
//!   with `relative: 0`. An include through a macro is no import.
//! - A function definition is a `Function` with its body; a prototype is a
//!   `Function` with `body_hash: 0` (so is an `extern` variable). A `static`
//!   definition is `Private`: its reach is its file, and a header's `static
//!   inline` function is every includer's. A prototype beside the definition in
//!   one file is told from it by `Cfg("declaration")`.
//! - Anything under `#if`, `#ifdef`, `#ifndef`, `#elif` or `#else` (a header
//!   guard apart) carries `Cfg(<directive text>)`, the `#else` branch the same text
//!   prefixed `else `: what a build defines is not evaluated, so the definition
//!   may not exist.
//! - A struct or union is a `Struct`, an enum an `Enum`, a typedef a `TypeAlias`
//!   (`typedef struct n {..} N;` is the `Struct` `n` and the alias `N`; an
//!   unnamed one is the `Struct` `N`). A member is a `Field` of its struct, an
//!   enumerator an `EnumMember` of its enum (container empty when the enum is
//!   unnamed: the name is global). The members of an anonymous struct or union in
//!   a struct are its own.
//! - A macro is a `Macro` whose signature is its `#define` line; the names in its
//!   replacement text are references from it (`f(` is a `Call`, any other name a
//!   `Read`), unless the name is a parameter or touches `##`.
//! - A reference names the name written; a bare name that is a local or a parameter
//!   of the function around is no reference. A member access `a->f` / `a.f` is `f`
//!   with the chain before it: `Typed(T)` for a local or parameter whose declared
//!   type names `T` (a tag or a typedef; pointer or not), `None` with the global's
//!   name first in `qualifier` for any other name, `CallResult([g])` for `g()->f`,
//!   `Unknown` for the rest; casts type their operand. A call through a local, a
//!   parameter or a member is a call through a pointer: `Unknown`.
//! - A name written where a type is expected (`struct n`, a typedef name, in a
//!   declaration, a cast or `sizeof`) is a `TypeUse`.
//! - C++: a namespace block is a `Module` definition, and what it holds has its
//!   name in its container (a key's container is the namespaces, then the
//!   classes around, then what the name was written with: `A::f` out of line is
//!   `f` in `[A]`). A class or struct is a `Class` or `Struct`; its members are
//!   `Method`s, `Constructor`s (named like the class), destructors (`~A`, a
//!   `Method`), `Field`s, with the access the specifier before them gave. A member
//!   declared in the class and defined out of line in the same file is a twin
//!   (`Cfg("declaration")`); overloads of one name are twins in source order. A
//!   template is the one definition it declares, its `template <...>` header the
//!   start of the signature. `using a::f;` is an import of `f` from `a`, `using
//!   namespace a;` an import of everything of `a`, both enclosed by the namespace
//!   block they sit in; `using T = U;` is a `TypeAlias`. `new T` and an object
//!   declared by value are `Instantiate`s of `T` (written with its scope in
//!   `qualifier`); `this` is `SelfType`; `a::f()` is `f` with `[a]` and the
//!   receiver `Module(a)`, and `Read`s of `a::x` and types written `a::T` are the
//!   same: a chain `g.x` of a global keeps `None`, so the two are told apart.
//! - `main` is a `Main` entry point.
//! - Hashes are FNV-1a over the definition's own parts: members that are
//!   definitions themselves are left out of their container's.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};

use tree_sitter::Node;

use super::walker::hash_outside;
use super::{
    ERROR_RANGES_KEPT, children, lines_of, merge_ranges, parse_tree, span, through_errors,
};
use crate::facts::{
    BlobFacts, Def, DefKind, Disambig, EntryKind, EntryPoint, Extractor, Fact, Hasher, Import,
    ImportKind, Language, Local, LocalKey, LocalKind, ModulePath, Name, Namespace, Receiver, Ref,
    Scope, Span, Star, TypeName, UseKind, Visibility, expired, hash_text,
};
use crate::snapshot::{Loss, RepoPath, RunBudget};

const MAX_DEPTH: u16 = 256;
const POLL_EVERY: usize = 256;

pub struct CExtractor;

impl Extractor for CExtractor {
    fn language(&self) -> Language {
        Language::C
    }

    fn extract(&self, text: &[u8], budget: &RunBudget) -> BlobFacts {
        extract(text, budget, Language::C)
    }
}

/// C++ sources, read by the C walker with the C++ grammar; a `.h` that reads as
/// C++ is read the same way and stays a `C` file.
pub struct CppExtractor;

impl Extractor for CppExtractor {
    fn language(&self) -> Language {
        Language::Cpp
    }

    fn extract(&self, text: &[u8], budget: &RunBudget) -> BlobFacts {
        extract(text, budget, Language::Cpp)
    }
}

fn unbound() -> RepoPath {
    RepoPath::new("<blob>").expect("a valid path")
}

/// Whether the text is C++: a class, a namespace, a template or an access
/// specifier at the start of a line. A `.h` that is, is read as C++.
fn looks_like_cpp(text: &[u8]) -> bool {
    let head = &text[..text.len().min(1 << 16)];
    String::from_utf8_lossy(head).lines().any(|line| {
        let line = line.trim_start();
        let word = |w: &str| {
            line.strip_prefix(w)
                .is_some_and(|rest| rest.starts_with(|c: char| c.is_whitespace() || c == '<'))
        };
        (line.starts_with("class ") && (line.contains('{') || line.contains(':')))
            || word("namespace")
            || line.starts_with("template<")
            || line.starts_with("template <")
            || line.starts_with("using namespace")
            || matches!(line.trim_end(), "public:" | "private:" | "protected:")
    })
}

fn extract(text: &[u8], budget: &RunBudget, language: Language) -> BlobFacts {
    let mut blob = BlobFacts::new(language);
    let cpp = language == Language::Cpp || looks_like_cpp(text);
    let Some(tree) = parse_tree(if cpp { Language::Cpp } else { Language::C }, text, budget) else {
        if expired(budget) {
            blob.deadline();
        } else {
            blob.losses.push(Loss::Unsupported { path: unbound() });
        }
        return blob;
    };
    let root = tree.root_node();
    if root.has_error() {
        blob.parse_error(error_lines(root, cpp));
    }
    let mut walker = Walker::new(text, budget, cpp);
    walker.run(root);
    if walker.halted {
        blob.deadline();
    }
    if walker.too_deep.get() {
        blob.losses.push(Loss::ExtractorFailed { path: unbound() });
    }
    blob.facts = std::mem::take(&mut walker.facts);
    blob.record_lexical(root);
    blob.in_source_order();
    blob
}

/// The nodes whose named children the walk reads as items, an error node
/// among them: `Walker::items` and the loops over a block's members.
fn is_item_container(kind: &str) -> bool {
    matches!(
        kind,
        "translation_unit"
            | "preproc_if"
            | "preproc_ifdef"
            | "preproc_else"
            | "preproc_elif"
            | "preproc_elifdef"
            | "declaration_list"
            | "field_declaration_list"
            | "template_declaration"
    )
}

/// An item the walk reads whole when the grammar parked it in an error node.
fn is_read(kind: &str, cpp: bool) -> bool {
    matches!(
        kind,
        "comment"
            | "preproc_include"
            | "preproc_def"
            | "preproc_function_def"
            | "preproc_if"
            | "preproc_ifdef"
            | "function_definition"
            | "declaration"
            | "type_definition"
            | "namespace_definition"
            | "template_declaration"
            | "using_declaration"
            | "alias_declaration"
            | "class_specifier"
            | "struct_specifier"
            | "union_specifier"
            | "enum_specifier"
            | "linkage_specification"
    ) || (cpp && kind == "field_declaration")
}

/// The line ranges the parse could not read, merged. An error node among a
/// file's items is read by the walk (`through_errors`): the items it holds
/// whole are left out and its stray tokens, and any item that has an error
/// of its own, are what stays unread. An error node anywhere else (inside a
/// body, say) is unread whole.
fn error_lines(root: Node, cpp: bool) -> Vec<(u32, u32)> {
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
                } else if child.is_missing()
                    || child.has_error()
                    || !child.is_named()
                    || !is_read(child.kind(), cpp)
                {
                    ranges.push(lines_of(child));
                }
            }
        } else if node.is_error() || node.is_missing() {
            ranges.push(lines_of(node));
        } else if node.has_error() {
            let among = is_item_container(node.kind());
            stack.extend(node.children(&mut cursor).map(|c| (c, among)));
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
            let tight = matches!(token, ")" | "]" | "," | ";" | "[")
                || matches!(a, "(" | "[" | "*" | "&" | "~" | "!")
                || (token == "(" && (word_end(a) || a == ")" || a == "*"));
            if !tight {
                out.push(' ');
            }
        }
        out.push_str(token);
        previous = Some(token);
    }
    out
}

const KEYWORDS: &[&str] = &[
    "if", "else", "for", "while", "do", "switch", "case", "default", "return", "sizeof", "goto",
    "break", "continue", "struct", "union", "enum", "typedef", "static", "extern", "const",
    "volatile", "inline", "register", "restrict", "void", "char", "short", "int", "long", "float",
    "double", "signed", "unsigned", "defined", "NULL", "true", "false", "bool",
];

#[derive(Clone)]
struct Cx {
    scope: Scope,
    /// The conditional-compilation directive the code sits under.
    cond: Option<String>,
    /// The namespaces around, outermost first (C++).
    ns: Vec<String>,
    /// The classes around, outermost first: what a member's container adds.
    within: Vec<String>,
    /// The access a class member has, from the specifier before it.
    access: Option<Visibility>,
    /// The `template <...>` header the declaration sits under.
    template: Option<String>,
}

/// The name a declarator declares, its parameter list when it declares a
/// function, and whether that function is a pointer (`void (*f)(int)`).
struct Declared<'t> {
    name: Node<'t>,
    function: Option<Node<'t>>,
    pointer: bool,
    /// The scope an out-of-line member is written with (`A::B::f` is `[A, B]`).
    scope: Vec<String>,
}

fn declared<'t>(declarator: Node<'t>, bytes: &[u8], too_deep: &Cell<bool>) -> Option<Declared<'t>> {
    let mut node = declarator;
    let mut function: Option<Node<'t>> = None;
    let mut pointer = false;
    let mut scope: Vec<String> = Vec::new();
    let mut depth = 0;
    loop {
        depth += 1;
        if depth > 32 {
            too_deep.set(true);
            return None;
        }
        match node.kind() {
            "identifier" | "field_identifier" | "type_identifier" | "destructor_name"
            | "operator_name" => {
                return Some(Declared {
                    name: node,
                    function,
                    pointer,
                    scope,
                });
            }
            "qualified_identifier" => {
                if let Some(s) = node.child_by_field_name("scope") {
                    let text =
                        std::str::from_utf8(&bytes[s.start_byte()..s.end_byte()]).unwrap_or("");
                    scope.extend(
                        text.split("::")
                            .filter(|p| !p.is_empty())
                            .map(str::to_string),
                    );
                }
                node = node.child_by_field_name("name")?;
            }
            "template_function" => {
                node = node.child_by_field_name("name")?;
            }
            "function_declarator" => {
                let inner = node.child_by_field_name("declarator")?;
                if function.is_none() {
                    function = node.child_by_field_name("parameters");
                    pointer = inner.kind() == "parenthesized_declarator";
                }
                node = inner;
            }
            "pointer_declarator"
            | "array_declarator"
            | "attributed_declarator"
            | "init_declarator" => {
                node = node.child_by_field_name("declarator")?;
            }
            "parenthesized_declarator" | "reference_declarator" => {
                node = node.named_child(0)?;
            }
            _ => return None,
        }
    }
}

struct Walker<'t> {
    bytes: &'t [u8],
    budget: &'t RunBudget,
    ticks: usize,
    halted: bool,
    /// A cut of depth, set also by the declarator chain a free function
    /// follows.
    too_deep: Cell<bool>,
    facts: Vec<Fact>,
    twins: HashMap<LocalKey, u16>,
    /// The functions this file defines: a prototype of one is a twin.
    defined_here: HashSet<String>,
    /// The locals of the function and block scopes around, innermost last, with
    /// the type name their declaration states.
    locals: Vec<HashMap<String, Option<String>>>,
    /// The text is C++: the C++ grammar was read.
    cpp: bool,
}

impl<'t> Walker<'t> {
    fn new(bytes: &'t [u8], budget: &'t RunBudget, cpp: bool) -> Self {
        Self {
            bytes,
            budget,
            ticks: 0,
            halted: false,
            too_deep: Cell::new(false),
            facts: Vec::new(),
            twins: HashMap::new(),
            defined_here: HashSet::new(),
            locals: Vec::new(),
            cpp,
        }
    }

    fn run(&mut self, root: Node<'t>) {
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            for child in children(node) {
                match child.kind() {
                    "function_definition" => {
                        if let Some(d) = child
                            .child_by_field_name("declarator")
                            .and_then(|d| declared(d, self.bytes, &self.too_deep))
                        {
                            let name = self.text(d.name).to_string();
                            let full = if d.scope.is_empty() {
                                name
                            } else {
                                format!("{}::{name}", d.scope.join("::"))
                            };
                            self.defined_here.insert(full);
                        }
                    }
                    "preproc_if"
                    | "preproc_ifdef"
                    | "preproc_else"
                    | "preproc_elif"
                    | "preproc_elifdef"
                    | "linkage_specification"
                    | "namespace_definition"
                    | "template_declaration"
                    | "declaration_list"
                    | "ERROR" => {
                        stack.push(child);
                    }
                    _ => {}
                }
            }
        }
        let cx = Cx {
            scope: Scope::Module,
            cond: None,
            ns: Vec::new(),
            within: Vec::new(),
            access: None,
            template: None,
        };
        self.items(root, &cx, 0);
    }

    walker_methods!();
    walker_emit!();

    fn deeper(&mut self, depth: u16) -> bool {
        if depth > MAX_DEPTH {
            self.too_deep.set(true);
            return false;
        }
        self.tick()
    }

    // ---- locals

    fn lookup(&self, name: &str) -> Option<&Option<String>> {
        self.locals.iter().rev().find_map(|frame| frame.get(name))
    }

    fn bind(&mut self, node: Node<'t>, cx: &Cx, kind: LocalKind, ty: Option<String>) {
        let name = self.text(node);
        self.facts.push(Fact::LocalBinding(Local {
            name: Name::new(name),
            site: span(node),
            enclosing: cx.scope.clone(),
            kind,
        }));
        if let Some(frame) = self.locals.last_mut() {
            frame.insert(name.to_string(), ty);
        }
    }

    // ---- items

    /// The children of a file, a conditional block or a linkage block, each with
    /// the comment before it.
    fn items(&mut self, parent: Node<'t>, cx: &Cx, depth: u16) {
        if !self.deeper(depth) {
            return;
        }
        let mut doc: Option<&'t str> = None;
        for child in through_errors(parent) {
            if child.kind() == "comment" {
                doc = Some(self.text(child));
                continue;
            }
            self.item(child, cx, doc, depth + 1);
            doc = None;
        }
    }

    fn item(&mut self, node: Node<'t>, cx: &Cx, doc: Option<&'t str>, depth: u16) {
        if !self.deeper(depth) {
            return;
        }
        match node.kind() {
            "preproc_include" => self.include(node, cx),
            "preproc_def" | "preproc_function_def" => self.macro_def(node, cx, doc),
            "preproc_if" | "preproc_ifdef" => self.conditional(node, cx, depth),
            "function_definition" => self.function(node, cx, doc, depth),
            "declaration" => self.declaration(node, cx, doc, depth),
            // A member written in a class body.
            "field_declaration" if self.cpp => self.declaration(node, cx, doc, depth),
            "type_definition" => self.type_definition(node, cx, doc, depth),
            "namespace_definition" => self.namespace(node, cx, doc, depth),
            "template_declaration" => self.template(node, cx, doc, depth),
            "using_declaration" => self.using(node, cx),
            "alias_declaration" => self.alias(node, cx, doc, depth),
            "class_specifier" | "struct_specifier" | "union_specifier" | "enum_specifier" => {
                self.type_specifier(node, None, cx, doc, depth)
            }
            "namespace_alias_definition" | "friend_declaration" | "static_assert_declaration" => {}
            "linkage_specification" => {
                if let Some(body) = node.child_by_field_name("body") {
                    if body.kind() == "declaration_list" {
                        self.items(body, cx, depth + 1);
                    } else {
                        self.item(body, cx, doc, depth + 1);
                    }
                }
            }
            "comment" | "preproc_call" | "preproc_arg" => {}
            _ => self.expr(node, cx, depth),
        }
    }

    /// A namespace block is a `Module` definition of its own, and what it holds is
    /// in its scope: a `using` there is for its names. A name written `a::b` is
    /// two blocks.
    fn namespace(&mut self, node: Node<'t>, cx: &Cx, doc: Option<&'t str>, depth: u16) {
        let Some(body) = node.child_by_field_name("body") else {
            return;
        };
        let written = node
            .child_by_field_name("name")
            .map_or("(anonymous)", |n| self.text(n));
        let segments: Vec<String> = written
            .split("::")
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .collect();
        let mut inner = cx.clone();
        let members = children(body);
        let name_node = node.child_by_field_name("name").unwrap_or(node);
        for segment in segments {
            let key = self.key_for(&inner, &[], &segment, Namespace::Type);
            self.facts.push(Fact::Definition(Def {
                local: key.clone(),
                kind: DefKind::Module,
                span: span(node),
                name_span: span(name_node),
                signature: Some(format!("namespace {segment}")),
                decorators: Vec::new(),
                visibility: Visibility::Public,
                body_hash: 0,
                text_hash: hash_outside(self.bytes, node, &members),
                doc_hash: Self::doc_hash(doc),
            }));
            inner.ns.push(segment);
            inner.scope = Scope::Def(key);
        }
        self.items(body, &inner, depth + 1);
    }

    /// A template is the definition it declares, once: its header is part of
    /// the signature.
    fn template(&mut self, node: Node<'t>, cx: &Cx, doc: Option<&'t str>, depth: u16) {
        let header = node.child_by_field_name("parameters").map(|p| {
            self.text(p)
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        });
        let mut inner = cx.clone();
        inner.template = header.map(|h| format!("template {h}"));
        for child in through_errors(node) {
            if matches!(child.kind(), "template_parameter_list" | "comment") {
                continue;
            }
            self.item(child, &inner, doc, depth + 1);
        }
    }

    /// `using a::f;` takes a name, `using namespace a;` takes a namespace's.
    fn using(&mut self, node: Node<'t>, cx: &Cx) {
        let text = self
            .text(node)
            .trim()
            .trim_start_matches("using")
            .trim()
            .trim_end_matches(';')
            .trim();
        let (directive, path) = match text.strip_prefix("namespace") {
            Some(rest) => (true, rest.trim()),
            None => (false, text),
        };
        let mut segments: Vec<&str> = path.split("::").filter(|p| !p.is_empty()).collect();
        let rooted = path.starts_with("::");
        let (module, names, star) = if directive {
            (segments.join("::"), Vec::new(), Star::Glob)
        } else {
            let Some(last) = segments.pop() else { return };
            (
                segments.join("::"),
                vec![(Name::new(last), None)],
                Star::None,
            )
        };
        self.facts.push(Fact::Import(Import {
            site: span(node),
            enclosing: cx.scope.clone(),
            module: ModulePath::new(
                if rooted {
                    format!("::{module}")
                } else {
                    module
                }
                .as_str(),
            ),
            relative: 0,
            kind: ImportKind::Static,
            names,
            star,
            default: None,
        }));
    }

    /// `using Name = Type;` is a typedef.
    fn alias(&mut self, node: Node<'t>, cx: &Cx, doc: Option<&'t str>, depth: u16) {
        let Some(name) = node.child_by_field_name("name") else {
            return;
        };
        let signature = self.signature(node, &[]);
        let key = self.define(
            node,
            name,
            &[],
            cx,
            DefKind::TypeAlias,
            Namespace::Type,
            Visibility::Public,
            signature,
            None,
            &[],
            doc,
        );
        if let Some(ty) = node.child_by_field_name("type") {
            let inner = Self::inside(cx, &key);
            self.type_refs(ty, &inner, depth + 1);
        }
    }

    /// A class or struct with members: the class is a definition, its bases are
    /// uses of it, its members are definitions inside it (an access specifier
    /// sets what follows).
    fn class(&mut self, ty: Node<'t>, cx: &Cx, doc: Option<&'t str>, depth: u16) {
        let Some(body) = ty.child_by_field_name("body") else {
            return;
        };
        let Some(name_node) = ty.child_by_field_name("name") else {
            return;
        };
        let written = self.text(name_node);
        let name = written
            .split('<')
            .next()
            .unwrap_or(written)
            .rsplit("::")
            .next()
            .unwrap_or(written)
            .trim()
            .to_string();
        let is_struct = ty.kind() == "struct_specifier";
        let members: Vec<Node<'t>> = children(body)
            .into_iter()
            .filter(|c| c.kind() != "comment")
            .collect();
        let mut signature = self.signature(ty, &[body]);
        if let Some(template) = &cx.template {
            signature = format!("{template} {signature}");
        }
        let key = self.key_for(cx, &[], &name, Namespace::Type);
        self.facts.push(Fact::Definition(Def {
            local: key.clone(),
            kind: if is_struct {
                DefKind::Struct
            } else {
                DefKind::Class
            },
            span: span(ty),
            name_span: span(name_node),
            signature: Some(signature),
            decorators: Vec::new(),
            visibility: Visibility::Public,
            body_hash: self.hash_tree(ty, &members),
            text_hash: hash_outside(self.bytes, ty, &members),
            doc_hash: Self::doc_hash(doc),
        }));
        let mut inner = Self::inside(cx, &key);
        inner.template = None;
        for child in children(ty) {
            if child.kind() == "base_class_clause" {
                self.type_refs(child, &inner, depth + 1);
            }
        }
        let mut member_cx = inner;
        member_cx.within.push(name);
        member_cx.access = Some(if is_struct {
            Visibility::Public
        } else {
            Visibility::Private
        });
        let mut doc: Option<&'t str> = None;
        for member in through_errors(body) {
            match member.kind() {
                "comment" => doc = Some(self.text(member)),
                "access_specifier" => {
                    member_cx.access = Some(match self.text(member).trim_end_matches(':') {
                        "public" => Visibility::Public,
                        "protected" => Visibility::Protected,
                        _ => Visibility::Private,
                    });
                }
                _ => {
                    self.item(member, &member_cx, doc, depth + 1);
                    doc = None;
                }
            }
        }
    }

    fn include(&mut self, node: Node<'t>, cx: &Cx) {
        let Some(path) = node.child_by_field_name("path") else {
            return;
        };
        let written = self.text(path).trim();
        let (module, relative) = match path.kind() {
            "string_literal" => (written.trim_matches('"'), 1),
            "system_lib_string" => (written.trim_start_matches('<').trim_end_matches('>'), 0),
            _ => return,
        };
        self.facts.push(Fact::Import(Import {
            site: span(node),
            enclosing: cx.scope.clone(),
            module: ModulePath::new(module),
            relative,
            kind: ImportKind::Require,
            names: Vec::new(),
            star: Star::Glob,
            default: None,
        }));
    }

    /// `#ifndef X` with `#define X` first and nothing after the block's end.
    fn is_guard(&self, node: Node<'t>) -> bool {
        node.kind() == "preproc_ifdef"
            && self.text(node).trim_start().starts_with("#ifndef")
            && node.child_by_field_name("alternative").is_none()
            && children(node)
                .into_iter()
                .find(|c| c.kind() == "preproc_def")
                .is_some_and(|d| {
                    d.child_by_field_name("name").map(|n| self.text(n))
                        == node.child_by_field_name("name").map(|n| self.text(n))
                })
    }

    fn conditional(&mut self, node: Node<'t>, cx: &Cx, depth: u16) {
        let mut cx_here = cx.clone();
        let mut branch = Some(node);
        let mut first = true;
        while let Some(current) = branch {
            let skip: Vec<usize> = ["name", "condition", "alternative"]
                .iter()
                .filter_map(|f| current.child_by_field_name(f))
                .map(|n| n.id())
                .collect();
            let text = match current.kind() {
                "preproc_else" => "#else".to_string(),
                "preproc_ifdef" | "preproc_elifdef" => {
                    let head = self
                        .text(current)
                        .split_whitespace()
                        .next()
                        .unwrap_or("#ifdef");
                    let name = current
                        .child_by_field_name("name")
                        .map_or("", |n| self.text(n));
                    format!("{head} {name}")
                }
                _ => {
                    let cond = current
                        .child_by_field_name("condition")
                        .map_or("", |c| self.text(c));
                    let head = if first { "#if" } else { "#elif" };
                    format!(
                        "{head} {}",
                        cond.split_whitespace().collect::<Vec<_>>().join(" ")
                    )
                }
            };
            let text = if first { text } else { format!("else {text}") };
            cx_here.cond = if self.is_guard(current) {
                cx.cond.clone()
            } else {
                Some(match &cx.cond {
                    Some(outer) => format!("{outer} && {text}"),
                    None => text,
                })
            };
            let mut doc = None;
            for child in through_errors(current) {
                if skip.contains(&child.id()) {
                    continue;
                }
                if child.kind() == "comment" {
                    doc = Some(self.text(child));
                    continue;
                }
                self.item(child, &cx_here, doc, depth + 1);
                doc = None;
            }
            branch = current.child_by_field_name("alternative");
            first = false;
        }
    }

    // ---- definitions

    fn key_for(&mut self, cx: &Cx, container: &[String], name: &str, ns: Namespace) -> LocalKey {
        let mut key = LocalKey::new(
            cx.ns
                .iter()
                .chain(&cx.within)
                .chain(container)
                .map(|c| Name::new(c.as_str()))
                .collect(),
            name,
            ns,
        );
        if let Some(cond) = &cx.cond {
            key = key.with_disambig(Disambig::Cfg(cond.clone()));
        }
        let seen = self.twins.entry(key.clone()).or_insert(0);
        let nth = *seen;
        *seen += 1;
        if nth == 0 {
            key
        } else {
            LocalKey {
                disambig: Some(Disambig::Overload(nth)),
                ..key
            }
        }
    }

    fn doc_hash(doc: Option<&'t str>) -> Option<u64> {
        let doc = doc?;
        let mut hasher = Hasher::new();
        for word in doc.split_whitespace() {
            hasher.token(word.trim_matches(|c| c == '/' || c == '*').as_bytes());
        }
        hasher.finish()
    }

    fn signature(&self, node: Node<'t>, skip: &[Node<'t>]) -> String {
        let mut tokens: Vec<&'t str> = Vec::new();
        let mut stack = vec![node];
        while let Some(n) = stack.pop() {
            if n.kind() == "comment" || skip.iter().any(|s| s.id() == n.id()) {
                continue;
            }
            if n.child_count() == 0 || matches!(n.kind(), "string_literal" | "char_literal") {
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

    fn hash_tree(&self, root: Node<'t>, members: &[Node<'t>]) -> u64 {
        let mut hash = Hasher::new();
        let mut stack = vec![(root, false)];
        while let Some((node, closing)) = stack.pop() {
            if closing {
                hash.bytes(&[2]);
                continue;
            }
            let held = node.id() != root.id() && members.iter().any(|m| m.id() == node.id());
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

    #[allow(clippy::too_many_arguments)]
    fn define(
        &mut self,
        node: Node<'t>,
        name_node: Node<'t>,
        container: &[String],
        cx: &Cx,
        kind: DefKind,
        ns: Namespace,
        visibility: Visibility,
        signature: String,
        body: Option<Node<'t>>,
        members: &[Node<'t>],
        doc: Option<&'t str>,
    ) -> LocalKey {
        let name = self.text(name_node);
        let key = self.key_for(cx, container, name, ns);
        self.facts.push(Fact::Definition(Def {
            local: key.clone(),
            kind,
            span: span(node),
            name_span: span(name_node),
            signature: Some(signature),
            decorators: Vec::new(),
            visibility,
            body_hash: body.map_or(0, |b| self.hash_tree(b, members)),
            text_hash: hash_outside(self.bytes, node, members),
            doc_hash: Self::doc_hash(doc),
        }));
        key
    }

    fn inside(cx: &Cx, key: &LocalKey) -> Cx {
        Cx {
            scope: Scope::Def(key.clone()),
            ..cx.clone()
        }
    }

    fn storage(&self, node: Node<'t>) -> (bool, bool) {
        let mut is_static = false;
        let mut is_extern = false;
        for child in children(node) {
            if child.kind() == "storage_class_specifier" {
                match self.text(child) {
                    "static" => is_static = true,
                    "extern" => is_extern = true,
                    _ => {}
                }
            }
        }
        (is_static, is_extern)
    }

    fn function(&mut self, node: Node<'t>, cx: &Cx, doc: Option<&'t str>, depth: u16) {
        let Some(declarator) = node
            .child_by_field_name("declarator")
            .and_then(|d| declared(d, self.bytes, &self.too_deep))
        else {
            return;
        };
        let body = node.child_by_field_name("body");
        let (is_static, _) = self.storage(node);
        let mut signature = self.signature(node, &body.into_iter().collect::<Vec<_>>());
        if let Some(template) = &cx.template {
            signature = format!("{template} {signature}");
        }
        let name = self.text(declarator.name).to_string();
        let kind = self.member_kind(cx, &declarator);
        let visibility = self.visibility_of(cx, is_static);
        let key = self.define(
            node,
            declarator.name,
            &declarator.scope,
            cx,
            kind,
            Namespace::Value,
            visibility,
            signature,
            body,
            &[],
            doc,
        );
        if name == "main" && cx.cond.is_none() {
            self.facts.push(Fact::EntryPoint(EntryPoint {
                site: span(node),
                kind: EntryKind::Main,
                target: Scope::Def(key.clone()),
                framework: None,
            }));
        }
        let mut inner = Self::inside(cx, &key);
        inner.template = None;
        if !declarator.scope.is_empty() {
            inner.within.extend(declarator.scope.iter().cloned());
        }
        if let Some(ty) = node.child_by_field_name("type") {
            self.type_refs(ty, &inner, depth + 1);
        }
        self.locals.push(HashMap::new());
        if let Some(parameters) = declarator.function {
            self.parameters(parameters, &inner, depth + 1);
        }
        for child in children(node) {
            if child.kind() == "field_initializer_list" {
                self.expr(child, &inner, depth + 1);
            }
        }
        if let Some(body) = body {
            self.block(body, &inner, depth + 1);
        }
        self.locals.pop();
    }

    /// What a function declarator declares: a constructor when it names the class
    /// it belongs to, a method in a class or written `A::f`, else a function.
    fn member_kind(&self, cx: &Cx, d: &Declared<'t>) -> DefKind {
        let owner = d
            .scope
            .last()
            .or_else(|| cx.within.last())
            .map(String::as_str);
        match owner {
            Some(class) if self.cpp => {
                let base = class.split('<').next().unwrap_or(class);
                if self.text(d.name) == base {
                    DefKind::Constructor
                } else {
                    DefKind::Method
                }
            }
            _ => DefKind::Function,
        }
    }

    /// A class member has the access its specifier gave; a `static` or an
    /// unnamed-namespace name outside a class stays in its file.
    fn visibility_of(&self, cx: &Cx, is_static: bool) -> Visibility {
        if !cx.within.is_empty() {
            return cx.access.unwrap_or(Visibility::Public);
        }
        if is_static || cx.ns.iter().any(|n| n == "(anonymous)") {
            Visibility::Private
        } else {
            Visibility::Public
        }
    }

    fn parameters(&mut self, parameters: Node<'t>, cx: &Cx, depth: u16) {
        for parameter in children(parameters) {
            if parameter.kind() != "parameter_declaration" {
                continue;
            }
            let ty = parameter.child_by_field_name("type");
            if let Some(ty) = ty {
                self.type_refs(ty, cx, depth);
            }
            let named = ty.and_then(|t| self.type_name(t));
            if let Some(d) = parameter
                .child_by_field_name("declarator")
                .and_then(|d| declared(d, self.bytes, &self.too_deep))
            {
                let ty = if d.pointer { None } else { named };
                self.bind(d.name, cx, LocalKind::Parameter, ty);
            }
        }
    }

    /// What a name written with a scope is reached through: the namespace or class
    /// it names; a bare name is reached through nothing.
    fn scoped(scope: &[Name]) -> Receiver {
        if scope.is_empty() {
            return Receiver::None;
        }
        let joined: Vec<&str> = scope.iter().map(Name::as_str).collect();
        Receiver::Module(ModulePath::new(joined.join("::").as_str()))
    }

    /// A type written with its scope: `a::B<int>` is `B` in `[a]`.
    fn type_path(&self, ty: Node<'t>) -> Option<(Vec<Name>, String)> {
        let text = match ty.kind() {
            "type_identifier"
            | "qualified_identifier"
            | "template_type"
            | "namespace_identifier" => self.text(ty),
            _ => return None,
        };
        let bare = text.split('<').next().unwrap_or(text);
        let mut parts: Vec<&str> = bare.split("::").collect();
        let name = parts.pop()?.trim().to_string();
        if name.is_empty() {
            return None;
        }
        Some((
            parts.into_iter().map(|p| Name::new(p.trim())).collect(),
            name,
        ))
    }

    /// The tag or typedef a type specifier names; none for a builtin.
    fn type_name(&self, ty: Node<'t>) -> Option<String> {
        match ty.kind() {
            "type_identifier" => Some(self.text(ty).to_string()),
            "qualified_identifier" | "template_type" => self.type_path(ty).map(|(scope, name)| {
                scope
                    .iter()
                    .map(Name::as_str)
                    .chain([name.as_str()])
                    .collect::<Vec<_>>()
                    .join("::")
            }),
            "struct_specifier" | "union_specifier" | "enum_specifier" => ty
                .child_by_field_name("name")
                .map(|n| self.text(n).to_string()),
            _ => None,
        }
    }

    fn declaration(&mut self, node: Node<'t>, cx: &Cx, doc: Option<&'t str>, depth: u16) {
        if !self.locals.is_empty() {
            self.local_declaration(node, cx, depth);
            return;
        }
        let (is_static, is_extern) = self.storage(node);
        let ty = node.child_by_field_name("type");
        if let Some(ty) = ty {
            self.type_specifier(ty, None, cx, doc, depth);
        }
        let mut cursor = node.walk();
        let declarators: Vec<Node<'t>> = node
            .children_by_field_name("declarator", &mut cursor)
            .collect();
        let declarators_len = declarators.len();
        for declarator in declarators {
            let Some(d) = declared(declarator, self.bytes, &self.too_deep) else {
                continue;
            };
            let visibility = self.visibility_of(cx, is_static);
            let spelled = {
                let mut parts: Vec<&str> = Vec::new();
                for child in children(node) {
                    if matches!(child.kind(), "storage_class_specifier" | "type_qualifier")
                        || Some(child.id()) == ty.map(|t| t.id())
                    {
                        parts.push(self.text(child));
                    }
                }
                let decl_text = match declarator.kind() {
                    "init_declarator" => declarator
                        .child_by_field_name("declarator")
                        .map_or("", |c| self.text(c)),
                    _ => self.text(declarator),
                };
                parts.push(decl_text);
                parts.join(" ")
            };
            let mut spelled = spelled.split_whitespace().collect::<Vec<_>>().join(" ");
            if let Some(template) = &cx.template {
                spelled = format!("{template} {spelled}");
            }
            let value = (declarator.kind() == "init_declarator")
                .then(|| declarator.child_by_field_name("value"))
                .flatten();
            if d.function.is_some() && !d.pointer {
                // The whole declaration as written: `virtual`, `= 0` and the like
                // are part of what a prototype states.
                let spelled = if declarators_len == 1 {
                    let written = self.signature(node, &[]);
                    match &cx.template {
                        Some(template) => format!("{template} {written}"),
                        None => written,
                    }
                } else {
                    spelled
                };
                let name = self.text(d.name).to_string();
                let mut local_cx = cx.clone();
                let qualified = |scope: &[String]| {
                    if scope.is_empty() {
                        name.clone()
                    } else {
                        format!("{}::{name}", scope.join("::"))
                    }
                };
                let full: Vec<String> = cx.ns.iter().chain(&cx.within).cloned().collect();
                if (self.defined_here.contains(&qualified(&cx.within))
                    || self.defined_here.contains(&qualified(&full)))
                    && cx.cond.is_none()
                {
                    local_cx.cond = Some("declaration".to_string());
                }
                let kind = self.member_kind(cx, &d);
                let key = self.define(
                    node,
                    d.name,
                    &d.scope,
                    &local_cx,
                    kind,
                    Namespace::Value,
                    visibility,
                    spelled,
                    None,
                    &[],
                    doc,
                );
                let inner = Self::inside(cx, &key);
                if let Some(ty) = ty {
                    self.type_refs(ty, &inner, depth + 1);
                }
                if let Some(parameters) = d.function {
                    self.locals.push(HashMap::new());
                    self.parameters(parameters, &inner, depth + 1);
                    self.locals.pop();
                }
                continue;
            }
            let variable = if cx.within.is_empty() {
                DefKind::Variable
            } else {
                DefKind::Field
            };
            let key = self.define(
                declarator,
                d.name,
                &d.scope,
                cx,
                variable,
                Namespace::Value,
                visibility,
                spelled,
                if is_extern { None } else { value },
                &[],
                doc,
            );
            let inner = Self::inside(cx, &key);
            if let Some(ty) = ty {
                self.type_refs(ty, &inner, depth + 1);
            }
            if let Some(value) = value {
                self.locals.push(HashMap::new());
                self.expr(value, &inner, depth + 1);
                self.locals.pop();
            }
        }
    }

    fn local_declaration(&mut self, node: Node<'t>, cx: &Cx, depth: u16) {
        let ty = node.child_by_field_name("type");
        if let Some(ty) = ty {
            self.type_refs(ty, cx, depth);
        }
        let named = ty.and_then(|t| self.type_name(t));
        let mut cursor = node.walk();
        let declarators: Vec<Node<'t>> = node
            .children_by_field_name("declarator", &mut cursor)
            .collect();
        for declarator in declarators {
            if declarator.kind() == "init_declarator" {
                if let Some(value) = declarator.child_by_field_name("value") {
                    self.expr(value, cx, depth + 1);
                }
            }
            if let Some(d) = declared(declarator, self.bytes, &self.too_deep) {
                let local = if d.function.is_some() && !d.pointer {
                    None
                } else if d.pointer {
                    None
                } else {
                    named.clone()
                };
                // A class-typed object declared by value is built there.
                let by_value = matches!(declarator.kind(), "identifier")
                    || (declarator.kind() == "init_declarator"
                        && declarator
                            .child_by_field_name("declarator")
                            .is_some_and(|i| i.kind() == "identifier"));
                if self.cpp && by_value && local.is_some() {
                    if let Some((scope, name)) = ty.and_then(|t| self.type_path(t)) {
                        let receiver = Self::scoped(&scope);
                        self.emit(d.name, cx, receiver, UseKind::Instantiate, &name, scope);
                    }
                }
                self.bind(d.name, cx, LocalKind::Variable, local);
            }
        }
    }

    fn type_definition(&mut self, node: Node<'t>, cx: &Cx, doc: Option<&'t str>, depth: u16) {
        let ty = node.child_by_field_name("type");
        let mut cursor = node.walk();
        let declarators: Vec<Node<'t>> = node
            .children_by_field_name("declarator", &mut cursor)
            .collect();
        let first_name = declarators
            .first()
            .and_then(|d| declared(*d, self.bytes, &self.too_deep))
            .map(|d| self.text(d.name).to_string());
        if let Some(ty) = ty {
            self.type_specifier(ty, first_name.as_deref(), cx, doc, depth);
        }
        for declarator in declarators {
            let Some(d) = declared(declarator, self.bytes, &self.too_deep) else {
                continue;
            };
            let name = self.text(d.name).to_string();
            let anonymous_body = ty.is_some_and(|t| {
                matches!(
                    t.kind(),
                    "struct_specifier" | "union_specifier" | "enum_specifier"
                ) && t.child_by_field_name("name").is_none()
                    && t.child_by_field_name("body").is_some()
            });
            if anonymous_body && first_name.as_deref() == Some(name.as_str()) {
                continue;
            }
            let skip: Vec<Node<'t>> = ty
                .and_then(|t| t.child_by_field_name("body"))
                .into_iter()
                .collect();
            let signature = self.signature(node, &skip);
            let key = self.define(
                node,
                d.name,
                &[],
                cx,
                DefKind::TypeAlias,
                Namespace::Type,
                Visibility::Public,
                signature,
                None,
                &[],
                doc,
            );
            if let Some(ty) = ty {
                let inner = Self::inside(cx, &key);
                self.type_refs(ty, &inner, depth + 1);
            }
        }
    }

    /// A struct, union or enum written with its members defines its tag; a
    /// typedef of an unnamed one defines the typedef's name.
    fn type_specifier(
        &mut self,
        ty: Node<'t>,
        typedef_name: Option<&str>,
        cx: &Cx,
        doc: Option<&'t str>,
        depth: u16,
    ) {
        if !matches!(
            ty.kind(),
            "struct_specifier" | "union_specifier" | "enum_specifier" | "class_specifier"
        ) {
            return;
        }
        if self.cpp && matches!(ty.kind(), "struct_specifier" | "class_specifier") {
            self.class(ty, cx, doc, depth);
            return;
        }
        let Some(body) = ty.child_by_field_name("body") else {
            return;
        };
        let tag = ty.child_by_field_name("name");
        let name_node = match tag {
            Some(tag) => tag,
            None => {
                // Named by the typedef: the declarator's node is not at hand, so the
                // specifier itself stands for it.
                if typedef_name.is_none() {
                    // An unnamed enum's constants are global names all the same.
                    if ty.kind() == "enum_specifier" {
                        self.enumerators(body, "", false, cx, depth);
                    }
                    return;
                }
                ty
            }
        };
        let name = tag.map_or_else(
            || typedef_name.unwrap_or("").to_string(),
            |t| self.text(t).to_string(),
        );
        let is_enum = ty.kind() == "enum_specifier";
        let members: Vec<Node<'t>> = children(body)
            .into_iter()
            .filter(|c| c.kind() != "comment")
            .collect();
        let signature = self.signature(ty, &[body]);
        let key = {
            let key = self.key_for(cx, &[], &name, Namespace::Type);
            self.facts.push(Fact::Definition(Def {
                local: key.clone(),
                kind: if is_enum {
                    DefKind::Enum
                } else {
                    DefKind::Struct
                },
                span: span(ty),
                name_span: span(name_node),
                signature: Some(signature),
                decorators: Vec::new(),
                visibility: Visibility::Public,
                body_hash: self.hash_tree(ty, &members),
                text_hash: hash_outside(self.bytes, ty, &members),
                doc_hash: Self::doc_hash(doc),
            }));
            key
        };
        let inner = Self::inside(cx, &key);
        if is_enum {
            self.enumerators(
                body,
                &name,
                tag.is_some() || typedef_name.is_some(),
                cx,
                depth,
            );
        } else {
            self.fields(body, &[name], &inner, cx, depth);
        }
    }

    fn enumerators(&mut self, body: Node<'t>, enum_name: &str, named: bool, cx: &Cx, depth: u16) {
        let container: Vec<String> = if named {
            vec![enum_name.to_string()]
        } else {
            Vec::new()
        };
        for member in children(body) {
            match member.kind() {
                "enumerator" => {
                    let Some(name_node) = member.child_by_field_name("name") else {
                        continue;
                    };
                    let value = member.child_by_field_name("value");
                    let signature = self.signature(member, &[]);
                    let key = self.define(
                        member,
                        name_node,
                        &container,
                        cx,
                        DefKind::EnumMember,
                        Namespace::Value,
                        Visibility::Public,
                        signature,
                        value,
                        &[],
                        None,
                    );
                    if let Some(value) = value {
                        let inner = Self::inside(cx, &key);
                        self.locals.push(HashMap::new());
                        self.expr(value, &inner, depth + 1);
                        self.locals.pop();
                    }
                }
                "preproc_if" | "preproc_ifdef" => {
                    self.enumerators(member, enum_name, named, cx, depth)
                }
                _ => {}
            }
        }
    }

    fn fields(&mut self, body: Node<'t>, container: &[String], owner: &Cx, cx: &Cx, depth: u16) {
        for member in children(body) {
            match member.kind() {
                "field_declaration" => {
                    let ty = member.child_by_field_name("type");
                    if let Some(ty) = ty {
                        self.type_specifier(ty, None, cx, None, depth);
                        self.type_refs(ty, owner, depth + 1);
                        let anonymous = matches!(ty.kind(), "struct_specifier" | "union_specifier")
                            && ty.child_by_field_name("name").is_none();
                        if let (true, Some(inner)) = (anonymous, ty.child_by_field_name("body")) {
                            if member.child_by_field_name("declarator").is_none() {
                                self.fields(inner, container, owner, cx, depth + 1);
                                continue;
                            }
                        }
                    }
                    let mut cursor = member.walk();
                    let declarators: Vec<Node<'t>> = member
                        .children_by_field_name("declarator", &mut cursor)
                        .collect();
                    for declarator in declarators {
                        let Some(d) = declared(declarator, self.bytes, &self.too_deep) else {
                            continue;
                        };
                        let signature = {
                            let mut parts: Vec<&str> = Vec::new();
                            for child in children(member) {
                                if Some(child.id()) == ty.map(|t| t.id())
                                    || child.kind() == "type_qualifier"
                                {
                                    parts.push(self.text(child));
                                }
                            }
                            parts.push(self.text(declarator));
                            parts
                                .join(" ")
                                .split_whitespace()
                                .collect::<Vec<_>>()
                                .join(" ")
                        };
                        let key = self.define(
                            declarator,
                            d.name,
                            container,
                            cx,
                            DefKind::Field,
                            Namespace::Value,
                            Visibility::Public,
                            signature,
                            None,
                            &[],
                            None,
                        );
                        let _ = key;
                    }
                }
                "preproc_if" | "preproc_ifdef" => {
                    self.fields(member, container, owner, cx, depth + 1)
                }
                _ => {}
            }
        }
    }

    // ---- macros

    fn macro_def(&mut self, node: Node<'t>, cx: &Cx, doc: Option<&'t str>) {
        let Some(name_node) = node.child_by_field_name("name") else {
            return;
        };
        let value = node.child_by_field_name("value");
        let params: Vec<String> = node
            .child_by_field_name("parameters")
            .map(|p| {
                children(p)
                    .into_iter()
                    .map(|i| self.text(i).to_string())
                    .collect()
            })
            .unwrap_or_else(Vec::new);
        let text = self
            .text(node)
            .replace("\\\n", " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let words: Vec<&str> = value
            .map(|v| self.text(v).split_whitespace().collect())
            .unwrap_or_else(Vec::new);
        let body_hash = hash_tokens_of(&words);
        let name = self.text(name_node);
        let key = self.key_for(cx, &[], name, Namespace::Value);
        self.facts.push(Fact::Definition(Def {
            local: key.clone(),
            kind: DefKind::Macro,
            span: span(node),
            name_span: span(name_node),
            signature: Some(text),
            decorators: Vec::new(),
            visibility: Visibility::Public,
            body_hash,
            text_hash: hash_text(self.text(node).as_bytes()),
            doc_hash: Self::doc_hash(doc),
        }));
        if let Some(value) = value {
            self.macro_refs(value, &params, &Self::inside(cx, &key));
        }
    }

    /// The names in a replacement text: `f(` is a call, another name a read,
    /// unless a parameter or joined by `##`.
    fn macro_refs(&mut self, value: Node<'t>, params: &[String], cx: &Cx) {
        let text = self.text(value);
        let bytes = text.as_bytes();
        let base = value.start_byte() as u32;
        let base_line = value.start_position().row as u32 + 1;
        let mut at = 0;
        while at < bytes.len() {
            let c = bytes[at];
            if c == b'"' || c == b'\'' {
                at += 1;
                while at < bytes.len() && bytes[at] != c {
                    if bytes[at] == b'\\' {
                        at += 1;
                    }
                    at += 1;
                }
                at += 1;
                continue;
            }
            if !(c.is_ascii_alphabetic() || c == b'_') {
                at += 1;
                continue;
            }
            let start = at;
            while at < bytes.len() && (bytes[at].is_ascii_alphanumeric() || bytes[at] == b'_') {
                at += 1;
            }
            let word = &text[start..at];
            let before = text[..start].trim_end();
            let after = text[at..].trim_start();
            if KEYWORDS.contains(&word)
                || params.iter().any(|p| p == word)
                || before.ends_with('#')
                || after.starts_with("##")
                || (start > 0 && bytes[start - 1].is_ascii_digit())
            {
                continue;
            }
            let line = base_line + text[..start].matches('\n').count() as u32;
            let site = Span {
                bytes: base + start as u32..base + at as u32,
                lines: (line, line),
            };
            self.facts.push(Fact::Reference(Ref {
                site,
                enclosing: cx.scope.clone(),
                receiver: Receiver::None,
                use_kind: if after.starts_with('(') {
                    UseKind::Call
                } else {
                    UseKind::Read
                },
                name: Name::new(word),
                qualifier: Vec::new(),
            }));
        }
    }

    // ---- bodies

    fn block(&mut self, body: Node<'t>, cx: &Cx, depth: u16) {
        if !self.deeper(depth) {
            return;
        }
        self.locals.push(HashMap::new());
        for child in children(body) {
            self.expr(child, cx, depth + 1);
        }
        self.locals.pop();
    }

    fn expr(&mut self, node: Node<'t>, cx: &Cx, depth: u16) {
        if !self.deeper(depth) {
            return;
        }
        match node.kind() {
            "call_expression" => self.call(node, cx, depth),
            "field_expression" => self.access(node, cx, UseKind::Read, depth),
            "assignment_expression" => {
                if let Some(left) = node.child_by_field_name("left") {
                    self.target(left, cx, depth + 1);
                }
                if let Some(right) = node.child_by_field_name("right") {
                    self.expr(right, cx, depth + 1);
                }
            }
            "update_expression" => {
                if let Some(argument) = node.child_by_field_name("argument") {
                    self.target(argument, cx, depth + 1);
                }
            }
            "identifier" => {
                let name = self.text(node);
                if self.lookup(name).is_none() && !KEYWORDS.contains(&name) {
                    self.emit(node, cx, Receiver::None, UseKind::Read, name, Vec::new());
                }
            }
            "declaration" => self.declaration(node, cx, None, depth),
            "compound_statement" => self.block(node, cx, depth),
            "for_statement" => {
                self.locals.push(HashMap::new());
                for child in children(node) {
                    self.expr(child, cx, depth + 1);
                }
                self.locals.pop();
            }
            "cast_expression" => {
                if let Some(ty) = node.child_by_field_name("type") {
                    self.type_refs(ty, cx, depth + 1);
                }
                if let Some(value) = node.child_by_field_name("value") {
                    self.expr(value, cx, depth + 1);
                }
            }
            "sizeof_expression" | "compound_literal_expression" => {
                if let Some(ty) = node.child_by_field_name("type") {
                    self.type_refs(ty, cx, depth + 1);
                }
                if let Some(value) = node.child_by_field_name("value") {
                    self.expr(value, cx, depth + 1);
                }
            }
            "type_descriptor" => self.type_refs(node, cx, depth + 1),
            "new_expression" if self.cpp => {
                if let Some(ty) = node.child_by_field_name("type") {
                    if let Some((scope, name)) = self.type_path(ty) {
                        let receiver = Self::scoped(&scope);
                        self.emit(ty, cx, receiver, UseKind::Instantiate, &name, scope);
                    }
                }
                for field in ["arguments", "placement"] {
                    if let Some(arguments) = node.child_by_field_name(field) {
                        self.expr(arguments, cx, depth + 1);
                    }
                }
            }
            "qualified_identifier" if self.cpp => {
                let text = self.text(node);
                let bare = text.split('<').next().unwrap_or(text);
                let mut parts: Vec<&str> = bare.split("::").collect();
                let name = parts.pop().unwrap_or("").to_string();
                let scope: Vec<Name> = parts.into_iter().map(Name::new).collect();
                let receiver = Self::scoped(&scope);
                self.emit(node, cx, receiver, UseKind::Read, &name, scope);
            }
            "this" | "lambda_capture_specifier" | "namespace_identifier" | "raw_string_literal" => {
            }
            "field_designator"
            | "statement_identifier"
            | "comment"
            | "string_literal"
            | "char_literal"
            | "number_literal"
            | "system_lib_string"
            | "preproc_arg"
            | "preproc_include"
            | "preproc_def"
            | "preproc_function_def" => {}
            "function_definition" | "type_definition" | "preproc_if" | "preproc_ifdef" => {
                self.item(node, cx, None, depth)
            }
            _ => {
                for child in children(node) {
                    self.expr(child, cx, depth + 1);
                }
            }
        }
    }

    /// A name written where a type is expected.
    fn type_refs(&mut self, node: Node<'t>, cx: &Cx, depth: u16) {
        if !self.deeper(depth) {
            return;
        }
        match node.kind() {
            "type_identifier" => {
                let name = self.text(node);
                self.emit(node, cx, Receiver::None, UseKind::TypeUse, name, Vec::new());
            }
            "struct_specifier" | "union_specifier" | "enum_specifier" => {
                if node.child_by_field_name("body").is_none() {
                    if let Some(tag) = node.child_by_field_name("name") {
                        let name = self.text(tag);
                        self.emit(tag, cx, Receiver::None, UseKind::TypeUse, name, Vec::new());
                    }
                }
            }
            "qualified_identifier" | "template_type" if self.cpp => {
                if let Some((scope, name)) = self.type_path(node) {
                    let receiver = Self::scoped(&scope);
                    self.emit(node, cx, receiver, UseKind::TypeUse, &name, scope);
                }
                if node.kind() == "template_type" {
                    if let Some(arguments) = node.child_by_field_name("arguments") {
                        for child in children(arguments) {
                            self.type_refs(child, cx, depth + 1);
                        }
                    }
                }
            }
            "class_specifier" if self.cpp => {
                if node.child_by_field_name("body").is_none() {
                    if let Some(tag) = node.child_by_field_name("name") {
                        if let Some((scope, name)) = self.type_path(tag) {
                            let receiver = Self::scoped(&scope);
                            self.emit(tag, cx, receiver, UseKind::TypeUse, &name, scope);
                        }
                    }
                }
            }
            "primitive_type"
            | "sized_type_specifier"
            | "identifier"
            | "namespace_identifier"
            | "placeholder_type_specifier" => {}
            _ => {
                for child in children(node) {
                    self.type_refs(child, cx, depth + 1);
                }
            }
        }
    }

    fn call(&mut self, node: Node<'t>, cx: &Cx, depth: u16) {
        if let Some(function) = node.child_by_field_name("function") {
            match function.kind() {
                "identifier" => {
                    let name = self.text(function);
                    let receiver = if self.lookup(name).is_some() {
                        Receiver::Unknown
                    } else {
                        Receiver::None
                    };
                    self.emit(function, cx, receiver, UseKind::Call, name, Vec::new());
                }
                "field_expression" => self.access(function, cx, UseKind::Call, depth),
                "template_function" if self.cpp => {
                    let name = function
                        .child_by_field_name("name")
                        .map_or("", |n| self.text(n));
                    self.emit(
                        function,
                        cx,
                        Receiver::None,
                        UseKind::Call,
                        name,
                        Vec::new(),
                    );
                }
                "qualified_identifier" if self.cpp => {
                    let text = self.text(function);
                    let bare = text.split('<').next().unwrap_or(text);
                    let mut parts: Vec<&str> = bare.split("::").collect();
                    let name = parts.pop().unwrap_or("").to_string();
                    let scope: Vec<Name> = parts.into_iter().map(Name::new).collect();
                    let receiver = Self::scoped(&scope);
                    self.emit(function, cx, receiver, UseKind::Call, &name, scope);
                }
                _ => {
                    let inner = Self::innermost(function);
                    let name = self.text(inner).to_string();
                    self.emit(
                        inner,
                        cx,
                        Receiver::Unknown,
                        UseKind::Call,
                        &name,
                        Vec::new(),
                    );
                    self.expr(function, cx, depth + 1);
                }
            }
        }
        if let Some(arguments) = node.child_by_field_name("arguments") {
            for child in children(arguments) {
                self.expr(child, cx, depth + 1);
            }
        }
    }

    /// The expression a `(*f)` or `a[i]` wraps.
    fn innermost(node: Node<'t>) -> Node<'t> {
        let mut node = node;
        while matches!(
            node.kind(),
            "parenthesized_expression" | "pointer_expression" | "subscript_expression"
        ) {
            let next = node
                .child_by_field_name("argument")
                .or_else(|| node.named_child(0));
            match next {
                Some(n) => node = n,
                None => break,
            }
        }
        node
    }

    /// `a->f`, `a.f`: a read, a write or a call of the member `f`.
    fn access(&mut self, node: Node<'t>, cx: &Cx, use_kind: UseKind, depth: u16) {
        let (Some(argument), Some(field)) = (
            node.child_by_field_name("argument"),
            node.child_by_field_name("field"),
        ) else {
            return;
        };
        let (receiver, qualifier) = self.head(argument, cx);
        let name = self.text(field);
        self.emit(field, cx, receiver, use_kind, name, qualifier);
        self.head_exprs(argument, cx, depth + 1);
    }

    /// The parts of a chain's head that hold references of their own: a call's
    /// arguments, an index.
    fn head_exprs(&mut self, node: Node<'t>, cx: &Cx, depth: u16) {
        match node.kind() {
            "identifier" => {
                let name = self.text(node);
                if self.lookup(name).is_none() && !KEYWORDS.contains(&name) {
                    self.emit(node, cx, Receiver::None, UseKind::Read, name, Vec::new());
                }
            }
            "field_expression" => self.access(node, cx, UseKind::Read, depth + 1),
            "parenthesized_expression" | "pointer_expression" => {
                if let Some(inner) = node.named_child(0) {
                    self.head_exprs(inner, cx, depth + 1);
                }
            }
            "subscript_expression" => {
                if let Some(argument) = node.child_by_field_name("argument") {
                    self.head_exprs(argument, cx, depth + 1);
                }
                if let Some(index) = node.child_by_field_name("index") {
                    self.expr(index, cx, depth + 1);
                }
            }
            _ => self.expr(node, cx, depth + 1),
        }
    }

    /// What the written chain before a member stands for.
    fn head(&self, node: Node<'t>, cx: &Cx) -> (Receiver, Vec<Name>) {
        match node.kind() {
            "identifier" => {
                let name = self.text(node);
                match self.lookup(name) {
                    Some(Some(ty)) => (Receiver::Typed(TypeName::new(ty.as_str())), Vec::new()),
                    Some(None) => (Receiver::Unknown, Vec::new()),
                    None => (Receiver::None, vec![Name::new(name)]),
                }
            }
            "field_expression" => {
                let (Some(argument), Some(field)) = (
                    node.child_by_field_name("argument"),
                    node.child_by_field_name("field"),
                ) else {
                    return (Receiver::Unknown, Vec::new());
                };
                let (receiver, mut qualifier) = self.head(argument, cx);
                if matches!(receiver, Receiver::Unknown) {
                    return (Receiver::Unknown, Vec::new());
                }
                qualifier.push(Name::new(self.text(field)));
                (receiver, qualifier)
            }
            "this" => (Receiver::SelfType, Vec::new()),
            "parenthesized_expression" | "pointer_expression" | "subscript_expression" => node
                .child_by_field_name("argument")
                .or_else(|| node.named_child(0))
                .map_or((Receiver::Unknown, Vec::new()), |inner| {
                    self.head(inner, cx)
                }),
            "cast_expression" => {
                let named = node
                    .child_by_field_name("type")
                    .and_then(|t| t.child_by_field_name("type"))
                    .and_then(|t| self.type_name(t));
                match named {
                    Some(ty) => (Receiver::Typed(TypeName::new(ty.as_str())), Vec::new()),
                    None => (Receiver::Unknown, Vec::new()),
                }
            }
            "call_expression" => match node.child_by_field_name("function") {
                Some(f) if f.kind() == "identifier" => (
                    Receiver::CallResult(vec![Name::new(self.text(f))]),
                    Vec::new(),
                ),
                _ => (Receiver::Unknown, Vec::new()),
            },
            _ => (Receiver::Unknown, Vec::new()),
        }
    }

    /// The left side of an assignment: a member or a global is written.
    fn target(&mut self, node: Node<'t>, cx: &Cx, depth: u16) {
        match node.kind() {
            "field_expression" => self.access(node, cx, UseKind::Write, depth),
            "identifier" => {
                let name = self.text(node);
                if self.lookup(name).is_none() && !KEYWORDS.contains(&name) {
                    self.emit(node, cx, Receiver::None, UseKind::Write, name, Vec::new());
                }
            }
            "parenthesized_expression" | "pointer_expression" | "subscript_expression" => {
                let inner = Self::innermost(node);
                if inner.id() != node.id()
                    && matches!(inner.kind(), "identifier" | "field_expression")
                {
                    self.target(inner, cx, depth + 1);
                }
                for child in children(node) {
                    if child.id() != inner.id() {
                        self.expr(child, cx, depth + 1);
                    }
                }
            }
            _ => self.expr(node, cx, depth + 1),
        }
    }
}

fn hash_tokens_of(words: &[&str]) -> u64 {
    let mut hasher = Hasher::new();
    for word in words {
        hasher.token(word.as_bytes());
    }
    hasher.finish().unwrap_or(0)
}
