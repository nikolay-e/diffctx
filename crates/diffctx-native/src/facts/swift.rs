//! Swift facts from the alex-pinkus grammar (`tree-sitter-swift`), in the
//! shape of the Kotlin facts: that grammar follows fwcd's, and one binding
//! model (`kotlin_bindings`) reads both (#526).
//!
//! Swift ties a module to its target directory, not to a line of the file,
//! so the package is left to the reader that knows the path, and an `import`
//! names a module: a wildcard import of it. An `extension` is a declaration
//! of its own kind; the members it adds are owned by the type it extends,
//! wherever that type is declared.

use tree_sitter::Node;

use super::kotlin::{
    Binding, Declaration, Import, InitCall, KotlinFacts, Receiver, Use, UseKind, Visibility, child,
    line_of, push_use, squash, string_text, text, walk,
};

/// The facts of one blob; `None` when the grammar is not compiled in or the
/// parse timed out.
pub fn facts(content: &str) -> Option<KotlinFacts> {
    let tree = crate::parsers::parse_tree("swift", content)?;
    Some(extract(tree.root_node(), content.as_bytes()))
}

type Owners = Vec<(String, Option<Visibility>)>;

fn extract(root: Node, bytes: &[u8]) -> KotlinFacts {
    walk(root, Owners::new(), |node, owners, facts| {
        let mut inner = owners.clone();
        match node.kind() {
            "import_declaration" => {
                let module = child(node, "identifier")
                    .and_then(|i| child(i, "simple_identifier"))
                    .and_then(|m| text(m, bytes));
                if let Some(module) = module {
                    facts.imports.push(Import {
                        path: module.to_string(),
                        alias: None,
                        wildcard: true,
                        line: line_of(node),
                    });
                }
                return None;
            }
            "comment" | "multiline_comment" => {
                facts.text.push(node.byte_range());
                return None;
            }
            "line_string_literal" | "multi_line_string_literal" | "raw_string_literal" => {
                string_text(node, &mut facts.text);
            }
            _ => {}
        }
        if let Some(kind) = declaration_kind(&node) {
            if is_local(&node) {
                local_binding(&node, bytes, &mut facts.bindings);
            } else if let Some(decl) = declaration(&node, kind, owners, bytes) {
                if matches!(decl.kind, "class" | "interface" | "extension") {
                    let named = supertypes(&node, bytes);
                    match facts.supertypes.iter_mut().find(|(n, _)| *n == decl.name) {
                        Some((_, known)) => known.extend(named),
                        None => facts.supertypes.push((decl.name.clone(), named)),
                    }
                    inner.extend(
                        decl.owners[owners.len()..]
                            .iter()
                            .chain(std::iter::once(&decl.name))
                            .map(|n| (n.clone(), decl.visibility)),
                    );
                }
                facts.declarations.push(decl);
            }
        }
        bindings_at(&node, bytes, &mut facts.bindings);
        uses_at(&node, bytes, &mut facts.uses);
        Some(inner)
    })
}

fn declaration_kind(node: &Node) -> Option<&'static str> {
    Some(match node.kind() {
        "class_declaration" => match node.child_by_field_name("declaration_kind")?.kind() {
            "extension" => "extension",
            _ => "class",
        },
        "protocol_declaration" => "interface",
        "function_declaration" | "protocol_function_declaration" => "fun",
        "property_declaration" | "protocol_property_declaration" => "property",
        "typealias_declaration" => "typealias",
        _ => return None,
    })
}

/// Anything not declared at the top level or in a type's body is a local:
/// nothing outside its block reaches it.
fn is_local(node: &Node) -> bool {
    !node.parent().is_some_and(|p| {
        matches!(
            p.kind(),
            "source_file" | "class_body" | "enum_class_body" | "protocol_body"
        )
    })
}

