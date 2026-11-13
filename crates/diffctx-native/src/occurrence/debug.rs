//! The shadow of the accounting, written. Under `DIFFCTX_DEBUG_OCCURRENCES=<file>`
//! the occurrences of each changed definition are accounted beside the coverage the
//! answer took, and one JSON line is appended per occurrence and per (definition,
//! relation). Unset, nothing here runs; an answer never reads any of it.

use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::Arc;

use serde_json::json;
use tree_sitter::Tree;

use super::{
    Account, Alias, AliasScope, Basis, Entry, Known, Occurrence, Outcome, Unread, Verdict, View,
    admitted, close_aliases, counts_against, implicit_kinds, outcomes, value_use,
};
use crate::decide::{Coverage, Family, Gap};
use crate::facts::{FactsCache, FileFacts, Name, facts_of};
use crate::resolve::{Definition, Relation};
use crate::snapshot::{AnalysisContext, Loss, RepoPath, SideTag};
use crate::tokens::{GroupTokens, Names};

const ENV: &str = "DIFFCTX_DEBUG_OCCURRENCES";

/// The file the knob names, opened for appending; `None` when it is unset.
pub fn sink() -> Option<File> {
    let path = std::env::var_os(ENV)?;
    let file = OpenOptions::new().create(true).append(true).open(path);
    file.map_err(|error| eprintln!("{ENV}: {error}")).ok()
}

/// What the cell knows of one changed definition's group, as the accounting asks.
pub struct Shadow<'a> {
    pub ctx: &'a AnalysisContext,
    pub cache: &'a FactsCache,
    pub family: Family,
    pub view: &'a View<'a>,
    /// The head's files the resolver read, by path.
    pub files: &'a [FileFacts],
    pub tokens: &'a GroupTokens,
    pub reach: &'a dyn Fn(&str) -> bool,
    /// Files the resolver read that no extension puts in the group.
    pub outside: &'a [RepoPath],
    /// The reference names of the definition.
    pub names: &'a [String],
    /// What the answer took, by relation.
    pub today: &'a [(Relation, Coverage)],
}

#[derive(Default)]
struct Found {
    entries: Vec<Entry>,
    unread: HashMap<RepoPath, Loss>,
    mentions: usize,
    files: HashSet<RepoPath>,
}

fn label(definition: &Definition) -> String {
    let local = &definition.key.local;
    let names = local.container.iter().chain([&local.name]);
    let path: Vec<&str> = names.map(Name::as_str).collect();
    format!("{}::{}", definition.file, path.join("."))
}

