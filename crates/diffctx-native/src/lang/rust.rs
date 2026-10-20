//! Rust, read off the syntax tree (#484, #506, #548). A module is a file or
//! a `mod` block inside one, named by its path from the crate root. A name
//! reaches a definition through the scope Rust gives it — a local of the
//! function around it, an item of that function or of its module, a `use`,
//! a glob — and a path through the modules it names (`crate::`, `super::`,
//! `self::`, a workspace crate). A method is reached through a value whose
//! type the syntax states (`self`, `self.field`, a typed parameter or
//! `let`, `T::new()`, what a called function declares it returns, an alias,
//! a unit struct); through any other value it is a candidate from a file
//! that names its type or trait. A same-named item of another module,
//! a method of another type, a local, a macro and a `mod` declaration are
//! never callers, and an `impl X` block is part of `X`, never a use of it.

use std::cell::{OnceCell, RefCell};
use std::ops::Range;
use std::path::Path;
use std::rc::Rc;

use rustc_hash::{FxHashMap, FxHashSet};
use tree_sitter::Node;

use super::{At, DefId, LanguageFacts, Relation, RemovedDef, Table, UseSite};
use crate::bindings::{self, Evidence, Finding, absorb};
use crate::impact::base_symbol;
use crate::source::Source;

pub fn is_rust(path: &str) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("rs"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Fn,
    Type,
    Trait,
    Value,
    Macro,
    Mod,
}

/// A type as a declaration states it: `a::T`, `&mut T` and `Rc<T>` name
/// `T`; `dyn Tr` and `impl Tr` name a trait any implementer stands behind.
/// A value a call returns has the type its function declares.
#[derive(Clone, Debug)]
struct Ty {
    path: Vec<String>,
    dynamic: bool,
    /// `path` names the function whose return type this is.
    returns: Option<Returns>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Returns {
    Value,
    /// `f()?`: what the `Result` or `Option` holds.
    Unwrapped,
}

impl Ty {
    fn named(path: Vec<String>) -> Self {
        Self {
            path,
            dynamic: false,
            returns: None,
        }
    }
}

#[derive(Debug)]
struct Item {
    name: String,
    kind: Kind,
    module: usize,
    /// The `impl` or `trait` block a member belongs to.
    block: Option<usize>,
    /// A local item: the function whose body declares it.
    scope: Option<Range<usize>>,
    span: Range<usize>,
    lines: (u32, u32),
    public: bool,
    /// No visibility modifier: named only in its module and the ones below.
    private: bool,
    fields: Vec<(String, Ty)>,
    generics: Vec<String>,
    /// A function's declared return type, and what a `Result` or `Option`
    /// it returns holds.
    returns: [Option<Ty>; 2],
    /// `type A = B;`: `B`.
    alias: Option<Ty>,
}

#[derive(Debug)]
struct Block {
    span: Range<usize>,
    module: usize,
    /// `impl X`, `impl Tr for X`: `X`. A trait has none: its `Self` is any
    /// implementer.
    self_ty: Option<Ty>,
    /// The trait implemented, or the trait itself.
    trait_: Option<Ty>,
    generics: Vec<String>,
}

#[derive(Debug)]
struct Import {
    /// The name bound; none for a glob.
    local: Option<String>,
    path: Vec<String>,
    module: usize,
    /// A `use` inside a function binds in its body only.
    scope: Option<Range<usize>>,
    exported: bool,
}

#[derive(Debug)]
struct ModDecl {
    name: String,
    module: usize,
}

#[derive(Clone, Debug)]
enum Recv {
    SelfValue,
    SelfField(String),
    Typed(Ty),
    Untyped,
}

#[derive(Clone, Debug)]
enum Via {
    /// A bare name; `local` when a binding of the function around it
    /// shadows every item.
    Bare {
        local: bool,
    },
    /// The first segment of a path: `Foo` in `Foo::new`.
    Head,
    /// `a::b::name`: the segments before the name.
    Path(Vec<String>),
    Method(Recv),
    Macro,
    /// The trait an `impl Tr for X` header names.
    Implements(Vec<String>),
}

#[derive(Debug)]
struct Use {
    name: String,
    line: u32,
    at: usize,
    module: usize,
    via: Via,
    call: bool,
}

#[derive(Default)]
struct Facts {
    /// Inline module paths inside the file; the first is the file's own.
    modules: Vec<Vec<String>>,
    items: Vec<Item>,
    blocks: Vec<Block>,
    imports: Vec<Import>,
    mods: Vec<ModDecl>,
    uses: Vec<Use>,
    /// Every name the code spells or imports.
    names: FxHashSet<String>,
}

impl Facts {
    fn module_of(&self, path: &[String]) -> Option<usize> {
        self.modules.iter().position(|m| m == path)
    }

    fn block_at(&self, at: usize) -> Option<&Block> {
        self.blocks
            .iter()
            .filter(|b| b.span.contains(&at))
            .min_by_key(|b| b.span.len())
    }

    fn generic_at(&self, at: usize, name: &str) -> bool {
        let named = |g: &Vec<String>| g.iter().any(|n| n == name);
        self.items
            .iter()
            .any(|i| i.span.contains(&at) && named(&i.generics))
            || self
                .blocks
                .iter()
                .any(|b| b.span.contains(&at) && named(&b.generics))
    }
}

/// A binding of the function around a use: the type its declaration
/// states, and where its scope opens. A `let` names nothing in its own
/// initializer: `let f = |x| f(x);` calls the outer `f`.
#[derive(Clone)]
struct Local {
    ty: Option<Ty>,
    from: usize,
}

type Locals = FxHashMap<String, Local>;

fn text<'b>(node: Node, bytes: &'b [u8]) -> &'b str {
    node.utf8_text(bytes).unwrap_or("")
}

fn line_of(node: Node) -> u32 {
    node.start_position().row as u32 + 1
}

fn named_children(node: Node) -> Vec<Node> {
    (0..node.named_child_count())
        .filter_map(|i| node.named_child(i))
        .collect()
}

fn visibility<'b>(node: Node, bytes: &'b [u8]) -> Option<&'b str> {
    named_children(node)
        .into_iter()
        .find(|c| c.kind() == "visibility_modifier")
        .map(|v| text(v, bytes))
}

/// A path segment as written; `$crate` in a macro is `crate`.
fn segment(node: Node, bytes: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" | "type_identifier" | "crate" | "self" | "super" => {
            Some(text(node, bytes).to_string())
        }
        "metavariable" if text(node, bytes) == "$crate" => Some("crate".to_string()),
        _ => None,
    }
}

fn segments(node: Node, bytes: &[u8]) -> Option<Vec<String>> {
    match node.kind() {
        "scoped_identifier" | "scoped_type_identifier" => {
            let mut path = match node.child_by_field_name("path") {
                Some(p) => segments(p, bytes)?,
                None => Vec::new(),
            };
            path.push(segment(node.child_by_field_name("name")?, bytes)?);
            Some(path)
        }
        "generic_type" | "generic_type_with_turbofish" => {
            segments(node.child_by_field_name("type")?, bytes)
        }
        _ => segment(node, bytes).map(|s| vec![s]),
    }
}

fn ty_of(node: Node, bytes: &[u8]) -> Option<Ty> {
    match node.kind() {
        "type_identifier" | "identifier" | "scoped_type_identifier" | "scoped_identifier" => {
            Some(Ty::named(segments(node, bytes)?))
        }
        "generic_type" => {
            let head = segments(node.child_by_field_name("type")?, bytes)?;
            // A smart pointer derefs to what it holds.
            if matches!(head.as_slice(), [p] if matches!(p.as_str(), "Box" | "Rc" | "Arc")) {
                let args = node.child_by_field_name("type_arguments")?;
                return named_children(args)
                    .into_iter()
                    .find_map(|a| ty_of(a, bytes));
            }
            Some(Ty::named(head))
        }
        "reference_type" | "pointer_type" => ty_of(node.child_by_field_name("type")?, bytes),
        "bounded_type" => named_children(node)
            .into_iter()
            .find_map(|c| ty_of(c, bytes)),
        "dynamic_type" | "abstract_type" => {
            let ty = ty_of(node.child_by_field_name("trait")?, bytes)?;
            Some(Ty {
                dynamic: true,
                ..ty
            })
        }
        _ => None,
    }
}

/// What a declared return type gives a caller, and what `?` unwraps of it.
fn returns_of(node: Node, bytes: &[u8]) -> [Option<Ty>; 2] {
    let Some(ret) = node.child_by_field_name("return_type") else {
        return [None, None];
    };
    let held = (ret.kind() == "generic_type")
        .then(|| ret.child_by_field_name("type"))
        .flatten()
        .filter(|t| matches!(text(*t, bytes), "Result" | "Option"))
        .and_then(|_| ret.child_by_field_name("type_arguments"))
        .and_then(|a| a.named_child(0))
        .and_then(|t| ty_of(t, bytes));
    [ty_of(ret, bytes), held]
}

/// The type a value expression builds: `T { .. }`, `T::new(..)` and
/// `a::T::parse(..)?` by Rust's constructor convention, and what a call of
/// a function returns.
fn built_ty(node: Node, bytes: &[u8]) -> Option<Ty> {
    let unwrapped = |value: Node| {
        let ty = built_ty(value, bytes)?;
        Some(Ty {
            returns: ty.returns.map(|_| Returns::Unwrapped),
            ..ty
        })
    };
    match node.kind() {
        "try_expression" => unwrapped(node.named_child(0)?),
        "parenthesized_expression" | "await_expression" => built_ty(node.named_child(0)?, bytes),
        "reference_expression" => built_ty(node.child_by_field_name("value")?, bytes),
        "struct_expression" => ty_of(node.child_by_field_name("name")?, bytes),
        "call_expression" => {
            let f = node.child_by_field_name("function")?;
            let f = match f.kind() {
                "generic_function" => f.child_by_field_name("function")?,
                // `.unwrap()` and `.expect(..)` take what `?` would.
                "field_expression" => {
                    let field = f.child_by_field_name("field").map(|n| text(n, bytes));
                    return matches!(field, Some("unwrap" | "expect"))
                        .then(|| unwrapped(f.child_by_field_name("value")?))
                        .flatten();
                }
                _ => f,
            };
            if !matches!(f.kind(), "identifier" | "scoped_identifier") {
                return None;
            }
            let path = segments(f, bytes)?;
            match path.as_slice() {
                [.., ty, _] if ty.starts_with(char::is_uppercase) => {
                    Some(Ty::named(path[..path.len() - 1].to_vec()))
                }
                _ => Some(Ty {
                    returns: Some(Returns::Value),
                    ..Ty::named(path)
                }),
            }
        }
        _ => None,
    }
}

