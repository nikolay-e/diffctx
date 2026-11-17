//! The calls that hand a string to a shell, read from what a file imports.

use crate::facts::{Fact, Name, Star};

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
