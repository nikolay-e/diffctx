//! The definitions a JavaScript or TypeScript file, or the inline scripts of an HTML
//! page, declares that the fragmenter does not cut: a property of the global object
//! or of the module's exports (#426), an interface or object type member, a class field
//! (#488), and a page's top-level script statements. The overlay lays them over the
//! fragments; the facts layer reads the files themselves for the cells.

use std::sync::Arc;

use once_cell::sync::Lazy;
use rustc_hash::FxHashMap;

use super::{DeclKind, Declaration};
use crate::types::FragmentKind;

/// What one JS/TS file or page declares beyond what the fragmenter cuts.
#[derive(Debug, Default)]
pub(super) struct JsFacts {
    /// Functions, classes and methods with the class and the function they sit in
    /// (#554), and as assignments what the fragmenter does not cut.
    pub(super) declarations: Vec<Declaration>,
    /// A page's top-level script statements: in a `.js` file they are the
    /// fragmenter's gap chunks, in a page the script element holds them.
    pub(super) script_statements: Vec<(u32, u32)>,
}

fn grammar_of(path: &str) -> &'static str {
    match path
        .rsplit('.')
        .next()
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("ts" | "mts" | "cts") => "typescript",
        Some("tsx") => "tsx",
        _ => "javascript",
    }
}

const FUNCTIONS: &[&str] = &[
    "function_declaration",
    "generator_function_declaration",
    "function_expression",
    "function",
    "generator_function",
    "arrow_function",
    "method_definition",
];

fn is_html(path: &str) -> bool {
    let ext = path.rsplit('.').next().map(str::to_ascii_lowercase);
    matches!(ext.as_deref(), Some("html" | "htm"))
}

/// The code of a file: an HTML page's is its inline scripts, at their own
/// positions in the page (#420).
fn script_code<'t>(path: &str, text: &'t str) -> std::borrow::Cow<'t, str> {
    if is_html(path) {
        crate::parsers::inline_scripts(text)
            .unwrap_or_default()
            .into()
    } else {
        text.into()
    }
}

/// The owner of a property set on the global object (`self.x = …`): any
/// script the realm loads reaches it by name, bare or through the global
/// object, with no import to bind it (#426).
pub(super) const GLOBAL_OWNER: &str = "<global>";

const GLOBAL_OBJECTS: &[&str] = &["self", "globalThis", "window", "global"];

const FACTS_LIMIT: usize = 8192;

static FACTS: Lazy<std::sync::Mutex<FxHashMap<u64, Arc<JsFacts>>>> =
    Lazy::new(|| std::sync::Mutex::new(FxHashMap::default()));

/// The facts of one blob, parsed once per run whoever asks first. `None` when the
/// grammar is not compiled in or the parse timed out.
pub(super) fn js_facts(path: &str, text: &str) -> Option<Arc<JsFacts>> {
    use std::hash::{Hash, Hasher};
    let key = {
        let mut h = rustc_hash::FxHasher::default();
        (grammar_of(path), is_html(path), text).hash(&mut h);
        h.finish()
    };
    if let Some(hit) = FACTS.lock().ok().and_then(|c| c.get(&key).cloned()) {
        return Some(hit);
    }
    let facts = Arc::new(read_js_facts(path, &script_code(path, text))?);
    if let Ok(mut cache) = FACTS.lock() {
        if cache.len() >= FACTS_LIMIT {
            cache.clear();
        }
        cache.insert(key, Arc::clone(&facts));
    }
    Some(facts)
}

fn read_js_facts(path: &str, text: &str) -> Option<JsFacts> {
    let tree = crate::parsers::parse_tree(grammar_of(path), text)?;
    let bytes = text.as_bytes();
    // The lines of each scope's function: what a definition inside is local to. The
    // module is the first scope and has none.
    let mut spans: Vec<Option<(u32, u32)>> = vec![None];
    let mut facts = JsFacts::default();
    let mut stack = vec![(tree.root_node(), 0usize)];
    while let Some((node, scope)) = stack.pop() {
        let mut inner = scope;
        let kind = node.kind();
        let local_to = spans[scope];
        let mut define = |name: Option<tree_sitter::Node>, kind, class: Option<String>| {
            if let Some(name) = name.filter(|n| n.kind().ends_with("identifier")) {
                facts.declarations.push(Declaration {
                    name: name.utf8_text(bytes).unwrap_or("").to_string(),
                    kind,
                    lines: lines_of(node),
                    class,
                    scope: local_to,
                });
            }
        };
        if FUNCTIONS.contains(&kind) {
            let name = node.child_by_field_name("name");
            if kind == "method_definition" {
                if let Some(class) = class_of(node, bytes) {
                    define(name, DeclKind::Function, Some(class));
                }
            } else if kind.ends_with("_declaration") {
                define(name, DeclKind::Function, None);
            }
            spans.push(Some(lines_of(node)));
            inner = spans.len() - 1;
        }
        match kind {
            "variable_declarator" => {
                // `const helper = () => …` defines a function, like a declaration.
                let value = node.child_by_field_name("value");
                if let Some(value) =
                    value.filter(|v| FUNCTIONS.contains(&v.kind()) || v.kind() == "class")
                {
                    let kind = if value.kind() == "class" {
                        DeclKind::Class
                    } else {
                        DeclKind::Function
                    };
                    define(node.child_by_field_name("name"), kind, None);
                }
            }
            "class_declaration"
            | "abstract_class_declaration"
            | "interface_declaration"
            | "type_alias_declaration"
            | "enum_declaration" => {
                if kind.ends_with("class_declaration") {
                    define(node.child_by_field_name("name"), DeclKind::Class, None);
                }
            }
            "assignment_expression"
            | "property_signature"
            | "method_signature"
            | "public_field_definition"
            | "field_definition" => {
                if let Some((name, class, span)) = declared_member(node, bytes) {
                    facts.declarations.push(Declaration {
                        name: name.utf8_text(bytes).unwrap_or("").to_string(),
                        kind: DeclKind::Assignment,
                        lines: lines_of(span),
                        class,
                        scope: None,
                    });
                }
            }
            // Comments, strings and JSX text hold no declarations.
            "comment" | "string" | "regex" | "jsx_text" => continue,
            _ => {}
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor).map(|c| (c, inner)));
    }
    if is_html(path) {
        let root = tree.root_node();
        let mut cursor = root.walk();
        facts.script_statements = root
            .named_children(&mut cursor)
            .filter(|n| {
                let kind = n.kind();
                !(kind.ends_with("_declaration")
                    || kind == "comment"
                    || kind == "import_statement"
                    || kind == "export_statement")
            })
            .map(lines_of)
            .collect();
    }
    Some(facts)
}