/// A declaration's name and, for `extension Outer.Inner`, the types the
/// extended name is nested in.
fn declared_name(node: &Node, bytes: &[u8]) -> Option<(String, Vec<String>)> {
    let name = match node.kind() {
        "property_declaration" | "protocol_property_declaration" => node
            .child_by_field_name("name")
            .and_then(|p| p.child_by_field_name("bound_identifier")),
        _ => node.child_by_field_name("name"),
    }?;
    if name.kind() == "user_type" {
        let mut cursor = name.walk();
        let mut path: Vec<String> = name
            .children(&mut cursor)
            .filter(|c| c.kind() == "type_identifier")
            .filter_map(|c| text(c, bytes).map(str::to_string))
            .collect();
        let last = path.pop()?;
        return Some((last, path));
    }
    Some((text(name, bytes)?.trim_matches('`').to_string(), Vec::new()))
}

fn declaration(
    node: &Node,
    kind: &'static str,
    owners: &Owners,
    bytes: &[u8],
) -> Option<Declaration> {
    let (name, nested_in) = declared_name(node, bytes)?;
    let modifiers = child(*node, "modifiers");
    let visibility = modifiers.and_then(|m| visibility_of(m, bytes));
    let body = node
        .child_by_field_name("body")
        .or_else(|| child(*node, "computed_property"))
        .or_else(|| child(*node, "willset_didset_block"));
    let header_end = body.map_or_else(
        || {
            let mut cursor = node.walk();
            let found = node
                .children(&mut cursor)
                .find(|c| c.kind() == "=")
                .map_or(node.end_byte(), |c| c.start_byte());
            found
        },
        |b| b.start_byte(),
    );
    let mut all_owners: Vec<String> = owners.iter().map(|(n, _)| n.clone()).collect();
    all_owners.extend(nested_in);
    Some(Declaration {
        name,
        kind,
        owners: all_owners,
        receiver: None,
        type_name: declared_type(node, bytes),
        init_call: node
            .child_by_field_name("value")
            .and_then(|v| initializer_call(v, bytes)),
        visibility,
        effective: Some(Visibility::bounded(
            visibility,
            owners.iter().map(|(_, v)| *v),
        )),
        is_override: modifiers.is_some_and(|m| {
            let mut cursor = m.walk();
            let found = m
                .children(&mut cursor)
                .any(|c| c.kind() == "member_modifier" && text(c, bytes) == Some("override"));
            found
        }),
        inner: false,
        abstract_member: node.kind() == "protocol_function_declaration",
        annotations: modifiers.map(|m| attributes(m, bytes)).unwrap_or_default(),
        required_params: None,
        params: None,
        start_line: line_of(*node),
        end_line: node.end_position().row as u32 + 1,
        span: node.byte_range(),
        header: node.start_byte()..header_end,
        signature: squash(bytes, node.start_byte()..header_end),
    })
}

/// `private` and `fileprivate` stop at the file.
fn visibility_of(modifiers: Node, bytes: &[u8]) -> Option<Visibility> {
    let mut cursor = modifiers.walk();
    let found = modifiers
        .children(&mut cursor)
        .find(|c| c.kind() == "visibility_modifier")?;
    let written = text(found, bytes)?;
    // `private(set)` narrows the setter alone.
    if written.contains('(') {
        return None;
    }
    Some(match written.trim() {
        "private" | "fileprivate" => Visibility::Private,
        "internal" | "package" => Visibility::Internal,
        _ => Visibility::Public,
    })
}

fn attributes(modifiers: Node, bytes: &[u8]) -> Vec<String> {
    let mut cursor = modifiers.walk();
    modifiers
        .children(&mut cursor)
        .filter(|c| c.kind() == "attribute")
        .filter_map(|a| child(a, "user_type").and_then(|t| type_name(t, bytes)))
        .collect()
}

fn supertypes(node: &Node, bytes: &[u8]) -> Vec<String> {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .filter(|c| c.kind() == "inheritance_specifier")
        .filter_map(|s| s.child_by_field_name("inherits_from"))
        .filter_map(|t| type_name(t, bytes))
        .collect()
}

