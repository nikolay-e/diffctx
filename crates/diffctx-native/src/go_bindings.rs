//! The Go binding model, read off the syntax tree.
//!
//! A package is a directory: a file reaches a package-level name bare from inside its package, and
//! through the name an import binds to the package's path from anywhere
//! else; an unexported name never leaves its package. A method is reached
//! through a value — resolved when the syntax states the value's type (a
//! receiver, a parameter, `var x T`, `x := &pkg.T{}`), a candidate from a
//! file that can see the method's package otherwise. A same-named function
//! of another package, a struct field and a local are never callers.
//!
//! A value of the method's type that reaches a file which never imports the
//! type's package (returned by a third package) is out of reach here.

use std::cell::RefCell;
use std::ops::Range;
use std::path::Path;
use std::rc::Rc;

use rustc_hash::FxHashMap;
use tree_sitter::Node;

use crate::bindings::{Evidence, Finding, Relation, absorb};
use crate::source::Source;

pub fn is_go(path: &str) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("go"))
}

/// A named type as a declaration states it: `*T`, `pkg.T` and `T[int]`
/// all name `T`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TypeName {
    package: Option<String>,
    name: String,
}

#[derive(Debug, Clone)]
enum Via {
    /// `Total(…)`, `Ledger{}`; `local` when the enclosing function declares
    /// the name itself.
    Bare { local: bool },
    /// `x.Name`: `x` a local, with the type its declaration states, or an
    /// imported package.
    Named {
        operand: String,
        local: bool,
        typed: Option<TypeName>,
    },
    /// `f().Name`, `a.b.Name`.
    Expression,
}

#[derive(Debug, Clone)]
struct Use {
    name: String,
    line: u32,
    at: usize,
    via: Via,
    call: bool,
}

#[derive(Debug, Clone)]
struct Declaration {
    name: String,
    /// The type a method or an interface method belongs to.
    receiver: Option<String>,
    span: Range<usize>,
    lines: (u32, u32),
}

struct Import {
    path: String,
    /// An alias, `.` or `_`.
    name: Option<String>,
}

#[derive(Default)]
struct GoFacts {
    package: String,
    imports: Vec<Import>,
    declarations: Vec<Declaration>,
    uses: Vec<Use>,
}

/// A package-level declaration and the package it lives in.
#[derive(Clone, Debug)]
pub struct GoDef {
    path: String,
    package: String,
    decl: Declaration,
}

impl GoDef {
    pub fn name(&self) -> &str {
        &self.decl.name
    }

    pub fn lines(&self) -> (u32, u32) {
        self.decl.lines
    }
}

type Locals = FxHashMap<String, Option<TypeName>>;

fn text<'b>(node: Node, bytes: &'b [u8]) -> &'b str {
    node.utf8_text(bytes).unwrap_or("")
}

fn line_of(node: Node) -> u32 {
    node.start_position().row as u32 + 1
}

fn dir_of(rel: &str) -> &str {
    rel.rsplit_once('/').map_or("", |(d, _)| d)
}

fn type_name(node: Node, bytes: &[u8]) -> Option<TypeName> {
    match node.kind() {
        "type_identifier" => Some(TypeName {
            package: None,
            name: text(node, bytes).to_string(),
        }),
        "qualified_type" => Some(TypeName {
            package: Some(text(node.child_by_field_name("package")?, bytes).to_string()),
            name: text(node.child_by_field_name("name")?, bytes).to_string(),
        }),
        "pointer_type" | "parenthesized_type" => type_name(node.named_child(0)?, bytes),
        "generic_type" => type_name(node.child_by_field_name("type")?, bytes),
        _ => None,
    }
}

/// The type a value expression constructs: `T{}`, `&pkg.T{}`.
fn constructed(node: Node, bytes: &[u8]) -> Option<TypeName> {
    match node.kind() {
        "composite_literal" => type_name(node.child_by_field_name("type")?, bytes),
        "unary_expression" if node.child_by_field_name("operator")?.kind() == "&" => {
            constructed(node.child_by_field_name("operand")?, bytes)
        }
        _ => None,
    }
}

