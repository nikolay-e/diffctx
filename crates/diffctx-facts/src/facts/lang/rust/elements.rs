//! What a closure parameter or loop variable holds: the element of the sequence it
//! walks, where a local's declaration says what that sequence is made of.

use std::collections::HashMap;

use tree_sitter::Node;

use super::{Cx, Ty, Walker, path_of};
use crate::facts::LocalKind;

/// Methods whose closure arguments take the element as their first parameter.
const ELEMENT_METHODS: &str = "any all find position map filter filter_map flat_map for_each \
    take_while skip_while inspect is_some_and is_none_or and_then";

/// Methods that give back the elements they were called on.
const ELEMENT_STEPS: &str = "iter iter_mut into_iter rev skip take filter cloned";

/// Types whose first type argument is their element.
const SEQUENCES: &str = "Vec VecDeque HashSet BTreeSet Option";

fn listed(words: &str, word: &str) -> bool {
    words.split(' ').any(|w| w == word)
}

impl<'t> Walker<'t> {
    /// A closure's parameters and body; `element` is what its first parameter holds when
    /// it states no type.
    pub(super) fn closure(&mut self, node: Node<'t>, cx: &Cx, depth: u16, element: Option<Ty>) {
        let mut element = element;
        self.locals.push(HashMap::new());
        if let Some(parameters) = node.child_by_field_name("parameters") {
            let mut cursor = parameters.walk();
            let list: Vec<Node<'t>> = parameters.named_children(&mut cursor).collect();
            for parameter in list {
                let unstated = element.take();
                if parameter.kind() == "parameter" {
                    let ty = parameter.child_by_field_name("type");
                    if let Some(ty) = ty {
                        self.type_refs(ty, cx, depth + 1);
                    }
                    let known = ty.and_then(|t| self.ty_of(t, cx));
                    if let Some(pattern) = parameter.child_by_field_name("pattern") {
                        self.pattern(pattern, known, cx, LocalKind::Parameter, depth + 1);
                    }
                } else {
                    self.pattern(parameter, unstated, cx, LocalKind::Parameter, depth + 1);
                }
            }
        }
        if let Some(ret) = node.child_by_field_name("return_type") {
            self.type_refs(ret, cx, depth + 1);
        }
        if let Some(body) = node.child_by_field_name("body") {
            self.expr(body, cx, depth + 1);
        }
        self.locals.pop();
    }

    /// The element a closure argument of the method called through `function` holds.
    pub(super) fn closure_element(&self, function: Node<'t>) -> Option<Ty> {
        let method = function
            .child_by_field_name("field")
            .map(|n| self.ident(n))?;
        if function.kind() != "field_expression" || !listed(ELEMENT_METHODS, method) {
            return None;
        }
        self.element_of(function.child_by_field_name("value")?)
    }

    /// The type of the elements a value walks: a local declared as a sequence, or what
    /// `iter()` and the adapters that keep the elements make of one.
    pub(super) fn element_of(&self, node: Node<'t>) -> Option<Ty> {
        match node.kind() {
            "identifier" => self
                .locals
                .iter()
                .rev()
                .find_map(|scope| scope.get(self.ident(node)))
                .and_then(|(_, element)| element.clone()),
            "reference_expression" => self.element_of(node.child_by_field_name("value")?),
            "parenthesized_expression" => self.element_of(node.named_child(0)?),
            "call_expression" => {
                let function = node.child_by_field_name("function")?;
                let step = function
                    .child_by_field_name("field")
                    .map(|n| self.ident(n))?;
                if function.kind() != "field_expression" || !listed(ELEMENT_STEPS, step) {
                    return None;
                }
                self.element_of(function.child_by_field_name("value")?)
            }
            _ => None,
        }
    }

    /// Records the element type of the local a pattern binds, from the type it was declared with.
    pub(super) fn bind_elements(&mut self, pattern: Node<'t>, declared: Option<Node<'t>>, cx: &Cx) {
        let Some(element) = declared.and_then(|d| self.element_ty(d, cx)) else {
            return;
        };
        let name = match pattern.kind() {
            "identifier" => pattern,
            "mut_pattern" => match pattern.named_child(0) {
                Some(inner) if inner.kind() == "identifier" => inner,
                _ => return,
            },
            _ => return,
        };
        let name = self.ident(name);
        if let Some(local) = self.locals.last_mut().and_then(|scope| scope.get_mut(name)) {
            local.1 = Some(element);
        }
    }

    /// The element of a declared slice, array, `Vec`, set or `Option`.
    fn element_ty(&self, node: Node<'t>, cx: &Cx) -> Option<Ty> {
        match node.kind() {
            "reference_type" => self.element_ty(node.child_by_field_name("type")?, cx),
            "array_type" => self.ty_of(node.child_by_field_name("element")?, cx),
            "generic_type" => {
                let head = path_of(self.bytes, node.child_by_field_name("type")?)?;
                if !head.last().is_some_and(|last| listed(SEQUENCES, last)) {
                    return None;
                }
                let arguments = node.child_by_field_name("type_arguments")?;
                self.ty_of(arguments.named_child(0)?, cx)
            }
            _ => None,
        }
    }
}
