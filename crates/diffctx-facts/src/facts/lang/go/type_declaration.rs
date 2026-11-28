use tree_sitter::Node;

use super::{Cx, Walker, children};
use crate::facts::{DefKind, Scope};

impl<'t> Walker<'t> {
    pub(super) fn type_declaration(&mut self, node: Node<'t>, docs: &[&'t str]) {
        let specs: Vec<Node<'t>> = children(node)
            .into_iter()
            .filter(|c| matches!(c.kind(), "type_spec" | "type_alias"))
            .collect();
        let lone = specs.len() == 1;
        for spec in specs {
            let Some(name_node) = spec.child_by_field_name("name") else {
                continue;
            };
            let Some(ty) = spec.child_by_field_name("type") else {
                continue;
            };
            let members: Vec<Node<'t>> = match ty.kind() {
                "struct_type" => children(ty)
                    .into_iter()
                    .filter(|c| c.kind() == "field_declaration_list")
                    .flat_map(children)
                    .filter(|c| c.kind() == "field_declaration")
                    .collect(),
                "interface_type" => children(ty)
                    .into_iter()
                    .filter(|c| c.kind() == "method_elem")
                    .collect(),
                _ => Vec::new(),
            };
            let kind = match (spec.kind(), ty.kind()) {
                ("type_alias", _) => DefKind::TypeAlias,
                (_, "struct_type") => DefKind::Struct,
                (_, "interface_type") => DefKind::Interface,
                _ => DefKind::TypeAlias,
            };
            let span_node = if lone { node } else { spec };
            let signature = self.signature(spec, &members, None);
            let key = self.define(
                span_node,
                name_node,
                &[],
                kind,
                spec,
                &members,
                None,
                signature,
                docs,
            );
            let inside = Cx {
                container: vec![self.text(name_node).to_string()],
                scope: Scope::Def(key),
            };
            if let Some(tparams) = spec.child_by_field_name("type_parameters") {
                self.type_refs(tparams, &inside, 2);
            }
            match ty.kind() {
                "struct_type" => {
                    for field in &members {
                        self.field(*field, &inside, docs);
                    }
                }
                "interface_type" => self.interface(ty, &inside, docs),
                _ => self.type_refs(ty, &inside, 2),
            }
        }
    }

    fn field(&mut self, node: Node<'t>, cx: &Cx, docs: &[&'t str]) {
        let Some(ty) = node.child_by_field_name("type") else {
            return;
        };
        self.type_refs(ty, cx, 3);
        let mut cursor = node.walk();
        let names: Vec<Node<'t>> = node
            .children_by_field_name("name", &mut cursor)
            .filter(|n| n.kind() == "field_identifier")
            .collect();
        let mut written = self.signature(ty, &[], None);
        if names.is_empty() {
            // `*Base` embeds a pointer: the `*` belongs to the field, not to the
            // type node, and the two embeddings promote different method sets.
            let mut walker = node.walk();
            if node
                .children(&mut walker)
                .any(|c| c.kind() == "*" && c.end_byte() <= ty.start_byte())
            {
                written = format!("* {written}");
            }
            // An embedded field is named after its type, and is that type alone.
            if let Some(name_node) = embedded_name_node(ty) {
                self.define(
                    node,
                    name_node,
                    &cx.container,
                    DefKind::Field,
                    node,
                    &[],
                    None,
                    written,
                    docs,
                );
            }
            return;
        }
        for name in names {
            let signature = format!("{} {written}", self.text(name));
            self.define(
                node,
                name,
                &cx.container,
                DefKind::Field,
                node,
                &[],
                None,
                signature,
                docs,
            );
        }
    }

    fn interface(&mut self, node: Node<'t>, cx: &Cx, docs: &[&'t str]) {
        for member in children(node) {
            match member.kind() {
                "method_elem" => {
                    let Some(name) = member.child_by_field_name("name") else {
                        continue;
                    };
                    let key = self.define(
                        member,
                        name,
                        &cx.container,
                        DefKind::Method,
                        member,
                        &[],
                        None,
                        self.signature(member, &[], None),
                        docs,
                    );
                    let inside = Cx {
                        container: cx.container.clone(),
                        scope: Scope::Def(key),
                    };
                    for part in ["parameters", "result"] {
                        if let Some(part) = member.child_by_field_name(part) {
                            self.type_refs(part, &inside, 3);
                        }
                    }
                }
                _ => self.type_refs(member, cx, 3),
            }
        }
    }
}

/// The identifier a type names as an embedded field's own name.
fn embedded_name_node(ty: Node<'_>) -> Option<Node<'_>> {
    match ty.kind() {
        "type_identifier" => Some(ty),
        "qualified_type" => ty.child_by_field_name("name"),
        "pointer_type" | "parenthesized_type" => {
            embedded_name_node(children(ty).into_iter().next()?)
        }
        "generic_type" => embedded_name_node(ty.child_by_field_name("type")?),
        _ => None,
    }
}
