//! A field that names a place: an Argo CD `path`, a Kustomize `resources` item, a
//! Helmfile `chart`, a `COPY` source, a `source` of a script. Resolving one is
//! text work against the directory of the file that holds it, with no look at the
//! disk; what lies at the resolved path is the resolver's question.

use serde::{Deserialize, Serialize};

use crate::facts::Span;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum PointerKind {
    /// A file or a directory of the repository.
    Path,
    /// A file or a directory a list includes (a Kustomize `resources` entry, a
    /// promtool `rule_files` entry): the entry names it and reads none of what it
    /// says, which flows on through the objects it holds. Resolved as a `Path`.
    Listed,
    /// A chart: a directory of the repository when it is written as one
    /// (`./`, `../`, `/`, `file://`), else a name in a repository of charts or
    /// an OCI reference.
    Chart,
    /// A repository (`repoURL`, `source = "git::..."`): never a path.
    Repo,
    /// An image reference: never a path.
    Image,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SourcePointer {
    pub site: Span,
    pub kind: PointerKind,
    /// As written, decoded.
    pub text: String,
    /// A template wrote it: `text` is filler.
    pub templated: bool,
}

/// What a pointer's text names, read as text.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Resolved {
    /// A repository path, normalised, relative to the repository root; empty for
    /// the root itself. May hold glob characters.
    Local(String),
    /// Written from a root (`/apps/x`): the root of what the family means (the
    /// repository, a build context, a source), which the resolver of the family
    /// knows. Normalised, without the leading `/`.
    Rooted(String),
    /// Outside the repository: a URL, `git::`, `git@`, a registry or image
    /// reference, a path not of this repository.
    External,
    /// Climbs out of the repository (`../../x` from the top).
    Escapes,
    /// Not known from the text: written by a template, or a variable or a
    /// substitution (`$values/x`, `${DIR}/x`, `$(pwd)`, `~/x`) that something
    /// else expands.
    Computed,
}

impl SourcePointer {
    /// What the pointer names, from the directory (repository-relative, `""` for
    /// the root) of the file that holds it.
    pub fn resolve(&self, base: &str) -> Resolved {
        if self.templated {
            return Resolved::Computed;
        }
        match self.kind {
            PointerKind::Repo | PointerKind::Image => Resolved::External,
            PointerKind::Path | PointerKind::Listed => resolve_text(&self.text, base),
            PointerKind::Chart => {
                // `./charts/app`, `../app`, `/app` and `file://../app` are
                // directories; `stable/nginx` and `oci://...` are not.
                let text = self.text.trim();
                let file = text.strip_prefix("file://");
                let local = text.starts_with("./")
                    || text.starts_with("../")
                    || text.starts_with('/')
                    || matches!(text, "." | "..");
                match file {
                    Some(path) => resolve_text(path, base),
                    None if local => resolve_text(text, base),
                    None => Resolved::External,
                }
            }
        }
    }
}

impl SourcePointer {
    /// Whether the text names a place of this repository as far as the text
    /// says: not a remote, a registry reference or a computed path. Where it
    /// lands depends on the file that holds it and is [`Self::resolve`]'s.
    pub fn is_local(&self) -> bool {
        !matches!(
            self.resolve("a/b/c/d/e/f/g/h/i/j"),
            Resolved::External | Resolved::Computed
        )
    }
}

/// The directory of a repository path: `""` for a file at the root.
pub fn directory_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(directory, _)| directory)
}

fn resolve_text(text: &str, base: &str) -> Resolved {
    let text = text.trim();
    if text.is_empty() || text.starts_with(['$', '~']) || text.contains("${") || text.contains("$(")
    {
        return Resolved::Computed;
    }
    if is_remote(text) {
        return Resolved::External;
    }
    if let Some(rooted) = text.strip_prefix('/') {
        return match normalise(rooted, "") {
            Some(path) => Resolved::Rooted(path),
            None => Resolved::Escapes,
        };
    }
    match normalise(text, base) {
        Some(path) => Resolved::Local(path),
        None => Resolved::Escapes,
    }
}

/// `scheme://`, `git::`, `git@host:`, and the shorthands Kustomize and Terraform
/// read as a remote (`github.com/org/repo`).
fn is_remote(text: &str) -> bool {
    let scheme = text.split_once("://").is_some_and(|(scheme, _)| {
        !scheme.is_empty()
            && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    });
    scheme
        || text.contains("::")
        || text.starts_with("git@")
        || ["github.com/", "gitlab.com/", "bitbucket.org/"]
            .iter()
            .any(|host| text.starts_with(host))
}

/// `text` joined to `base`, with `.`, `..` and empty components resolved;
/// `None` when it climbs above the root.
fn normalise(text: &str, base: &str) -> Option<String> {
    let mut parts: Vec<&str> = base.split('/').filter(|part| !part.is_empty()).collect();
    for part in text.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            other => parts.push(other),
        }
    }
    Some(parts.join("/"))
}
