//! The calls that hand a string to a shell, read from what a file imports.

use tree_sitter::Node;

use super::span;
use crate::facts::{Fact, HeldAs, HeldString, Name, Scope, Star};

const NODE_RUNNERS: &[&str] = &[
    "exec",
    "execSync",
    "spawn",
    "spawnSync",
    "execFile",
    "execFileSync",
];

/// The command a list of literal words hands to a shell: the words after
/// `-c`, else all of them.
pub(super) fn command_text(words: &[String]) -> String {
    let code = words.iter().position(|w| w == "-c").map_or(0, |at| at + 1);
    words[code..].join(" ")
}

/// `exec.Command("bash", "-c", "f x")` in Go: the literal words are a command.
pub(super) fn go_command(
    bytes: &[u8],
    function: Node<'_>,
    arguments: &[Node<'_>],
    scope: &Scope,
) -> Option<Fact> {
    let text = |node: Node<'_>| node.utf8_text(bytes).unwrap_or("");
    let runner = function.kind() == "selector_expression"
        && function
            .child_by_field_name("operand")
            .is_some_and(|operand| text(operand) == "exec")
        && function
            .child_by_field_name("field")
            .is_some_and(|field| matches!(text(field), "Command" | "CommandContext"));
    let words: Vec<String> = arguments
        .iter()
        .filter(|a| {
            matches!(
                a.kind(),
                "interpreted_string_literal" | "raw_string_literal"
            )
        })
        .map(|a| text(*a).trim_matches(['"', '`']).to_string())
        .collect();
    (runner && !words.is_empty()).then(|| {
        Fact::HeldString(HeldString {
            site: span(function),
            enclosing: scope.clone(),
            text: command_text(&words),
            held: HeldAs::Command,
        })
    })
}

fn child_process(module: &str) -> bool {
    matches!(module, "child_process" | "node:child_process")
}

/// Whether `callee` (`execSync`, `cp.exec`) is a runner the file imports from
/// `child_process`, by a name, a destructured name, a default or a namespace.
pub(super) fn node_runs_shell(callee: &[Name], facts: &[Fact]) -> bool {
    let mut imports = facts.iter().filter_map(|fact| match fact {
        Fact::Import(import) if child_process(import.module.as_str()) => Some(import),
        _ => None,
    });
    match callee {
        [function] => imports.flat_map(|i| i.names.iter()).any(|(name, alias)| {
            NODE_RUNNERS.contains(&name.as_str()) && alias.as_ref().unwrap_or(name) == function
        }),
        [module, function] => {
            NODE_RUNNERS.contains(&function.as_str())
                && imports.any(|i| {
                    i.default.as_ref() == Some(module)
                        || matches!(&i.star, Star::Alias(alias) if alias == module)
                })
        }
        _ => false,
    }
}
