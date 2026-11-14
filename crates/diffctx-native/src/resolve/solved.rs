//! What every resolver keeps of the files it bound: the definitions, the
//! bindings and what the answer is partial for.

use std::cell::RefCell;
use std::collections::HashMap;

use super::{Binding, Definition};
use std::sync::Arc;

use crate::facts::{DefKey, Fact, FactsCache, FileFacts, Language, expired, facts_of};
use crate::snapshot::{
    AnalysisContext, Entry, Listing, Loss, RepoPath, Result, SideTag, SnapshotReader, Stage,
};

pub struct Solved {
    definitions: Vec<Definition>,
    bindings: Vec<Binding>,
    by_target: HashMap<DefKey, Vec<usize>>,
    losses: Vec<Loss>,
    read: Vec<RepoPath>,
}

impl Solved {
    pub(super) fn new(
        definitions: Vec<Definition>,
        bindings: Vec<Binding>,
        losses: Vec<Loss>,
        read: Vec<RepoPath>,
    ) -> Self {
        let mut by_target: HashMap<DefKey, Vec<usize>> = HashMap::new();
        for (at, binding) in bindings.iter().enumerate() {
            for key in binding.targets() {
                by_target.entry(key.clone()).or_default().push(at);
            }
        }
        Self {
            definitions,
            bindings,
            by_target,
            losses,
            read,
        }
    }

    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }

    pub fn definitions(&self) -> &[Definition] {
        &self.definitions
    }

    /// The bindings that resolve to `key` or have it among their candidates.
    pub fn references_to(&self, key: &DefKey) -> Vec<&Binding> {
        match self.by_target.get(key) {
            Some(at) => at.iter().map(|i| &self.bindings[*i]).collect(),
            None => Vec::new(),
        }
    }

    /// What the answer is partial for: files the extractor could not fully read,
    /// reads that failed, a run cut short.
    pub fn losses(&self) -> &[Loss] {
        &self.losses
    }

    /// The files the resolver read: what an absence may rest on.
    pub fn read_paths(&self) -> &[RepoPath] {
        &self.read
    }
}

/// The snapshot a side of the range names.
pub(super) fn reader_of(ctx: &AnalysisContext, side: SideTag) -> &SnapshotReader {
    match side {
        SideTag::Base => &ctx.base,
        SideTag::Head => &ctx.head,
    }
}

/// What a resolver read of one side: the files it wants as facts, the losses of reading
/// them and their paths.
pub(super) struct Gathered {
    pub listing: Arc<Listing>,
    pub files: Vec<FileFacts>,
    pub losses: Vec<Loss>,
    pub read: Vec<RepoPath>,
}

/// The facts of every listed file `wants` names whose language `keeps` accepts, until the
/// run's deadline. `wants` may read or lose other entries (a manifest) on its way.
pub(super) fn gather(
    ctx: &AnalysisContext,
    side: SideTag,
    cache: &FactsCache,
    mut wants: impl FnMut(&Entry, &mut Vec<Loss>) -> bool,
    keeps: impl Fn(&FileFacts) -> bool,
) -> Result<Gathered> {
    let listing = cache.listing(reader_of(ctx, side))?;
    let (mut files, mut losses, mut read) = (Vec::new(), Vec::new(), Vec::new());
    for entry in &listing.entries {
        if !wants(entry, &mut losses) {
            continue;
        }
        if expired(&ctx.run) {
            losses.push(Loss::Deadline {
                stage: Stage::Search,
            });
            break;
        }
        if let Some(file) = facts_of(ctx, side, cache, &entry.path)?.filter(&keeps) {
            losses.extend(file.losses());
            read.push(entry.path.clone());
            files.push(file);
        }
    }
    Ok(Gathered {
        listing,
        files,
        losses,
        read,
    })
}

/// [`gather`] for the files of one language.
pub(super) fn gather_of(
    ctx: &AnalysisContext,
    side: SideTag,
    cache: &FactsCache,
    language: Language,
) -> Result<Gathered> {
    gather(
        ctx,
        side,
        cache,
        |entry, _| Language::from_path(&entry.path) == Some(language),
        |file| file.language() == language,
    )
}

/// The facts of a file up to the run's deadline, which the engine's `losses` then
/// name. An engine binds every reference of every file it read, minutes of work
/// on a large range, so each fact is taken after one poll.
pub(super) fn before_deadline<'a>(
    ctx: &'a AnalysisContext,
    losses: &'a RefCell<Vec<Loss>>,
    facts: &'a [Fact],
) -> impl Iterator<Item = &'a Fact> + 'a {
    facts.iter().take_while(move |_| {
        let over = expired(&ctx.run);
        let search = Loss::Deadline {
            stage: Stage::Search,
        };
        if over && !losses.borrow().contains(&search) {
            losses.borrow_mut().push(search);
        }
        !over
    })
}

/// A resolver's `solved` field answers the methods above as its own.
macro_rules! solved_by {
    ($resolver:ident) => {
        impl std::ops::Deref for $resolver {
            type Target = super::Solved;

            fn deref(&self) -> &Self::Target {
                &self.solved
            }
        }
    };
}
