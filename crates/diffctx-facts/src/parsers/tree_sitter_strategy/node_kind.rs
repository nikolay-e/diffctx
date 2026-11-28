const NODE_TYPE_KEYWORDS: &[(&[&str], &str)] = &[
    (
        &[
            "function",
            "method",
            "subroutine",
            "FnDecl",
            "short_function",
        ],
        "function",
    ),
    (
        &[
            "class",
            "object_definition",
            "class_interface",
            "class_implementation",
            "category_interface",
            "category_implementation",
        ],
        "class",
    ),
    (&["struct", "struct_definition", "ContainerDecl"], "struct"),
    (&["impl"], "impl"),
    (
        &[
            "trait",
            "interface",
            "protocol",
            "protocol_declaration",
            "mixin",
        ],
        "interface",
    ),
    (&["enum", "adt", "newtype"], "enum"),
    (
        &[
            "module",
            "module_definition",
            "abstract_definition",
            "package",
        ],
        "module",
    ),
    (
        &[
            "type_alias",
            "alias_declaration",
            "type_definition",
            "type_declaration",
        ],
        "type",
    ),
    (
        &[
            "variable_declarator",
            "VarDecl",
            "let_binding",
            "binding",
            "left_assignment",
        ],
        "variable",
    ),
    (
        &[
            "record",
            "model_declaration",
            "datasource_declaration",
            "generator_declaration",
        ],
        "record",
    ),
    (&["property", "property_declaration"], "property"),
    (
        &[
            "declaration",
            "using_declaration",
            "attribute",
            "type_spec",
            "signature",
            "instance",
        ],
        "declaration",
    ),
    (&["rule_set", "rule"], "definition"),
    (&["block"], "definition"),
    (&["section", "subsection"], "definition"),
    (&["environment", "new_command_definition"], "definition"),
    (
        &["element", "script_element", "style_element"],
        "definition",
    ),
    (&["pair", "block_mapping_pair"], "definition"),
    (&["TestDecl"], "definition"),
    (&["closure"], "function"),
    (&["call", "list_lit"], "definition"),
    (&["extension_declaration"], "definition"),
    (
        &["macro_definition", "macro_def", "function_def"],
        "function",
    ),
    (&["if_condition", "foreach_loop"], "definition"),
    (
        &["media_statement", "keyframes_statement", "import_statement"],
        "definition",
    ),
    (
        &[
            "object_type_definition",
            "interface_type_definition",
            "enum_type_definition",
            "input_object_type_definition",
            "union_type_definition",
            "scalar_type_definition",
        ],
        "type",
    ),
    (&["singleton_method"], "function"),
    (&["inherit"], "declaration"),
];
pub(in crate::parsers) fn node_type_to_kind(
    node_type: &str,
    node: Option<&tree_sitter::Node>,
) -> &'static str {
    if node_type == "decorated_definition" {
        if let Some(n) = node {
            return super::decorated_definition_kind(n);
        }
    }
    thread_local! {
        static KINDS: std::cell::RefCell<rustc_hash::FxHashMap<String, &'static str>> = Default::default();
    }
    KINDS.with(|cache| {
        if let Some(&kind) = cache.borrow().get(node_type) {
            return kind;
        }
        let kind = NODE_TYPE_KEYWORDS
            .iter()
            .find(|(keywords, _)| keywords.iter().any(|kw| node_type.contains(kw)))
            .map_or("definition", |&(_, kind)| kind);
        cache.borrow_mut().insert(node_type.to_string(), kind);
        kind
    })
}