/// The names a pattern binds. A capitalised name is a constant, a unit
/// struct or a variant, never a binding; the whole pattern's name takes
/// the declared type.
fn bind(pattern: Option<Node>, ty: Option<&Ty>, from: usize, bytes: &[u8], locals: &mut Locals) {
    let Some(p) = pattern else { return };
    let mut put = |name: &str, ty: Option<Ty>| {
        let from = locals.get(name).map_or(from, |l| l.from.min(from));
        locals.insert(name.to_string(), Local { ty, from });
    };
    let whole = match p.kind() {
        "mut_pattern" => named_children(p)
            .into_iter()
            .find(|c| c.kind() == "identifier"),
        _ => Some(p),
    };
    let mut stack = vec![p];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "identifier" => {
                let name = text(n, bytes);
                if !name.starts_with(char::is_uppercase) {
                    put(name, (Some(n) == whole).then(|| ty.cloned()).flatten());
                }
            }
            "shorthand_field_identifier" => put(text(n, bytes), None),
            "tuple_struct_pattern" | "struct_pattern" => {
                let ty = n.child_by_field_name("type").map(|t| t.id());
                stack.extend(named_children(n).into_iter().filter(|c| Some(c.id()) != ty));
            }
            "scoped_identifier" | "scoped_type_identifier" | "field_identifier" => {}
            _ => stack.extend(named_children(n)),
        }
    }
}

/// The names a function or closure binds — parameters, `let`s, loop and
/// match patterns — over the ones it closes over, with the type each
/// declaration states. A nested function or closure binds its own.
fn declared_locals(func: Node, bytes: &[u8], outer: &Locals) -> Locals {
    let mut locals = outer.clone();
    let mut stack = named_children(func);
    while let Some(n) = stack.pop() {
        match n.kind() {
            "function_item" | "closure_expression" | "mod_item" | "impl_item" | "trait_item"
            | "macro_definition" => continue,
            "parameter" => {
                let ty = n.child_by_field_name("type").and_then(|t| ty_of(t, bytes));
                let pattern = n.child_by_field_name("pattern");
                bind(pattern, ty.as_ref(), func.start_byte(), bytes, &mut locals);
            }
            "let_declaration" => {
                let ty = n
                    .child_by_field_name("type")
                    .and_then(|t| ty_of(t, bytes))
                    .or_else(|| {
                        n.child_by_field_name("value")
                            .and_then(|v| built_ty(v, bytes))
                    });
                bind(
                    n.child_by_field_name("pattern"),
                    ty.as_ref(),
                    n.end_byte(),
                    bytes,
                    &mut locals,
                );
            }
            "for_expression" | "let_condition" => {
                let from = n
                    .child_by_field_name("body")
                    .map_or_else(|| n.end_byte(), |b| b.start_byte());
                bind(
                    n.child_by_field_name("pattern"),
                    None,
                    from,
                    bytes,
                    &mut locals,
                );
            }
            "match_pattern" => {
                let condition = n.child_by_field_name("condition").map(|c| c.id());
                let pattern = named_children(n)
                    .into_iter()
                    .find(|c| Some(c.id()) != condition);
                bind(pattern, None, n.end_byte(), bytes, &mut locals);
            }
            "closure_parameters" => {
                for p in named_children(n)
                    .into_iter()
                    .filter(|p| p.kind() != "parameter")
                {
                    bind(Some(p), None, func.start_byte(), bytes, &mut locals);
                }
            }
            _ => {}
        }
        stack.extend(named_children(n));
    }
    // A closure the function binds returns what no declaration states.
    let bound: FxHashSet<String> = locals.keys().cloned().collect();
    for local in locals.values_mut() {
        if local.ty.as_ref().is_some_and(|t| {
            t.returns.is_some() && matches!(t.path.as_slice(), [f] if bound.contains(f))
        }) {
            local.ty = None;
        }
    }
    locals
}

fn generics_of(node: Node, bytes: &[u8]) -> Vec<String> {
    node.child_by_field_name("type_parameters")
        .map(named_children)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|p| p.child_by_field_name("name"))
        .map(|n| text(n, bytes).to_string())
        .collect()
}

/// The type a receiver named `name` at `at` has: what its declaration
/// states, or a unit struct (`Strategy.run()`) when the name is no local.
fn local_ty(name: &str, at: usize, ctx: &Ctx) -> Recv {
    match ctx.local(name, at) {
        Some(Local { ty: Some(ty), .. }) => Recv::Typed(ty.clone()),
        None if name.starts_with(char::is_uppercase) => {
            Recv::Typed(Ty::named(vec![name.to_string()]))
        }
        _ => Recv::Untyped,
    }
}

fn is_call(node: Node) -> bool {
    let target = match node.parent() {
        Some(p)
            if p.kind() == "generic_function"
                && p.child_by_field_name("function") == Some(node) =>
        {
            p
        }
        _ => node,
    };
    target.parent().is_some_and(|c| {
        c.kind() == "call_expression" && c.child_by_field_name("function") == Some(target)
    })
}

#[derive(Clone, Default)]
struct Ctx {
    module: usize,
    locals: Rc<Locals>,
    pattern: bool,
    scope: Option<Range<usize>>,
    block: Option<usize>,
}

impl Ctx {
    /// The binding `name` names at byte `at`.
    fn local(&self, name: &str, at: usize) -> Option<&Local> {
        self.locals.get(name).filter(|l| l.from <= at)
    }
}

struct Extract<'b> {
    bytes: &'b [u8],
    facts: Facts,
}