fn named_children(node: Node) -> Vec<Node> {
    (0..node.named_child_count())
        .filter_map(|i| node.named_child(i))
        .collect()
}

fn field_children<'t>(node: Node<'t>, field: &str) -> Vec<Node<'t>> {
    let mut cursor = node.walk();
    node.children_by_field_name(field, &mut cursor).collect()
}

/// The names a function declares — receiver, parameters, results, its
/// body's variables — over the ones it closes over, each with the type its
/// declaration states. A nested function literal declares its own.
fn declared_locals(func: Node, bytes: &[u8], outer: &Locals) -> Locals {
    let mut locals = outer.clone();
    let mut stack = vec![func];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "parameter_declaration"
            | "variadic_parameter_declaration"
            | "var_spec"
            | "const_spec" => {
                let stated = n
                    .child_by_field_name("type")
                    .and_then(|t| type_name(t, bytes));
                let values = n
                    .child_by_field_name("value")
                    .map(named_children)
                    .unwrap_or_default();
                for (i, name) in field_children(n, "name").into_iter().enumerate() {
                    let ty = stated
                        .clone()
                        .or_else(|| values.get(i).and_then(|v| constructed(*v, bytes)));
                    locals.insert(text(name, bytes).to_string(), ty);
                }
            }
            "short_var_declaration" | "range_clause" => {
                let values = n
                    .child_by_field_name("right")
                    .filter(|_| n.kind() == "short_var_declaration")
                    .map(named_children)
                    .unwrap_or_default();
                let left = n.child_by_field_name("left").map(named_children);
                for (i, name) in left.unwrap_or_default().into_iter().enumerate() {
                    if name.kind() == "identifier" {
                        let ty = values.get(i).and_then(|v| constructed(*v, bytes));
                        locals.insert(text(name, bytes).to_string(), ty);
                    }
                }
            }
            _ => {}
        }
        stack.extend(
            named_children(n)
                .into_iter()
                .filter(|c| c.kind() != "func_literal"),
        );
    }
    locals
}

fn is_top_level(node: Node) -> bool {
    let mut up = node.parent();
    while let Some(p) = up {
        match p.kind() {
            "source_file" => return true,
            "function_declaration" | "method_declaration" | "func_literal" => return false,
            _ => up = p.parent(),
        }
    }
    false
}

/// A type named in a method's receiver belongs to the method's declaration.
fn in_receiver(node: Node) -> bool {
    let mut up = node.parent();
    while let Some(p) = up {
        if p.kind() == "parameter_list" {
            return p.parent().is_some_and(|m| {
                m.kind() == "method_declaration" && m.child_by_field_name("receiver") == Some(p)
            });
        }
        up = p.parent();
    }
    false
}

/// A name a declaration introduces, a struct literal's field key and a
/// selector's operand are no bare use.
fn is_bare_use(node: Node, parent_kind: &str) -> bool {
    let declares = matches!(
        parent_kind,
        "function_declaration"
            | "parameter_declaration"
            | "variadic_parameter_declaration"
            | "var_spec"
            | "const_spec"
            | "selector_expression"
            | "labeled_statement"
    );
    let field_key = parent_kind == "literal_element"
        && node.parent().and_then(|p| p.prev_named_sibling()).is_none()
        && node.parent().and_then(|p| p.parent()).map(|k| k.kind()) == Some("keyed_element");
    !declares && !field_key
}

fn is_call(node: Node) -> bool {
    node.parent().is_some_and(|p| {
        p.kind() == "call_expression" && p.child_by_field_name("function") == Some(node)
    })
}

fn declaration(name: &str, receiver: Option<String>, node: Node) -> Declaration {
    Declaration {
        name: name.to_string(),
        receiver,
        span: node.byte_range(),
        lines: (line_of(node), node.end_position().row as u32 + 1),
    }
}

