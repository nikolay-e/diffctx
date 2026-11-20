//! The names a config file defines that a reader elsewhere spells back: a
//! compose service, a CI job or workflow, a tox env or supervisor program, a
//! package's export subpath. Read from the same parse the key table uses; a key
//! nested under any of them is a setting, never a name.

use rustc_hash::FxHashSet;

use super::extract::{FileClass, children, classify, parse_yaml, unquote};

/// A top-level GitLab key is a job when it holds one of these.
const GITLAB_JOB_KEYS: &[&str] = &["script", "trigger", "extends"];

/// The ini sections whose suffix a command line names (`tox -e lint`,
/// `supervisorctl restart mailer`); `[coverage:run]` is one tool's own section.
const NAMED_SECTIONS: &[&str] = &[
    "testenv",
    "program",
    "group",
    "eventlistener",
    "fcgi-program",
];

pub fn defined_names(rel: &str, text: &str) -> FxHashSet<String> {
    let file = rel.rsplit('/').next().unwrap_or(rel);
    let ext = file.rsplit_once('.').map_or("", |(_, e)| e);
    let names = if matches!(ext, "ini" | "cfg" | "conf") {
        ini_sections(text)
    } else {
        match classify(rel) {
            Some(FileClass::Yaml { compose }) => yaml_definitions(rel, text, compose),
            Some(FileClass::Json) if file == "package.json" => package_exports(text),
            _ => Vec::new(),
        }
    };
    names
        .into_iter()
        .filter(|n| n.chars().count() >= 2 && !n.contains(['$', '{', '*']))
        .collect()
}

fn yaml_definitions(rel: &str, text: &str, compose: bool) -> Vec<String> {
    let entries = parse_yaml(text);
    let kids = children(&entries);
    let top = |i: usize| entries[i].parent.is_none() && !entries[i].plain;
    let under = |i: usize, parent: &str| {
        entries[i]
            .parent
            .is_some_and(|p| top(p) && entries[p].key == parent)
    };
    let workflow = rel.contains(".github/workflows/");
    (0..entries.len())
        .filter(|&i| !entries[i].plain && !entries[i].key.is_empty())
        .filter_map(|i| {
            let entry = &entries[i];
            if compose {
                under(i, "services").then(|| entry.key.clone())
            } else if workflow {
                if under(i, "jobs") {
                    Some(entry.key.clone())
                } else {
                    (top(i) && entry.key == "name").then(|| unquote(&entry.value).to_string())
                }
            } else {
                (top(i)
                    && kids[i]
                        .iter()
                        .any(|&k| GITLAB_JOB_KEYS.contains(&entries[k].key.as_str())))
                .then(|| entry.key.clone())
            }
        })
        .collect()
}

fn ini_sections(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| {
            let header = line.trim().strip_prefix('[')?.strip_suffix(']')?;
            let (prefix, suffix) = header.split_once(':')?;
            NAMED_SECTIONS
                .contains(&prefix.trim())
                .then(|| suffix.trim().to_string())
        })
        .collect()
}

/// The subpath each `exports` key opens: `./legacy` is imported as `pkg/legacy`.
fn package_exports(text: &str) -> Vec<String> {
    let Ok(serde_json::Value::Object(package)) = serde_json::from_str(text) else {
        return Vec::new();
    };
    let Some(serde_json::Value::Object(exports)) = package.get("exports") else {
        return Vec::new();
    };
    exports
        .keys()
        .filter_map(|key| key.strip_prefix('.'))
        .map(|subpath| subpath.trim_start_matches('/').to_string())
        .collect()
}