impl Shadow<'_> {
    /// Appends the lines of `definition`, read at `side`, to `out`.
    pub fn write(&self, out: &mut File, definition: &Definition, side: SideTag, known: Known) {
        let mut found = Found::default();
        let names: Vec<Name> = self
            .names
            .iter()
            .map(|name| Name::from(name.as_str()))
            .collect();
        let (_, alias_bound) = close_aliases(&names, |name, scope| {
            self.account_name(&mut found, definition, name, scope)
        });
        let implicit = match facts_of(self.ctx, side, self.cache, &definition.file) {
            Ok(Some(home)) => home
                .defs()
                .find(|def| home.key(&def.local) == definition.key)
                .map_or_else(Vec::new, |def| implicit_kinds(&home, def, known)),
            _ => Vec::new(),
        };
        let (def, side_text, family) = (
            label(definition),
            format!("{side:?}").to_lowercase(),
            format!("{:?}", self.family).to_lowercase(),
        );
        let unread: Vec<Unread> = (found.unread.iter())
            .map(|(file, loss)| Unread {
                file: file.clone(),
                loss: loss.clone(),
            })
            .collect();
        let mut lines = String::new();
        let mut trees = HashMap::new();
        for entry in &found.entries {
            let at = &entry.occurrence;
            lines.push_str(
                &json!({
                    "def": def, "side": side_text, "family": family,
                    "file": at.file.as_str(), "line": at.line,
                    "bytes": [at.bytes.start, at.bytes.end],
                    "class": at.class.as_str(), "outcome": entry.outcome.as_str(),
                    "basis": entry.basis.as_str(),
                    "relation": entry.relation.map(Relation::name),
                    "detail": format!("{:?}", entry.outcome),
                    "position": self.position(&mut trees, at),
                })
                .to_string(),
            );
            lines.push('\n');
        }
        // The relations the answer covers, then those an unexplained occurrence may
        // be a use through that it has no coverage of.
        let unanswered = admitted(definition.kind)
            .iter()
            .filter(|relation| self.today.iter().all(|(known, _)| known != *relation))
            .map(|relation| (*relation, None));
        let pairs = (self.today.iter())
            .map(|(relation, coverage)| (*relation, Some(coverage)))
            .chain(unanswered);
        for (relation, coverage) in pairs {
            let mut account = Account::new(definition.key.clone(), relation, side);
            account.files = found.files.len();
            account.entries = found
                .entries
                .iter()
                .filter(|entry| {
                    !matches!(entry.outcome, Outcome::Candidate(_))
                        || counts_against(entry, definition.kind, relation)
                })
                .cloned()
                .collect();
            account.unread.clone_from(&unread);
            account.mentions = found.mentions;
            account.implicit.clone_from(&implicit);
            account.alias_bound = alias_bound;
            let verdict = account.verdict();
            let reasons: Vec<String> = match &verdict {
                Verdict::Complete => Vec::new(),
                Verdict::Partial(hedges) => hedges.iter().map(ToString::to_string).collect(),
            };
            // What the answer was also held by, besides the file status and the
            // unplaced uses that occurrences replace.
            let gaps: Vec<String> = (coverage.into_iter())
                .flat_map(|coverage| coverage.gaps())
                .filter(|gap| !matches!(gap, Gap::Unaccounted { .. } | Gap::Unlanded { .. }))
                .map(ToString::to_string)
                .collect();
            let today = coverage.map(|coverage| {
                if coverage.witness().is_some() {
                    "complete"
                } else {
                    "partial"
                }
            });
            lines.push_str(
                &json!({
                    "def": def, "side": side_text, "relation": relation.name(),
                    "today": today,
                    "occurrences": verdict.as_str(), "reasons": reasons, "gaps": gaps,
                })
                .to_string(),
            );
            lines.push('\n');
        }
        if let Err(error) = out.write_all(lines.as_bytes()) {
            eprintln!("{ENV}: {error}");
        }
    }

    /// Accounts the occurrences of one name, a reference name of the definition
    /// (`scope` is `None`) or an alias found before, and returns the aliases they
    /// make. A use of an alias is a use of the definition: it is read as a value
    /// position or an alias again, and else stays `ours`.
    fn account_name(
        &self,
        found: &mut Found,
        definition: &Definition,
        name: &Name,
        scope: Option<&AliasScope>,
    ) -> Vec<Alias> {
        let mut aliases = Vec::new();
        for file in self.spelling(name) {
            if matches!(scope, Some(AliasScope::File(only)) if *only != file) {
                continue;
            }
            found.files.insert(file.clone());
            let unread = Loss::ReadFailed {
                path: file.clone(),
                side: SideTag::Head,
            };
            let Some((facts, text)) = self.read(&file) else {
                found.unread.entry(file).or_insert(unread);
                continue;
            };
            let done = outcomes(self.view, facts, &text, definition, &[name.as_str()]);
            found.mentions += done.mentions;
            if done.lost && done.entries.is_empty() {
                let loss = facts.losses().into_iter().next().unwrap_or(unread);
                found.unread.entry(file).or_insert(loss);
            }
            for mut entry in done.entries {
                let through = scope.is_some()
                    && matches!(entry.outcome, Outcome::NotOurs)
                    && entry.basis == Basis::OtherDefinition;
                if through {
                    entry.outcome = Outcome::Ours;
                    entry.basis = Basis::Resolved;
                }
                if matches!(entry.outcome, Outcome::Ours) {
                    let relation = entry.relation.filter(|_| !through);
                    let bytes = &entry.occurrence.bytes;
                    if let Some((outcome, basis)) =
                        value_use(facts, &text, bytes, definition.kind, relation)
                    {
                        entry.outcome = outcome;
                        entry.basis = basis;
                    }
                }
                if let Outcome::Alias(alias) = &entry.outcome {
                    aliases.push(alias.clone());
                }
                found.entries.push(entry);
            }
        }
        aliases
    }

    /// The files of the group in reach that spell `name`, or could not be tokenised.
    fn spelling(&self, name: &Name) -> Vec<RepoPath> {
        let mut at = self.tokens.spelling(&Names::new([name.as_str()]));
        at.extend_from_slice(self.tokens.unread());
        at.sort_unstable();
        at.dedup();
        at.into_iter()
            .map(|at| &self.tokens.files()[at as usize])
            .chain(self.outside)
            .filter(|file| (self.reach)(file.as_str()))
            .cloned()
            .collect()
    }

    fn read(&self, file: &RepoPath) -> Option<(&FileFacts, Arc<[u8]>)> {
        let at = self.files.partition_point(|facts| facts.path < *file);
        let facts = self.files.get(at).filter(|facts| facts.path == *file)?;
        let Ok(Some(text)) = self.ctx.head.read(file) else {
            return None;
        };
        Some((facts, text))
    }

    /// The grammar's parent node kind and field of the identifier: a re-parse of
    /// the file, once per file, here only.
    fn position(
        &self,
        trees: &mut HashMap<RepoPath, Option<Tree>>,
        at: &Occurrence,
    ) -> Option<String> {
        let tree = trees.entry(at.file.clone()).or_insert_with(|| {
            let Ok(Some(text)) = self.ctx.head.read(&at.file) else {
                return None;
            };
            let text = std::str::from_utf8(&text).ok()?;
            crate::parsers::parse_file(at.file.as_str(), text).map(|(tree, _)| tree)
        });
        let (start, end) = (at.bytes.start as usize, at.bytes.end as usize);
        let node = tree
            .as_ref()?
            .root_node()
            .descendant_for_byte_range(start, end)?;
        let parent = node.parent()?;
        let mut cursor = parent.walk();
        let child = parent
            .children(&mut cursor)
            .position(|c| c.id() == node.id());
        let field = child.and_then(|child| parent.field_name_for_child(child as u32));
        Some(format!("{}/{}", parent.kind(), field.unwrap_or("")))
    }
}
