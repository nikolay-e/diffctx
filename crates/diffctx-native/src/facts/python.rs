//! What a Python file's syntax states: its declarations, with the class and
//! function they sit in, and the lines each identifier is spelled on. A
//! name a module or a class body assigns (`TABLE: dict = {…}`, `valued_at:
//! Mapped[str] = …`) is declared there like a `def`, and read elsewhere.

use std::rc::Rc;
use std::sync::{Arc, Mutex};

use once_cell::sync::Lazy;
use rustc_hash::FxHashMap;
use tree_sitter::Node;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclKind {
    Function,
    Class,
    Assignment,
}

#[derive(Debug, Clone)]
pub struct Declaration {
    pub name: String,
    pub kind: DeclKind,
    /// Decorators included.
    pub lines: (u32, u32),
    /// The class whose body declares it: a method, a field.
    pub class: Option<String>,
    /// The function it is local to: only that function's lines reach it.
    pub scope: Option<(u32, u32)>,
}

/// One step of a path an expression builds from `__file__`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Up,
    Down(String),
}

#[derive(Debug, Default)]
pub struct PythonFacts {
    /// What each `sys.path.insert(…)` or `sys.path.append(…)` adds, as steps
    /// from the file itself; `None` for an expression the syntax cannot
    /// evaluate.
    pub sys_path: Vec<Option<Vec<Step>>>,
    pub declarations: Vec<Declaration>,
    /// Every identifier the code spells (never a string or a comment), by
    /// name, with the lines it is spelled on in order.
    names: FxHashMap<String, Vec<u32>>,
}

impl PythonFacts {
    pub fn lines_naming(&self, name: &str) -> &[u32] {
        self.names.get(name).map_or(&[], Vec::as_slice)
    }

    /// The declaration named `name` a fragment spanning `lines` stands for:
    /// the first one opening inside it, else the innermost one around it (a
    /// chunk of a long function).
    pub fn declaration(&self, name: &str, lines: (u32, u32)) -> Option<&Declaration> {
        let named = || self.declarations.iter().filter(|d| d.name == name);
        named()
            .filter(|d| lines.0 <= d.lines.0 && d.lines.0 <= lines.1)
            .min_by_key(|d| d.lines.0)
            .or_else(|| {
                named()
                    .filter(|d| d.lines.0 <= lines.0 && d.lines.1 >= lines.1)
                    .max_by_key(|d| d.lines.0)
            })
    }
}

const CACHE_LIMIT: usize = 8192;

static CACHE: Lazy<Mutex<FxHashMap<u64, Arc<PythonFacts>>>> =
    Lazy::new(|| Mutex::new(FxHashMap::default()));

fn key(content: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = rustc_hash::FxHasher::default();
    content.hash(&mut h);
    h.finish()
}

/// The facts of one blob, parsed once per run whoever asks first. `None`
/// when the grammar is not compiled in or the parse timed out.
pub fn facts(content: &str) -> Option<Arc<PythonFacts>> {
    let key = key(content);
    if let Some(hit) = CACHE.lock().ok().and_then(|c| c.get(&key).cloned()) {
        return Some(hit);
    }
    let tree = crate::parsers::parse_tree("python", content)?;
    let facts = Arc::new(extract(tree.root_node(), content.as_bytes()));
    if let Ok(mut cache) = CACHE.lock() {
        if cache.len() >= CACHE_LIMIT {
            cache.clear();
        }
        cache.insert(key, Arc::clone(&facts));
    }
    Some(facts)
}

