//! Java facts from `tree-sitter-java`, in the shape of the Kotlin facts, so
//! one binding model (`kotlin_bindings`) reads the JVM's two languages
//! (#335).
//!
//! An anonymous class is a type of its own, named for what it instantiates
//! (`new Service`): its members are reached through that supertype, never by
//! a name of their own. An unmarked declaration is package-private, which
//! the Kotlin shape spells `internal`. Java declares no function or value
//! outside a class, so every declaration is one, local classes included.

use std::ops::Range;

use tree_sitter::Node;

use super::kotlin::{
    Binding, Declaration, Import, InitCall, KotlinFacts, Receiver, Use, UseKind, Visibility, child,
    line_of, push_use, squash, text, walk,
};

pub fn facts(content: &str) -> Option<KotlinFacts> {
    let tree = crate::parsers::parse_tree("java", content)?;
    Some(extract(tree.root_node(), content.as_bytes()))
}

/// The types around a node: each name, its visibility, and whether it is an
/// interface, whose members are public unless they say otherwise.
type Owners = Vec<(String, Option<Visibility>, bool)>;

fn extract(root: Node, bytes: &[u8]) -> KotlinFacts {
    walk(root, Owners::new(), |node, owners, facts| {
        let mut inner = owners.clone();
        match node.kind() {
            "package_declaration" => {
                facts.package = node.named_child(0).map(|n| dotted(n, bytes));
                return None;
            }
            "import_declaration" => {
                if let Some(path) = node
                    .named_children(&mut node.walk())
                    .find(|c| matches!(c.kind(), "scoped_identifier" | "identifier"))
                    .map(|n| dotted(n, bytes))
                {
                    facts.imports.push(Import {
                        path,
                        alias: None,
                        wildcard: child(node, "asterisk").is_some(),
                        line: line_of(node),
                    });
                }
                return None;
            }
            "line_comment" | "block_comment" | "string_literal" | "text_block"
            | "character_literal" => {
                facts.text.push(node.byte_range());
                return None;
            }
            _ => {}
        }
        if node.kind() == "formal_parameter"
            && node
                .parent()
                .and_then(|p| p.parent())
                .is_some_and(|r| r.kind() == "record_declaration")
        {
            facts
                .declarations
                .extend(record_component(&node, owners, bytes));
        }
        // A local or anonymous class is a type like any other: its members
        // are its own, and they see the instance around it.
        if let Some(kind) = declaration_kind(&node) {
            for decl in declarations(&node, kind, owners, bytes) {
                if decl.is_type() {
                    facts
                        .supertypes
                        .push((decl.name.clone(), supertypes(&node, bytes)));
                    inner.push((decl.name.clone(), decl.visibility, kind == "interface"));
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
        "class_declaration" | "enum_declaration" | "record_declaration" => "class",
        "interface_declaration" | "annotation_type_declaration" => "interface",
        "object_creation_expression" if child(*node, "class_body").is_some() => "class",
        "method_declaration" => "fun",
        "constructor_declaration" | "compact_constructor_declaration" => "constructor",
        "field_declaration" | "constant_declaration" => "property",
        _ => return None,
    })
}

/// One declaration per name: a field declares each of its variables.
fn declarations(
    node: &Node,
    kind: &'static str,
    owners: &Owners,
    bytes: &[u8],
) -> Vec<Declaration> {
    let modifiers = child(*node, "modifiers");
    let anonymous = node.kind() == "object_creation_expression";
    let visibility = if anonymous {
        Some(Visibility::Private)
    } else {
        modifiers.and_then(|m| visibility_of(m, bytes))
    };
    let names: Vec<String> = match node.kind() {
        "field_declaration" | "constant_declaration" => {
            let mut cursor = node.walk();
            node.children_by_field_name("declarator", &mut cursor)
                .filter_map(|d| d.child_by_field_name("name"))
                .filter_map(|n| text(n, bytes).map(str::to_string))
                .collect()
        }
        "constructor_declaration" | "compact_constructor_declaration" => owners
            .last()
            .map(|(n, _, _)| n.clone())
            .into_iter()
            .collect(),
        "object_creation_expression" => node
            .child_by_field_name("type")
            .and_then(|t| type_name(t, bytes))
            .map(|t| format!("new {t}"))
            .into_iter()
            .collect(),
        _ => node
            .child_by_field_name("name")
            .and_then(|n| text(n, bytes).map(str::to_string))
            .into_iter()
            .collect(),
    };
    let body = node
        .child_by_field_name("body")
        .or_else(|| child(*node, "class_body"));
    let header_end = body.map_or_else(|| node.end_byte(), |b| b.start_byte());
    let annotations = modifiers.map(|m| annotations(m, bytes)).unwrap_or_default();
    let in_interface = owners.last().is_some_and(|(_, _, interface)| *interface);
    let abstract_member =
        kind == "fun" && body.is_none() && (in_interface || has_modifier(modifiers, "abstract"));
    let own = visibility.or(in_interface.then_some(Visibility::Public));
    names
        .into_iter()
        .map(|name| Declaration {
            name,
            kind,
            owners: owners.iter().map(|(n, _, _)| n.clone()).collect(),
            receiver: None,
            type_name: node
                .child_by_field_name("type")
                .filter(|_| !anonymous)
                .and_then(|t| type_name(t, bytes)),
            init_call: None,
            visibility,
            effective: Some(Visibility::bounded(own, owners.iter().map(|(_, v, _)| *v))),
            is_override: annotations.iter().any(|a| a == "Override"),
            // An inner or anonymous class sees the instance that holds it;
            // a nested interface, enum or record is static.
            inner: anonymous
                || (node.kind() == "class_declaration" && !has_modifier(modifiers, "static")),
            abstract_member,
            annotations: annotations.clone(),
            required_params: None,
            params: None,
            start_line: line_of(*node),
            end_line: node.end_position().row as u32 + 1,
            span: node.byte_range(),
            header: node.start_byte()..header_end,
            signature: squash(bytes, node.start_byte()..header_end),
        })
        .collect()
}

/// `record Pair(int a, int b)`: each component is a field of the record.
fn record_component(node: &Node, owners: &Owners, bytes: &[u8]) -> Option<Declaration> {
    let name = text(node.child_by_field_name("name")?, bytes)?.to_string();
    Some(Declaration {
        name,
        kind: "property",
        owners: owners.iter().map(|(n, _, _)| n.clone()).collect(),
        receiver: None,
        type_name: node
            .child_by_field_name("type")
            .and_then(|t| type_name(t, bytes)),
        init_call: None,
        visibility: Some(Visibility::Private),
        effective: Some(Visibility::Private),
        is_override: false,
        inner: false,
        abstract_member: false,
        annotations: Vec::new(),
        required_params: None,
        params: None,
        start_line: line_of(*node),
        end_line: node.end_position().row as u32 + 1,
        span: node.byte_range(),
        header: node.byte_range(),
        signature: squash(bytes, node.byte_range()),
    })
}

fn visibility_of(modifiers: Node, bytes: &[u8]) -> Option<Visibility> {
    let mut cursor = modifiers.walk();
    let found = modifiers.children(&mut cursor).find_map(|c| {
        Some(match text(c, bytes)? {
            "public" => Visibility::Public,
            "protected" => Visibility::Protected,
            "private" => Visibility::Private,
            _ => return None,
        })
    });
    found
}

fn has_modifier(modifiers: Option<Node>, word: &str) -> bool {
    modifiers.is_some_and(|m| {
        let mut cursor = m.walk();
        let found = m.children(&mut cursor).any(|c| c.kind() == word);
        found
    })
}

fn annotations(modifiers: Node, bytes: &[u8]) -> Vec<String> {
    let mut cursor = modifiers.walk();
    modifiers
        .children(&mut cursor)
        .filter(|c| matches!(c.kind(), "marker_annotation" | "annotation"))
        .filter_map(|a| a.child_by_field_name("name"))
        .filter_map(|n| last_identifier(n, bytes))
        .collect()
}

fn supertypes(node: &Node, bytes: &[u8]) -> Vec<String> {
    if node.kind() == "object_creation_expression" {
        return node
            .child_by_field_name("type")
            .and_then(|t| type_name(t, bytes))
            .into_iter()
            .collect();
    }
    let mut out = Vec::new();
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        if !matches!(
            c.kind(),
            "superclass" | "super_interfaces" | "extends_interfaces"
        ) {
            continue;
        }
        let mut stack = vec![c];
        while let Some(n) = stack.pop() {
            match n.kind() {
                "type_identifier" | "generic_type" | "scoped_type_identifier" => {
                    out.extend(type_name(n, bytes));
                }
                _ => {
                    let mut cursor = n.walk();
                    let children: Vec<Node> = n.named_children(&mut cursor).collect();
                    stack.extend(children.into_iter().rev());
                }
            }
        }
    }
    out
}

/// The outer type a written type names: `Repo` for `Repo<T>` and for
/// `a.b.Repo`. A primitive, an array or `var` names none.
fn type_name(node: Node, bytes: &[u8]) -> Option<String> {
    match node.kind() {
        "type_identifier" => text(node, bytes)
            .filter(|t| *t != "var")
            .map(str::to_string),
        "generic_type" => node.named_child(0).and_then(|t| type_name(t, bytes)),
        "scoped_type_identifier" => node
            .named_child(node.named_child_count().checked_sub(1)?)
            .and_then(|t| type_name(t, bytes)),
        _ => None,
    }
}

fn last_identifier(node: Node, bytes: &[u8]) -> Option<String> {
    match node.kind() {
        "scoped_identifier" => node
            .child_by_field_name("name")
            .and_then(|n| text(n, bytes))
            .map(str::to_string),
        _ => text(node, bytes).map(str::to_string),
    }
}

fn dotted(node: Node, bytes: &[u8]) -> String {
    text(node, bytes)
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect()
}

/// `new Make(…)`, `make(…)` or `repo.find(…)`: the call a value with no
/// written type (`var`) takes its type from.
fn initializer_call(value: Node, bytes: &[u8]) -> Option<InitCall> {
    match value.kind() {
        "object_creation_expression" => Some(InitCall {
            receiver: None,
            callee: value
                .child_by_field_name("type")
                .and_then(|t| type_name(t, bytes))?,
        }),
        "method_invocation" => {
            let receiver = match value.child_by_field_name("object") {
                None => None,
                Some(o) if o.kind() == "identifier" => Some(text(o, bytes)?.to_string()),
                Some(_) => return None,
            };
            Some(InitCall {
                receiver,
                callee: text(value.child_by_field_name("name")?, bytes)?.to_string(),
            })
        }
        _ => None,
    }
}

/// Parameters, local variables, loop, `catch`, resource and pattern
/// variables, each with its written type.
fn bindings_at(node: &Node, bytes: &[u8], bindings: &mut Vec<Binding>) {
    let name = |n: &Node| {
        n.child_by_field_name("name")
            .and_then(|i| text(i, bytes))
            .map(str::to_string)
    };
    let written = |n: &Node| {
        n.child_by_field_name("type")
            .and_then(|t| type_name(t, bytes))
    };
    match node.kind() {
        "formal_parameter" | "spread_parameter" => {
            let scope = std::iter::successors(node.parent(), Node::parent).find(|p| {
                matches!(
                    p.kind(),
                    "method_declaration"
                        | "constructor_declaration"
                        | "lambda_expression"
                        | "record_declaration"
                )
            });
            if let (Some(scope), Some(n)) = (scope, name(node)) {
                bindings.push(Binding::new(
                    n,
                    written(node),
                    scope.start_byte(),
                    scope.byte_range(),
                ));
            }
        }
        "lambda_expression" => {
            let mut params: Vec<Node> = Vec::new();
            if let Some(p) = node.child_by_field_name("parameters") {
                match p.kind() {
                    "identifier" => params.push(p),
                    "inferred_parameters" => {
                        let mut cursor = p.walk();
                        params.extend(p.named_children(&mut cursor));
                    }
                    _ => {}
                }
            }
            for p in params {
                if let Some(n) = text(p, bytes) {
                    bindings.push(Binding::new(
                        n.to_string(),
                        None,
                        node.start_byte(),
                        node.byte_range(),
                    ));
                }
            }
        }
        "local_variable_declaration" => {
            let Some(scope) = node.parent().map(|p| p.byte_range()) else {
                return;
            };
            let declared = written(node);
            let mut cursor = node.walk();
            for d in node.children_by_field_name("declarator", &mut cursor) {
                let Some(n) = name(&d) else {
                    continue;
                };
                // A local is not in scope in its own initializer.
                let mut b = Binding::new(n, declared.clone(), d.end_byte(), scope.clone());
                b.init_call = d
                    .child_by_field_name("value")
                    .filter(|_| declared.is_none())
                    .and_then(|v| initializer_call(v, bytes));
                bindings.push(b);
            }
        }
        "enhanced_for_statement" | "resource" => {
            let scope = if node.kind() == "resource" {
                std::iter::successors(node.parent(), Node::parent)
                    .find(|p| p.kind() == "try_with_resources_statement")
                    .map(|t| t.byte_range())
            } else {
                Some(node.byte_range())
            };
            if let (Some(scope), Some(n)) = (scope, name(node)) {
                bindings.push(Binding::new(n, written(node), scope.start, scope));
            }
        }
        "catch_formal_parameter" => {
            let scope = node.parent().map(|c| c.byte_range());
            let caught = child(*node, "catch_type")
                .and_then(|t| t.named_child(0))
                .and_then(|t| type_name(t, bytes));
            if let (Some(scope), Some(n)) = (scope, name(node)) {
                bindings.push(Binding::new(n, caught, scope.start, scope));
            }
        }
        "instanceof_expression" => {
            let tested = node
                .child_by_field_name("right")
                .and_then(|t| type_name(t, bytes));
            if let (Some(scope), Some(n)) = (pattern_scope(node), name(node)) {
                bindings.push(Binding::new(n, tested, node.end_byte(), scope));
            }
        }
        // `case Foo foo ->`, `Pair(var a, var b)`.
        "type_pattern" | "record_pattern_component" => {
            let mut cursor = node.walk();
            let parts: Vec<Node> = node.named_children(&mut cursor).collect();
            if let (Some(scope), [kind, .., id]) = (pattern_scope(node), parts.as_slice()) {
                if let Some(n) = text(*id, bytes).filter(|_| id.kind() == "identifier") {
                    let typed = type_name(*kind, bytes);
                    bindings.push(Binding::new(n.to_string(), typed, node.end_byte(), scope));
                }
            }
        }
        _ => {}
    }
}

/// Where a pattern's variables are in scope: the rest of the statement, the
/// `case` or the block that holds it.
fn pattern_scope(node: &Node) -> Option<Range<usize>> {
    std::iter::successors(node.parent(), Node::parent)
        .find(|p| {
            p.kind().ends_with("_statement")
                || matches!(
                    p.kind(),
                    "switch_rule" | "switch_block_statement_group" | "block" | "lambda_expression"
                )
        })
        .map(|s| s.byte_range())
}

/// The uses a node is the head of, each recorded once.
fn uses_at(node: &Node, bytes: &[u8], uses: &mut Vec<Use>) {
    let mut push = |name, kind, receiver, at| push_use(uses, name, kind, receiver, at);
    match node.kind() {
        "method_invocation" => {
            let Some(name) = node.child_by_field_name("name") else {
                return;
            };
            let receiver = node
                .child_by_field_name("object")
                .map(|o| receiver_of(o, bytes));
            push(text(name, bytes), UseKind::Call, receiver, name);
        }
        "field_access" => {
            let Some(field) = node.child_by_field_name("field") else {
                return;
            };
            let receiver = node
                .child_by_field_name("object")
                .map(|o| receiver_of(o, bytes));
            push(text(field, bytes), UseKind::Access, receiver, field);
        }
        "object_creation_expression" => {
            if let Some(t) = node.child_by_field_name("type") {
                let at = type_head(t);
                push(type_name(t, bytes).as_deref(), UseKind::Call, None, at);
            }
        }
        "method_reference" => {
            let mut cursor = node.walk();
            let named: Vec<Node> = node.named_children(&mut cursor).collect();
            if let [recv, .., target] = named.as_slice() {
                let receiver = match recv.kind() {
                    "this" => Receiver::This,
                    "identifier" | "type_identifier" => {
                        text(*recv, bytes).map_or(Receiver::Expr, |t| Receiver::Name(t.to_string()))
                    }
                    _ => receiver_of(*recv, bytes),
                };
                let name = if target.kind() == "identifier" {
                    text(*target, bytes)
                } else {
                    None
                };
                push(name, UseKind::Reference, Some(receiver), *target);
            }
        }
        "type_identifier" if is_type_use(node) && text(*node, bytes) != Some("var") => {
            let receiver = node
                .parent()
                .filter(|p| p.kind() == "scoped_type_identifier")
                .and_then(|p| p.named_child(0))
                .filter(|q| q.id() != node.id())
                .map(|q| Receiver::Path(dotted(q, bytes).split('.').map(str::to_string).collect()));
            push(text(*node, bytes), UseKind::TypeUse, receiver, *node);
        }
        "identifier" if is_bare_reference(node) => {
            push(text(*node, bytes), UseKind::Access, None, *node);
        }
        _ => {}
    }
}

/// The node a type is written at, for the line its use is on.
fn type_head(t: Node) -> Node {
    if t.kind() == "generic_type" {
        return t.named_child(0).unwrap_or(t);
    }
    t
}

/// A type name written as a type, not a constructor call (which records
/// it) nor the qualifier of a longer one.
fn is_type_use(node: &Node) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    match parent.kind() {
        "object_creation_expression" => false,
        "generic_type" => !parent
            .parent()
            .is_some_and(|g| g.kind() == "object_creation_expression"),
        "scoped_type_identifier" => parent
            .named_child(parent.named_child_count().saturating_sub(1))
            .is_some_and(|last| last.id() == node.id()),
        _ => true,
    }
}

/// A plain name read as a value: not a declaration's name, not a member
/// after a dot, not a method name.
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
        "method_invocation" | "field_access" => field == Some("object"),
        "variable_declarator" => field == Some("value"),
        "lambda_expression" => field == Some("body"),
        "enhanced_for_statement" => field == Some("value"),
        "instanceof_expression" => field == Some("left"),
        "argument_list"
        | "binary_expression"
        | "unary_expression"
        | "update_expression"
        | "assignment_expression"
        | "parenthesized_expression"
        | "ternary_expression"
        | "cast_expression"
        | "array_access"
        | "array_initializer"
        | "return_statement"
        | "expression_statement"
        | "throw_statement"
        | "yield_statement"
        | "switch_label"
        | "element_value_pair"
        | "annotation_argument_list" => true,
        _ => false,
    }
}

