//! A `Dockerfile`'s `ENV PYTHONPATH`: the directories of the repository the
//! image puts on the import path, read through the `COPY` that places them.

use super::{Draft, Files, Result, dir_of, join, name_of};

pub(super) fn python_path(files: &Files, draft: &mut Draft) -> Result<()> {
    let dockerfiles: Vec<&str> = files.all().filter(|p| is_dockerfile(name_of(p))).collect();
    for path in dockerfiles {
        let Some(text) = files.read(path)? else {
            continue;
        };
        let image = Image::read(&text);
        if image.python_path.is_empty() {
            continue;
        }
        let context = [dir_of(path), ""];
        let mut dirs = vec![dir_of(path).to_string()];
        for entry in &image.python_path {
            let found = image.repository_dirs(files, &context, entry);
            if found.is_empty() {
                draft.lose(
                    path,
                    format!("PYTHONPATH names /{entry}, which no COPY places from the repository"),
                );
            }
            dirs.extend(found);
        }
        dirs.sort();
        dirs.dedup();
        draft.mutual(dirs);
    }
    Ok(())
}

fn is_dockerfile(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name == "dockerfile" || name.starts_with("dockerfile.") || name.ends_with(".dockerfile")
}

#[derive(Default)]
struct Image {
    /// `(destination in the image, source directory in the context)`.
    copies: Vec<(String, String)>,
    /// The entries of every `ENV PYTHONPATH`, as normalised image paths.
    python_path: Vec<String>,
}

impl Image {
    fn read(text: &str) -> Self {
        let mut image = Self::default();
        let mut workdir = String::new();
        for line in logical_lines(text) {
            let (word, rest) = line.split_once(char::is_whitespace).unwrap_or((&line, ""));
            match word.to_ascii_uppercase().as_str() {
                "FROM" => workdir.clear(),
                "WORKDIR" => {
                    if let Some(dir) = image_path(&workdir, rest.trim()) {
                        workdir = dir;
                    }
                }
                "ENV" => {
                    for (key, value) in env_pairs(rest) {
                        if key == "PYTHONPATH" {
                            image.python_path.extend(path_entries(&workdir, &value));
                        }
                    }
                }
                "COPY" | "ADD" => image.copy(&workdir, rest),
                _ => {}
            }
        }
        image
    }

    fn copy(&mut self, workdir: &str, rest: &str) {
        let mut args = words(rest);
        if args.iter().any(|w| w.starts_with("--from")) {
            return;
        }
        args.retain(|w| !w.starts_with("--"));
        let Some(destination) = args.pop().and_then(|d| image_path(workdir, &d)) else {
            return;
        };
        for source in args {
            let source = source.trim_start_matches("./").trim_end_matches('/');
            if !source.contains(['*', '?', '[', '$']) {
                self.copies.push((destination.clone(), source.to_string()));
            }
        }
    }

    /// The repository directories behind `entry`: what a copy puts at it, and what
    /// a copy puts below it. Empty when no copy of the image names it.
    fn repository_dirs(&self, files: &Files, context: &[&str], entry: &str) -> Vec<String> {
        let mut found = Vec::new();
        for (destination, source) in &self.copies {
            let inside = below(entry, destination).and_then(|rest| join(source, rest));
            let beneath = below(destination, entry).and_then(|_| join(source, ""));
            for from in [inside, beneath].into_iter().flatten() {
                for base in context {
                    if let Some(dir) = join(base, &from).filter(|d| files.is_dir(d)) {
                        found.push(dir);
                    }
                }
            }
        }
        found
    }
}

/// The path of `path` below `prefix`, `""` for the prefix itself.
fn below<'a>(path: &'a str, prefix: &str) -> Option<&'a str> {
    if prefix.is_empty() {
        return Some(path);
    }
    let rest = path.strip_prefix(prefix)?;
    rest.strip_prefix('/')
        .or_else(|| rest.is_empty().then_some(""))
}

/// An image path against the working directory, without its leading slash.
fn image_path(workdir: &str, path: &str) -> Option<String> {
    match path.strip_prefix('/') {
        Some(absolute) => join("", absolute),
        None => join(workdir, path),
    }
}

fn path_entries(workdir: &str, value: &str) -> Vec<String> {
    value
        .split(':')
        .filter(|entry| !entry.is_empty() && !entry.contains("PYTHONPATH"))
        .filter_map(|entry| image_path(workdir, entry))
        .collect()
}

/// Instructions with their backslash continuations joined and comments dropped.
fn logical_lines(text: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with('#') {
            continue;
        }
        match line.strip_suffix('\\') {
            Some(more) => {
                current.push_str(more);
                current.push(' ');
            }
            None => {
                current.push_str(line);
                let done = std::mem::take(&mut current);
                if !done.trim().is_empty() {
                    lines.push(done.trim().to_string());
                }
            }
        }
    }
    lines
}

/// The words of an instruction, a quoted run one word with its quotes dropped.
/// A JSON array (`COPY ["a", "b"]`) is its strings.
fn words(text: &str) -> Vec<String> {
    let text = text.trim();
    if text.starts_with('[') {
        if let Ok(list) = serde_json::from_str::<Vec<String>>(text) {
            return list;
        }
    }
    let mut out = Vec::new();
    let mut word = String::new();
    let mut quote: Option<char> = None;
    for c in text.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            None if matches!(c, '"' | '\'') => quote = Some(c),
            None if c.is_whitespace() => {
                if !word.is_empty() {
                    out.push(std::mem::take(&mut word));
                }
            }
            _ => word.push(c),
        }
    }
    if !word.is_empty() {
        out.push(word);
    }
    out
}

/// `KEY=value KEY2=value` or the older `KEY value`.
fn env_pairs(rest: &str) -> Vec<(String, String)> {
    let words = words(rest);
    if words.first().is_some_and(|w| !w.contains('=')) {
        let (key, value) = rest
            .trim()
            .split_once(char::is_whitespace)
            .unwrap_or((rest, ""));
        return vec![(
            key.to_string(),
            value.trim().trim_matches(['"', '\'']).to_string(),
        )];
    }
    words
        .iter()
        .filter_map(|w| w.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}