/// A property's annotated type or a function's return type.
fn declared_type(node: &Node, bytes: &[u8]) -> Option<String> {
    if let Some(annotation) = child(*node, "type_annotation") {
        return annotation
            .child_by_field_name("name")
            .and_then(|t| type_name(t, bytes));
    }
    let mut cursor = node.walk();
    let children: Vec<Node> = node.children(&mut cursor).collect();
    let arrow = children.iter().position(|c| c.kind() == "->")?;
    children[arrow + 1..]
        .iter()
        .find(|c| c.is_named())
        .and_then(|t| type_name(*t, bytes))
}

/// The type a value of a written type has: `Track`, `Track?`, `Outer.Inner`
/// as its last name, `some Sink` as `Sink`. A collection, a function or a
/// tuple is no type declared here.
fn type_name(node: Node, bytes: &[u8]) -> Option<String> {
    match node.kind() {
        "user_type" => {
            let mut cursor = node.walk();
            let last = node
                .children(&mut cursor)
                .filter(|c| c.kind() == "type_identifier")
                .last()?;
            text(last, bytes).map(str::to_string)
        }
        "optional_type" | "opaque_type" | "existential_type" => {
            node.named_child(0).and_then(|t| type_name(t, bytes))
        }
        _ => None,
    }
}

/// `Make(…)`, `make(…)` or `repo.find(…)`: the call a value with no written
/// type takes its type from.
fn initializer_call(value: Node, bytes: &[u8]) -> Option<InitCall> {
    if value.kind() != "call_expression" {
        return None;
    }
    let head = value.named_child(0)?;
    match head.kind() {
        "simple_identifier" => Some(InitCall {
            receiver: None,
            callee: text(head, bytes)?.to_string(),
        }),
        "navigation_expression" => {
            let receiver = head
                .child_by_field_name("target")
                .filter(|r| r.kind() == "simple_identifier")?;
            Some(InitCall {
                receiver: Some(text(receiver, bytes)?.to_string()),
                callee: member_name(head, bytes)?.to_string(),
            })
        }
        _ => None,
    }
}

/// `f` in `x.f` and `x?.f`.
fn member_name<'a>(navigation: Node, bytes: &'a [u8]) -> Option<&'a str> {
    navigation
        .child_by_field_name("suffix")?
        .child_by_field_name("suffix")
        .filter(|n| n.kind() == "simple_identifier")
        .and_then(|n| text(n, bytes))
}

/// A local `let`/`var`, or a nested `func`, which shadows by name too.
fn local_binding(node: &Node, bytes: &[u8], bindings: &mut Vec<Binding>) {
    let Some(scope) = node.parent().map(|p| p.byte_range()) else {
        return;
    };
    if node.kind() == "function_declaration" {
        if let Some((name, _)) = declared_name(node, bytes) {
            bindings.push(Binding::new(name, None, node.start_byte(), scope));
        }
        return;
    }
    // A local is not in scope in its own initializer.
    let start = node.end_byte();
    let names = node
        .child_by_field_name("name")
        .map(|p| pattern_names(p, bytes))
        .unwrap_or_default();
    let single = names.len() == 1;
    for name in names {
        let mut b = Binding::new(name, None, start, scope.clone());
        if single {
            b.type_name = declared_type(node, bytes);
            if let Some(value) = node.child_by_field_name("value") {
                b.init_call = initializer_call(value, bytes);
                b.alias = (value.kind() == "simple_identifier")
                    .then(|| text(value, bytes).map(str::to_string))
                    .flatten();
            }
        }
        bindings.push(b);
    }
}

