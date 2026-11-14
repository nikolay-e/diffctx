use std::fmt;

use serde::{Deserialize, Serialize};

use crate::snapshot::RepoPath;

/// The languages a cell exists for or is a candidate for. A file of any other
/// language has no facts and is no loss.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub enum Language {
    Python,
    JavaScript,
    TypeScript,
    Rust,
    Kotlin,
    Go,
    Java,
    Php,
    C,
    Cpp,
    /// YAML read as configuration data: objects, the references between them,
    /// the places they name.
    Config,
    /// A Helm chart's template (`templates/**.yaml`, `*.tpl`): YAML around Go
    /// template actions, read around them. Named templates and the values it
    /// reads are its facts.
    HelmTemplate,
    /// A Helm values file (`values.yaml`, `values-prod.yaml`): the keys it sets.
    HelmValues,
    Terraform,
    /// A Dockerfile: its stages, the places and variables its instructions name.
    Dockerfile,
    /// A shell script. The bash grammar reads `sh` and `bash`; a script whose
    /// shebang names another shell is unread, with a loss that says so.
    Shell,
    /// A script for a shell the bash grammar does not read (zsh, ksh, fish),
    /// by its extension or its shebang: no facts, and a loss that says so.
    ShellUnread,
    /// A web page: the scripts it embeds are read as JavaScript, at their own
    /// positions in the page (`facts::lang::html`).
    Html,
    /// An Astro component: its frontmatter and scripts, read as TypeScript.
    Astro,
    /// A Swift source file: the syntax facts of the Kotlin walk, read by the
    /// Kotlin reader and resolver in their Swift dialect.
    Swift,
    /// A SQL file: the routines and views it defines and the calls and reads it
    /// makes, read off the tokens.
    Sql,
}

/// How far into a file the shebang line is looked for.
const SHEBANG_BYTES: usize = 256;

impl Language {
    /// By the file name's extension.
    pub fn from_path(path: &RepoPath) -> Option<Self> {
        let name = path.file_name().to_ascii_lowercase();
        if matches!(name.as_str(), "dockerfile" | "containerfile")
            || name.starts_with("dockerfile.")
            || name.starts_with("containerfile.")
            || name.ends_with(".dockerfile")
            || name.ends_with(".containerfile")
        {
            return Some(Self::Dockerfile);
        }
        let (stem, extension) = path.file_name().rsplit_once('.')?;
        if stem.is_empty() {
            return None;
        }
        let extension = extension.to_ascii_lowercase();
        if is_helm_template(path.as_str(), &extension) {
            return Some(Self::HelmTemplate);
        }
        if matches!(extension.as_str(), "yaml" | "yml") && is_values_stem(stem) {
            return Some(Self::HelmValues);
        }
        Some(match extension.as_str() {
            "py" | "pyi" => Self::Python,
            "js" | "jsx" | "mjs" | "cjs" => Self::JavaScript,
            "ts" | "tsx" | "mts" | "cts" => Self::TypeScript,
            "rs" => Self::Rust,
            "kt" | "kts" => Self::Kotlin,
            "go" => Self::Go,
            "java" => Self::Java,
            "php" | "phtml" | "php3" | "php4" | "php5" | "php7" | "phps" => Self::Php,
            "c" | "h" => Self::C,
            "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" | "c++" | "h++" | "ipp" | "tpp" => {
                Self::Cpp
            }
            "yaml" | "yml" => Self::Config,
            "tf" | "tfvars" => Self::Terraform,
            "sh" | "bash" | "dash" => Self::Shell,
            "zsh" | "ksh" | "fish" => Self::ShellUnread,
            "html" | "htm" => Self::Html,
            "astro" => Self::Astro,
            "swift" => Self::Swift,
            "sql" => Self::Sql,
            _ => return None,
        })
    }

    /// The languages the configuration family reads: objects, their references,
    /// and what a chart's templates and values say.
    pub const fn is_config_family(self) -> bool {
        matches!(self, Self::Config | Self::HelmTemplate | Self::HelmValues)
    }

    /// Whether the file name carries no extension, which leaves its language to
    /// the shebang (#680).
    pub fn needs_shebang(path: &RepoPath) -> bool {
        !path.file_name().trim_start_matches('.').contains('.')
    }

    /// By the extension, else for a file without one by its shebang line.
    pub fn detect(path: &RepoPath, text: &[u8]) -> Option<Self> {
        Self::from_path(path).or_else(|| {
            Self::needs_shebang(path)
                .then(|| Self::from_shebang(text))
                .flatten()
        })
    }

    /// `#!/usr/bin/env python3`, `#!/usr/bin/python`, `#! /usr/bin/env -S node`.
    pub fn from_shebang(text: &[u8]) -> Option<Self> {
        let head = &text[..text.len().min(SHEBANG_BYTES)];
        let line = head.split(|byte| *byte == b'\n').next()?;
        #[allow(
            clippy::disallowed_methods,
            reason = "a first line that is not UTF-8 is not a shebang"
        )]
        let line = std::str::from_utf8(line).ok()?.strip_prefix("#!")?;
        let mut words = line.split_whitespace();
        let program = basename(words.next()?);
        let interpreter = if program == "env" {
            words.find(|word| !word.starts_with('-') && !word.contains('='))?
        } else {
            program
        };
        let interpreter = basename(interpreter);
        if interpreter.starts_with("python") {
            return Some(Self::Python);
        }
        match interpreter {
            "node" | "nodejs" | "bun" | "deno" => Some(Self::JavaScript),
            "ts-node" | "tsx" => Some(Self::TypeScript),
            "bash" | "sh" | "dash" | "ash" => Some(Self::Shell),
            "zsh" | "ksh" | "fish" => Some(Self::ShellUnread),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Python => "python",
            Self::JavaScript => "javascript",
            Self::TypeScript => "typescript",
            Self::Rust => "rust",
            Self::Kotlin => "kotlin",
            Self::Go => "go",
            Self::Java => "java",
            Self::Php => "php",
            Self::C => "c",
            Self::Cpp => "cpp",
            Self::Config => "config",
            Self::HelmTemplate => "helm-template",
            Self::HelmValues => "helm-values",
            Self::Terraform => "terraform",
            Self::Dockerfile => "dockerfile",
            Self::Shell => "shell",
            Self::ShellUnread => "shell-unread",
            Self::Html => "html",
            Self::Astro => "astro",
            Self::Swift => "swift",
            Self::Sql => "sql",
        }
    }
}

/// A file of a chart's `templates/` directory that is YAML or a named template,
/// or a `_*.tpl` anywhere.
fn is_helm_template(path: &str, extension: &str) -> bool {
    let in_templates = path.split('/').rev().skip(1).any(|dir| dir == "templates");
    match extension {
        "yaml" | "yml" => in_templates,
        "tpl" => in_templates || path.rsplit('/').next().is_some_and(|n| n.starts_with('_')),
        _ => false,
    }
}

/// `values`, `values-prod`, `values.prod`, `prod-values`, `prod.values`.
fn is_values_stem(stem: &str) -> bool {
    let lower = stem.to_ascii_lowercase();
    lower == "values"
        || lower.starts_with("values-")
        || lower.starts_with("values.")
        || lower.starts_with("values_")
        || lower.ends_with("-values")
        || lower.ends_with(".values")
        || lower.ends_with("_values")
}

fn basename(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

impl fmt::Display for Language {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}