fn extract(root: Node, bytes: &[u8]) -> PythonFacts {
    let mut facts = PythonFacts::default();
    let mut bound: FxHashMap<String, Node> = FxHashMap::default();
    let mut added: Vec<Node> = Vec::new();
    // `self.x = …` in a method: (class, x, lines), the first of each pair
    // declaring it unless the class body or a method already does.
    let mut attributes: Vec<(String, String, (u32, u32))> = Vec::new();
    let mut stack: Vec<Frame> = vec![(root, None, None, None)];
    while let Some((node, class, scope, owner)) = stack.pop() {
        let (mut inner_class, mut inner_scope) = (class.clone(), scope);
        let mut inner_owner = owner.clone();
        let mut declare = |name: Node, kind: DeclKind, lines: (u32, u32)| {
            facts.declarations.push(Declaration {
                name: text(name, bytes).to_string(),
                kind,
                lines,
                class: class.clone(),
                scope,
            });
        };
        match node.kind() {
            "function_definition" | "class_definition" => {
                let lines = lines_of(decorated(node));
                let is_class = node.kind() == "class_definition";
                if let Some(name) = node.child_by_field_name("name") {
                    let kind = if is_class {
                        DeclKind::Class
                    } else {
                        DeclKind::Function
                    };
                    declare(name, kind, lines);
                }
                if is_class {
                    inner_class = node
                        .child_by_field_name("name")
                        .map(|n| text(n, bytes).to_string());
                } else {
                    if scope.is_none() {
                        inner_owner = class
                            .clone()
                            .zip(receiver_parameter(node, bytes))
                            .map(|(c, p)| Rc::new((c, p)));
                    }
                    inner_class = None;
                    inner_scope = Some(lines);
                }
            }
            "expression_statement" if scope.is_none() => {
                if let Some(left) = assigned_target(&node) {
                    declare(left, DeclKind::Assignment, lines_of(node));
                    if let Some(right) = node
                        .named_child(0)
                        .and_then(|a| a.child_by_field_name("right"))
                        .filter(|_| class.is_none())
                    {
                        bound.insert(text(left, bytes).to_string(), right);
                    }
                }
            }
            "expression_statement" => {
                let target = assigned_attribute(&node).filter(|(object, _)| {
                    owner.as_ref().is_some_and(|o| text(*object, bytes) == o.1)
                });
                if let (Some((_, attribute)), Some(o)) = (target, &owner) {
                    attributes.push((
                        o.0.clone(),
                        text(attribute, bytes).to_string(),
                        lines_of(node),
                    ));
                }
            }
            "call" => {
                let function = node.child_by_field_name("function");
                let at = match function.map(|f| text(f, bytes)) {
                    Some("sys.path.insert") => Some(1),
                    Some("sys.path.append") => Some(0),
                    _ => None,
                };
                if let Some(arg) = at.and_then(|i| arguments(node).into_iter().nth(i)) {
                    added.push(arg);
                }
            }
            "identifier" => {
                facts
                    .names
                    .entry(text(node, bytes).to_string())
                    .or_default()
                    .push(line_of(node));
            }
            _ => {}
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push((child, inner_class.clone(), inner_scope, inner_owner.clone()));
        }
    }
    attributes.sort_by_key(|a| a.2);
    for (class, name, lines) in attributes {
        let declared = facts.declarations.iter().any(|d| {
            d.name == name && d.class.as_deref() == Some(class.as_str()) && d.scope.is_none()
        });
        if !declared {
            facts.declarations.push(Declaration {
                name,
                kind: DeclKind::Assignment,
                lines,
                class: Some(class),
                scope: None,
            });
        }
    }
    facts.sys_path = added
        .into_iter()
        .map(|arg| steps(arg, bytes, &bound, 0))
        .collect();
    for lines in facts.names.values_mut() {
        lines.sort_unstable();
        lines.dedup();
    }
    facts
}

/// A node, its class, the function it is local to, and the method's class
/// and receiver name when it sits in a method.
type Frame<'t> = (
    Node<'t>,
    Option<String>,
    Option<(u32, u32)>,
    Option<Rc<(String, String)>>,
);

/// The first parameter of a method, the instance (or class) it reaches its
/// attributes through; a `@staticmethod` has none.
fn receiver_parameter(function: Node, bytes: &[u8]) -> Option<String> {
    let wrapper = decorated(function);
    let mut cursor = wrapper.walk();
    let is_static = wrapper
        .children(&mut cursor)
        .any(|c| c.kind() == "decorator" && text(c, bytes).trim() == "@staticmethod");
    if is_static {
        return None;
    }
    let first = function.child_by_field_name("parameters")?.named_child(0)?;
    let name = match first.kind() {
        "identifier" => first,
        "typed_parameter" => first.named_child(0)?,
        _ => return None,
    };
    (name.kind() == "identifier").then(|| text(name, bytes).to_string())
}

/// `obj.x = …` as a whole statement: `obj` and `x`.
fn assigned_attribute<'a>(node: &Node<'a>) -> Option<(Node<'a>, Node<'a>)> {
    if node.named_child_count() != 1 {
        return None;
    }
    let left = node
        .named_child(0)
        .filter(|a| a.kind() == "assignment")?
        .child_by_field_name("left")
        .filter(|l| l.kind() == "attribute")?;
    let object = left
        .child_by_field_name("object")
        .filter(|o| o.kind() == "identifier")?;
    Some((object, left.child_by_field_name("attribute")?))
}

fn arguments(call: Node) -> Vec<Node> {
    let Some(args) = call.child_by_field_name("arguments") else {
        return Vec::new();
    };
    let mut cursor = args.walk();
    args.named_children(&mut cursor)
        .filter(|a| a.kind() != "comment")
        .collect()
}