/// The names a pattern binds: `x`, `(a, b)`, `.some(let w)`.
fn pattern_names(pattern: Node, bytes: &[u8]) -> Vec<String> {
    if pattern.kind() == "simple_identifier" {
        return text(pattern, bytes)
            .map(str::to_string)
            .into_iter()
            .collect();
    }
    let mut names = Vec::new();
    let mut stack = vec![pattern];
    while let Some(n) = stack.pop() {
        let mut cursor = n.walk();
        for (i, c) in n.children(&mut cursor).enumerate() {
            match c.kind() {
                "simple_identifier"
                    if n.kind() == "pattern"
                        && n.field_name_for_child(i as u32) != Some("type") =>
                {
                    names.extend(text(c, bytes).map(str::to_string));
                }
                "pattern" | "value_binding_pattern" => stack.push(c),
                _ => {}
            }
        }
    }
    names
}

/// Parameters, closure parameters, loop variables and the names an `if`,
/// `guard`, `while`, `case` or `catch` binds.
fn bindings_at(node: &Node, bytes: &[u8], bindings: &mut Vec<Binding>) {
    match node.kind() {
        "parameter" | "lambda_parameter" => {
            let scope = std::iter::successors(node.parent(), Node::parent).find(|p| {
                matches!(
                    p.kind(),
                    "function_declaration"
                        | "protocol_function_declaration"
                        | "init_declaration"
                        | "subscript_declaration"
                        | "lambda_literal"
                )
            });
            let name = node
                .child_by_field_name("name")
                .filter(|n| n.kind() == "simple_identifier")
                .and_then(|n| text(n, bytes));
            if let (Some(scope), Some(name)) = (scope, name) {
                bindings.push(Binding::new(
                    name.to_string(),
                    written_after_colon(node, bytes),
                    scope.start_byte(),
                    scope.byte_range(),
                ));
            }
        }
        "for_statement" => {
            for name in node
                .child_by_field_name("item")
                .map(|p| pattern_names(p, bytes))
                .unwrap_or_default()
            {
                bindings.push(Binding::new(
                    name,
                    None,
                    node.start_byte(),
                    node.byte_range(),
                ));
            }
        }
        "if_statement" | "guard_statement" | "while_statement" => {
            condition_bindings(node, bytes, bindings);
        }
        "switch_entry" | "catch_block" => {
            let mut cursor = node.walk();
            let patterns: Vec<Node> = node
                .children(&mut cursor)
                .filter(|c| matches!(c.kind(), "switch_pattern" | "pattern"))
                .collect();
            for p in patterns {
                let caught = (node.kind() == "catch_block")
                    .then(|| p.child_by_field_name("name"))
                    .flatten()
                    .and_then(|t| type_name(t, bytes));
                for name in pattern_names(p, bytes) {
                    bindings.push(Binding::new(
                        name,
                        caught.clone(),
                        node.start_byte(),
                        node.byte_range(),
                    ));
                }
            }
        }
        _ => {}
    }
}

/// The type written after a parameter's `:`.
fn written_after_colon(node: &Node, bytes: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    let children: Vec<Node> = node.children(&mut cursor).collect();
    let colon = children.iter().position(|c| c.kind() == ":")?;
    children[colon + 1..]
        .iter()
        .find(|c| c.is_named())
        .and_then(|t| type_name(*t, bytes))
}

/// `if let t = maybe`, `guard let t: Track = make()`, `if let maybe`: each
/// name takes what it is bound from. A `guard` binds to the end of its
/// block, the others inside their own statement.
fn condition_bindings(node: &Node, bytes: &[u8], bindings: &mut Vec<Binding>) {
    let scope = if node.kind() == "guard_statement" {
        let Some(block) = node.parent() else {
            return;
        };
        node.end_byte()..block.end_byte()
    } else {
        node.byte_range()
    };
    let mut cursor = node.walk();
    let children: Vec<(Option<&str>, Node)> = node
        .children(&mut cursor)
        .enumerate()
        .map(|(i, c)| (node.field_name_for_child(i as u32), c))
        .collect();
    for (i, (field, c)) in children.iter().enumerate() {
        if *field != Some("bound_identifier") {
            continue;
        }
        let Some(name) = text(*c, bytes) else {
            continue;
        };
        let rest = &children[i + 1..];
        let mut b = Binding::new(name.to_string(), None, c.end_byte(), scope.clone());
        b.type_name = rest
            .first()
            .filter(|(_, n)| n.kind() == "type_annotation")
            .and_then(|(_, n)| n.child_by_field_name("name"))
            .and_then(|t| type_name(t, bytes));
        let value = rest
            .iter()
            .take_while(|(f, _)| *f != Some("bound_identifier"))
            .skip_while(|(_, n)| n.kind() != "=")
            .nth(1)
            .map(|(_, n)| *n);
        match value {
            Some(v) => {
                b.start = v.end_byte();
                b.init_call = initializer_call(v, bytes);
                b.alias = (v.kind() == "simple_identifier")
                    .then(|| text(v, bytes).map(str::to_string))
                    .flatten();
            }
            // `if let maybe`: the outer `maybe`, unwrapped.
            None => b.alias = Some(name.to_string()),
        }
        bindings.push(b);
    }
}