/// A member a statement or a body declares: its name, its owner and the
/// span of the declaration. `self.x = …`, `globalThis.x`, `window.x` set a
/// property of the global object, which any script of the realm reaches
/// by name; `exports.x`, `module.exports.x` export `x` from the module
/// (#426). An interface or object type states its members, a class its
/// fields (#488).
fn declared_member<'t>(
    node: tree_sitter::Node<'t>,
    bytes: &[u8],
) -> Option<(tree_sitter::Node<'t>, Option<String>, tree_sitter::Node<'t>)> {
    if node.kind() == "assignment_expression" {
        let left = node
            .child_by_field_name("left")
            .filter(|l| l.kind() == "member_expression")?;
        let name = left
            .child_by_field_name("property")
            .filter(|p| p.kind() == "property_identifier")?;
        let class = match left.child_by_field_name("object")?.utf8_text(bytes).ok()? {
            o if GLOBAL_OBJECTS.contains(&o) => Some(GLOBAL_OWNER.to_string()),
            "exports" | "module.exports" => None,
            _ => return None,
        };
        let statement = node
            .parent()
            .filter(|p| p.kind() == "expression_statement")
            .unwrap_or(node);
        return Some((name, class, statement));
    }
    let name = node
        .child_by_field_name("name")
        .or_else(|| node.child_by_field_name("property"))
        .filter(|n| {
            matches!(
                n.kind(),
                "property_identifier" | "private_property_identifier"
            )
        })?;
    let class = match node.kind() {
        "public_field_definition" | "field_definition" => class_of(node, bytes)?,
        _ => {
            let body = node
                .parent()
                .filter(|b| matches!(b.kind(), "interface_body" | "object_type"))?;
            let owner = body.parent().filter(|o| {
                matches!(o.kind(), "interface_declaration" | "type_alias_declaration")
            })?;
            owner
                .child_by_field_name("name")?
                .utf8_text(bytes)
                .ok()?
                .to_string()
        }
    };
    Some((name, Some(class), node))
}

/// The named class whose body holds `member`; a method of an object
/// literal has none the syntax names.
fn class_of(member: tree_sitter::Node, bytes: &[u8]) -> Option<String> {
    let class = member
        .parent()
        .filter(|b| b.kind() == "class_body")?
        .parent()
        .filter(|c| {
            matches!(
                c.kind(),
                "class_declaration" | "abstract_class_declaration" | "class"
            )
        })?;
    Some(
        class
            .child_by_field_name("name")?
            .utf8_text(bytes)
            .ok()?
            .to_string(),
    )
}

fn lines_of(node: tree_sitter::Node) -> (u32, u32) {
    (
        node.start_position().row as u32 + 1,
        node.end_position().row as u32 + 1,
    )
}

/// A JavaScript file's or a page's assignments the fragmenter does not cut
/// — a property of the global object or of the module's exports (#426), an
/// interface member, a class field (#488) — and a page's top-level script
/// statements, so a call there is a caller at its own lines rather than
/// the whole script element (#577).
pub(super) fn declarations(
    path: &str,
    text: &str,
) -> Vec<(Option<String>, (u32, u32), FragmentKind)> {
    js_facts(path, text).map_or_else(Vec::new, |facts| {
        facts
            .declarations
            .iter()
            .filter(|d| d.kind == DeclKind::Assignment)
            .map(|d| (Some(d.name.clone()), d.lines, FragmentKind::Property))
            .chain(
                facts
                    .script_statements
                    .iter()
                    .map(|&lines| (None, lines, FragmentKind::Chunk)),
            )
            .collect()
    })
}
