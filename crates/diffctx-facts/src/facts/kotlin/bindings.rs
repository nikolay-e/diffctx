use std::ops::Range;

use tree_sitter::Node;

use super::{
    Binding, child, declared_name, declared_type, elvis_source, first_child_type, initializer_call,
    last_type_name, name_of, name_text, text,
};

/// What a type test or a cast proves about a name: inside the code the test
/// guards, the name has the tested type (`if (x is Repo) x.load()`).
pub(super) fn narrowings(node: &Node, bytes: &[u8]) -> Vec<Binding> {
    let mut cursor = node.walk();
    let Some(tested) = node
        .children(&mut cursor)
        .find(|c| matches!(c.kind(), "user_type" | "nullable_type"))
    else {
        return Vec::new();
    };
    let is_when_test = node.kind() == "type_test";
    let subject = if is_when_test {
        when_subject(node)
    } else {
        node.named_child(0)
            .filter(|s| s.kind() == "simple_identifier")
    };
    let (Some(subject), Some(type_name)) = (subject, last_type_name(tested, bytes)) else {
        return Vec::new();
    };
    let from = if is_when_test {
        node.start_byte()
    } else {
        subject.end_byte()
    };
    let Some(between) = bytes.get(from..tested.start_byte()) else {
        return Vec::new();
    };
    let operator: String = String::from_utf8_lossy(between)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let Some(name) = name_text(subject, bytes) else {
        return Vec::new();
    };
    let holds: Vec<(usize, usize)> = match (node.kind(), operator.as_str()) {
        ("as_expression", "as") => statement_tail(node).into_iter().collect(),
        ("type_test", "is") => when_entry_scope(node).into_iter().collect(),
        ("check_expression", "is") => flow_scopes(node, bytes, false),
        ("check_expression", "!is") => flow_scopes(node, bytes, true),
        _ => Vec::new(),
    };
    holds
        .into_iter()
        .map(|(start, end)| {
            Binding::new(
                name.to_string(),
                Some(type_name.clone()),
                start,
                node.start_byte()..end,
            )
        })
        .collect()
}

/// The name a `when (x)` tests, when it is a plain one.
fn when_subject<'t>(test: &Node<'t>) -> Option<Node<'t>> {
    let when = std::iter::successors(test.parent(), Node::parent)
        .find(|p| p.kind() == "when_expression")?;
    let subject = child(when, "when_subject")?;
    if child(subject, "variable_declaration").is_some() {
        return None;
    }
    subject
        .named_child(0)
        .filter(|s| s.kind() == "simple_identifier")
}

/// A `when` entry `is T -> …` with that one condition: the entry's body.
fn when_entry_scope(test: &Node) -> Option<(usize, usize)> {
    let condition = test.parent().filter(|c| c.kind() == "when_condition")?;
    let entry = condition.parent().filter(|e| e.kind() == "when_entry")?;
    let mut cursor = entry.walk();
    let conditions = entry
        .children(&mut cursor)
        .filter(|c| c.kind() == "when_condition")
        .count();
    (conditions == 1).then(|| (test.end_byte(), entry.end_byte()))
}

/// The rest of the block after a statement.
fn statement_tail(statement: &Node) -> Option<(usize, usize)> {
    let block = statement.parent().filter(|p| p.kind() == "statements")?;
    Some((statement.end_byte(), block.end_byte()))
}

/// Where `x is T` (or `x !is T`) leaves `x` typed: through the `&&` chain it
/// sits in, the branch an `if` or `while` takes on it, the entry of a
/// subjectless `when`, the rest of the block after `require(x is T)` and
/// after `if (x !is T) return`.
fn flow_scopes(test: &Node, bytes: &[u8], negated: bool) -> Vec<(usize, usize)> {
    let mut at = *test;
    let mut reach = test.end_byte();
    while let Some(parent) = at.parent() {
        match parent.kind() {
            "parenthesized_expression" => {}
            "value_argument" | "value_arguments" | "call_suffix" | "when_condition" if !negated => {
            }
            "conjunction_expression" if !negated => reach = parent.end_byte(),
            "if_expression"
                if parent
                    .child_by_field_name("condition")
                    .is_some_and(|c| c.id() == at.id()) =>
            {
                return branch_scopes(&parent, test, negated);
            }
            "while_statement" if !negated => {
                return child(parent, "control_structure_body")
                    .map(|body| (test.end_byte(), body.end_byte()))
                    .into_iter()
                    .collect();
            }
            "when_entry" if !negated && at.kind() == "when_condition" => {
                return vec![(test.end_byte(), parent.end_byte())];
            }
            "call_expression" if !negated && is_guard_call(&parent, bytes) => {
                return statement_tail(&parent).into_iter().collect();
            }
            _ => break,
        }
        at = parent;
    }
    if reach > test.end_byte() {
        vec![(test.end_byte(), reach)]
    } else {
        Vec::new()
    }
}