impl Extract<'_> {
    fn module_index(&mut self, path: Vec<String>) -> usize {
        if let Some(i) = self.facts.module_of(&path) {
            return i;
        }
        self.facts.modules.push(path);
        self.facts.modules.len() - 1
    }

    fn record(&mut self, node: Node, name: &str, ctx: &Ctx, via: Via, call: bool) {
        self.facts.names.insert(name.to_string());
        self.facts.uses.push(Use {
            name: name.to_string(),
            line: line_of(node),
            at: node.start_byte(),
            module: ctx.module,
            via,
            call,
        });
    }

    fn item(&mut self, node: Node, kind: Kind, ctx: &Ctx) -> Option<usize> {
        let name = text(node.child_by_field_name("name")?, self.bytes).to_string();
        self.facts.names.insert(name.clone());
        self.facts.items.push(Item {
            name,
            kind,
            module: ctx.module,
            block: ctx.block,
            scope: ctx.scope.clone(),
            span: node.byte_range(),
            lines: (line_of(node), node.end_position().row as u32 + 1),
            public: visibility(node, self.bytes) == Some("pub"),
            private: visibility(node, self.bytes).is_none(),
            fields: Vec::new(),
            generics: generics_of(node, self.bytes),
            returns: returns_of(node, self.bytes),
            alias: (node.kind() == "type_item")
                .then(|| node.child_by_field_name("type"))
                .flatten()
                .and_then(|t| ty_of(t, self.bytes)),
        });
        Some(self.facts.items.len() - 1)
    }

    fn import(&mut self, local: Option<String>, path: Vec<String>, ctx: &Ctx, exported: bool) {
        self.facts.names.extend(path.iter().chain(&local).cloned());
        self.facts.imports.push(Import {
            local,
            path,
            module: ctx.module,
            scope: ctx.scope.clone(),
            exported,
        });
    }

    fn imports(&mut self, node: Node, ctx: &Ctx) {
        let bytes = self.bytes;
        let exported = visibility(node, bytes).is_some();
        let Some(arg) = node.child_by_field_name("argument") else {
            return;
        };
        let mut stack: Vec<(Node, Vec<String>)> = vec![(arg, Vec::new())];
        while let Some((n, prefix)) = stack.pop() {
            match n.kind() {
                "use_as_clause" => {
                    let (Some(path), Some(alias)) = (
                        n.child_by_field_name("path")
                            .and_then(|p| segments(p, bytes)),
                        n.child_by_field_name("alias").map(|a| text(a, bytes)),
                    ) else {
                        continue;
                    };
                    if alias != "_" {
                        self.import(
                            Some(alias.to_string()),
                            [prefix, path].concat(),
                            ctx,
                            exported,
                        );
                    }
                }
                "use_wildcard" => {
                    let path = n
                        .named_child(0)
                        .and_then(|p| segments(p, bytes))
                        .unwrap_or_default();
                    self.import(None, [prefix, path].concat(), ctx, exported);
                }
                "scoped_use_list" => {
                    let path = n
                        .child_by_field_name("path")
                        .and_then(|p| segments(p, bytes))
                        .unwrap_or_default();
                    let path = [prefix, path].concat();
                    for c in n
                        .child_by_field_name("list")
                        .map(named_children)
                        .unwrap_or_default()
                    {
                        stack.push((c, path.clone()));
                    }
                }
                "use_list" => {
                    stack.extend(named_children(n).into_iter().map(|c| (c, prefix.clone())));
                }
                _ => {
                    let Some(path) = segments(n, bytes) else {
                        continue;
                    };
                    let mut path = [prefix, path].concat();
                    // `use a::{self}` binds `a`.
                    if path.last().is_some_and(|s| s == "self") {
                        path.pop();
                    }
                    if let Some(local) = path.last().cloned() {
                        self.import(Some(local), path, ctx, exported);
                    }
                }
            }
        }
    }

    /// `a::b::name`: each segment a use of what the ones before it name.
    /// `Vec::<T>::new` names `Vec`; `<T as Tr>::f` calls through `T`.
    fn path<'t>(&mut self, node: Node<'t>, ctx: &Ctx, out: &mut Vec<(Node<'t>, Ctx)>) {
        let bytes = self.bytes;
        let plain = Ctx {
            pattern: false,
            ..ctx.clone()
        };
        let mut segs: Vec<(String, Node)> = Vec::new();
        let mut through: Option<Recv> = None;
        let mut cur = Some(node);
        while let Some(n) = cur.take() {
            match n.kind() {
                "scoped_identifier" | "scoped_type_identifier" => {
                    if let Some(name) = n.child_by_field_name("name") {
                        segs.extend(segment(name, bytes).map(|s| (s, name)));
                    }
                    cur = n.child_by_field_name("path");
                }
                "generic_type" | "generic_type_with_turbofish" => {
                    out.extend(
                        n.child_by_field_name("type_arguments")
                            .map(|a| (a, plain.clone())),
                    );
                    cur = n.child_by_field_name("type");
                }
                "bracketed_type" => {
                    let ty = n
                        .named_child(0)
                        .and_then(|q| match q.kind() {
                            "qualified_type" => q.child_by_field_name("type"),
                            _ => Some(q),
                        })
                        .and_then(|t| ty_of(t, bytes));
                    through = Some(ty.map_or(Recv::Untyped, Recv::Typed));
                    out.push((n, plain.clone()));
                }
                _ => match segment(n, bytes) {
                    Some(s) => segs.push((s, n)),
                    None => out.push((n, plain.clone())),
                },
            }
        }
        segs.reverse();
        let call = is_call(node);
        if let Some(recv) = through {
            if let [(name, n)] = segs.as_slice() {
                self.record(*n, name, &plain, Via::Method(recv), call);
            }
            return;
        }
        let names: Vec<String> = segs.iter().map(|(s, _)| s.clone()).collect();
        for (i, (name, n)) in segs.iter().enumerate() {
            if matches!(name.as_str(), "crate" | "self" | "super" | "Self") {
                continue;
            }
            let via = if i == 0 {
                Via::Head
            } else {
                Via::Path(names[..i].to_vec())
            };
            self.record(*n, name, &plain, via, call && i + 1 == segs.len());
        }
    }

    /// A macro's tokens: `a::b(…)`, `x.m(…)` and `name!` read as the code
    /// they expand to.
    fn tokens(&mut self, tree: Node, ctx: &Ctx) {
        let bytes = self.bytes;
        let seg = |n: &Node| segment(*n, bytes);
        let mut trees = vec![tree];
        while let Some(t) = trees.pop() {
            let toks: Vec<Node> = (0..t.child_count()).filter_map(|i| t.child(i)).collect();
            let kind = |i: Option<usize>| i.and_then(|i| toks.get(i)).map_or("", |n| n.kind());
            let mut i = 0;
            while i < toks.len() {
                if toks[i].kind() == "token_tree" {
                    trees.push(toks[i]);
                    i += 1;
                    continue;
                }
                // `mod x;` inside a macro (`cfg_if!`) declares a module all the same.
                if toks[i].kind() == "mod" && kind(Some(i + 2)) == ";" {
                    if let Some(name) = toks.get(i + 1).filter(|n| n.kind() == "identifier") {
                        self.facts.mods.push(ModDecl {
                            name: text(*name, bytes).to_string(),
                            module: ctx.module,
                        });
                    }
                }
                if seg(&toks[i]).is_none() {
                    i += 1;
                    continue;
                }
                let mut run = vec![i];
                while let Some(&j) = run.last() {
                    if kind(Some(j + 1)) == "::" && toks.get(j + 2).and_then(seg).is_some() {
                        run.push(j + 2);
                    } else {
                        break;
                    }
                }
                let end = run[run.len() - 1];
                let names: Vec<String> = run.iter().filter_map(|&j| seg(&toks[j])).collect();
                let next = toks.get(end + 1);
                let call = next.is_some_and(|n| {
                    n.kind() == "token_tree" && n.child(0).is_some_and(|c| c.kind() == "(")
                });
                let back = |k: usize| i.checked_sub(k);
                if next.is_some_and(|n| n.kind() == "!") {
                    self.record(toks[end], &names[names.len() - 1], ctx, Via::Macro, false);
                } else if kind(back(1)) == "." {
                    let recv = match (kind(back(2)), kind(back(3)), kind(back(4))) {
                        ("identifier", ".", "self") => Recv::SelfField(
                            back(2)
                                .map(|j| text(toks[j], bytes).to_string())
                                .unwrap_or_default(),
                        ),
                        ("self", k, _) if k != "." => Recv::SelfValue,
                        ("identifier", k, _) if k != "." && k != "::" => back(2)
                            .map_or(Recv::Untyped, |j| {
                                local_ty(text(toks[j], bytes), toks[j].start_byte(), ctx)
                            }),
                        _ => Recv::Untyped,
                    };
                    self.record(
                        toks[i],
                        &names[0],
                        ctx,
                        Via::Method(recv),
                        call && run.len() == 1,
                    );
                } else if kind(back(1)) != "::" {
                    for (k, &j) in run.iter().enumerate() {
                        let name = &names[k];
                        if matches!(name.as_str(), "crate" | "self" | "super" | "Self") {
                            continue;
                        }
                        let via = match (run.len(), k) {
                            (1, _) => Via::Bare {
                                local: ctx.local(name, toks[j].start_byte()).is_some(),
                            },
                            (_, 0) => Via::Head,
                            _ => Via::Path(names[..k].to_vec()),
                        };
                        self.record(toks[j], name, ctx, via, call && k + 1 == run.len());
                    }
                }
                i = end + 1;
            }
        }
    }

    fn visit<'t>(&mut self, node: Node<'t>, ctx: &Ctx, out: &mut Vec<(Node<'t>, Ctx)>) {
        let bytes = self.bytes;
        let push = |out: &mut Vec<(Node<'t>, Ctx)>, node: Node<'t>, ctx: &Ctx, skip: &[&str]| {
            let skipped: Vec<usize> = skip
                .iter()
                .filter_map(|f| node.child_by_field_name(f))
                .map(|n| n.id())
                .collect();
            out.extend(
                named_children(node)
                    .into_iter()
                    .filter(|c| !skipped.contains(&c.id()))
                    .map(|c| (c, ctx.clone())),
            );
        };
        let with_pattern = |pattern: bool| Ctx {
            pattern,
            ..ctx.clone()
        };
        let outside = Ctx {
            block: None,
            ..ctx.clone()
        };
        match node.kind() {
            "line_comment"
            | "block_comment"
            | "attribute_item"
            | "inner_attribute_item"
            | "string_literal"
            | "raw_string_literal"
            | "char_literal"
            | "lifetime"
            | "label"
            | "visibility_modifier"
            | "extern_crate_declaration"
            | "self_parameter"
            | "primitive_type"
            | "field_identifier"
            | "shorthand_field_identifier"
            | "metavariable"
            | "token_tree_pattern" => {}
            "use_declaration" => self.imports(node, ctx),
            "mod_item" => {
                self.item(node, Kind::Mod, ctx);
                let Some(name) = node.child_by_field_name("name") else {
                    return;
                };
                let name = text(name, bytes).to_string();
                self.facts.mods.push(ModDecl {
                    name: name.clone(),
                    module: ctx.module,
                });
                if let Some(body) = node.child_by_field_name("body") {
                    let mut path = self.facts.modules[ctx.module].clone();
                    path.push(name);
                    let inner = Ctx {
                        module: self.module_index(path),
                        ..Ctx::default()
                    };
                    push(out, body, &inner, &[]);
                }
            }
            "function_item" | "function_signature_item" => {
                self.item(node, Kind::Fn, ctx);
                let inner = Ctx {
                    module: ctx.module,
                    locals: Rc::new(declared_locals(node, bytes, &Locals::default())),
                    pattern: false,
                    scope: Some(node.byte_range()),
                    block: None,
                };
                push(out, node, &inner, &["name"]);
            }
            "closure_expression" => {
                let inner = Ctx {
                    locals: Rc::new(declared_locals(node, bytes, &ctx.locals)),
                    ..with_pattern(false)
                };
                push(out, node, &inner, &[]);
            }
            "impl_item" => {
                let field = |f: &str| node.child_by_field_name(f);
                self.facts.blocks.push(Block {
                    span: node.byte_range(),
                    module: ctx.module,
                    self_ty: field("type").and_then(|t| ty_of(t, bytes)),
                    trait_: field("trait").and_then(|t| ty_of(t, bytes)),
                    generics: generics_of(node, bytes),
                });
                let inner = Ctx {
                    block: Some(self.facts.blocks.len() - 1),
                    ..ctx.clone()
                };
                if let Some(t) = field("trait") {
                    self.header(t, &outside, out, true);
                }
                if let Some(t) = field("type") {
                    self.header(t, &outside, out, false);
                }
                if let Some(body) = field("body") {
                    push(out, body, &inner, &[]);
                }
                push(out, node, &outside, &["trait", "type", "body"]);
            }
            "trait_item" => {
                self.item(node, Kind::Trait, ctx);
                let name = node
                    .child_by_field_name("name")
                    .map(|n| text(n, bytes).to_string());
                self.facts.blocks.push(Block {
                    span: node.byte_range(),
                    module: ctx.module,
                    self_ty: None,
                    trait_: name.map(|n| Ty::named(vec![n])),
                    generics: generics_of(node, bytes),
                });
                let inner = Ctx {
                    block: Some(self.facts.blocks.len() - 1),
                    ..ctx.clone()
                };
                if let Some(body) = node.child_by_field_name("body") {
                    push(out, body, &inner, &[]);
                }
                push(out, node, &outside, &["name", "body"]);
            }
            "struct_item" | "union_item" => {
                if let Some(idx) = self.item(node, Kind::Type, ctx) {
                    let fields = node
                        .child_by_field_name("body")
                        .filter(|b| b.kind() == "field_declaration_list")
                        .map(named_children)
                        .unwrap_or_default()
                        .into_iter()
                        .filter_map(|f| {
                            let name = text(f.child_by_field_name("name")?, bytes).to_string();
                            Some((name, ty_of(f.child_by_field_name("type")?, bytes)?))
                        })
                        .collect();
                    self.facts.items[idx].fields = fields;
                }
                push(out, node, &outside, &["name"]);
            }
            "enum_item" | "type_item" => {
                self.item(node, Kind::Type, ctx);
                push(out, node, &outside, &["name"]);
            }
            "const_item" | "static_item" => {
                self.item(node, Kind::Value, ctx);
                push(out, node, &outside, &["name"]);
            }
            "macro_definition" => {
                self.item(node, Kind::Macro, ctx);
                push(out, node, &outside, &["name"]);
            }
            "associated_type" | "enum_variant" | "field_declaration" | "type_parameter"
            | "const_parameter" | "field_pattern" | "field_initializer" => {
                push(out, node, ctx, &["name", "field"]);
            }
            "let_declaration" | "parameter" | "for_expression" | "let_condition" => {
                if let Some(p) = node.child_by_field_name("pattern") {
                    out.push((p, with_pattern(true)));
                }
                push(out, node, &with_pattern(false), &["pattern"]);
            }
            "match_pattern" => {
                let condition = node.child_by_field_name("condition").map(|c| c.id());
                for c in named_children(node) {
                    out.push((c, with_pattern(Some(c.id()) != condition)));
                }
            }
            "closure_parameters" => push(out, node, &with_pattern(true), &[]),
            "tuple_struct_pattern" | "struct_pattern" => {
                if let Some(t) = node.child_by_field_name("type") {
                    out.push((t, with_pattern(false)));
                }
                push(out, node, ctx, &["type"]);
            }
            "identifier" => {
                let name = text(node, bytes);
                if ctx.pattern && !name.starts_with(char::is_uppercase) {
                    return;
                }
                let local = ctx.local(name, node.start_byte()).is_some();
                self.record(node, name, ctx, Via::Bare { local }, is_call(node));
            }
            "type_identifier" => {
                let name = text(node, bytes);
                self.record(node, name, ctx, Via::Bare { local: false }, false);
            }
            "scoped_identifier" | "scoped_type_identifier" => self.path(node, ctx, out),
            "field_expression" => {
                let (Some(field), Some(value)) = (
                    node.child_by_field_name("field"),
                    node.child_by_field_name("value"),
                ) else {
                    return;
                };
                if field.kind() == "field_identifier" {
                    let recv = self.receiver(value, ctx);
                    self.record(
                        field,
                        text(field, bytes),
                        ctx,
                        Via::Method(recv),
                        is_call(node),
                    );
                }
                out.push((value, ctx.clone()));
            }
            "macro_invocation" => {
                if let Some(m) = node.child_by_field_name("macro") {
                    let name = match m.kind() {
                        "scoped_identifier" => m.child_by_field_name("name").unwrap_or(m),
                        _ => m,
                    };
                    self.record(name, text(name, bytes), ctx, Via::Macro, false);
                }
                for t in named_children(node)
                    .into_iter()
                    .filter(|c| c.kind() == "token_tree")
                {
                    self.tokens(t, ctx);
                }
            }
            "token_tree" => self.tokens(node, ctx),
            _ => push(out, node, ctx, &[]),
        }
    }

    /// An `impl` header: the trait it names is used, the type it extends is
    /// that type's own definition; their type arguments are uses either way.
    fn header<'t>(
        &mut self,
        ty: Node<'t>,
        ctx: &Ctx,
        out: &mut Vec<(Node<'t>, Ctx)>,
        trait_: bool,
    ) {
        let head = match ty.kind() {
            "generic_type" => {
                out.extend(
                    ty.child_by_field_name("type_arguments")
                        .map(|a| (a, ctx.clone())),
                );
                ty.child_by_field_name("type")
            }
            _ => Some(ty),
        };
        if !trait_ {
            return;
        }
        if let Some(mut path) = head.and_then(|h| segments(h, self.bytes)) {
            let name = path.pop().unwrap_or_default();
            self.record(ty, &name, ctx, Via::Implements(path), false);
        }
    }

    fn receiver(&self, value: Node, ctx: &Ctx) -> Recv {
        let bytes = self.bytes;
        match value.kind() {
            "self" => Recv::SelfValue,
            "field_expression"
                if value
                    .child_by_field_name("value")
                    .is_some_and(|v| v.kind() == "self") =>
            {
                value
                    .child_by_field_name("field")
                    .filter(|f| f.kind() == "field_identifier")
                    .map_or(Recv::Untyped, |f| {
                        Recv::SelfField(text(f, bytes).to_string())
                    })
            }
            "identifier" => local_ty(text(value, bytes), value.start_byte(), ctx),
            _ => built_ty(value, bytes).map_or(Recv::Untyped, Recv::Typed),
        }
    }
}

