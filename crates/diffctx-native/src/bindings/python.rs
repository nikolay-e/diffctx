//! Python imports: `import a.b as c`, `from .m import x`, read off the text as a map of
//! the names a file binds. The selection graph's Python builder is the only reader.

use once_cell::sync::Lazy;
use regex::Regex;

use super::{FileBindings, Target};

static PY_IMPORT: Lazy<Regex> = Lazy::new(|| Regex::new(r"^import\s+(.+)$").unwrap());
static PY_FROM: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^from\s+([.\w]+)\s+import\s+(.+)$").unwrap());

/// Import statements as single logical lines: parenthesised and
/// backslash-continued imports joined, comments dropped.
fn paren_balance(line: &str) -> isize {
    line.chars()
        .map(|c| match c {
            '(' => 1,
            ')' => -1,
            _ => 0,
        })
        .sum()
}

fn python_statements(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut pending: Option<String> = None;
    let mut depth = 0isize;
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if let Some(acc) = pending.as_mut() {
            acc.push(' ');
            acc.push_str(line.trim_end_matches('\\'));
            depth += paren_balance(line);
            if depth <= 0 && !line.ends_with('\\') {
                out.push(pending.take().unwrap_or_default());
            }
            continue;
        }
        if !(line.starts_with("import ") || line.starts_with("from ")) {
            continue;
        }
        depth = paren_balance(line);
        if depth > 0 || line.ends_with('\\') {
            pending = Some(line.trim_end_matches('\\').to_string());
        } else {
            out.push(line.to_string());
        }
    }
    out.extend(pending);
    out
}

pub fn python_bindings(text: &str) -> FileBindings {
    let mut b = FileBindings::default();
    for stmt in python_statements(text) {
        let stmt = stmt.replace(['(', ')'], " ");
        if let Some(c) = PY_FROM.captures(stmt.trim()) {
            for item in c[2].split(',') {
                bind_from_item(&mut b, &c[1], item);
            }
        } else if let Some(c) = PY_IMPORT.captures(stmt.trim()) {
            for item in c[1].split(',') {
                bind_import_item(&mut b, item);
            }
        }
    }
    b
}

/// `x`, `x as y` or `*` of `from module import …`.
fn bind_from_item(b: &mut FileBindings, module: &str, item: &str) {
    let words: Vec<&str> = item.split_whitespace().collect();
    let (name, local) = match words.as_slice() {
        ["*"] => {
            b.star_from.push(module.to_string());
            return;
        }
        [name] => (*name, *name),
        [name, "as", local] => (*name, *local),
        _ => return,
    };
    b.names.insert(
        local.to_string(),
        Target::Member {
            module: module.to_string(),
            name: name.to_string(),
        },
    );
}

/// `a.b.c` (binds `a`) or `a.b as x` (binds `x` to `a.b`) of `import …`.
fn bind_import_item(b: &mut FileBindings, item: &str) {
    let words: Vec<&str> = item.split_whitespace().collect();
    let (local, module) = match words.as_slice() {
        [dotted] => {
            let head = dotted.split('.').next().unwrap_or(dotted);
            (head, head)
        }
        [dotted, "as", local] => (*local, *dotted),
        _ => return,
    };
    b.names
        .insert(local.to_string(), Target::Module(module.to_string()));
}