/// The uses a node is the head of, each recorded once.
fn uses_at(node: &Node, bytes: &[u8], uses: &mut Vec<Use>) {
    let mut push = |name: Option<&str>, kind, receiver, at| {
        push_use(uses, name.map(|n| n.trim_matches('`')), kind, receiver, at);
    };
    let called = || {
        node.parent().is_some_and(|p| {
            p.kind() == "call_expression" && p.named_child(0).is_some_and(|c| c.id() == node.id())
        })
    };
    let call_or_access = |called: bool| {
        if called {
            UseKind::Call
        } else {
            UseKind::Access
        }
    };
    match node.kind() {
        "navigation_expression" => {
            let Some(name_node) = node
                .child_by_field_name("suffix")
                .and_then(|s| s.child_by_field_name("suffix"))
                .filter(|n| n.kind() == "simple_identifier")
            else {
                return;
            };
            let receiver = node
                .child_by_field_name("target")
                .map(|r| receiver_of(r, bytes));
            push(
                text(name_node, bytes),
                call_or_access(called()),
                receiver,
                name_node,
            );
        }
        // `.make()`: a member of a type the context gives, never named here.
        "prefix_expression" => {
            let dot = node
                .child_by_field_name("operation")
                .is_some_and(|o| o.kind() == ".");
            if let Some(target) = node
                .child_by_field_name("target")
                .filter(|t| dot && t.kind() == "simple_identifier")
            {
                push(
                    text(target, bytes),
                    call_or_access(called()),
                    Some(Receiver::Expr),
                    target,
                );
            }
        }
        "call_expression" => {
            if let Some(callee) = node
                .named_child(0)
                .filter(|c| c.kind() == "simple_identifier")
            {
                push(text(callee, bytes), UseKind::Call, None, callee);
            }
        }
        "user_type" => {
            let mut cursor = node.walk();
            let path: Vec<Node> = node
                .children(&mut cursor)
                .filter(|c| c.kind() == "type_identifier")
                .collect();
            if let Some((last, outer)) = path.split_last() {
                let receiver = (!outer.is_empty()).then(|| {
                    Receiver::Path(
                        outer
                            .iter()
                            .filter_map(|p| text(*p, bytes).map(str::to_string))
                            .collect(),
                    )
                });
                push(text(*last, bytes), UseKind::TypeUse, receiver, *last);
            }
        }
        "simple_identifier" if is_bare_reference(node) => {
            push(text(*node, bytes), UseKind::Access, None, *node);
        }
        _ => {}
    }
}

