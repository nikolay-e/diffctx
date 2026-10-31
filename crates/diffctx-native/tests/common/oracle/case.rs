//! Case schema v1, as `tests/cases/impact/SCHEMA.md` documents it, and the
//! compilation of its convenience fields into typed relations.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use super::model::{Class, Kind, Node, Relation, Resolution, UnknownClass};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Impact,
    Symbol,
}

impl Mode {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Impact => "impact",
            Self::Symbol => "symbol",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueState {
    Open,
    Closed,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    #[serde(default)]
    pub name: String,
    pub mode: Mode,
    pub source: String,
    #[serde(default)]
    pub issue_state: Option<IssueState>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub repo: Repo,
    #[serde(default)]
    pub query: Query,
    pub expect: Expect,
    #[serde(default)]
    pub dispute: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Repo {
    #[serde(default)]
    pub initial_files: BTreeMap<String, String>,
    #[serde(default)]
    pub changed_files: BTreeMap<String, String>,
    #[serde(default)]
    pub deleted_files: Vec<String>,
    #[serde(default)]
    pub renamed_files: BTreeMap<String, String>,
    #[serde(default)]
    pub untracked_files: BTreeMap<String, String>,
    #[serde(default)]
    pub worktree: bool,
    #[serde(default = "default_commit_message")]
    pub commit_message: String,
}

fn default_commit_message() -> String {
    "Update files".into()
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Query {
    #[serde(default)]
    pub symbol: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelationSpec {
    pub from: String,
    pub to: String,
    pub kind: Kind,
    #[serde(default = "resolved")]
    pub resolution: Resolution,
    #[serde(default)]
    pub site: Option<u32>,
}

const fn resolved() -> Resolution {
    Resolution::Resolved
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Coverage {
    Complete,
    Partial,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

impl OneOrMany {
    pub fn items(&self) -> Vec<&str> {
        match self {
            Self::One(one) => vec![one.as_str()],
            Self::Many(many) => many.iter().map(String::as_str).collect(),
        }
    }
}

const fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expect {
    #[serde(default)]
    pub changed: Option<Vec<String>>,
    #[serde(default)]
    pub relations: Vec<RelationSpec>,
    #[serde(default = "yes")]
    pub exact: bool,
    #[serde(default)]
    pub decoys: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub coverage: BTreeMap<String, BTreeMap<String, Coverage>>,
    /// Present and empty: the answer names no unknown at all.
    #[serde(default)]
    pub unknowns: Option<BTreeMap<String, UnknownClass>>,
    #[serde(default)]
    pub definitions: Option<Vec<String>>,
    #[serde(default)]
    pub callers: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub tested_by: BTreeMap<String, OneOrMany>,
    #[serde(default)]
    pub untested: Vec<String>,
    #[serde(default)]
    pub not_untested: Vec<String>,
    #[serde(default)]
    pub stale: Vec<String>,
    #[serde(default)]
    pub not_stale: Vec<String>,
    #[serde(default)]
    pub named_by: Vec<String>,
    #[serde(default)]
    pub not_named_by: Vec<String>,
    #[serde(default)]
    pub contracts: Vec<String>,
    #[serde(default)]
    pub not_contracts: Vec<String>,
    /// Present and empty: the change removes no definition.
    #[serde(default)]
    pub removed: Option<Vec<String>>,
    #[serde(default)]
    pub never_empty: bool,
    #[serde(default)]
    pub discloses: bool,
    #[serde(default)]
    pub no_negative_claims: bool,
}

/// The expectation as typed relations, plus the (target, family) pairs it
/// lists: an empty set for a listed pair is an assertion of nothing.
pub struct Compiled {
    pub relations: Vec<Relation>,
    pub listed: BTreeSet<(Node, Class)>,
}

impl Expect {
    pub fn compile(&self) -> Result<Compiled, String> {
        let mut relations = Vec::new();
        let mut listed = BTreeSet::new();
        for spec in &self.relations {
            let to = Node::parse(&spec.to);
            listed.insert((to.clone(), spec.kind.class()));
            relations.push(Relation {
                from: Node::parse(&spec.from),
                to,
                kind: spec.kind,
                resolution: spec.resolution,
                sites: spec.site.into_iter().collect(),
                loose_kind: false,
            });
        }
        for (target, froms) in &self.callers {
            let to = Node::target(target);
            listed.insert((to.clone(), Class::Code));
            for from in froms {
                relations.push(Relation {
                    from: Node::parse(from),
                    to: to.clone(),
                    kind: Kind::Calls,
                    resolution: Resolution::Resolved,
                    sites: Vec::new(),
                    loose_kind: true,
                });
            }
        }
        for (file, tests) in &self.tested_by {
            let to = Node::parse(file);
            listed.insert((to.clone(), Class::Tests));
            for test in tests.items() {
                relations.push(Relation {
                    from: Node::parse(test),
                    to: to.clone(),
                    kind: Kind::TestsReach,
                    resolution: Resolution::Resolved,
                    sites: Vec::new(),
                    loose_kind: false,
                });
            }
        }
        for (target, kinds) in &self.coverage {
            for kind in kinds.keys() {
                if Kind::parse(kind).is_none() {
                    return Err(format!(
                        "coverage of `{target}` names `{kind}`, which is no relation kind"
                    ));
                }
            }
        }
        Ok(Compiled {
            relations: merge_sites(relations),
            listed,
        })
    }
}

/// One caller reached the target on several lines: the lines of its
/// relations pool into one, since the engine reports a caller once.
fn merge_sites(relations: Vec<Relation>) -> Vec<Relation> {
    let mut merged: Vec<Relation> = Vec::new();
    for relation in relations {
        let twin = merged.iter_mut().find(|m| {
            relation.from.symbol.is_some()
                && m.from == relation.from
                && m.to == relation.to
                && m.kind == relation.kind
                && m.resolution == relation.resolution
                && m.loose_kind == relation.loose_kind
        });
        match twin {
            Some(twin) => {
                for site in relation.sites {
                    if !twin.sites.contains(&site) {
                        twin.sites.push(site);
                    }
                }
            }
            None => merged.push(relation),
        }
    }
    merged
}

/// Parses and validates a case file. `stem` is the file name without its
/// extension; `dir_mode` is the corpus directory the file sits in.
pub fn parse(raw: &str, stem: &str, dir_mode: Mode) -> Result<Case, String> {
    let case: Case = serde_yaml::from_str(raw).map_err(|e| format!("parse YAML: {e}"))?;
    if !case.name.is_empty() && case.name != stem {
        return Err(format!(
            "name `{}` is not the file stem `{stem}`",
            case.name
        ));
    }
    if case.mode != dir_mode {
        return Err(format!(
            "mode `{}` in a `{}` directory",
            case.mode.name(),
            dir_mode.name()
        ));
    }
    check_source(&case.source)?;
    match case.mode {
        Mode::Symbol => {
            if case.query.symbol.as_deref().is_none_or(str::is_empty) {
                return Err("a symbol case needs query.symbol".into());
            }
        }
        Mode::Impact => {
            if case.query.symbol.is_some() {
                return Err("query.symbol belongs to symbol mode".into());
            }
            let repo = &case.repo;
            if repo.changed_files.is_empty()
                && repo.deleted_files.is_empty()
                && repo.renamed_files.is_empty()
                && repo.untracked_files.is_empty()
            {
                return Err("an impact case changes nothing".into());
            }
        }
    }
    for flag in &case.query.args {
        if matches!(
            flag.as_str(),
            "--diff" | "--mode" | "-f" | "--format" | "--symbol" | "-q"
        ) || flag.starts_with("--diff=")
            || flag.starts_with("--mode=")
            || flag.starts_with("--format=")
        {
            return Err(format!("query.args may not carry `{flag}`"));
        }
    }
    Ok(case)
}

fn check_source(source: &str) -> Result<(), String> {
    let number = source
        .strip_prefix("gh#")
        .or_else(|| source.strip_prefix("fj#"));
    let well_formed = source == "synthetic"
        || number.is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
    if well_formed {
        Ok(())
    } else {
        Err(format!(
            "source `{source}` is none of gh#N, fj#N, synthetic"
        ))
    }
}

/// Whether the case file at `path` carries `tag`; a file that does not parse
/// has none (its own run reports the error).
pub fn has_tag(path: &std::path::Path, tag: &str) -> bool {
    #[derive(Deserialize)]
    struct Tags {
        #[serde(default)]
        tags: Vec<String>,
    }
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_yaml::from_str::<Tags>(&raw).ok())
        .is_some_and(|t| t.tags.iter().any(|x| x == tag))
}
