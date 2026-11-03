//! What a range did to the definitions, from the facts of both snapshots and not
//! from the text between them: a definition moved to another file is one move, a
//! reformat is a change with nothing in its delta (#442, #540, #593, #648).
//!
//! A definition is the same on both sides when their keys are equal: a changed
//! signature is the same definition. Among the definitions left unmatched, a move
//! or a rename is declared only on an equal body hash and an unambiguous pairing;
//! an ambiguous one is `Unknown`, never a guess. Git's rename detection decides
//! only which files are read, never what is claimed about their contents.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use crate::facts::cache::FactsCache;
use crate::facts::extractor::expired;
use crate::facts::file::facts_of;
use crate::facts::ids::Name;
use crate::facts::language::Language;
use crate::facts::model::{Def, DefKey, DefKind};
use crate::snapshot::{
    AnalysisContext, Change, ChangeKind, Decision, Loss, RepoPath, Result, SideTag, SnapshotError,
    Stage,
};

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Origin {
    Same,
    /// The same definition in another module or container, name unchanged. The
    /// module change is visible in the keys: a git R100 is evidence of a moved
    /// file, not of an unchanged API.
    Moved {
        from: DefKey,
    },
    Renamed {
        from: DefKey,
    },
}

/// What differs between the two sides. All false with `Changed`: only the layout
/// (whitespace, comments) or only the place changed.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Delta {
    pub signature: bool,
    pub body: bool,
    pub doc: bool,
    pub visibility: bool,
    pub decorators: bool,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum DefChange {
    Added,
    Removed,
    Changed {
        origin: Origin,
        delta: Delta,
    },
    /// Not known what happened to it, and why.
    Unknown {
        loss: Loss,
    },
}

/// One definition's change, with the files it was read from.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SemanticDef {
    /// A removed definition is under its base key, the rest under their head key.
    pub key: DefKey,
    pub change: DefChange,
    /// The file holding the definition in the base: set for a removal, a change,
    /// a move or a rename, and for an `Unknown` found on the base side.
    pub base: Option<RepoPath>,
    /// The file holding it in the head: set for an addition, a change, a move or
    /// a rename, and for an `Unknown` found on the head side.
    pub head: Option<RepoPath>,
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct SemanticChangeSet {
    /// Sorted by key.
    pub defs: Vec<SemanticDef>,
    /// What the answer is partial for.
    pub losses: Vec<Loss>,
}

/// A definition with the file it was read from and its key on its side.
struct Item {
    path: RepoPath,
    key: DefKey,
    def: Def,
}

#[derive(Default)]
struct Walk {
    base: Vec<Item>,
    head: Vec<Item>,
    losses: Vec<Loss>,
    seen: HashSet<String>,
    /// Files the extractor could not fully read, on either side: a definition
    /// missing there may only be one it lost.
    unsure: HashMap<RepoPath, Loss>,
    stopped: bool,
}

pub fn semantic_changes(ctx: &AnalysisContext, cache: &FactsCache) -> Result<SemanticChangeSet> {
    let mut walk = Walk::default();
    for change in &ctx.changes.changes {
        if !walk.stopped && expired(&ctx.run) {
            walk.stopped = true;
        }
        if walk.stopped {
            break;
        }
        match change.decision {
            Decision::Admit => walk.collect(ctx, cache, change)?,
            Decision::NotAnalysed(reason) => {
                if Language::from_path(&change.path).is_some() {
                    walk.lose(Loss::NotAnalysed {
                        path: change.path.clone(),
                        reason,
                    });
                }
            }
            Decision::Withhold(_) => {}
        }
    }
    if ctx.changes.withheld > 0 {
        walk.lose(Loss::Withheld {
            count: ctx.changes.withheld,
        });
    }
    if walk.stopped {
        walk.lose(Loss::Deadline {
            stage: Stage::Extraction,
        });
    }
    Ok(walk.finish())
}

impl Walk {
    fn lose(&mut self, loss: Loss) {
        if self.seen.insert(format!("{loss:?}")) {
            self.losses.push(loss);
        }
    }

    fn collect(
        &mut self,
        ctx: &AnalysisContext,
        cache: &FactsCache,
        change: &Change,
    ) -> Result<()> {
        let (before, after) = match change.kind {
            ChangeKind::Added | ChangeKind::Copied => (None, Some(&change.path)),
            ChangeKind::Deleted => (Some(&change.path), None),
            ChangeKind::Modified | ChangeKind::TypeChanged => {
                (Some(&change.path), Some(&change.path))
            }
            ChangeKind::Renamed => (change.from.as_ref(), Some(&change.path)),
        };
        let old = match before {
            Some(path) => self.items(ctx, cache, SideTag::Base, path)?,
            None => Vec::new(),
        };
        let new = match after {
            Some(path) => self.items(ctx, cache, SideTag::Head, path)?,
            None => Vec::new(),
        };
        // A file read on one side only would read as added or removed.
        if !self.stopped {
            self.base.extend(old);
            self.head.extend(new);
        }
        Ok(())
    }