fn extract(root: Node, bytes: &[u8]) -> Facts {
    let mut x = Extract {
        bytes,
        facts: Facts {
            modules: vec![Vec::new()],
            ..Facts::default()
        },
    };
    let mut stack = vec![(root, Ctx::default())];
    let mut out = Vec::new();
    while let Some((node, ctx)) = stack.pop() {
        x.visit(node, &ctx, &mut out);
        stack.append(&mut out);
    }
    x.facts.uses.sort_by_key(|u| (u.line, u.at));
    x.facts
}

fn facts_of(text: &str) -> Option<Facts> {
    let tree = crate::parsers::parse_tree("rust", text)?;
    Some(extract(tree.root_node(), text.as_bytes()))
}

/// A module as a path from its crate root: the root file — or the directory
/// whose files are each a crate root (`tests/`, `src/bin/`) — and the `mod`
/// names below it, inline ones included.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ModKey {
    root: String,
    segs: Vec<String>,
}

impl ModKey {
    fn child(&self, name: &str) -> Self {
        let mut segs = self.segs.clone();
        segs.push(name.to_string());
        Self {
            root: self.root.clone(),
            segs,
        }
    }
}

/// An item by where it is declared.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ItemId {
    file: String,
    start: usize,
}

#[derive(Clone, Debug)]
enum Res {
    Item(ItemId, Kind),
    /// A name inside a type: a method, an associated function, a variant.
    Assoc(ItemId, String),
    Module(ModKey),
    /// Nothing of that name in these modules: a removed definition's
    /// broken use.
    Missing(Vec<ModKey>, String),
    /// A generic parameter or a trait's `Self`: any implementer.
    Generic,
    External,
    Unknown,
}

#[derive(Clone, Copy)]
struct Site<'s> {
    file: &'s str,
    facts: &'s Facts,
    module: usize,
    at: usize,
    /// The `use` being resolved, which cannot bind its own path.
    skip: Option<usize>,
}

impl<'s> Site<'s> {
    fn new(file: &'s str, facts: &'s Facts, module: usize, at: usize) -> Self {
        Self {
            file,
            facts,
            module,
            at,
            skip: None,
        }
    }
}

/// What `make` gives for `key`, made once. `pending` answers for the key
/// while it is being made: a `use` or glob cycle that reaches it again.
fn memo<K: std::hash::Hash + Eq + Clone, V: Clone>(
    cell: &RefCell<FxHashMap<K, V>>,
    key: K,
    pending: Option<V>,
    make: impl FnOnce() -> V,
) -> V {
    if let Some(known) = cell.borrow().get(&key) {
        return known.clone();
    }
    if let Some(p) = pending {
        cell.borrow_mut().insert(key.clone(), p);
    }
    let value = make();
    cell.borrow_mut().insert(key, value.clone());
    value
}

/// What owns a member: its type and the trait it implements, or the trait
/// declaring it; `name` is what a caller has to name to reach it.
pub(super) struct Owner {
    ty: Option<ItemId>,
    trait_: Option<ItemId>,
    in_trait: bool,
    /// A trait's member, or one an `impl Tr for X` implements: calls may
    /// arrive through `dyn Tr` or a generic bound.
    dispatched: bool,
    name: String,
}

pub(super) struct RustDef {
    path: String,
    key: ModKey,
    /// None once the range removed it.
    id: Option<ItemId>,
    name: String,
    kind: Kind,
    span: Option<Range<usize>>,
    owner: Option<Owner>,
    package: Option<String>,
    private: bool,
}

const MAX_DEPTH: usize = 8;
const RECEIVER: &str = "receiver_unresolved";
const IMPORT: &str = "import_unresolved";
const TARGET_DIRS: &[&str] = &["tests", "examples", "benches"];

fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

fn join(a: &str, b: &str) -> String {
    match (a.is_empty(), b.is_empty()) {
        (true, _) => b.to_string(),
        (_, true) => a.to_string(),
        _ => format!("{a}/{b}"),
    }
}

pub(super) struct RustResolver<'a> {
    root: &'a Path,
    source: &'a Source,
    /// Every path a module may live at, deleted and renamed-away ones too.
    files: FxHashSet<String>,
    texts: RefCell<FxHashMap<String, Option<Rc<str>>>>,
    facts: RefCell<FxHashMap<String, Option<Rc<Facts>>>>,
    facts_at: RefCell<FxHashMap<(String, String), Option<Rc<Facts>>>>,
    batch: RefCell<Option<crate::git::CatFileBatch>>,
    keys: RefCell<FxHashMap<String, ModKey>>,
    packages: RefCell<FxHashMap<String, Option<String>>>,
    crates: OnceCell<FxHashMap<String, String>>,
    referenced: RefCell<FxHashMap<String, Rc<FxHashSet<String>>>>,
    block_types: RefCell<FxHashMap<(String, usize), Option<ItemId>>>,
    /// Globs and re-exports chain modules into one another: each lookup and
    /// each `use` is resolved once.
    lookups: RefCell<FxHashMap<(ModKey, String), Res>>,
    imports: RefCell<FxHashMap<(String, usize), Res>>,
}

impl<'a> RustResolver<'a> {
    pub(super) fn new(root: &'a Path, source: &'a Source, files: FxHashSet<String>) -> Self {
        Self {
            root,
            source,
            files,
            texts: RefCell::default(),
            facts: RefCell::default(),
            facts_at: RefCell::default(),
            batch: RefCell::default(),
            keys: RefCell::default(),
            packages: RefCell::default(),
            crates: OnceCell::new(),
            referenced: RefCell::default(),
            block_types: RefCell::default(),
            lookups: RefCell::default(),
            imports: RefCell::default(),
        }
    }

    fn text(&self, rel: &str) -> Option<Rc<str>> {
        memo(&self.texts, rel.to_string(), None, || {
            self.source
                .read_to_string(&self.root.join(rel))
                .map(Rc::from)
        })
    }