fn extract(root: Node, bytes: &[u8]) -> GoFacts {
    let mut facts = GoFacts::default();
    let mut stack: Vec<(Node, Rc<Locals>)> = vec![(root, Rc::default())];
    while let Some((node, locals)) = stack.pop() {
        let mut inner = Rc::clone(&locals);
        let parent_kind = node.parent().map_or("", |p| p.kind());
        let mut record = |name: Node, via: Via, call: bool| {
            facts.uses.push(Use {
                name: text(name, bytes).to_string(),
                line: line_of(name),
                at: name.start_byte(),
                via,
                call,
            });
        };
        match node.kind() {
            "package_clause" => {
                if let Some(p) = node.named_child(0) {
                    facts.package = text(p, bytes).to_string();
                }
            }
            "import_spec" => {
                if let Some(path) = node.child_by_field_name("path") {
                    facts.imports.push(Import {
                        path: text(path, bytes).trim_matches(['"', '`']).to_string(),
                        name: node
                            .child_by_field_name("name")
                            .map(|n| text(n, bytes).to_string()),
                    });
                }
            }
            "function_declaration" | "method_declaration" | "func_literal" => {
                inner = Rc::new(declared_locals(node, bytes, &locals));
                if let Some(name) = node
                    .child_by_field_name("name")
                    .filter(|_| parent_kind == "source_file")
                {
                    let receiver = node
                        .child_by_field_name("receiver")
                        .and_then(|r| r.named_child(0))
                        .and_then(|p| p.child_by_field_name("type"))
                        .and_then(|t| type_name(t, bytes))
                        .map(|t| t.name);
                    let decl = declaration(text(name, bytes), receiver, node);
                    facts.declarations.push(decl);
                }
            }
            "type_spec" | "type_alias" if is_top_level(node) => {
                if let Some(name) = node.child_by_field_name("name") {
                    let owner = text(name, bytes);
                    facts.declarations.push(declaration(owner, None, node));
                    let methods = node
                        .child_by_field_name("type")
                        .filter(|t| t.kind() == "interface_type")
                        .map(named_children)
                        .unwrap_or_default();
                    for m in methods.into_iter().filter(|m| m.kind() == "method_elem") {
                        if let Some(n) = m.child_by_field_name("name") {
                            let decl = declaration(text(n, bytes), Some(owner.to_string()), m);
                            facts.declarations.push(decl);
                        }
                    }
                }
            }
            "var_spec" | "const_spec" if is_top_level(node) => {
                for name in field_children(node, "name") {
                    facts
                        .declarations
                        .push(declaration(text(name, bytes), None, node));
                }
            }
            "identifier" if is_bare_use(node, parent_kind) => {
                let local = locals.contains_key(text(node, bytes));
                record(node, Via::Bare { local }, is_call(node));
            }
            "type_identifier"
                if !matches!(parent_kind, "type_spec" | "type_alias" | "qualified_type")
                    && !in_receiver(node) =>
            {
                record(node, Via::Bare { local: false }, false);
            }
            "selector_expression" => {
                if let (Some(field), Some(operand)) = (
                    node.child_by_field_name("field"),
                    node.child_by_field_name("operand"),
                ) {
                    let via = if operand.kind() == "identifier" {
                        let name = text(operand, bytes);
                        Via::Named {
                            operand: name.to_string(),
                            local: locals.contains_key(name),
                            typed: locals.get(name).cloned().flatten(),
                        }
                    } else {
                        Via::Expression
                    };
                    record(field, via, is_call(node));
                }
            }
            "qualified_type" if !in_receiver(node) => {
                if let (Some(package), Some(name)) = (
                    node.child_by_field_name("package"),
                    node.child_by_field_name("name"),
                ) {
                    let via = Via::Named {
                        operand: text(package, bytes).to_string(),
                        local: false,
                        typed: None,
                    };
                    record(name, via, false);
                }
            }
            _ => {}
        }
        stack.extend(
            named_children(node)
                .into_iter()
                .map(|c| (c, Rc::clone(&inner))),
        );
    }
    facts.uses.sort_by_key(|u| (u.line, u.at));
    facts
}

/// How one file can reach a definition's package.
struct View {
    same_package: bool,
    aliases: Vec<String>,
    dot: bool,
}

