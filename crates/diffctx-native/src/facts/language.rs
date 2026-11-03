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
}

/// How far into a file the shebang line is looked for.
const SHEBANG_BYTES: usize = 256;

impl Language {
    /// By the file name's extension.
    pub fn from_path(path: &RepoPath) -> Option<Self> {
        let (stem, extension) = path.file_name().rsplit_once('.')?;
        if stem.is_empty() {
            return None;
        }
        Some(match extension.to_ascii_lowercase().as_str() {
            "py" | "pyi" => Self::Python,
            "js" | "jsx" | "mjs" | "cjs" => Self::JavaScript,
            "ts" | "tsx" | "mts" | "cts" => Self::TypeScript,
            "rs" => Self::Rust,
            "kt" | "kts" => Self::Kotlin,
            "go" => Self::Go,
            "java" => Self::Java,
            _ => return None,
        })
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
        }
    }
}

fn basename(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

impl fmt::Display for Language {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}
