//! Swift: the Kotlin engine in its Swift dialect (`kotlin::solve_swift`) over
//! the Swift files of a snapshot. A module is the SwiftPM target a file sits in,
//! so Swift has no neighbour language to read through the JVM table.

use super::Solved;
use super::kotlin;
use crate::facts::{FactsCache, Language, expired, facts_of};
use crate::snapshot::{AnalysisContext, Loss, Result, SideTag, Stage};

/// The bindings of one snapshot's Swift files.
pub struct SwiftResolver {
    solved: Solved,
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
        Ok(Self {
            solved: Solved::new(definitions, bindings, losses, read),
        })
    }
}

solved_by!(SwiftResolver);
