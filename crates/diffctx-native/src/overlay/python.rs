//! What a Python file's syntax declares: with the class and function they sit
//! in, the names a module or a class body assigns, which the fragmenter does not
//! cut. A name a module or a class body assigns (`TABLE: dict = {…}`, `valued_at:
//! Mapped[str] = …`) is declared there like a `def`, and read elsewhere.

use std::rc::Rc;
use std::sync::{Arc, Mutex};

use once_cell::sync::Lazy;
use rustc_hash::FxHashMap;
use tree_sitter::Node;

use super::{DeclKind, Declaration};

#[derive(Debug, Default)]
pub(super) struct PythonFacts {
    pub(super) declarations: Vec<Declaration>,
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
#[allow(
    clippy::disallowed_methods,
    reason = "a poisoned cache lock is a miss: the blob is parsed again"
)]
pub(super) fn facts(content: &str) -> Option<Arc<PythonFacts>> {
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