    fn facts(&self, rel: &str) -> Option<Rc<Facts>> {
        memo(&self.facts, rel.to_string(), None, || {
            let text = self.text(rel).filter(|_| is_rust(rel))?;
            facts_of(&text).map(Rc::new)
        })
    }

    fn facts_at(&self, rev: &str, rel: &str) -> Option<Rc<Facts>> {
        memo(
            &self.facts_at,
            (rev.to_string(), rel.to_string()),
            None,
            || {
                let mut batch = self.batch.borrow_mut();
                if batch.is_none() {
                    *batch = crate::git::CatFileBatch::new(self.root).ok();
                }
                let text = batch
                    .as_mut()
                    .and_then(|b| b.get(rev, Path::new(rel)).ok())
                    .or_else(|| {
                        crate::git::show_file_at_revision(self.root, rev, Path::new(rel)).ok()
                    })?;
                facts_of(&text).map(Rc::new)
            },
        )
    }

    fn readable(&self, rel: &str) -> bool {
        self.files.contains(rel) && self.text(rel).is_some()
    }

    /// The directory of the nearest `Cargo.toml` above `file`.
    fn package_of(&self, file: &str) -> Option<String> {
        let dir = parent_dir(file);
        memo(&self.packages, dir.to_string(), None, || {
            let mut at = dir;
            loop {
                if self.files.contains(&join(at, "Cargo.toml")) {
                    return Some(at.to_string());
                }
                if at.is_empty() {
                    return None;
                }
                at = parent_dir(at);
            }
        })
    }

    /// Library crates of the workspace by the name a path spells them with.
    fn crates(&self) -> &FxHashMap<String, String> {
        self.crates.get_or_init(|| {
            let mut out = FxHashMap::default();
            for manifest in self
                .files
                .iter()
                .filter(|f| f.rsplit('/').next() == Some("Cargo.toml"))
            {
                let Some(doc) = self
                    .text(manifest)
                    .and_then(|t| t.parse::<toml::Table>().ok())
                else {
                    continue;
                };
                let field = |section: &str, key: &str| doc.get(section)?.get(key)?.as_str();
                let root = join(
                    parent_dir(manifest),
                    field("lib", "path").unwrap_or("src/lib.rs"),
                );
                if self.files.contains(&root) {
                    for name in [field("package", "name"), field("lib", "name")]
                        .into_iter()
                        .flatten()
                    {
                        out.insert(name.replace('-', "_"), root.clone());
                    }
                }
            }
            out
        })
    }

    fn key_of(&self, file: &str) -> ModKey {
        memo(&self.keys, file.to_string(), None, || {
            self.module_key_of(file)
        })
    }

    fn module_key_of(&self, file: &str) -> ModKey {
        let package = self.package_of(file);
        let pkg = package.as_deref().unwrap_or("");
        let rel = file
            .strip_prefix(pkg)
            .map_or(file, |r| r.trim_start_matches('/'));
        let parts: Vec<&str> = rel.split('/').collect();
        let (dirs, last) = parts.split_at(parts.len() - 1);
        let stem = last[0].strip_suffix(".rs").unwrap_or(last[0]);
        let dir_at = |k: usize| join(pkg, &dirs[..k].join("/"));
        let has_main = |k: usize| self.files.contains(&join(&dir_at(k), "main.rs"));
        // (directories in the base, whether each file there is a crate root
        // whose modules the others share, whether the base holds one crate
        // with a `main.rs`)
        let (base, flat, dir_root) = match dirs {
            _ if package.is_none() => (
                dirs.iter()
                    .rposition(|d| *d == "src")
                    .map_or(dirs.len(), |i| i + 1),
                false,
                false,
            ),
            ["src", "bin", _, ..] if has_main(3) => (3, false, true),
            ["src", "bin", ..] => (2, true, false),
            ["src", ..] => (1, false, false),
            [t, _, ..] if TARGET_DIRS.contains(t) && has_main(2) => (2, false, true),
            [t, ..] if TARGET_DIRS.contains(t) => (1, true, false),
            _ => (0, false, false),
        };
        let base_dir = dir_at(base);
        let at_base = dirs.len() == base;
        let mut segs: Vec<String> = dirs[base..].iter().map(|s| s.to_string()).collect();
        segs.push(stem.to_string());
        if segs.last().is_some_and(|s| s == "mod") {
            segs.pop();
        }
        let root = if flat && !at_base {
            base_dir
        } else if at_base && (flat || matches!((stem, base), ("lib" | "main", _) | ("build", 0))) {
            segs.clear();
            file.to_string()
        } else if dir_root {
            join(&base_dir, "main.rs")
        } else {
            self.crate_root(&base_dir, segs.first().map(String::as_str))
        };
        ModKey { root, segs }
    }

    /// The crate a module of `dir` belongs to: the library, unless only the
    /// binary declares the module.
    fn crate_root(&self, dir: &str, first: Option<&str>) -> String {
        let (lib, main) = (join(dir, "lib.rs"), join(dir, "main.rs"));
        let declares = |root: &str| {
            first.is_some_and(|m| {
                self.facts(root)
                    .is_some_and(|f| f.mods.iter().any(|d| d.module == 0 && d.name == m))
            })
        };
        let (has_lib, has_main) = (self.files.contains(&lib), self.files.contains(&main));
        match (has_lib, has_main) {
            (true, true) if !declares(&lib) && declares(&main) => main,
            (true, _) => lib,
            (false, true) => main,
            (false, false) => dir.to_string(),
        }
    }

    fn module_key(&self, site: Site) -> ModKey {
        let mut key = self.key_of(site.file);
        key.segs
            .extend(site.facts.modules[site.module].iter().cloned());
        key
    }

    /// The file holding a module, and the inline modules inside it: the
    /// file the snapshot has, else one the range deleted.
    fn module_file(&self, key: &ModKey) -> Option<(String, Vec<String>)> {
        let root_file = is_rust(&key.root).then_some(&key.root);
        let dir = root_file.map_or(key.root.as_str(), |r| parent_dir(r));
        for k in (1..=key.segs.len()).rev() {
            let p = join(dir, &key.segs[..k].join("/"));
            let found = [format!("{p}.rs"), format!("{p}/mod.rs")];
            let pick = found
                .iter()
                .find(|f| self.readable(f))
                .or_else(|| found.iter().find(|f| self.files.contains(*f)));
            if let Some(f) = pick {
                return Some((f.clone(), key.segs[k..].to_vec()));
            }
        }
        root_file.map(|r| (r.clone(), key.segs.clone()))
    }

    /// Two paths to one module: the crates under `tests/` share theirs.
    fn same_module(&self, a: &ModKey, b: &ModKey) -> bool {
        a == b
            || self
                .module_file(a)
                .is_some_and(|f| Some(f) == self.module_file(b))
    }

    /// A module its parent declares: `src/c.rs` is no module of a
    /// `main.rs` that never says `mod c`.
    fn is_child(site: Site, name: &str) -> bool {
        site.facts
            .mods
            .iter()
            .any(|m| m.module == site.module && m.name == name)
    }

    /// What a name means where it stands, from the innermost scope out:
    /// an item the function body declares, a `use` there, an item of the
    /// module, a `use` of the module.
    fn in_scope(&self, site: Site, name: &str, depth: usize) -> Option<Res> {
        let f = site.facts;
        let id = |i: &Item| {
            Res::Item(
                ItemId {
                    file: site.file.to_string(),
                    start: i.span.start,
                },
                i.kind,
            )
        };
        let value = |i: &&Item| i.name == name && !matches!(i.kind, Kind::Mod | Kind::Macro);
        if let Some(i) = f
            .items
            .iter()
            .filter(value)
            .filter(|i| i.scope.as_ref().is_some_and(|s| s.contains(&site.at)))
            .min_by_key(|i| i.scope.as_ref().map_or(0, |s| s.len()))
        {
            return Some(id(i));
        }
        let binds =
            |(n, m): &(usize, &Import)| Some(*n) != site.skip && m.local.as_deref() == Some(name);
        if let Some((n, _)) = f
            .imports
            .iter()
            .enumerate()
            .filter(binds)
            .filter(|(_, m)| m.scope.as_ref().is_some_and(|s| s.contains(&site.at)))
            .min_by_key(|(_, m)| m.scope.as_ref().map_or(0, |s| s.len()))
        {
            return Some(self.resolve_import(site.file, f, n, depth + 1));
        }
        if let Some(i) = f
            .items
            .iter()
            .filter(value)
            .find(|i| i.scope.is_none() && i.block.is_none() && i.module == site.module)
        {
            return Some(id(i));
        }
        f.imports
            .iter()
            .enumerate()
            .filter(binds)
            .find(|(_, m)| m.scope.is_none() && m.module == site.module)
            .map(|(n, _)| self.resolve_import(site.file, f, n, depth + 1))
    }

    /// A name the module's globs bring in.
    fn globbed(&self, site: Site, name: &str, depth: usize) -> Res {
        let mut missing = Vec::new();
        let mut unknown = false;
        for (n, m) in site.facts.imports.iter().enumerate() {
            let in_scope = m
                .scope
                .as_ref()
                .map_or(m.module == site.module, |s| s.contains(&site.at));
            if m.local.is_some() || !in_scope || Some(n) == site.skip {
                continue;
            }
            match self.resolve_import(site.file, site.facts, n, depth + 1) {
                Res::Module(key) => match self.lookup(&key, name, depth + 1) {
                    found @ (Res::Item(..) | Res::Module(_) | Res::Assoc(..)) => return found,
                    Res::Missing(keys, _) => missing.extend(keys),
                    Res::Unknown => unknown = true,
                    _ => {}
                },
                Res::Unknown => unknown = true,
                // An enum's variants, an outside crate's prelude.
                _ => {}
            }
        }
        if unknown {
            Res::Unknown
        } else {
            Res::Missing(missing, name.to_string())
        }
    }

    /// A `use` reaching itself again binds nothing it can be held to.
    fn resolve_import(&self, file: &str, facts: &Facts, n: usize, depth: usize) -> Res {
        memo(
            &self.imports,
            (file.to_string(), n),
            Some(Res::Unknown),
            || {
                let m = &facts.imports[n];
                let at = m.scope.as_ref().map_or(usize::MAX, |s| s.start);
                let site = Site {
                    skip: Some(n),
                    ..Site::new(file, facts, m.module, at)
                };
                self.resolve_path(site, &m.path, depth)
            },
        )
    }

