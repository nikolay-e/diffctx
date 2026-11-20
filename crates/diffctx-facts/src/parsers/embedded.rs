//! The program a shell command feeds an interpreter (`python - <<EOF`, `node -e`),
//! off the bash grammar: nothing else reads that code, so a name in it is only
//! found here.

use super::code_text;

/// Whether `text` holds `name` as a whole word; `$` is a word character,
/// as in a JavaScript identifier.
pub fn names_word(text: &str, name: &str) -> bool {
    let word = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    text.match_indices(name).any(|(at, _)| {
        !text[..at].chars().next_back().is_some_and(word)
            && !text[at + name.len()..].chars().next().is_some_and(word)
    })
}

fn children_of<'t>(node: tree_sitter::Node<'t>, kind: &str) -> Vec<tree_sitter::Node<'t>> {
    let mut cursor = node.walk();
    (node.named_children(&mut cursor))
        .filter(|child| child.kind() == kind)
        .collect()
}

/// The line of the first command in `script` that runs `runtime`'s
/// interpreter on code naming `name`: a heredoc fed to it, or its `-c`/`-e`
/// argument. The line, and the bytes of the command.
pub fn embedded_naming(
    script: &str,
    runtime: &str,
    name: &str,
) -> Option<(u32, std::ops::Range<u32>)> {
    let (interpreters, as_file): (&[&str], &str) = match runtime {
        "python" => (&["python", "pypy"], "embedded.py"),
        "js" => (&["node", "deno", "bun"], "embedded.js"),
        _ => return None,
    };
    let tree = crate::parsers::parse_tree("bash", script)?;
    let text = |n: tree_sitter::Node| n.utf8_text(script.as_bytes()).unwrap_or("");
    // Only code names it: a comment or a string in the embedded program
    // does not (`# column-aligned`).
    let names = |embedded: &str| names_word(&code_text(as_file, embedded), name);
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
        if node.kind() != "command" {
            continue;
        }
        let mut cursor = node.walk();
        let words: Vec<&str> = node.named_children(&mut cursor).map(text).collect();
        if !words.iter().any(|w| {
            let base = w.rsplit('/').next().unwrap_or(w);
            interpreters.iter().any(|i| base.starts_with(i))
        }) {
            continue;
        }
        // Any `-c`/`-e`: a container runner's own `-e K=V` comes before the
        // interpreter's `-c`.
        let inline = words
            .windows(2)
            .any(|pair| matches!(pair[0], "-c" | "-e") && names(pair[1].trim_matches(['\'', '"'])));
        let fed = node.parent().is_some_and(|p| {
            p.kind() == "redirected_statement"
                && children_of(p, "heredoc_redirect")
                    .iter()
                    .any(|r| (children_of(*r, "heredoc_body").iter()).any(|b| names(text(*b))))
        });
        if inline || fed {
            let at = |offset: usize| u32::try_from(offset).unwrap_or(u32::MAX);
            return Some((
                at(node.start_position().row + 1),
                at(node.start_byte())..at(node.end_byte()),
            ));
        }
    }
    None
}
