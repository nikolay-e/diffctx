//! What every resolver keeps of the files it bound: the definitions, the
//! bindings and what the answer is partial for.

use std::collections::HashMap;

use super::{Binding, Definition};
use crate::facts::DefKey;
use crate::snapshot::{Loss, RepoPath};

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
