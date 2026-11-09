//! Configuration files read as data: YAML, JSON, TOML and INI into one tree of
//! nodes with spans, and from it the facts the config families of wave 7 are
//! built on.
//!
//! - [`read`] gives a [`Reading`]: the documents as [`Node`]s, the line ranges a
//!   parse could not read, the templates it read around, and a [`Flaw`] for every
//!   way the read stopped short. A read that is not whole says so; it never looks
//!   like an empty file.
//! - [`objects`] are the Kubernetes-style objects a reading states, by identity
//!   (group, kind, namespace, name).
//! - [`extract`] reads the references between them through a table of [`Row`]s:
//!   a field is a reference because a row names it.
//! - A [`SourcePointer`] is a field that names a place, resolved as text against
//!   the directory of its file.
//!
//! Nothing here reads a repository or runs a tool: the input is bytes, and the
//! same bytes give the same facts, as the cache that will key on them needs.
//! These types are not yet [`crate::facts::Fact`]s: no cell reads them, and the
//! blob facts, the cache schema and [`crate::facts::Language`] stay as they are
//! until the first config cell does.

#![allow(clippy::cognitive_complexity)]

mod candidates;
pub mod dockerfile;
mod helm;
mod ini;
mod json;
pub mod k8s;
mod object;
mod pointer;
mod pointers;
mod reference;
mod selectors;
pub(crate) mod syntax;
mod template;
mod toml_file;
mod tree;
mod yaml;

use serde::{Deserialize, Serialize};

use crate::facts::Span;
use crate::snapshot::RunBudget;

pub use candidates::{Candidate, candidates};
pub use helm::{Analysis, ConfigUse, Define, UseOf, analyse};
pub use object::{Found, Object, ObjectId, identity_of, local_key, objects};
pub use pointer::{PointerKind, Resolved, SourcePointer, directory_of};
pub use pointers::{Release, document_kind, pointers_of, releases_of};
pub use reference::{ConfigRef, Row, Strength, extract};
pub use selectors::{LabelSet, Labels, Selector, SelectorRow, pairs_of, selectors};
pub use template::Template;
pub use tree::{Entry, FieldPath, Key, Match, Node, Scalar, ScalarKind, Segment, Value};

/// The syntax a file is read in.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub enum Dialect {
    Yaml,
    Json,
    Toml,
    Ini,
}

impl Dialect {
    /// By the file name: `.yaml`, `.yml`, `.json`, `.toml`, `.ini`, `.cfg` and
    /// `.editorconfig`. `.conf` is no dialect: nginx, systemd and sshd share it.
    pub fn of_path(path: &str) -> Option<Self> {
        let name = path.rsplit('/').next()?.to_ascii_lowercase();
        if name == ".editorconfig" {
            return Some(Self::Ini);
        }
        let (stem, extension) = name.rsplit_once('.')?;
        if stem.is_empty() {
            return None;
        }
        Some(match extension {
            "yaml" | "yml" => Self::Yaml,
            "json" => Self::Json,
            "toml" => Self::Toml,
            "ini" | "cfg" => Self::Ini,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Yaml => "yaml",
            Self::Json => "json",
            Self::Toml => "toml",
            Self::Ini => "ini",
        }
    }
}

/// How `{{ ... }}` in a YAML file is read. A Go template is not YAML; only the
/// caller knows whether a file is one (a chart's `templates/`, a Helmfile's
/// `.gotmpl`), because `{{ }}` is also ordinary text in an alert rule or a
/// GitHub Actions expression.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Templating {
    /// Read the text as it stands: an action is YAML or a parse error.
    #[default]
    None,
    /// Read around the actions of a Go template; see [`Template`].
    Go,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Options {
    pub templating: Templating,
}

/// A way a read stopped short of the whole file.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Flaw {
    /// The bytes are not UTF-8: nothing was read.
    NotUtf8,
    /// This build has no reader for the dialect.
    Unsupported,
    /// The run's deadline or cancellation came; what follows was not read.
    Deadline,
    /// Nesting past the depth the walk follows; the node is null.
    TooDeep,
    /// More nodes than one file may make (aliases that expand without end
    /// among them); the rest is null.
    TooLarge,
    /// `*name` with no `&name` before it, on `line`; the node is null. YAML
    /// requires the anchor before the alias, so this is a parse error and the
    /// line is in [`Reading::errors`] as well.
    UnresolvedAlias { name: String, line: u32 },
}

/// What one file says, as data.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Reading {
    pub dialect: Dialect,
    /// A YAML stream has one per `---` document, JSON one per top-level value,
    /// TOML and INI one.
    pub documents: Vec<Node>,
    /// The Go template actions read around, in source order.
    pub templates: Vec<Template>,
    /// The 1-based line ranges the grammar could not read; the facts around
    /// them are still given.
    pub errors: Vec<(u32, u32)>,
    pub flaws: Vec<Flaw>,
}

impl Reading {
    fn new(dialect: Dialect) -> Self {
        Self {
            dialect,
            documents: Vec::new(),
            templates: Vec::new(),
            errors: Vec::new(),
            flaws: Vec::new(),
        }
    }

    /// Whether the file was read to its end without a parse error: only then is
    /// what is not in the documents not in the file.
    pub fn is_whole(&self) -> bool {
        self.errors.is_empty() && self.flaws.is_empty()
    }

    /// The templates read around inside a region of the file.
    pub fn templates_in<'a>(&'a self, span: &'a Span) -> impl Iterator<Item = &'a Template> + 'a {
        self.templates.iter().filter(move |t| {
            t.span.bytes.start < span.bytes.end && span.bytes.start < t.span.bytes.end
        })
    }
}

/// Reads `text` in `dialect`. Pure: the same bytes and options give the same
/// reading. Every walk of the input polls the run's deadline.
pub fn read(dialect: Dialect, text: &[u8], options: Options, budget: &RunBudget) -> Reading {
    let mut reading = Reading::new(dialect);
    let Ok(source) = std::str::from_utf8(text) else {
        reading.flaws.push(Flaw::NotUtf8);
        return reading;
    };
    match dialect {
        Dialect::Yaml => yaml::read(source, options, budget, &mut reading),
        Dialect::Json => json::read(source, budget, &mut reading),
        Dialect::Toml => toml_file::read(source, budget, &mut reading),
        Dialect::Ini => ini::read(source, budget, &mut reading),
    }
    reading
}