    /// The first segment of a path.
    fn resolve_head(&self, site: Site, name: &str, depth: usize) -> Res {
        if depth > MAX_DEPTH {
            return Res::Unknown;
        }
        match name {
            "crate" => {
                return Res::Module(ModKey {
                    root: self.key_of(site.file).root,
                    segs: Vec::new(),
                });
            }
            "self" => return Res::Module(self.module_key(site)),
            "super" => {
                let mut key = self.module_key(site);
                return match key.segs.pop() {
                    Some(_) => Res::Module(key),
                    None => Res::Unknown,
                };
            }
            "Self" => return self.self_type(site),
            _ => {}
        }
        if site.facts.generic_at(site.at, name) {
            return Res::Generic;
        }
        if let Some(found) = self.in_scope(site, name, depth) {
            return found;
        }
        if Self::is_child(site, name) {
            return Res::Module(self.module_key(site).child(name));
        }
        if let Some(root) = self.crates().get(name) {
            return Res::Module(ModKey {
                root: root.clone(),
                segs: Vec::new(),
            });
        }
        match self.globbed(site, name, depth) {
            found @ (Res::Item(..) | Res::Module(_) | Res::Assoc(..) | Res::Unknown) => found,
            // An outside crate, unless no manifest says which crates are ours.
            _ if matches!(name, "std" | "core" | "alloc")
                || self.package_of(site.file).is_some() =>
            {
                Res::External
            }
            _ => Res::Unknown,
        }
    }

    /// A bare name: in scope, through a glob, or nothing of this module's.
    fn resolve_bare(&self, site: Site, name: &str) -> Res {
        if let Some(found) = self.in_scope(site, name, 0) {
            return found;
        }
        match self.globbed(site, name, 0) {
            Res::Missing(mut keys, name) => {
                keys.insert(0, self.module_key(site));
                Res::Missing(keys, name)
            }
            found => found,
        }
    }

    fn resolve_path(&self, site: Site, segs: &[String], depth: usize) -> Res {
        let Some((head, rest)) = segs.split_first() else {
            return Res::Unknown;
        };
        let mut cur = self.resolve_head(site, head, depth);
        for seg in rest {
            cur = match cur {
                // `super::super::m`, `self::super::m`.
                Res::Module(key) if seg == "self" => Res::Module(key),
                Res::Module(mut key) if seg == "super" => match key.segs.pop() {
                    Some(_) => Res::Module(key),
                    None => Res::Unknown,
                },
                Res::Module(key) => self.lookup(&key, seg, depth + 1),
                Res::Item(id, Kind::Type | Kind::Trait) => Res::Assoc(id, seg.clone()),
                Res::Generic | Res::External | Res::Unknown => return cur,
                _ => return Res::Unknown,
            };
        }
        cur
    }

    /// A name inside a module a path reached.
    /// A glob cycle back to this module adds no name to it.
    fn lookup(&self, key: &ModKey, name: &str, depth: usize) -> Res {
        let at = (key.clone(), name.to_string());
        if depth > MAX_DEPTH && !self.lookups.borrow().contains_key(&at) {
            return Res::Unknown;
        }
        let pending = Res::Missing(Vec::new(), name.to_string());
        memo(&self.lookups, at, Some(pending), || {
            self.look_in(key, name, depth)
        })
    }

    fn look_in(&self, key: &ModKey, name: &str, depth: usize) -> Res {
        let Some((file, inline)) = self.module_file(key) else {
            // A crate with no root file in the snapshot: its files are its
            // modules.
            return match self.module_file(&key.child(name)) {
                Some((_, inline)) if inline.is_empty() => Res::Module(key.child(name)),
                _ => Res::Unknown,
            };
        };
        let missing = || Res::Missing(vec![key.clone()], name.to_string());
        let Some(facts) = self.facts(&file) else {
            return missing();
        };
        let Some(module) = facts.module_of(&inline) else {
            return missing();
        };
        let site = Site::new(&file, &facts, module, usize::MAX);
        if let Some(found) = self.in_scope(site, name, depth) {
            return found;
        }
        if Self::is_child(site, name) {
            return Res::Module(key.child(name));
        }
        match self.globbed(site, name, depth) {
            Res::Missing(mut keys, name) => {
                keys.insert(0, key.clone());
                Res::Missing(keys, name)
            }
            found => found,
        }
    }

    fn resolve_ty(&self, site: Site, ty: &Ty) -> Res {
        match ty.path.as_slice() {
            _ if ty.returns.is_some() => self.returned(site, ty),
            [s] if s == "Self" => self.self_type(site),
            [name] if site.facts.generic_at(site.at, name) => Res::Generic,
            path => self.unalias(self.resolve_path(site, path, 0)),
        }
    }

    /// A type alias stands for the type it names.
    fn unalias(&self, mut res: Res) -> Res {
        for _ in 0..MAX_DEPTH {
            let Res::Item(id, Kind::Type) = &res else {
                return res;
            };
            let Some(facts) = self.facts(&id.file) else {
                return res;
            };
            let Some(item) = facts.items.iter().find(|i| i.span.start == id.start) else {
                return res;
            };
            let Some(target) = &item.alias else {
                return res;
            };
            let at = Site::new(&id.file, &facts, item.module, item.span.start);
            res = self.resolve_path(at, &target.path, 0);
        }
        Res::Unknown
    }

    /// The type the function a call names declares it returns; a generic
    /// one is whatever the caller asks for.
    fn returned(&self, site: Site, ty: &Ty) -> Res {
        let Res::Item(id, Kind::Fn) = self.resolve_path(site, &ty.path, 0) else {
            return Res::Unknown;
        };
        let held = usize::from(ty.returns == Some(Returns::Unwrapped));
        match self.item_ty(&id, |i| i.returns[held].as_ref()) {
            Some((Res::Generic, _)) | None => Res::Unknown,
            Some((found, _)) => found,
        }
    }

    /// A type an item states (a field's, the one it returns), resolved
    /// where the item stands, and whether it is a trait object.
    fn item_ty(&self, id: &ItemId, ty: impl FnOnce(&Item) -> Option<&Ty>) -> Option<(Res, bool)> {
        let facts = self.facts(&id.file)?;
        let item = facts.items.iter().find(|i| i.span.start == id.start)?;
        let ty = ty(item)?;
        let at = Site::new(&id.file, &facts, item.module, item.span.start);
        Some((self.resolve_ty(at, ty), ty.dynamic))
    }

    /// `Self` where the site stands: the type its `impl` extends, any
    /// implementer inside a trait.
    fn self_type(&self, site: Site) -> Res {
        let Some(block) = site.facts.block_at(site.at) else {
            return Res::Unknown;
        };
        match &block.self_ty {
            Some(ty) if ty.path.as_slice() != ["Self"] => self.resolve_ty(
                Site {
                    module: block.module,
                    at: block.span.start,
                    ..site
                },
                ty,
            ),
            Some(_) => Res::Unknown,
            None => Res::Generic,
        }
    }

    fn block_type(&self, file: &str, facts: &Facts, block: &Block) -> Option<ItemId> {
        memo(
            &self.block_types,
            (file.to_string(), block.span.start),
            None,
            || {
                let site = Site::new(file, facts, block.module, block.span.start);
                match self.resolve_ty(site, block.self_ty.as_ref()?) {
                    Res::Item(id, _) => Some(id),
                    _ => None,
                }
            },
        )
    }

    fn field_type(&self, site: Site, field: &str) -> Option<(Res, bool)> {
        let Res::Item(id, _) = self.self_type(site) else {
            return None;
        };
        self.item_ty(&id, |i| {
            i.fields.iter().find(|(n, _)| n == field).map(|(_, t)| t)
        })
    }

    fn owner(&self, file: &str, facts: &Facts, block: &Block) -> Owner {
        let site = Site::new(file, facts, block.module, block.span.start);
        let id = |ty: &Ty| match self.resolve_ty(site, ty) {
            Res::Item(id, _) => Some(id),
            _ => None,
        };
        let name = |ty: &Ty| ty.path.last().cloned().unwrap_or_default();
        match &block.self_ty {
            Some(ty) => Owner {
                ty: id(ty),
                trait_: block.trait_.as_ref().and_then(id),
                in_trait: false,
                dispatched: block.trait_.is_some(),
                name: name(block.trait_.as_ref().unwrap_or(ty)),
            },
            None => {
                let declared = Some(ItemId {
                    file: file.to_string(),
                    start: block.span.start,
                });
                Owner {
                    ty: declared.clone(),
                    trait_: declared,
                    in_trait: true,
                    dispatched: true,
                    name: block.trait_.as_ref().map(name).unwrap_or_default(),
                }
            }
        }
    }

    fn def_of(&self, file: &str, facts: &Facts, idx: usize) -> RustDef {
        let item = &facts.items[idx];
        let mut key = self.key_of(file);
        key.segs.extend(facts.modules[item.module].iter().cloned());
        RustDef {
            path: file.to_string(),
            key,
            id: Some(ItemId {
                file: file.to_string(),
                start: item.span.start,
            }),
            name: item.name.clone(),
            kind: item.kind,
            span: Some(item.span.clone()),
            owner: item
                .block
                .map(|b| self.owner(file, facts, &facts.blocks[b])),
            package: self.package_of(file),
            private: item.private,
        }
    }

    /// The item a fragment named `name` over `lines` stands for: the one
    /// spanning its first line, or the first below it (a doc comment opens
    /// the fragment). An `impl` block stands for the type it extends.
    fn definition(&self, rel: &str, name: &str, lines: (u32, u32)) -> Option<RustDef> {
        let facts = self.facts(rel)?;
        let named = || {
            facts
                .items
                .iter()
                .enumerate()
                .filter(|(_, i)| i.name == name)
        };
        let found = named()
            .filter(|(_, i)| i.lines.0 <= lines.0 && lines.0 <= i.lines.1)
            .min_by_key(|(_, i)| i.span.len())
            .or_else(|| {
                named()
                    .filter(|(_, i)| i.lines.0 >= lines.0 && i.lines.0 <= lines.1)
                    .min_by_key(|(_, i)| i.lines.0)
            });
        if let Some((idx, _)) = found {
            return Some(self.def_of(rel, &facts, idx));
        }
        let text = self.text(rel)?;
        let start = text
            .split_inclusive('\n')
            .take(lines.0.saturating_sub(1) as usize)
            .map(str::len)
            .sum::<usize>();
        let block = facts.blocks.iter().find(|b| {
            b.span.contains(&start)
                && b.self_ty
                    .as_ref()
                    .is_some_and(|t| t.path.last().is_some_and(|n| n == name))
        })?;
        let id = self.block_type(rel, &facts, block)?;
        let owner = self.facts(&id.file)?;
        let idx = owner.items.iter().position(|i| i.span.start == id.start)?;
        Some(self.def_of(&id.file, &owner, idx))
    }

