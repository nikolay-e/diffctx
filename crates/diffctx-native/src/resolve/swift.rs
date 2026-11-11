//! Swift: the Kotlin engine in its Swift dialect (`kotlin::solve_swift`) over
//! the Swift files of a snapshot. A module is the SwiftPM target a file sits in,
//! so Swift has no neighbour language to read through the JVM table.

use std::collections::HashMap;

use super::kotlin;
use super::{Binding, Definition};
use crate::facts::{DefKey, FactsCache, Language, expired, facts_of};
use crate::snapshot::{AnalysisContext, Loss, RepoPath, Result, SideTag, Stage};

/// The bindings of one snapshot's Swift files.
pub struct SwiftResolver {
    definitions: Vec<Definition>,
    bindings: Vec<Binding>,
    by_target: HashMap<DefKey, Vec<usize>>,
    losses: Vec<Loss>,
    read: Vec<RepoPath>,
}

impl SwiftResolver {
    /// Resolves every Swift file of `side`.
    pub fn open(ctx: &AnalysisContext, side: SideTag, cache: &FactsCache) -> Result<Self> {
        let reader = match side {
            SideTag::Base => &ctx.base,
            SideTag::Head => &ctx.head,
        };
        let listing = cache.listing(reader)?;
        let mut files = Vec::new();
        let mut losses = Vec::new();
        let mut read = Vec::new();
        for entry in &listing.entries {
            if Language::from_path(&entry.path) != Some(Language::Swift) {
                continue;
            }
            if expired(&ctx.run) {
                losses.push(Loss::Deadline {
                    stage: Stage::Search,
                });
                break;
            }
            if let Some(file) = facts_of(ctx, side, cache, &entry.path)? {
                losses.extend(file.losses());
                read.push(entry.path.clone());
                files.push(file);
            }
        }
        let (definitions, bindings, stopped) = kotlin::solve_swift(ctx, &files);
        if stopped {
            losses.push(Loss::Deadline {
                stage: Stage::Search,
            });
        }
        let mut by_target: HashMap<DefKey, Vec<usize>> = HashMap::new();
        for (at, binding) in bindings.iter().enumerate() {
            for key in binding.targets() {
                by_target.entry(key.clone()).or_default().push(at);
            }
        }
        Ok(Self {
            definitions,
            bindings,
            by_target,
            losses,
            read,
        })
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
