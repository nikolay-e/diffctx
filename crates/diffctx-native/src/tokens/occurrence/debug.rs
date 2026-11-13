//! The accounting of every changed definition's occurrences. It runs for every
//! answer and its hedges are what stands in the way of `Complete`
//! (`Coverage::close`). Under `DIFFCTX_DEBUG_OCCURRENCES=<file>` it also appends
//! one JSON line per occurrence and per (definition, relation).

use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::Arc;

use serde_json::json;
use tree_sitter::Tree;

use super::{
    Account, Alias, AliasScope, Basis, Class, Entry, Hedge, Known, Occurrence, Outcome, Reason,
    Unread, Verdict, View, admitted, close_aliases, counts_against, implicit_kinds, outcomes,
    value_use,
};
use crate::decide::{Family, Gap, USES};
use crate::facts::{FactsCache, FileFacts, Name, facts_of};
use crate::resolve::{Definition, Relation, UnresolvedReason};
use crate::snapshot::{AnalysisContext, Loss, RepoPath, SideTag};
use crate::tokens::{GroupTokens, Names};

const ENV: &str = "DIFFCTX_DEBUG_OCCURRENCES";

/// The file the knob names, opened for appending; `None` when it is unset.
pub fn sink() -> Option<File> {
    let path = std::env::var_os(ENV)?;
    let file = OpenOptions::new().create(true).append(true).open(path);
    file.map_err(|error| eprintln!("{ENV}: {error}")).ok()
}

