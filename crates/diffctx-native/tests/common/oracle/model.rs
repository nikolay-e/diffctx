//! The vocabulary a case and an answer share: typed relations between
//! definitions and files.

use std::fmt;

use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Calls,
    Reads,
    Writes,
    Imports,
    Reexports,
    Overrides,
    Instantiates,
    PassesAsValue,
    Configures,
    Executes,
    ReferencesPath,
    TestsReach,
}

/// The families an answer tells apart without a registry: a reference (in
/// code, or a line naming a path) and a test that reaches code. Exactness is
/// judged per family because today's engine does not say which kind it found
/// (its `named_by` rows are always path references).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Class {
    Code,
    Tests,
}

impl Kind {
    const ALL: [Self; 12] = [
        Self::Calls,
        Self::Reads,
        Self::Writes,
        Self::Imports,
        Self::Reexports,
        Self::Overrides,
        Self::Instantiates,
        Self::PassesAsValue,
        Self::Configures,
        Self::Executes,
        Self::ReferencesPath,
        Self::TestsReach,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Calls => "calls",
            Self::Reads => "reads",
            Self::Writes => "writes",
            Self::Imports => "imports",
            Self::Reexports => "reexports",
            Self::Overrides => "overrides",
            Self::Instantiates => "instantiates",
            Self::PassesAsValue => "passes_as_value",
            Self::Configures => "configures",
            Self::Executes => "executes",
            Self::ReferencesPath => "references_path",
            Self::TestsReach => "tests_reach",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.name() == name)
    }

    pub const fn class(self) -> Class {
        match self {
            Self::TestsReach => Class::Tests,
            _ => Class::Code,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resolution {
    Resolved,
    Candidate,
    Unresolved,
}

impl Resolution {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Resolved => "resolved",
            Self::Candidate => "candidate",
            Self::Unresolved => "unresolved",
        }
    }
}

/// Why the analysis could not say who reaches a definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnknownClass {
    NotModelled,
    Dispatched,
    Ambiguous,
    CommonWord,
    UnresolvedReads,
    Overridable,
}

impl UnknownClass {
    pub const fn name(self) -> &'static str {
        match self {
            Self::NotModelled => "not_modelled",
            Self::Dispatched => "dispatched",
            Self::Ambiguous => "ambiguous",
            Self::CommonWord => "common_word",
            Self::UnresolvedReads => "unresolved_reads",
            Self::Overridable => "overridable",
        }
    }

    /// The class of the sentence the engine renders an `Unknown` as.
    pub fn of_sentence(sentence: &str) -> Option<Self> {
        if sentence.starts_with("implements `") && sentence.contains("calls through `dyn ") {
            Some(Self::Dispatched)
        } else if sentence.contains(" is ambiguous: ") {
            Some(Self::Ambiguous)
        } else if sentence.contains(" is a whole word on ") {
            Some(Self::CommonWord)
        } else if sentence.contains(" read(s) of `.") {
            Some(Self::UnresolvedReads)
        } else if sentence.starts_with("calls through ")
            && sentence.contains(" are not traced, and no direct caller was found")
        {
            Some(Self::Overridable)
        } else if sentence.starts_with("no binding model reads ") {
            Some(Self::NotModelled)
        } else {
            None
        }
    }
}

/// A definition (`path::symbol`) or a file (`path`). A node with an empty
/// path names a symbol wherever it is defined.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Node {
    pub path: String,
    pub symbol: Option<String>,
}

impl Node {
    pub fn new(path: &str, symbol: Option<&str>) -> Self {
        Self {
            path: path.to_string(),
            symbol: symbol.map(str::to_string),
        }
    }

    /// `path::symbol`, or a bare `path`.
    pub fn parse(text: &str) -> Self {
        match text.split_once("::") {
            Some((path, symbol)) => Self::new(path, Some(symbol)),
            None => Self::new(text, None),
        }
    }

    /// Where a target is named: a bare identifier is a symbol anywhere, the
    /// rest reads as `parse`.
    pub fn target(text: &str) -> Self {
        if !text.contains("::") && is_identifier(text) {
            Self::new("", Some(text))
        } else {
            Self::parse(text)
        }
    }

    /// Whether `self`, read as an expectation, names `found`: an empty path
    /// or a missing symbol stands for any, but a symbol the expectation names
    /// is never satisfied by a node without one (an import line is not the
    /// function below it).
    pub fn covers(&self, found: &Self) -> bool {
        (self.path.is_empty() || self.path == found.path)
            && self.symbol.as_ref().is_none_or(|wanted| {
                found.symbol.as_ref().is_some_and(|have| {
                    // The engine names a member by its bare name; a case may
                    // write `Container.name` or `Container::name`.
                    have == wanted || (!have.contains(['.', ':']) && last_segment(wanted) == have)
                })
            })
    }
}

fn last_segment(symbol: &str) -> &str {
    symbol.rsplit(['.', ':']).next().unwrap_or(symbol)
}

impl fmt::Display for Node {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.path.is_empty(), &self.symbol) {
            (true, Some(symbol)) => write!(f, "{symbol}"),
            (_, Some(symbol)) => write!(f, "{}::{symbol}", self.path),
            (_, None) => write!(f, "{}", self.path),
        }
    }
}

fn is_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    chars
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$')
        && chars.all(|c| c.is_alphanumeric() || c == '_' || c == '$')
}

#[derive(Debug, Clone)]
pub struct Relation {
    pub from: Node,
    pub to: Node,
    pub kind: Kind,
    pub resolution: Resolution,
    /// Lines of the reference in the `from` file. An expectation's sites must
    /// all be among an answer's.
    pub sites: Vec<u32>,
    /// A convenience field named no kind: any code kind satisfies it.
    pub loose_kind: bool,
}

impl Relation {
    pub fn show(&self) -> String {
        let sites = if self.sites.is_empty() {
            String::new()
        } else {
            let lines: Vec<String> = self.sites.iter().map(u32::to_string).collect();
            format!(", line {}", lines.join("+"))
        };
        format!(
            "{} -{}-> {} [{}{sites}]",
            self.from,
            self.kind.name(),
            self.to,
            self.resolution.name()
        )
    }
}

/// Cells the registry declares `precise`: for them `kind` is compared
/// strictly. The registry does not exist yet (wave 2), so none is.
const PRECISE: &[(&str, Kind)] = &[];

pub fn precise(language: &str, kind: Kind) -> bool {
    PRECISE.iter().any(|(l, k)| *l == language && *k == kind)
}