impl View {
    fn evidence(&self, u: &Use, def: &GoDef) -> Evidence {
        let imported = |name: &str| self.aliases.iter().any(|a| a == name);
        match (&def.decl.receiver, &u.via) {
            (None, Via::Bare { local: false }) if self.same_package || self.dot => {
                Evidence::Resolved
            }
            (
                None,
                Via::Named {
                    operand,
                    local: false,
                    ..
                },
            ) if imported(operand) => Evidence::Resolved,
            (None, _) | (Some(_), Via::Bare { .. }) => Evidence::Rejected,
            (
                Some(_),
                Via::Named {
                    operand,
                    local: false,
                    ..
                },
            ) if imported(operand) => Evidence::Rejected,
            (Some(receiver), Via::Named { typed: Some(t), .. })
                if t.name == *receiver
                    && t.package.as_deref().map_or(self.same_package, imported) =>
            {
                Evidence::Resolved
            }
            (Some(_), _) => Evidence::Candidate("receiver_unresolved"),
        }
    }
}

pub struct GoResolver<'a> {
    root: &'a Path,
    source: &'a Source,
    facts: RefCell<FxHashMap<String, Option<Rc<GoFacts>>>>,
    import_paths: RefCell<FxHashMap<String, Option<String>>>,
}

impl<'a> GoResolver<'a> {
    pub fn new(root: &'a Path, source: &'a Source) -> Self {
        Self {
            root,
            source,
            facts: RefCell::default(),
            import_paths: RefCell::default(),
        }
    }

    fn facts(&self, rel: &str) -> Option<Rc<GoFacts>> {
        if let Some(known) = self.facts.borrow().get(rel) {
            return known.clone();
        }
        let read = self
            .source
            .read_to_string(&self.root.join(rel))
            .and_then(|text| {
                let tree = crate::parsers::parse_tree("go", &text)?;
                Some(Rc::new(extract(tree.root_node(), text.as_bytes())))
            });
        self.facts
            .borrow_mut()
            .insert(rel.to_string(), read.clone());
        read
    }

    /// The declaration a fragment of `rel` named `name` starting at `line`
    /// stands for: the one spanning the line, or the first below it (a doc
    /// comment opens the fragment).
    pub fn definition(&self, rel: &str, name: &str, line: u32) -> Option<GoDef> {
        let facts = self.facts(rel)?;
        let named = || facts.declarations.iter().filter(|d| d.name == name);
        let decl = named()
            .find(|d| d.lines.0 <= line && line <= d.lines.1)
            .or_else(|| {
                named()
                    .filter(|d| d.lines.0 >= line)
                    .min_by_key(|d| d.lines.0)
            })?;
        Some(GoDef {
            path: rel.to_string(),
            package: facts.package.clone(),
            decl: decl.clone(),
        })
    }