/// A plain name read as a value: not a declaration's name, not a member
/// after a dot, not a callee (a call records it), not an argument label.
fn is_bare_reference(node: &Node) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    let mut cursor = parent.walk();
    let field = parent
        .children(&mut cursor)
        .position(|c| c.id() == node.id())
        .and_then(|i| parent.field_name_for_child(i as u32));
    match parent.kind() {
        "navigation_expression" | "prefix_expression" => field == Some("target"),
        "value_argument" | "property_declaration" => field == Some("value"),
        "if_statement" | "guard_statement" | "while_statement" => field == Some("condition"),
        "assignment" => field == Some("result"),
        "statements"
        | "directly_assignable_expression"
        | "control_transfer_statement"
        | "tuple_expression"
        | "array_literal"
        | "dictionary_literal"
        | "additive_expression"
        | "multiplicative_expression"
        | "comparison_expression"
        | "equality_expression"
        | "conjunction_expression"
        | "disjunction_expression"
        | "nil_coalescing_expression"
        | "range_expression"
        | "infix_expression"
        | "postfix_expression"
        | "as_expression"
        | "check_expression"
        | "ternary_expression"
        | "try_expression"
        | "await_expression"
        | "interpolated_expression"
        | "switch_statement"
        | "for_statement" => true,
        _ => false,
    }
}

fn receiver_of(node: Node, bytes: &[u8]) -> Receiver {
    match node.kind() {
        "simple_identifier" => text(node, bytes)
            .map(|t| Receiver::Name(t.to_string()))
            .unwrap_or(Receiver::Expr),
        "self_expression" => Receiver::This,
        // `x!.f()` is `x`'s member.
        "postfix_expression" if unwraps(node) => node
            .child_by_field_name("target")
            .map_or(Receiver::Expr, |t| receiver_of(t, bytes)),
        "call_expression" => node
            .named_child(0)
            .filter(|c| c.kind() == "simple_identifier")
            .and_then(|c| text(c, bytes))
            .map_or(Receiver::Expr, |callee| Receiver::Call(callee.to_string())),
        "navigation_expression" => {
            let mut parts = Vec::new();
            let mut current = node;
            loop {
                let Some(name) = member_name(current, bytes) else {
                    return Receiver::Expr;
                };
                parts.push(name.to_string());
                let mut target = current.child_by_field_name("target");
                while let Some(t) =
                    target.filter(|t| t.kind() == "postfix_expression" && unwraps(*t))
                {
                    target = t.child_by_field_name("target");
                }
                match target {
                    Some(inner) if inner.kind() == "navigation_expression" => current = inner,
                    Some(inner) if inner.kind() == "simple_identifier" => {
                        parts.extend(text(inner, bytes).map(str::to_string));
                        break;
                    }
                    Some(inner) if inner.kind() == "self_expression" => {
                        parts.push("this".to_string());
                        break;
                    }
                    _ => return Receiver::Expr,
                }
            }
            parts.reverse();
            Receiver::Path(parts)
        }
        _ => Receiver::Expr,
    }
}

