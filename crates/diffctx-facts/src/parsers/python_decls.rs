//! What a Python file's syntax declares: with the class and function they sit
//! in, the names a module or a class body assigns, which the fragmenter does not
//! cut. Walked off the tree the fragmenter already parsed when there is one, so
//! the overlay never parses the same Python blob twice.

use std::hash::{Hash, Hasher};
use std::rc::Rc;
use std::sync::{Arc, Mutex, MutexGuard};

use once_cell::sync::Lazy;
use rustc_hash::{FxHashMap, FxHasher};
use tree_sitter::{Node, Tree};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PyDeclKind {
    Function,
    Class,
    Assignment,
}

#[derive(Debug, Clone)]
pub(crate) struct PyDeclaration {
    pub(crate) name: String,
    pub(crate) kind: PyDeclKind,
    pub(crate) lines: (u32, u32),
    pub(crate) class: Option<String>,
    pub(crate) scope: Option<(u32, u32)>,
}

const CACHE_LIMIT: usize = 8192;
type Key = (u64, usize);

static CACHE: Lazy<Mutex<FxHashMap<Key, Arc<[PyDeclaration]>>>> =
    Lazy::new(|| Mutex::new(FxHashMap::default()));

fn key(content: &str) -> Key {
    let mut h = FxHasher::default();
    content.hash(&mut h);
    (h.finish(), content.len())
}

fn lock_cache() -> MutexGuard<'static, FxHashMap<Key, Arc<[PyDeclaration]>>> {
    CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn remember(key: Key, found: Arc<[PyDeclaration]>) {
    let mut cache = lock_cache();
    if cache.len() >= CACHE_LIMIT {
        cache.clear();
    }
    cache.insert(key, found);
}

pub(crate) fn record(tree: &Tree, content: &str) {
    let key = key(content);
    let known = lock_cache().contains_key(&key);
    if known {
        return;
    }
    let found: Arc<[PyDeclaration]> = extract(tree.root_node(), content.as_bytes()).into();
    remember(key, found);
}

pub(crate) fn facts(content: &str) -> Option<Arc<[PyDeclaration]>> {
    let key = key(content);
    let hit = lock_cache().get(&key).cloned();
    if hit.is_some() {
        return hit;
    }
    let tree = super::parse_tree("python", content)?;
    let found: Arc<[PyDeclaration]> = extract(tree.root_node(), content.as_bytes()).into();
    remember(key, Arc::clone(&found));
    Some(found)
}

type Frame<'t> = (
    Node<'t>,
    Option<Node<'t>>,
    Option<Rc<str>>,
    Option<(u32, u32)>,
    Option<Rc<(Rc<str>, Box<str>)>>,
);

fn extract(root: Node, bytes: &[u8]) -> Vec<PyDeclaration> {
    let mut declarations = Vec::new();
    let mut attributes: Vec<(Rc<str>, String, (u32, u32))> = Vec::new();
    let mut stack: Vec<Frame> = vec![(root, None, None, None, None)];
    while let Some((node, parent, class, scope, owner)) = stack.pop() {
        let (mut inner_class, mut inner_scope) = (class.clone(), scope);
        let mut inner_owner = owner.clone();
        let kind_str = node.kind();
        let mut declare = |name: Node, kind: PyDeclKind, lines: (u32, u32)| {
            declarations.push(PyDeclaration {
                name: text(name, bytes).to_string(),
                kind,
                lines,
                class: class.as_deref().map(str::to_string),
                scope,
            });
        };
        match kind_str {
            "function_definition" | "class_definition" => {
                let wrapper = decorated(node, parent);
                let lines = lines_of(wrapper);
                let is_class = kind_str == "class_definition";
                if let Some(name) = node.child_by_field_name("name") {
                    let kind = if is_class {
                        PyDeclKind::Class
                    } else {
                        PyDeclKind::Function
                    };
                    declare(name, kind, lines);
                }
                if is_class {
                    inner_class = node
                        .child_by_field_name("name")
                        .map(|n| Rc::from(text(n, bytes)));
                } else {
                    if scope.is_none() {
                        inner_owner = class
                            .clone()
                            .zip(receiver_parameter(node, wrapper, bytes))
                            .map(|(c, p)| Rc::new((c, p)));
                    }
                    inner_class = None;
                    inner_scope = Some(lines);
                }
            }
            "expression_statement" if scope.is_none() => {
                if let Some(left) = assigned_target(&node) {
                    declare(left, PyDeclKind::Assignment, lines_of(node));
                }
                if !node.has_error() {
                    continue;
                }
            }
            "expression_statement" => {
                let target = assigned_attribute(&node).filter(|(object, _)| {
                    owner
                        .as_ref()
                        .is_some_and(|o| text(*object, bytes) == &*o.1)
                });
                if let (Some((_, attribute)), Some(o)) = (target, &owner) {
                    attributes.push((
                        Rc::clone(&o.0),
                        text(attribute, bytes).to_string(),
                        lines_of(node),
                    ));
                }
                if !node.has_error() {
                    continue;
                }
            }
            "import_statement"
            | "import_from_statement"
            | "future_import_statement"
            | "parameters"
            | "comment"
            | "string"
                if !node.has_error() =>
            {
                continue;
            }
            _ => {}
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if child.named_child_count() > 0
                || matches!(
                    child.kind(),
                    "function_definition" | "class_definition" | "expression_statement"
                )
            {
                stack.push((
                    child,
                    Some(node),
                    inner_class.clone(),
                    inner_scope,
                    inner_owner.clone(),
                ));
            }
        }
    }
    attributes.sort_by_key(|a| a.2);
    for (class, name, lines) in attributes {
        let declared = declarations.iter().any(|d| {
            d.name == name && d.class.as_deref() == Some(class.as_ref()) && d.scope.is_none()
        });
        if !declared {
            declarations.push(PyDeclaration {
                name,
                kind: PyDeclKind::Assignment,
                lines,
                class: Some(class.as_ref().to_string()),
                scope: None,
            });
        }
    }
    declarations
}

fn receiver_parameter(function: Node, wrapper: Node, bytes: &[u8]) -> Option<Box<str>> {
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
    (name.kind() == "identifier").then(|| text(name, bytes).into())
}

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

fn lines_of(node: Node) -> (u32, u32) {
    (
        node.start_position().row as u32 + 1,
        node.end_position().row as u32 + 1,
    )
}

fn decorated<'t>(node: Node<'t>, parent: Option<Node<'t>>) -> Node<'t> {
    parent
        .filter(|p| p.kind() == "decorated_definition")
        .unwrap_or(node)
}

fn assigned_target<'a>(node: &Node<'a>) -> Option<Node<'a>> {
    if node.kind() != "expression_statement" || node.named_child_count() != 1 {
        return None;
    }
    node.named_child(0)
        .filter(|a| a.kind() == "assignment")?
        .child_by_field_name("left")
        .filter(|l| l.kind() == "identifier")
}