fn receiver_of(node: Node, bytes: &[u8]) -> Receiver {
    match node.kind() {
        "identifier" => text(node, bytes)
            .map(|t| Receiver::Name(t.to_string()))
            .unwrap_or(Receiver::Expr),
        "this" => Receiver::This,
        "method_invocation" if node.child_by_field_name("object").is_none() => node
            .child_by_field_name("name")
            .and_then(|n| text(n, bytes))
            .map_or(Receiver::Expr, |callee| Receiver::Call(callee.to_string())),
        "object_creation_expression" if child(node, "class_body").is_none() => node
            .child_by_field_name("type")
            .and_then(|t| type_name(t, bytes))
            .map_or(Receiver::Expr, Receiver::Call),
        "field_access" => {
            let mut parts = Vec::new();
            let mut current = node;
            loop {
                let Some(field) = current
                    .child_by_field_name("field")
                    .and_then(|f| text(f, bytes))
                else {
                    return Receiver::Expr;
                };
                parts.push(field.to_string());
                match current.child_by_field_name("object") {
                    Some(o) if o.kind() == "field_access" => current = o,
                    Some(o) if o.kind() == "identifier" => {
                        parts.extend(text(o, bytes).map(str::to_string));
                        break;
                    }
                    Some(o) if o.kind() == "this" => {
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

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = "package com.acme.svc;\n\
\n\
import com.acme.mapper.DrugMapper;\n\
import com.acme.util.*;\n\
import static com.acme.Util.helper;\n\
\n\
@Service\n\
public class MedicineService extends Base implements Svc, Other<T> {\n\
    private final DrugMapper mapper;\n\
    static int COUNT = 0, MAX = 2;\n\
\n\
    public MedicineService(DrugMapper mapper) {\n\
        this.mapper = mapper;\n\
    }\n\
\n\
    @Override\n\
    public Dto details(Draft draft) {\n\
        var local = new DrugMapper();\n\
        Svc s = new Svc() {\n\
            @Override public int get(int id) { return 7; }\n\
        };\n\
        String str = \"toDto(x)\"; // toDto in a comment\n\
        helper();\n\
        return this.mapper.toDto(draft);\n\
    }\n\
\n\
    interface Port { void save(); }\n\
}\n";

    fn facts_of() -> KotlinFacts {
        facts(SOURCE).expect("java grammar")
    }

    fn decl<'a>(f: &'a KotlinFacts, name: &str) -> &'a Declaration {
        f.declarations
            .iter()
            .find(|d| d.name == name)
            .unwrap_or_else(|| panic!("{name}: {:?}", f.declarations))
    }

    #[test]
    fn package_imports_and_declarations_read_off_the_tree() {
        let f = facts_of();
        assert!(!f.partial(), "{:?}", f.error_lines);
        assert_eq!(f.package.as_deref(), Some("com.acme.svc"));
        assert_eq!(
            f.imported("DrugMapper").map(|i| i.path.as_str()),
            Some("com.acme.mapper.DrugMapper")
        );
        assert!(
            f.imports
                .iter()
                .any(|i| i.wildcard && i.path == "com.acme.util")
        );
        assert_eq!(
            f.imported("helper").map(|i| i.path.as_str()),
            Some("com.acme.Util.helper")
        );
        let mapper = decl(&f, "mapper");
        assert_eq!(
            (mapper.kind, mapper.type_name.as_deref(), mapper.effective),
            ("property", Some("DrugMapper"), Some(Visibility::Private))
        );
        assert_eq!(decl(&f, "MAX").owners, ["MedicineService"]);
        let ctor = f
            .declarations
            .iter()
            .find(|d| d.kind == "constructor")
            .expect("constructor");
        assert_eq!(ctor.name, "MedicineService");
        let details = decl(&f, "details");
        assert!(details.is_override);
        assert_eq!(details.type_name.as_deref(), Some("Dto"));
        assert_eq!(details.effective, Some(Visibility::Public));
        assert_eq!(
            f.supertypes_of("MedicineService"),
            ["Base".to_string(), "Svc".to_string(), "Other".to_string()]
        );
        assert_eq!(decl(&f, "MedicineService").annotations, ["Service"]);
        let save = decl(&f, "save");
        assert!(save.abstract_member);
        assert_eq!(
            save.effective,
            Some(Visibility::Internal),
            "a package-private owner"
        );
    }

    #[test]
    fn an_anonymous_class_is_a_type_its_members_belong_to() {
        let f = facts_of();
        let anon = decl(&f, "new Svc");
        assert_eq!(anon.owners, ["MedicineService"]);
        assert_eq!(f.supertypes_of("new Svc"), ["Svc".to_string()]);
        let get = decl(&f, "get");
        assert_eq!(get.owners, ["MedicineService", "new Svc"]);
        assert!(get.is_override);
    }

    #[test]
    fn uses_carry_receivers_and_text_is_no_use() {
        let f = facts_of();
        let uses = |name: &str| -> Vec<(UseKind, Option<Receiver>)> {
            f.uses
                .iter()
                .filter(|u| u.name == name)
                .map(|u| (u.kind, u.receiver.clone()))
                .collect()
        };
        assert_eq!(
            uses("toDto"),
            [(
                UseKind::Call,
                Some(Receiver::Path(vec!["this".into(), "mapper".into()]))
            )]
        );
        assert_eq!(uses("helper"), [(UseKind::Call, None)]);
        assert!(uses("DrugMapper").contains(&(UseKind::Call, None)));
        assert!(uses("DrugMapper").iter().any(|u| u.0 == UseKind::TypeUse));
        let at = |needle: &str| SOURCE.find(needle).expect(needle);
        let local = f.binding_at("local", at("String str")).expect("local");
        assert_eq!(
            local.init_call,
            Some(InitCall {
                receiver: None,
                callee: "DrugMapper".into()
            })
        );
        let draft = f
            .binding_at("draft", at("toDto(draft)"))
            .expect("parameter");
        assert_eq!(draft.type_name.as_deref(), Some("Draft"));
    }
}
