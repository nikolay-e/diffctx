//! The shell commands a file hands to a shell: a CI step's `run:` or `script:`,
//! a container's `command:` or `entrypoint:`, a manifest's `scripts`, a
//! Dockerfile's `RUN`, `CMD` and `ENTRYPOINT`. What the command says is the
//! shell's to read; the tree says only that it is one.

use super::{Dialect, Node, Options, Value, dockerfile, read};
use crate::snapshot::RunBudget;

/// A command and the lines of the file it spans.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Command {
    pub lines: (u32, u32),
    pub text: String,
}

/// How a list under a key reads: a script's items are commands of their own, a
/// container's are the words of one.
#[derive(Clone, Copy)]
enum Items {
    Commands,
    Words,
}

const KEYS: &[(&str, Items)] = &[
    ("run", Items::Commands),
    ("script", Items::Commands),
    ("before_script", Items::Commands),
    ("after_script", Items::Commands),
    ("command", Items::Words),
    ("entrypoint", Items::Words),
];
/// The keys whose map holds a command in each value.
const MAPS: &[&str] = &["scripts"];
const INSTRUCTIONS: &[&str] = &["RUN", "CMD", "ENTRYPOINT"];

/// The commands of the file `path` holds, by its name: a Dockerfile, a YAML file
/// or a JSON one. Any other file holds none.
pub fn commands(path: &str, text: &str, budget: &RunBudget) -> Vec<Command> {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    if name.starts_with("dockerfile") || name.ends_with(".dockerfile") {
        return dockerfile_commands(text);
    }
    let dialect = match name.rsplit_once('.') {
        Some((_, "yaml" | "yml")) => Dialect::Yaml,
        Some((_, "json")) => Dialect::Json,
        _ => return Vec::new(),
    };
    let reading = read(dialect, text.as_bytes(), Options::default(), budget);
    let mut found = Vec::new();
    for document in &reading.documents {
        walk(document, &mut found);
    }
    found
}

fn dockerfile_commands(text: &str) -> Vec<Command> {
    dockerfile::parse(text)
        .instructions
        .into_iter()
        .filter(|i| !i.heredoc && INSTRUCTIONS.contains(&i.keyword.as_str()))
        .map(|i| Command {
            lines: i.span.lines,
            text: i
                .words
                .iter()
                .map(|w| w.text.as_str())
                .collect::<Vec<_>>()
                .join(" "),
        })
        .collect()
}

fn walk(node: &Node, found: &mut Vec<Command>) {
    match &node.value {
        Value::Map(entries) => {
            for entry in entries {
                let key = entry.key.text.as_str();
                if let Some((_, items)) = KEYS.iter().find(|(k, _)| *k == key) {
                    held(&entry.value, *items, found);
                } else if MAPS.contains(&key) {
                    for script in entry.value.entries() {
                        held(&script.value, Items::Commands, found);
                    }
                }
                walk(&entry.value, found);
            }
        }
        Value::Seq(items) => items.iter().for_each(|item| walk(item, found)),
        _ => {}
    }
}

fn held(value: &Node, items: Items, found: &mut Vec<Command>) {
    let command = |node: &Node, text: String| Command {
        lines: node.span.lines,
        text,
    };
    match (&value.value, items) {
        (Value::Scalar(_), _) => found.extend(text_of(value).map(|t| command(value, t))),
        (Value::Seq(list), Items::Commands) => {
            found.extend(
                list.iter()
                    .filter_map(|i| text_of(i).map(|t| command(i, t))),
            );
        }
        (Value::Seq(list), Items::Words) => {
            let words: Vec<String> = list.iter().filter_map(text_of).collect();
            found.push(command(value, words.join(" ")));
        }
        _ => {}
    }
}

fn text_of(node: &Node) -> Option<String> {
    node.as_str()
        .filter(|_| !node.templated)
        .map(str::to_string)
}