fn unwraps(postfix: Node) -> bool {
    postfix
        .child_by_field_name("operation")
        .is_some_and(|o| o.kind() == "bang")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = "import Foundation\n\
@testable import Rec\n\
\n\
func lostFree(why: String) { print(why) }\n\
\n\
public final class Track: Base, Sink {\n\
    var name: String = \"\"\n\
    private let other: Other?\n\
    lazy var cache = Cache.make()\n\
    func sourceLost(why: String, nowNs: UInt64) -> Track? { nil }\n\
    func again() {\n\
        sourceLost(why: \"self\", nowNs: 3)\n\
        self.sourceLost(why: \"self\", nowNs: 4)\n\
        other?.sourceLost(why: \"o\", nowNs: 1)\n\
    }\n\
}\n\
\n\
private extension Outer.Inner {\n\
    func summary() -> String { \"x\" }\n\
}\n\
\n\
func check(_ track: Track, them: Track?) {\n\
    track.sourceLost(why: \"x\", nowNs: 2) // sourceLost in a comment\n\
    them!.drain(t0Ns: 1)\n\
    guard let t = them else { return }\n\
    t.drain(t0Ns: 2)\n\
    for x in [track] { x.summary() }\n\
    let s = \"\\(track.name) drain()\"\n\
}\n";

    fn facts_of() -> KotlinFacts {
        facts(SOURCE).expect("swift grammar")
    }

    fn decl<'a>(f: &'a KotlinFacts, name: &str) -> &'a Declaration {
        f.declarations
            .iter()
            .find(|d| d.name == name)
            .unwrap_or_else(|| panic!("{name}: {:?}", f.declarations))
    }

    #[test]
    fn modules_are_wildcard_imports_and_the_package_is_left_to_the_path() {
        let f = facts_of();
        assert!(!f.partial(), "{:?}", f.error_lines);
        assert_eq!(f.package, None);
        let modules: Vec<(&str, bool)> = f
            .imports
            .iter()
            .map(|i| (i.path.as_str(), i.wildcard))
            .collect();
        assert_eq!(modules, [("Foundation", true), ("Rec", true)]);
    }

    #[test]
    fn declarations_carry_owners_types_and_visibility() {
        let f = facts_of();
        assert_eq!(decl(&f, "lostFree").owners, Vec::<String>::new());
        let lost = decl(&f, "sourceLost");
        assert_eq!(lost.owners, ["Track"]);
        assert_eq!(lost.type_name.as_deref(), Some("Track"));
        assert_eq!(lost.effective, Some(Visibility::Internal));
        assert_eq!(decl(&f, "other").effective, Some(Visibility::Private));
        assert_eq!(decl(&f, "other").type_name.as_deref(), Some("Other"));
        assert_eq!(
            decl(&f, "cache").init_call,
            Some(InitCall {
                receiver: Some("Cache".into()),
                callee: "make".into()
            })
        );
        assert_eq!(
            f.supertypes_of("Track"),
            ["Base".to_string(), "Sink".to_string()]
        );
        let ext = decl(&f, "Inner");
        assert_eq!(
            (ext.kind, ext.owners.clone()),
            ("extension", vec!["Outer".to_string()])
        );
        let summary = decl(&f, "summary");
        assert_eq!(summary.owners, ["Outer", "Inner"]);
        assert_eq!(summary.effective, Some(Visibility::Private));
        assert!(f.declarations.iter().all(|d| d.name != "t"), "a local");
    }

    #[test]
    fn uses_carry_receivers_through_optionals_and_self() {
        let f = facts_of();
        let uses = |name: &str| -> Vec<(UseKind, Option<Receiver>)> {
            f.uses
                .iter()
                .filter(|u| u.name == name)
                .map(|u| (u.kind, u.receiver.clone()))
                .collect()
        };
        assert_eq!(
            uses("sourceLost"),
            [
                (UseKind::Call, None),
                (UseKind::Call, Some(Receiver::This)),
                (UseKind::Call, Some(Receiver::Name("other".into()))),
                (UseKind::Call, Some(Receiver::Name("track".into()))),
            ],
            "the comment is no use"
        );
        assert_eq!(
            uses("drain"),
            [
                (UseKind::Call, Some(Receiver::Name("them".into()))),
                (UseKind::Call, Some(Receiver::Name("t".into()))),
            ],
            "a string is no use"
        );
        assert!(uses("lostFree").is_empty());
        assert!(uses("Track").iter().any(|u| u.0 == UseKind::TypeUse));
        assert!(
            uses("name").contains(&(UseKind::Access, Some(Receiver::Name("track".into())))),
            "inside \\(…)"
        );
    }

    #[test]
    fn parameters_and_unwrapped_optionals_bind_with_their_source() {
        let f = facts_of();
        let at = |needle: &str| SOURCE.find(needle).expect(needle);
        let track = f
            .binding_at("track", at("track.sourceLost"))
            .expect("parameter");
        assert_eq!(track.type_name.as_deref(), Some("Track"));
        let t = f.binding_at("t", at("t.drain")).expect("guard binding");
        assert_eq!(t.alias.as_deref(), Some("them"));
        assert!(f.binding_at("t", at("guard let")).is_none());
        assert!(
            f.binding_at("x", at("x.summary")).is_some(),
            "a loop variable"
        );
        assert!(f.binding_at("x", at("let s")).is_none());
    }
}