    /// The names `rel` spells the definition with: its own, and what a
    /// `use … as` renames it to.
    fn spellings(&self, facts: &Facts, def: &RustDef) -> Vec<String> {
        let mut names = vec![def.name.clone()];
        names.extend(
            facts
                .imports
                .iter()
                .filter(|m| m.path.last() == Some(&def.name))
                .filter_map(|m| m.local.clone())
                .filter(|l| *l != def.name),
        );
        names
    }

    fn use_lines(&self, rel: &str, def: &RustDef) -> Vec<u32> {
        if !self
            .text(rel)
            .is_some_and(|t| t.contains(def.name.as_str()))
        {
            return Vec::new();
        }
        let Some(facts) = self.facts(rel) else {
            return Vec::new();
        };
        let names = self.spellings(&facts, def);
        let mut lines: Vec<u32> = facts
            .uses
            .iter()
            .filter(|u| names.contains(&u.name))
            .map(|u| u.line)
            .collect();
        lines.dedup();
        lines
    }

    /// A use of a type inside an `impl` of that type is part of its
    /// definition.
    fn extends(&self, rel: &str, facts: &Facts, at: usize, def: &RustDef) -> bool {
        def.kind == Kind::Type
            && def.id.is_some()
            && facts
                .blocks
                .iter()
                .filter(|b| b.span.contains(&at) && b.self_ty.is_some())
                .any(|b| self.block_type(rel, facts, b) == def.id)
    }

    fn classify(&self, rel: &str, lines: (u32, u32), def: &RustDef) -> Option<Finding> {
        let facts = self.facts(rel)?;
        let names = self.spellings(&facts, def);
        let first = facts.uses.partition_point(|u| u.line < lines.0);
        let mut best = None;
        for u in facts.uses[first..]
            .iter()
            .take_while(|u| u.line <= lines.1)
            .filter(|u| names.contains(&u.name))
        {
            if rel == def.path && def.span.as_ref().is_some_and(|s| s.contains(&u.at)) {
                continue;
            }
            if self.extends(rel, &facts, u.at, def) {
                continue;
            }
            let site = Site::new(rel, &facts, u.module, u.at);
            let evidence = self.evidence(site, u, def);
            if evidence == Evidence::Rejected {
                continue;
            }
            let relation = if u.call {
                bindings::Relation::Call
            } else {
                bindings::Relation::Reference
            };
            absorb(&mut best, evidence, relation, u.line);
        }
        best
    }

    fn evidence(&self, site: Site, u: &Use, def: &RustDef) -> Evidence {
        let member = def.owner.is_some();
        match &u.via {
            Via::Macro if def.kind == Kind::Macro => {
                if site.file == def.path || self.package_of(site.file) == def.package {
                    Evidence::Resolved
                } else {
                    Evidence::Rejected
                }
            }
            Via::Macro => Evidence::Rejected,
            _ if def.kind == Kind::Macro => Evidence::Rejected,
            Via::Method(recv) => self.through(site, recv, u.call, def),
            Via::Bare { local: true } => Evidence::Rejected,
            // A member is reached through its type or a value of it.
            Via::Bare { .. } | Via::Head if member => Evidence::Rejected,
            Via::Bare { .. } => self.judge(self.resolve_bare(site, &u.name), def),
            Via::Head => self.judge(self.resolve_head(site, &u.name, 0), def),
            Via::Path(q) | Via::Implements(q) => {
                let path: Vec<String> = q.iter().cloned().chain([u.name.clone()]).collect();
                self.judge(self.resolve_path(site, &path, 0), def)
            }
        }
    }

    fn judge(&self, res: Res, def: &RustDef) -> Evidence {
        match res {
            Res::Item(id, _) if def.id.as_ref() == Some(&id) => Evidence::Resolved,
            Res::Assoc(owner, name) if name == def.name => match &def.owner {
                Some(o) if o.ty.as_ref() == Some(&owner) => Evidence::Resolved,
                // `Tr::method(&x)`: whichever implementation `x` has.
                Some(o) if o.trait_.as_ref() == Some(&owner) => Evidence::Candidate(RECEIVER),
                _ => Evidence::Rejected,
            },
            Res::Module(key)
                if def.kind == Kind::Mod && self.same_module(&key, &def.key.child(&def.name)) =>
            {
                Evidence::Resolved
            }
            Res::Missing(keys, name)
                if def.id.is_none()
                    && name == def.name
                    && keys.iter().any(|k| self.same_module(k, &def.key)) =>
            {
                Evidence::Resolved
            }
            Res::Generic if def.owner.as_ref().is_some_and(|o| o.trait_.is_some()) => {
                Evidence::Candidate(RECEIVER)
            }
            Res::Unknown => Evidence::Candidate(IMPORT),
            _ => Evidence::Rejected,
        }
    }

    /// A method call through a value: resolved when the syntax types the
    /// value as the method's type, a candidate when nothing types it and
    /// the file names the type (or, for a trait's method, the trait a call
    /// needs in scope), rejected for any other type.
    fn through(&self, site: Site, recv: &Recv, call: bool, def: &RustDef) -> Evidence {
        let Some(owner) = def.owner.as_ref().filter(|_| call) else {
            return Evidence::Rejected;
        };
        let typed = match recv {
            Recv::SelfValue => Some((self.self_type(site), false)),
            Recv::SelfField(f) => self.field_type(site, f),
            Recv::Typed(ty) => Some((self.resolve_ty(site, ty), ty.dynamic)),
            Recv::Untyped => None,
        };
        let sees = site.file == def.path || site.facts.names.contains(&owner.name);
        let candidate = if sees {
            Evidence::Candidate(RECEIVER)
        } else {
            Evidence::Rejected
        };
        match typed {
            // A constant's type is not read.
            Some((Res::Item(_, Kind::Value) | Res::Unknown, _)) | None => candidate,
            Some((Res::Item(id, _), dynamic)) => {
                if owner.ty.as_ref() == Some(&id) && (owner.in_trait || !dynamic) {
                    Evidence::Resolved
                } else if owner.trait_.as_ref() == Some(&id) {
                    Evidence::Candidate(RECEIVER)
                } else if owner.in_trait {
                    candidate
                } else {
                    Evidence::Rejected
                }
            }
            Some((Res::Generic, _)) if owner.trait_.is_some() => Evidence::Candidate(RECEIVER),
            Some((Res::External, _)) if owner.in_trait => candidate,
            Some(_) => Evidence::Rejected,
        }
    }

    /// The files whose items or modules `rel` names.
    fn referenced(&self, rel: &str) -> Rc<FxHashSet<String>> {
        memo(&self.referenced, rel.to_string(), None, || {
            let mut out = FxHashSet::default();
            let Some(facts) = self.facts(rel) else {
                return Rc::new(out);
            };
            let file_of = |res: Res| match res {
                Res::Item(id, _) | Res::Assoc(id, _) => Some(id.file),
                Res::Module(key) => self.module_file(&key).map(|(f, _)| f),
                _ => None,
            };
            for n in 0..facts.imports.len() {
                out.extend(file_of(self.resolve_import(rel, &facts, n, 0)));
            }
            let mut seen: FxHashSet<&[String]> = FxHashSet::default();
            for u in &facts.uses {
                if let Via::Path(q) = &u.via {
                    if seen.insert(q) {
                        let site = Site::new(rel, &facts, u.module, u.at);
                        out.extend(file_of(self.resolve_path(site, q, 0)));
                    }
                }
            }
            Rc::new(out)
        })
    }

    /// Whether a test file reaches a caller's file: it is that file, a
    /// module below it, or names one of its items or modules.
    fn reaches(&self, test: &str, caller: &str) -> bool {
        let (t, c) = (self.key_of(test), self.key_of(caller));
        test == caller
            || (t.root == c.root && t.segs.len() > c.segs.len() && t.segs.starts_with(&c.segs))
            || self.referenced(test).contains(caller)
    }

    /// A definition of `old` at the base, as the head still names it: by
    /// the module it lived in, its type resolved where that module is now.
    fn removed_def(&self, old: &str, before: &Facts, idx: usize) -> RustDef {
        let item = &before.items[idx];
        let mut key = self.key_of(old);
        key.segs.extend(before.modules[item.module].iter().cloned());
        let head = self
            .module_file(&key)
            .and_then(|(file, inline)| Some((self.facts(&file)?, file, inline)));
        let owner = item.block.map(|b| {
            let block = &before.blocks[b];
            match &head {
                Some((facts, file, inline)) => {
                    let module = facts.module_of(inline).unwrap_or(0);
                    let at = Block {
                        span: usize::MAX..usize::MAX,
                        module,
                        self_ty: block.self_ty.clone(),
                        trait_: block.trait_.clone(),
                        generics: block.generics.clone(),
                    };
                    let mut owner = self.owner(file, facts, &at);
                    if owner.in_trait {
                        owner.ty = self.trait_named(file, facts, module, &owner.name);
                        owner.trait_ = owner.ty.clone();
                    }
                    owner
                }
                None => Owner {
                    ty: None,
                    trait_: None,
                    in_trait: block.self_ty.is_none(),
                    dispatched: block.trait_.is_some(),
                    name: block
                        .trait_
                        .as_ref()
                        .or(block.self_ty.as_ref())
                        .and_then(|t| t.path.last().cloned())
                        .unwrap_or_default(),
                },
            }
        });
        RustDef {
            path: head.map_or_else(|| old.to_string(), |(_, file, _)| file),
            key,
            id: None,
            name: item.name.clone(),
            kind: item.kind,
            span: None,
            owner,
            package: self.package_of(old),
            private: item.private,
        }
    }