/// The lines already written: a definition's occurrences once per side, a pair
/// once per relation, however many times the cell asks.
pub type Written = HashSet<(String, String, Option<&'static str>)>;

fn append(out: &mut File, lines: &str) {
    if let Err(error) = out.write_all(lines.as_bytes()) {
        eprintln!("{ENV}: {error}");
    }
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
    /// What else stands in the way of every relation of use: the run's gaps.
    pub gaps: &'a [Gap],
    /// The changed definitions nested in this one, and whether every one is whole
    /// and unused (only asked for when the lines are written).
    pub held: usize,
    pub nested: bool,
}

/// What the accounting of one definition found.
pub struct Accounted {
    /// What stands in the way of each relation of use.
    pub hedges: Vec<(Relation, Hedge)>,
    /// No occurrence is a use, an alias, a leak or a candidate.
    pub unused: bool,
    /// Every relation is complete and so is what the definition holds.
    pub whole: bool,
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
    /// Accounts `definition`, read at `side`, and appends its lines to `out` when
    /// there is one.
    pub fn write(
        &self,
        out: Option<&mut File>,
        written: &mut Written,
        definition: &Definition,
        side: SideTag,
        known: Known,
    ) -> Accounted {
        let writing = out.is_some();
        let (def, side_text, family) = (
            label(definition),
            format!("{side:?}").to_lowercase(),
            format!("{:?}", self.family).to_lowercase(),
        );
        let mut once = |relation: Option<Relation>| {
            writing
                && written.insert((def.clone(), side_text.clone(), relation.map(Relation::name)))
        };
        // The relations of use, then those an unexplained occurrence may be a use
        // through that the answer has no coverage of.
        let unanswered = admitted(definition.kind)
            .iter()
            .filter(|relation| !USES.contains(relation))
            .map(|relation| (*relation, &[][..]));
        let pairs: Vec<(Relation, &[Gap])> = (USES.iter())
            .map(|relation| (*relation, self.gaps))
            .chain(unanswered)
            .collect();
        let first = once(None);
        let mut found = Found::default();
        let names: Vec<Name> = self
            .names
            .iter()
            .map(|name| Name::from(name.as_str()))
            .collect();
        let (_, alias_bound) = close_aliases(&names, |name, scope| {
            self.account_name(&mut found, definition, name, scope)
        });
        found.entries.extend(self.unnamed(definition));
        let implicit = match facts_of(self.ctx, side, self.cache, &definition.file) {
            Ok(Some(home)) => home
                .defs()
                .find(|def| home.key(&def.local) == definition.key)
                .map_or_else(Vec::new, |def| implicit_kinds(&home, def, known)),
            _ => Vec::new(),
        };
        let unread: Vec<Unread> = (found.unread.iter())
            .map(|(file, loss)| Unread {
                file: file.clone(),
                loss: loss.clone(),
            })
            .collect();
        let mut lines = String::new();
        let mut trees = HashMap::new();
        for entry in found.entries.iter().filter(|_| first) {
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
        let (mut whole, mut hedges) = (true, Vec::new());
        let unused = found
            .entries
            .iter()
            .all(|entry| matches!(entry.outcome, Outcome::Declaration | Outcome::NotOurs));
        for (relation, gaps) in pairs {
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
            account.admits_calls = admitted(definition.kind).contains(&Relation::Calls);
            account.alias_bound = alias_bound;
            let verdict = account.verdict();
            let complete = verdict.is_complete() && gaps.is_empty();
            whole &= complete;
            if let Verdict::Partial(stuck) = &verdict {
                hedges.extend(stuck.iter().map(|hedge| (relation, hedge.clone())));
            }
            if !once(Some(relation)) {
                continue;
            }
            let reasons: Vec<String> = match &verdict {
                Verdict::Complete => Vec::new(),
                Verdict::Partial(stuck) => stuck.iter().map(ToString::to_string).collect(),
            };
            let gaps: Vec<String> = gaps.iter().map(ToString::to_string).collect();
            lines.push_str(
                &json!({
                    "def": def, "side": side_text, "relation": relation.name(),
                    "occurrences": verdict.as_str(),
                    "reasons": reasons, "gaps": gaps, "complete": complete,
                })
                .to_string(),
            );
            lines.push('\n');
        }
        whole &= self.nested;
        if writing
            && self.held > 0
            && written.insert((def.clone(), side_text.clone(), Some("holder")))
        {
            let line = json!({
                "def": def, "side": side_text, "holder": true, "held": self.held,
                "complete": whole,
            });
            lines.push_str(&format!("{line}\n"));
        }
        if let Some(out) = out {
            append(out, &lines);
        }
        Accounted {
            hedges,
            unused,
            whole,
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
            let Some((facts, text)) = self.read(&file) else {
                let loss = self.unread(&file);
                found.unread.entry(file).or_insert(loss);
                continue;
            };
            let done = outcomes(self.view, facts, &text, definition, &[name.as_str()]);
            let other_project = self.view.manifests_whole()
                && !(self.ctx.workspace).may_reference(file.as_str(), definition.file.as_str());
            found.mentions += done.mentions;
            if done.lost && done.entries.is_empty() {
                let loss =
                    (facts.losses().into_iter().next()).unwrap_or_else(|| self.unread(&file));
                found.unread.entry(file).or_insert(loss);
            }
            for mut entry in done.entries {
                if other_project && matches!(entry.outcome, Outcome::Candidate(_)) {
                    entry.outcome = Outcome::NotOurs;
                    entry.basis = Basis::OtherProject;
                }
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
                        if matches!(entry.outcome, Outcome::Candidate(_)) {
                            entry.relation = Some(Relation::Calls);
                        }
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

    /// A candidate at each use that spells no name, in files in reach that may
    /// reference the definition.
    fn unnamed<'s>(&'s self, definition: &'s Definition) -> impl Iterator<Item = Entry> + 's {
        let may = |file: &RepoPath| {
            (self.reach)(file.as_str())
                && self
                    .ctx
                    .workspace
                    .may_reference(file.as_str(), definition.file.as_str())
        };
        self.view
            .unnamed(definition.kind)
            .filter(move |binding| may(&binding.file))
            .map(|binding| Entry {
                occurrence: Occurrence {
                    file: binding.file.clone(),
                    bytes: binding.site.bytes.clone(),
                    line: binding.site.lines.0,
                    class: Class::Code,
                },
                relation: Some(binding.relation),
                outcome: Outcome::Candidate(Reason::Unresolved(UnresolvedReason::Opaque)),
                basis: Basis::Unproven,
            })
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

    /// Why no occurrence of `file` was read: the listing names it as not read as
    /// source (too large, a symlink, ...), else the read failed.
    fn unread(&self, file: &RepoPath) -> Loss {
        let listed = match self.cache.listing(&self.ctx.head) {
            Ok(listing) => (listing.not_analysed.iter())
                .find(|(path, _)| path == file)
                .map(|(_, reason)| *reason),
            Err(_) => None,
        };
        listed.map_or_else(
            || Loss::ReadFailed {
                path: file.clone(),
                side: SideTag::Head,
            },
            |reason| Loss::NotAnalysed {
                path: file.clone(),
                reason,
            },
        )
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