/// `require(…)`, `check(…)` and `assert(…)` fail unless the condition holds.
fn is_guard_call(call: &Node, bytes: &[u8]) -> bool {
    call.named_child(0)
        .filter(|callee| callee.kind() == "simple_identifier")
        .and_then(|callee| text(callee, bytes))
        .is_some_and(|name| matches!(name, "require" | "check" | "assert"))
}

/// What a test that is a whole `if` condition proves: in the branch it takes,
/// and, when the other branch ends the block, in the statements after it.
fn branch_scopes(branch: &Node, test: &Node, negated: bool) -> Vec<(usize, usize)> {
    let then = branch.child_by_field_name("consequence");
    let otherwise = branch.child_by_field_name("alternative");
    if !negated {
        return then
            .map(|b| (test.end_byte(), b.end_byte()))
            .into_iter()
            .collect();
    }
    if let Some(other) = otherwise {
        return vec![(other.start_byte(), other.end_byte())];
    }
    match statement_tail(branch) {
        Some(tail) if then.is_some_and(|b| ends_in_jump(&b)) => vec![tail],
        _ => Vec::new(),
    }
}

fn ends_in_jump(body: &Node) -> bool {
    let holder = child(*body, "statements").unwrap_or(*body);
    let mut cursor = holder.walk();
    let last = holder
        .named_children(&mut cursor)
        .filter(|c| !matches!(c.kind(), "line_comment" | "multiline_comment"))
        .last();
    last.is_some_and(|l| l.kind() == "jump_expression")
}

/// A local `val`/`var` (or a local `fun`, which shadows by name too).
pub(super) fn local_binding(node: &Node, bytes: &[u8], bindings: &mut Vec<Binding>) {
    let Some(name) = declared_name(node, bytes) else {
        return;
    };
    let Some(scope) = enclosing_scope(node) else {
        return;
    };
    // A local `val` is not in scope in its own initializer, so
    // `val p = p(…)` calls the outer `p`; a local `fun` is, for recursion.
    let start = if node.kind() == "property_declaration" {
        node.end_byte()
    } else {
        node.start_byte()
    };
    bindings.push(Binding {
        init_call: initializer_call(node, bytes),
        alias: elvis_source(node, bytes),
        ..Binding::new(name, declared_type(node, bytes), start, scope)
    });
}

/// Parameters, lambda parameters, loop and `catch` variables.
pub(super) fn parameter_bindings(node: &Node, bytes: &[u8], bindings: &mut Vec<Binding>) {
    let scope = match node.kind() {
        "parameter" => std::iter::successors(node.parent(), Node::parent)
            .find(|p| {
                matches!(
                    p.kind(),
                    "function_declaration" | "secondary_constructor" | "anonymous_function"
                )
            })
            .map(|f| f.byte_range()),
        "lambda_parameters" => node.parent().map(|l| l.byte_range()),
        _ => Some(node.byte_range()),
    };
    let Some(scope) = scope else {
        return;
    };
    let mut push = |n: Node| {
        if let Some(name) = name_of(&n, bytes) {
            bindings.push(Binding::new(
                name,
                first_child_type(&n, bytes),
                scope.start,
                scope.clone(),
            ));
        }
    };
    match node.kind() {
        "parameter" => push(*node),
        _ => {
            let mut cursor = node.walk();
            let vars: Vec<Node> = node
                .children(&mut cursor)
                .filter(|c| c.kind() == "variable_declaration")
                .collect();
            for v in vars {
                push(v);
            }
            if node.kind() == "catch_block" {
                let mut cursor = node.walk();
                if let Some(id) = node
                    .children(&mut cursor)
                    .find(|c| c.kind() == "simple_identifier")
                {
                    if let Some(name) = name_text(id, bytes) {
                        bindings.push(Binding::new(
                            name.to_string(),
                            first_child_type(node, bytes),
                            scope.start,
                            scope.clone(),
                        ));
                    }
                }
            }
        }
    }
}

fn enclosing_scope(node: &Node) -> Option<Range<usize>> {
    std::iter::successors(node.parent(), Node::parent)
        .find(|p| {
            matches!(
                p.kind(),
                "statements"
                    | "function_body"
                    | "lambda_literal"
                    | "control_structure_body"
                    | "anonymous_initializer"
            )
        })
        .map(|s| s.byte_range())
}