    /// The package-level declarations `old` held at `base` that no file of
    /// its package at the head still holds, `at` being its head path: a
    /// declaration moved to another file of the package is still there.
    pub fn removed<'f>(
        &self,
        base: &str,
        old: &str,
        at: &str,
        files: impl Iterator<Item = &'f str>,
    ) -> Vec<GoDef> {
        let Some(before) = crate::git::show_file_at_revision(self.root, base, Path::new(old))
            .ok()
            .and_then(|text| {
                let tree = crate::parsers::parse_tree("go", &text)?;
                Some(extract(tree.root_node(), text.as_bytes()))
            })
        else {
            return Vec::new();
        };
        let held: Vec<Rc<GoFacts>> = files
            .filter(|p| is_go(p) && dir_of(p) == dir_of(at))
            .filter_map(|p| self.facts(p))
            .filter(|f| f.package == before.package)
            .collect();
        before
            .declarations
            .into_iter()
            .filter(|d| {
                !matches!(d.name.as_str(), "_" | "init")
                    && !held.iter().any(|f| {
                        f.declarations
                            .iter()
                            .any(|x| x.name == d.name && x.receiver == d.receiver)
                    })
            })
            .map(|decl| GoDef {
                path: at.to_string(),
                package: before.package.clone(),
                // Its bytes are the base's: no use at the head sits inside it.
                decl: Declaration { span: 0..0, ..decl },
            })
            .collect()
    }

    /// The import path of the package in `dir`, from the nearest `go.mod`.
    fn import_path(&self, dir: &str) -> Option<String> {
        if let Some(known) = self.import_paths.borrow().get(dir) {
            return known.clone();
        }
        let mut module_dir = Some(dir);
        let found = loop {
            let Some(d) = module_dir else { break None };
            let module = self
                .source
                .read_to_string(&self.root.join(d).join("go.mod"))
                .and_then(|m| {
                    m.lines()
                        .find_map(|l| l.trim().strip_prefix("module "))
                        .map(|p| {
                            p.split("//")
                                .next()
                                .unwrap_or(p)
                                .trim()
                                .trim_matches('"')
                                .to_string()
                        })
                });
            if let Some(module) = module {
                let suffix = dir[d.len()..].trim_start_matches('/');
                break Some(if suffix.is_empty() {
                    module
                } else {
                    format!("{module}/{suffix}")
                });
            }
            module_dir = (!d.is_empty()).then(|| dir_of(d));
        };
        self.import_paths
            .borrow_mut()
            .insert(dir.to_string(), found.clone());
        found
    }

    /// Whether `import` names the package in `dir`. Without a `go.mod` the
    /// path has to end in the directory.
    fn binds(&self, import: &str, dir: &str) -> bool {
        match self.import_path(dir) {
            Some(path) => import == path,
            None => !dir.is_empty() && (import == dir || import.ends_with(&format!("/{dir}"))),
        }
    }

    fn view(&self, rel: &str, facts: &GoFacts, def: &GoDef) -> Option<View> {
        let dir = dir_of(&def.path);
        let same_package = dir_of(rel) == dir && facts.package == def.package;
        let mut view = View {
            same_package,
            aliases: Vec::new(),
            dot: false,
        };
        for i in facts.imports.iter().filter(|i| self.binds(&i.path, dir)) {
            match i.name.as_deref() {
                Some(".") => view.dot = true,
                Some("_") => {}
                Some(alias) => view.aliases.push(alias.to_string()),
                None => view.aliases.push(def.package.clone()),
            }
        }
        let exported = def.decl.name.starts_with(char::is_uppercase);
        (same_package || (exported && (view.dot || !view.aliases.is_empty()))).then_some(view)
    }

    /// The lines of `rel` where a use could reach `def`.
    pub fn use_lines(&self, rel: &str, def: &GoDef) -> Vec<u32> {
        let Some(facts) = self.facts(rel) else {
            return Vec::new();
        };
        if self.view(rel, &facts, def).is_none() {
            return Vec::new();
        }
        let mut lines: Vec<u32> = facts
            .uses
            .iter()
            .filter(|u| u.name == def.decl.name)
            .map(|u| u.line)
            .collect();
        lines.dedup();
        lines
    }

    /// How the uses inside `lines` of `rel` relate to `def`: the strongest
    /// evidence any of them carries, with the lines that carry it.
    pub fn classify(&self, rel: &str, lines: (u32, u32), def: &GoDef) -> Option<Finding> {
        let facts = self.facts(rel)?;
        let view = self.view(rel, &facts, def)?;
        let mut best = None;
        let first = facts.uses.partition_point(|u| u.line < lines.0);
        for u in facts.uses[first..]
            .iter()
            .take_while(|u| u.line <= lines.1)
            .filter(|u| u.name == def.decl.name)
        {
            if rel == def.path && def.decl.span.contains(&u.at) {
                continue;
            }
            let relation = if u.call {
                Relation::Call
            } else {
                Relation::Reference
            };
            match view.evidence(u, def) {
                Evidence::Rejected => {}
                // Reading `x.Name` through a value nothing types is any
                // struct's field.
                Evidence::Candidate(_) if relation == Relation::Reference => {}
                evidence => absorb(&mut best, evidence, relation, u.line),
            }
        }
        best
    }

    /// Whether `rel` sits in the package of `target` or imports it.
    pub fn sees(&self, rel: &str, target: &str) -> bool {
        let dir = dir_of(target);
        dir_of(rel) == dir
            || self
                .facts(rel)
                .is_some_and(|f| f.imports.iter().any(|i| self.binds(&i.path, dir)))
    }
}
