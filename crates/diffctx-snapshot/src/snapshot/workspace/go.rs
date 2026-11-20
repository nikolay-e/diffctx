//! Go: `go.mod` (module path, requirements, local replacements) and
//! `go.work` (the modules that resolve one another).

use super::{Draft, Ecosystem, Files, Result, RootOrigin, dir_of, join};

pub(super) fn discover(files: &Files, draft: &mut Draft) -> Result<()> {
    for path in files.named("go.mod") {
        let root = dir_of(path);
        let Some(text) = files.read(path)? else {
            continue;
        };
        let module = read_mod(root, &text);
        let project = draft.project(root, Ecosystem::Go);
        project.root(root.to_string(), RootOrigin::Manifest);
        if let Some(path) = module.path {
            project.name(Ecosystem::Go, path.clone());
            project.go_module = Some(path);
        }
        for required in module.required {
            project.link_name(Ecosystem::Go, required);
        }
        for dir in module.replaced_with_dirs {
            project.link_dir(dir);
        }
    }
    for path in files.named("go.work") {
        let root = dir_of(path);
        let Some(text) = files.read(path)? else {
            continue;
        };
        let dirs: Vec<String> = directives(&text, "use")
            .into_iter()
            .filter_map(|d| join(root, &d))
            .collect();
        if dirs.len() > 1 {
            draft.mutual(dirs);
        }
    }
    Ok(())
}

struct Module {
    path: Option<String>,
    required: Vec<String>,
    replaced_with_dirs: Vec<String>,
}

fn unquote(word: &str) -> &str {
    word.trim_matches(|c| c == '"' || c == '`')
}

/// The argument lists of `keyword x` and `keyword ( x \n y )`, comments
/// removed, one entry per line.
fn directives(text: &str, keyword: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_block = false;
    for raw in text.lines() {
        let line = raw.split("//").next().unwrap_or("").trim();
        if in_block {
            if line == ")" {
                in_block = false;
            } else if !line.is_empty() {
                out.push(unquote(line).to_string());
            }
            continue;
        }
        let Some(rest) = line.strip_prefix(keyword) else {
            continue;
        };
        if !rest.starts_with(char::is_whitespace) && !rest.starts_with('(') {
            continue;
        }
        let rest = rest.trim();
        if rest == "(" {
            in_block = true;
        } else if !rest.is_empty() {
            out.push(unquote(rest).to_string());
        }
    }
    out
}

fn read_mod(root: &str, text: &str) -> Module {
    let path = directives(text, "module")
        .into_iter()
        .next()
        .map(|m| unquote(&m).to_string());
    let required = directives(text, "require")
        .iter()
        .filter_map(|r| r.split_whitespace().next())
        .map(|m| unquote(m).to_string())
        .collect();
    let replaced_with_dirs = directives(text, "replace")
        .iter()
        .filter_map(|r| r.split_once("=>"))
        .filter_map(|(_, target)| target.split_whitespace().next())
        .map(unquote)
        .filter(|t| t.starts_with("./") || t.starts_with("../") || *t == "." || *t == "..")
        .filter_map(|t| join(root, t))
        .collect();
    Module {
        path,
        required,
        replaced_with_dirs,
    }
}
