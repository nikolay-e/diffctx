//! Swift: the Kotlin engine in its Swift dialect (`kotlin::solve_swift`) over
//! the Swift files of a snapshot. A module is the SwiftPM target a file sits in,
//! so Swift has no neighbour language to read through the JVM table.

use super::Solved;
use super::kotlin;
use super::solved::{Gathered, gather_of};
use crate::facts::{FactsCache, Language};
use crate::snapshot::{AnalysisContext, Loss, Result, SideTag, Stage};

/// The bindings of one snapshot's Swift files.
pub struct SwiftResolver {
    solved: Solved,
}

impl SwiftResolver {
    /// Resolves every Swift file of `side`.
    pub fn open(ctx: &AnalysisContext, side: SideTag, cache: &FactsCache) -> Result<Self> {
        let Gathered {
            mut losses,
            files,
            read,
            ..
        } = gather_of(ctx, side, cache, Language::Swift)?;
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
