//! What every resolver keeps of the files it bound: the definitions, the
//! bindings and what the answer is partial for.

use std::cell::RefCell;
use std::collections::HashMap;

use super::{Binding, Definition};
use crate::facts::{DefKey, Fact, expired};
use crate::snapshot::{AnalysisContext, Loss, RepoPath, Stage};

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