    /// Whether a method the module no longer declares moved to an `impl` of
    /// its type in another file the range changed.
    fn still_implemented<'p>(&self, def: &RustDef, heads: impl Iterator<Item = &'p str>) -> bool {
        let Some(ty) = def
            .owner
            .as_ref()
            .filter(|o| !o.in_trait)
            .and_then(|o| o.ty.clone())
        else {
            return false;
        };
        heads.filter(|f| is_rust(f)).any(|file| {
            self.facts(file).is_some_and(|facts| {
                facts.items.iter().any(|i| {
                    i.name == def.name
                        && i.kind == def.kind
                        && i.block.is_some_and(|b| {
                            self.block_type(file, &facts, &facts.blocks[b]).as_ref() == Some(&ty)
                        })
                })
            })
        })
    }

    fn trait_named(&self, file: &str, facts: &Facts, module: usize, name: &str) -> Option<ItemId> {
        facts
            .items
            .iter()
            .find(|i| i.kind == Kind::Trait && i.module == module && i.name == name)
            .map(|i| ItemId {
                file: file.to_string(),
                start: i.span.start,
            })
    }

    /// Whether the module still declares the item, or re-exports its name.
    /// A module no file holds is no evidence of a removal.
    fn kept(&self, key: &ModKey, before: &Facts, item: &Item) -> bool {
        let Some((file, inline)) = self.module_file(key) else {
            return true;
        };
        let Some(after) = self.facts(&file) else {
            return false;
        };
        let Some(module) = after.module_of(&inline) else {
            return false;
        };
        let owner_of = |f: &Facts, i: &Item| {
            i.block.and_then(|b| {
                let b = &f.blocks[b];
                b.self_ty
                    .as_ref()
                    .or(b.trait_.as_ref())
                    .and_then(|t| t.path.last().cloned())
            })
        };
        let owner = owner_of(before, item);
        after.items.iter().any(|i| {
            i.name == item.name
                && i.kind == item.kind
                && i.module == module
                && i.scope.is_none()
                && owner_of(&after, i) == owner
        }) || (item.block.is_none()
            && after.imports.iter().any(|m| {
                m.exported
                    && m.module == module
                    && m.scope.is_none()
                    && m.local.as_deref() == Some(item.name.as_str())
            }))
    }
}

/// Whether every module on a Rust file's path is public from its crate
/// root: `pub fn` in a `pub(crate) mod` is crate-internal (#359). A binary
/// crate exports nothing. Read off the snapshot, never the disk (an
/// uncommitted `mod.rs` -> `m.rs` move flipped a commit's verdict).
pub fn module_is_public(source: &Source, path: &str) -> bool {
    if !is_rust(path) {
        return true;
    }
    let read = |p: &Path| source.read_to_string(p);
    let file = Path::new(path);
    let Some(src_dir) = file.ancestors().find(|d| {
        d.file_name().is_some_and(|n| n == "src")
            && d.parent()
                .is_some_and(|c| read(&c.join("Cargo.toml")).is_some())
    }) else {
        return true;
    };
    let Ok(rel_path) = file.strip_prefix(src_dir) else {
        return true;
    };
    let mut segments: Vec<String> = rel_path
        .with_extension("")
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect();
    if segments.last().is_some_and(|s| s == "mod") {
        segments.pop();
    }
    if segments.first().is_some_and(|s| s == "main" || s == "bin") {
        return false;
    }
    if segments == ["lib"] {
        return true;
    }
    let mut parent = src_dir.join("lib.rs");
    for (i, seg) in segments.iter().enumerate() {
        let Some(text) = read(&parent) else {
            return i > 0;
        };
        let declared_pub = text.lines().map(without_attributes).any(|l| {
            l.strip_prefix("pub mod ")
                .and_then(|rest| {
                    rest.trim_start()
                        .trim_start_matches("r#")
                        .strip_prefix(seg.as_str())
                })
                .is_some_and(|after| after.trim_start().starts_with([';', '{']))
                || (l.starts_with("pub use ") && l.contains(&format!("{seg}::")))
        });
        if !declared_pub {
            return false;
        }
        let dir = parent.parent().unwrap_or(src_dir).to_path_buf();
        let base = if parent
            .file_name()
            .is_some_and(|n| n == "lib.rs" || n == "mod.rs")
        {
            dir
        } else {
            dir.join(parent.file_stem().unwrap_or_default())
        };
        let flat = base.join(format!("{seg}.rs"));
        parent = if read(&flat).is_some() {
            flat
        } else {
            base.join(seg).join("mod.rs")
        };
    }
    true
}

fn without_attributes(line: &str) -> &str {
    let mut rest = line.trim_start();
    while let Some(attr) = rest.strip_prefix("#[") {
        let mut depth = 1usize;
        let Some(end) = attr.char_indices().find_map(|(i, c)| {
            match c {
                '[' => depth += 1,
                ']' => depth -= 1,
                _ => {}
            }
            (depth == 0).then_some(i)
        }) else {
            return rest;
        };
        rest = attr[end + 1..].trim_start();
    }
    rest
}

pub(super) struct RustModel<'a> {
    resolver: RustResolver<'a>,
    defs: Table<(String, String, u32), RustDef>,
}

impl<'a> RustModel<'a> {
    pub(super) fn new(root: &'a Path, source: &'a Source, files: FxHashSet<String>) -> Self {
        Self {
            resolver: RustResolver::new(root, source, files),
            defs: Table::new(),
        }
    }
}

const MODELLED: &[Relation] = &[Relation::Callers, Relation::Uses, Relation::Removals];

impl LanguageFacts for RustModel<'_> {
    fn modelled(&self) -> &'static [Relation] {
        MODELLED
    }

    fn covers(&self, path: &str) -> bool {
        is_rust(path)
    }

    fn definition(&self, at: &At) -> Option<DefId> {
        let name = at.frag.symbol_name.as_deref().map(base_symbol)?;
        let lines = (at.frag.start_line(), at.frag.end_line());
        self.defs
            .intern((at.path.to_string(), name.to_string(), lines.0), || {
                self.resolver.definition(at.path, name, lines)
            })
    }

    fn classify(&self, site: &UseSite, def: DefId) -> Option<Finding> {
        self.resolver
            .classify(site.path, site.lines, &self.defs.get(def))
    }

    fn reaches_file(&self, test: &str, caller: &str) -> bool {
        self.resolver.reaches(test, caller)
    }

    fn use_lines(&self, path: &str, def: DefId) -> Option<Vec<u32>> {
        Some(self.resolver.use_lines(path, &self.defs.get(def)))
    }

    /// A private item is named only in its module and the modules below
    /// it; a trait's member is as visible as the trait (#599).
    fn names_from(&self, def: DefId, path: &str) -> bool {
        let def = self.defs.get(def);
        if !def.private || def.owner.as_ref().is_some_and(|o| o.dispatched) {
            return true;
        }
        let key = self.resolver.key_of(path);
        is_rust(path) && key.root == def.key.root && key.segs.starts_with(&def.key.segs)
    }

    fn dispatched(&self, def: DefId) -> Option<String> {
        let def = self.defs.get(def);
        def.owner
            .as_ref()
            .filter(|o| o.dispatched)
            .map(|o| o.name.clone())
    }

    /// An item the base declared that its module no longer declares nor
    /// re-exports (a split's `pub use`, #540). A trait impl's method is
    /// the trait's API, not one of its own.
    fn removed(&self, base: &str, pairs: &[(String, Option<String>)]) -> Option<Vec<RemovedDef>> {
        let r = &self.resolver;
        let mut out = Vec::new();
        for (old, new) in pairs.iter().filter(|(old, _)| is_rust(old)) {
            let Some(before) = r.facts_at(base, old) else {
                continue;
            };
            let at_head = new.as_deref().unwrap_or(old);
            let public_module = module_is_public(r.source, &r.root.join(at_head).to_string_lossy());
            for (idx, item) in before.items.iter().enumerate() {
                let trait_impl = item.block.is_some_and(|b| {
                    before.blocks[b].self_ty.is_some() && before.blocks[b].trait_.is_some()
                });
                if item.scope.is_some()
                    || item.name.len() < 3
                    || item.kind == Kind::Mod
                    || trait_impl
                {
                    continue;
                }
                let mut key = r.key_of(old);
                key.segs.extend(before.modules[item.module].iter().cloned());
                if r.kept(&key, &before, item) {
                    continue;
                }
                let def = r.removed_def(old, &before, idx);
                if r.still_implemented(&def, pairs.iter().filter_map(|(_, new)| new.as_deref())) {
                    continue;
                }
                out.push(RemovedDef {
                    path: old.clone(),
                    name: item.name.clone(),
                    lines: item.lines,
                    public: item.public && public_module,
                    def: self.defs.push(def)?,
                });
            }
        }
        Some(out)
    }
}

/// The lines of the functions in `text` an attribute marks as tests:
/// `#[test]`, `#[tokio::test]` — an inline unit test is a test like any in
/// `tests/` (#528).
pub fn test_functions(text: &str) -> Vec<(u32, u32)> {
    let Some(tree) = crate::parsers::parse_tree("rust", text) else {
        return Vec::new();
    };
    let bytes = text.as_bytes();
    let mut lines = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            match child.kind() {
                "function_item" if marked_test(child, bytes) => {
                    let row = |p: tree_sitter::Point| p.row as u32 + 1;
                    lines.push((row(child.start_position()), row(child.end_position())));
                }
                "mod_item" | "declaration_list" | "impl_item" => stack.push(child),
                _ => {}
            }
        }
    }
    lines
}

fn marked_test(func: Node, bytes: &[u8]) -> bool {
    let mut prev = func.prev_named_sibling();
    while let Some(p) = prev {
        match p.kind() {
            "attribute_item" => {
                let path = p
                    .named_child(0)
                    .and_then(|a| a.named_child(0))
                    .and_then(|n| n.utf8_text(bytes).ok())
                    .unwrap_or("");
                if path.rsplit("::").next() == Some("test") {
                    return true;
                }
            }
            "line_comment" | "block_comment" => {}
            _ => return false,
        }
        prev = p.prev_named_sibling();
    }
    false
}