    fn items(
        &mut self,
        ctx: &AnalysisContext,
        cache: &FactsCache,
        side: SideTag,
        path: &RepoPath,
    ) -> Result<Vec<Item>> {
        match facts_of(ctx, side, cache, path) {
            Ok(Some(file)) => {
                for loss in file.losses() {
                    if matches!(
                        loss,
                        Loss::ParseError { .. }
                            | Loss::ExtractorFailed { .. }
                            | Loss::Deadline { .. }
                    ) {
                        self.unsure.insert(path.clone(), loss.clone());
                    }
                    self.lose(loss);
                }
                Ok(file
                    .defs()
                    .map(|def| Item {
                        path: path.clone(),
                        key: file.key(&def.local),
                        def: def.clone(),
                    })
                    .collect())
            }
            Ok(None) => {
                let unsupported = Language::from_path(path)
                    .is_some_and(|language| cache.registry().get(language).is_none());
                if unsupported {
                    self.lose(Loss::Unsupported { path: path.clone() });
                }
                Ok(Vec::new())
            }
            Err(error) if error.is_interruption() => {
                self.stopped = true;
                Ok(Vec::new())
            }
            Err(SnapshotError::TooLarge { size, .. }) => {
                self.lose(Loss::TooLarge {
                    path: path.clone(),
                    size,
                });
                Ok(Vec::new())
            }
            Err(error) => Err(error),
        }
    }

    fn finish(self) -> SemanticChangeSet {
        let mut found: Vec<SemanticDef> = Vec::new();
        let (removed, added) = self.pair_by_key(&mut found);
        let (removed, added) = self.pair_by_body(&mut found, removed, added);
        for index in removed {
            let item = &self.base[index];
            let change = self.unmatched(item, DefChange::Removed);
            found.push(entry(&item.key, change, Some(&item.path), None));
        }
        for index in added {
            let item = &self.head[index];
            let change = self.unmatched(item, DefChange::Added);
            found.push(entry(&item.key, change, None, Some(&item.path)));
        }
        found.sort_by(|a, b| {
            a.key
                .cmp(&b.key)
                .then_with(|| rank(&a.change).cmp(&rank(&b.change)))
        });
        SemanticChangeSet {
            defs: found,
            losses: self.losses,
        }
    }

    /// An unmatched definition of a file the parser could not fully read may only
    /// be one it lost.
    fn unmatched(&self, item: &Item, plain: DefChange) -> DefChange {
        match self.unsure.get(&item.path) {
            Some(loss) => DefChange::Unknown { loss: loss.clone() },
            None => plain,
        }
    }

    /// Pairs equal keys, in source order where one key repeats (a redefinition).
    /// Returns what is left of the base and of the head, as indexes.
    fn pair_by_key(&self, found: &mut Vec<SemanticDef>) -> (Vec<usize>, Vec<usize>) {
        let mut waiting: HashMap<&DefKey, VecDeque<usize>> = HashMap::new();
        for (index, item) in self.head.iter().enumerate() {
            waiting.entry(&item.key).or_default().push_back(index);
        }
        let mut taken = vec![false; self.head.len()];
        let mut removed = Vec::new();
        for (index, before) in self.base.iter().enumerate() {
            let paired = waiting.get_mut(&before.key).and_then(VecDeque::pop_front);
            match paired {
                Some(at) => {
                    taken[at] = true;
                    let after = &self.head[at];
                    if let Some(change) = compare(&before.def, &after.def, Origin::Same) {
                        found.push(entry(
                            &after.key,
                            change,
                            Some(&before.path),
                            Some(&after.path),
                        ));
                    }
                }
                None => removed.push(index),
            }
        }
        let added = (0..self.head.len())
            .filter(|index| !taken[*index])
            .collect();
        (removed, added)
    }

    /// Pairs what is left by body hash and kind, where the pairing is the only one.
    fn pair_by_body(
        &self,
        found: &mut Vec<SemanticDef>,
        removed: Vec<usize>,
        added: Vec<usize>,
    ) -> (Vec<usize>, Vec<usize>) {
        type Group = (Vec<usize>, Vec<usize>);
        let mut groups: BTreeMap<(Language, DefKind, u64), Group> = BTreeMap::new();
        let mut left_removed = Vec::new();
        let mut left_added = Vec::new();
        for index in removed {
            let item = &self.base[index];
            let def = &item.def;
            if def.body_hash == 0 {
                left_removed.push(index);
            } else {
                groups
                    .entry((item.key.language, def.kind, def.body_hash))
                    .or_default()
                    .0
                    .push(index);
            }
        }
        for index in added {
            let item = &self.head[index];
            let def = &item.def;
            if def.body_hash == 0 {
                left_added.push(index);
            } else {
                groups
                    .entry((item.key.language, def.kind, def.body_hash))
                    .or_default()
                    .1
                    .push(index);
            }
        }
        for (_, (before, after)) in groups {
            if before.is_empty() || after.is_empty() {
                left_removed.extend(before);
                left_added.extend(after);
                continue;
            }
            let alone = before.len() == 1 && after.len() == 1;
            let (pairs, rest_before, rest_after) = self.narrow(&before, &after);
            for (b, a) in pairs {
                let (old, new) = (&self.base[b], &self.head[a]);
                found.push(entry(
                    &new.key,
                    compare_moved(old, new),
                    Some(&old.path),
                    Some(&new.path),
                ));
            }
            if alone || rest_before.is_empty() || rest_after.is_empty() {
                left_removed.extend(rest_before);
                left_added.extend(rest_after);
                continue;
            }
            for index in rest_before {
                let item = &self.base[index];
                found.push(entry(&item.key, ambiguous(item), Some(&item.path), None));
            }
            for index in rest_after {
                let item = &self.head[index];
                found.push(entry(&item.key, ambiguous(item), None, Some(&item.path)));
            }
        }
        left_removed.sort_unstable();
        left_added.sort_unstable();
        (left_removed, left_added)
    }