/// The path `node` builds from `__file__`: `Path(__file__).resolve().parent
/// / "src"`, `os.path.join(os.path.dirname(__file__), "..")`, a module-level
/// name bound to one. `None` for anything else.
fn steps(
    node: Node,
    bytes: &[u8],
    bound: &FxHashMap<String, Node>,
    depth: u8,
) -> Option<Vec<Step>> {
    if depth > 8 {
        return None;
    }
    let inner = |n: Node| steps(n, bytes, bound, depth + 1);
    let up = |n: Node, by: usize| {
        let mut s = inner(n)?;
        s.extend(std::iter::repeat_n(Step::Up, by));
        Some(s)
    };
    match node.kind() {
        "identifier" => match text(node, bytes) {
            "__file__" => Some(Vec::new()),
            name => inner(*bound.get(name)?),
        },
        "parenthesized_expression" => inner(node.named_child(0)?),
        "attribute" => {
            let attribute = text(node.child_by_field_name("attribute")?, bytes);
            (attribute == "parent").then_some(())?;
            up(node.child_by_field_name("object")?, 1)
        }
        "subscript" => {
            let value = node.child_by_field_name("value")?;
            let n: usize = text(node.child_by_field_name("subscript")?, bytes)
                .parse()
                .ok()?;
            let parents = value.kind() == "attribute"
                && text(value.child_by_field_name("attribute")?, bytes) == "parents";
            parents.then_some(())?;
            up(value.child_by_field_name("object")?, n + 1)
        }
        "binary_operator" => {
            (text(node.child_by_field_name("operator")?, bytes) == "/").then_some(())?;
            let mut s = inner(node.child_by_field_name("left")?)?;
            s.extend(segments(node.child_by_field_name("right")?, bytes)?);
            Some(s)
        }
        "call" => {
            let function = node.child_by_field_name("function")?;
            let args = arguments(node);
            match text(function, bytes) {
                "Path" | "pathlib.Path" | "str" | "os.fspath" | "os.path.abspath"
                | "os.path.realpath" | "os.path.normpath" => inner(*args.first()?),
                "os.path.dirname" => up(*args.first()?, 1),
                "os.path.join" => {
                    let (first, rest) = args.split_first()?;
                    let mut s = inner(*first)?;
                    for a in rest {
                        s.extend(segments(*a, bytes)?);
                    }
                    Some(s)
                }
                _ => {
                    let method = function
                        .child_by_field_name("attribute")
                        .map(|a| text(a, bytes));
                    (matches!(method, Some("resolve" | "absolute")) && args.is_empty())
                        .then_some(())?;
                    inner(function.child_by_field_name("object")?)
                }
            }
        }
        _ => None,
    }
}

/// A plain string literal as path steps: `"../src"`.
fn segments(node: Node, bytes: &[u8]) -> Option<Vec<Step>> {
    if node.kind() != "string" {
        return None;
    }
    let mut cursor = node.walk();
    let mut content = String::new();
    for part in node.named_children(&mut cursor) {
        match part.kind() {
            "string_content" => content.push_str(text(part, bytes)),
            "string_start" | "string_end" => {}
            _ => return None,
        }
    }
    Some(
        content
            .split('/')
            .filter(|p| !p.is_empty() && *p != ".")
            .map(|p| {
                if p == ".." {
                    Step::Up
                } else {
                    Step::Down(p.to_string())
                }
            })
            .collect(),
    )
}

fn text<'b>(node: Node, bytes: &'b [u8]) -> &'b str {
    node.utf8_text(bytes).unwrap_or("")
}

fn line_of(node: Node) -> u32 {
    node.start_position().row as u32 + 1
}

fn lines_of(node: Node) -> (u32, u32) {
    (line_of(node), node.end_position().row as u32 + 1)
}

fn decorated(node: Node) -> Node {
    node.parent()
        .filter(|p| p.kind() == "decorated_definition")
        .unwrap_or(node)
}

/// `X = …`, `X: T = …` or a bare `X: T` as a whole statement: the name it
/// binds. A tuple, an attribute or a subscript target binds none.
fn assigned_target<'a>(node: &Node<'a>) -> Option<Node<'a>> {
    if node.kind() != "expression_statement" || node.named_child_count() != 1 {
        return None;
    }
    node.named_child(0)
        .filter(|a| a.kind() == "assignment")?
        .child_by_field_name("left")
        .filter(|l| l.kind() == "identifier")
}
