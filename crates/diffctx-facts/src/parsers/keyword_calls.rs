//! The calls of a Python file that pass a value by a keyword of a given name, off the
//! Python grammar: where a `--symbol` value goes next (#708).

use tree_sitter::Node;

/// What a call writes before its parentheses: `a.b.f` is `f` qualified by `a.b`,
/// and a receiver that is no plain name (`make().f`) has no qualifier to read.
pub struct Callee {
    pub qualifier: Option<Vec<String>>,
    pub name: String,
}

fn dotted(node: Node<'_>, text: &str) -> Option<Vec<String>> {
    match node.kind() {
        "identifier" => Some(vec![text[node.byte_range()].to_string()]),
        "attribute" => {
            let mut chain = dotted(node.child_by_field_name("object")?, text)?;
            chain.push(text[node.child_by_field_name("attribute")?.byte_range()].to_string());
            Some(chain)
        }
        _ => None,
    }
}

fn callee_of(function: Node<'_>, text: &str) -> Option<Callee> {
    match function.kind() {
        "identifier" => Some(Callee {
            qualifier: Some(Vec::new()),
            name: text[function.byte_range()].to_string(),
        }),
        "attribute" => Some(Callee {
            qualifier: dotted(function.child_by_field_name("object")?, text),
            name: text[function.child_by_field_name("attribute")?.byte_range()].to_string(),
        }),
        _ => None,
    }
}

fn keyword_calls(node: Node<'_>, text: &str, name: &str, found: &mut Vec<Callee>) {
    if node.kind() == "call" {
        let passes = node.child_by_field_name("arguments").is_some_and(|args| {
            let mut cursor = args.walk();
            args.named_children(&mut cursor).any(|arg| {
                arg.kind() == "keyword_argument"
                    && arg
                        .child_by_field_name("name")
                        .is_some_and(|n| &text[n.byte_range()] == name)
            })
        });
        if passes {
            found.extend(
                node.child_by_field_name("function")
                    .and_then(|f| callee_of(f, text)),
            );
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        keyword_calls(child, text, name, found);
    }
}

/// The calls in `text` that pass a keyword argument named `name`, `None` when the file does not parse.
pub fn of_file(path: &str, text: &str, name: &str) -> Option<Vec<Callee>> {
    let (tree, _) = super::parse_file(path, text)?;
    let mut found = Vec::new();
    keyword_calls(tree.root_node(), text, name, &mut found);
    Some(found)
}