    /// The pairs a group of equal bodies allows: the only one of a single pair, or
    /// those whose local keys (container, name, overload) are each other's only
    /// match. What is not paired is returned.
    fn narrow(
        &self,
        before: &[usize],
        after: &[usize],
    ) -> (Vec<(usize, usize)>, Vec<usize>, Vec<usize>) {
        let same_project = |b: usize, a: usize| {
            self.base[b].key.project == self.head[a].key.project
                && self.base[b].key.language == self.head[a].key.language
        };
        if let ([b], [a]) = (before, after) {
            return if same_project(*b, *a) {
                (vec![(*b, *a)], Vec::new(), Vec::new())
            } else {
                (Vec::new(), vec![*b], vec![*a])
            };
        }
        let mut pairs = Vec::new();
        let mut used_after: HashSet<usize> = HashSet::new();
        for &b in before {
            let candidates: Vec<usize> = after
                .iter()
                .copied()
                .filter(|&a| self.base[b].key.local == self.head[a].key.local && same_project(b, a))
                .collect();
            let rivals = before
                .iter()
                .filter(|&&other| {
                    candidates
                        .first()
                        .is_some_and(|&a| self.base[other].key.local == self.head[a].key.local)
                })
                .count();
            if let ([a], 1) = (candidates.as_slice(), rivals) {
                pairs.push((b, *a));
                used_after.insert(*a);
            }
        }
        let paired_before: HashSet<usize> = pairs.iter().map(|(b, _)| *b).collect();
        let rest_before = before
            .iter()
            .copied()
            .filter(|b| !paired_before.contains(b))
            .collect();
        let rest_after = after
            .iter()
            .copied()
            .filter(|a| !used_after.contains(a))
            .collect();
        (pairs, rest_before, rest_after)
    }
}

fn entry(
    key: &DefKey,
    change: DefChange,
    base: Option<&RepoPath>,
    head: Option<&RepoPath>,
) -> SemanticDef {
    SemanticDef {
        key: key.clone(),
        change,
        base: base.cloned(),
        head: head.cloned(),
    }
}

fn ambiguous(item: &Item) -> DefChange {
    DefChange::Unknown {
        loss: Loss::AmbiguousMatch {
            path: item.path.clone(),
        },
    }
}

/// A definition paired by its body across keys: moved when the name is the same
/// and only the module or container differs, renamed when the name differs.
fn compare_moved(before: &Item, after: &Item) -> DefChange {
    let from = before.key.clone();
    let renamed = before.def.local.name != after.def.local.name;
    let origin = if renamed {
        Origin::Renamed { from }
    } else {
        Origin::Moved { from }
    };
    let names = renamed.then_some((&before.def.local.name, &after.def.local.name));
    changed(&before.def, &after.def, origin, names)
}

fn compare(before: &Def, after: &Def, origin: Origin) -> Option<DefChange> {
    let change = changed(before, after, origin, None);
    let text_differs = before.text_hash != after.text_hash;
    match &change {
        DefChange::Changed {
            origin: Origin::Same,
            delta,
        } if *delta == Delta::default() && !text_differs => None,
        _ => Some(change),
    }
}

fn changed(
    before: &Def,
    after: &Def,
    origin: Origin,
    renamed: Option<(&Name, &Name)>,
) -> DefChange {
    let signature = before.kind != after.kind
        || match renamed {
            None => before.signature != after.signature,
            Some((old, new)) => {
                without(before.signature.as_deref(), old)
                    != without(after.signature.as_deref(), new)
            }
        };
    DefChange::Changed {
        origin,
        delta: Delta {
            signature,
            body: before.body_hash != after.body_hash,
            doc: before.doc_hash != after.doc_hash,
            visibility: before.visibility != after.visibility,
            decorators: before.decorators != after.decorators,
        },
    }
}

/// A signature with its definition's own name taken out, so a rename alone is not
/// a signature change.
fn without(signature: Option<&str>, name: &Name) -> Option<String> {
    signature.map(|text| text.replacen(name.as_str(), "", 1))
}

fn rank(change: &DefChange) -> u8 {
    match change {
        DefChange::Added => 0,
        DefChange::Removed => 1,
        DefChange::Changed { .. } => 2,
        DefChange::Unknown { .. } => 3,
    }
}
