//! The module-level statements that change what a module variable holds.

use super::{MAX_DEPTH, Walker, is_container};
use tree_sitter::Node;

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

impl<'t> Walker<'t> {
    /// The module-level statements (in any block of the module's own code except
    /// a script's main guard, in none of a function or a class) that change what
    /// a module variable `X` holds: `X[k] = v`, `X.attr = v`, `del X[k]`,
    /// `X += v`, and a call of a mutating method (`pop`, `update`, `append`, …)
    /// on an `X` bound to a dict, list or set. Such a statement is no
    /// definition: it is kept against `X`, and its tree is part of the hashes of
    /// `X`'s definition, so editing it is a change of `X` and its readers are
    /// material. Any other module-level statement stays code outside every
    /// definition.
    pub(super) fn scan_mutations(&mut self, node: Node<'t>, depth: u16) {
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
}
