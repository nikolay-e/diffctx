//! One analysis context per question, built once from the range at the door
//! and handed to everything below it: both snapshots as readers, what changed
//! between them, the admission policy, the repository's workspace.

use std::sync::Arc;

use crate::snapshot::admission::Admission;
use crate::snapshot::changes::{self, ChangeSet};
use crate::snapshot::error::{Result, SnapshotError};
use crate::snapshot::git::RunBudget;
use crate::snapshot::loss::{Loss, SideTag};
use crate::snapshot::range::RangeSpec;
use crate::snapshot::reader::{Side, SnapshotReader};
use crate::snapshot::repo::{RepoRoot, is_ancestor};
use crate::snapshot::types::{ContentId, digest};
use crate::snapshot::workspace::WorkspaceModel;

/// What produced the answers, folded into the context's identity so a changed
/// analyser never reuses an old result. The facade replaces it with its own
/// version and extractor versions.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ToolchainId(pub String);

impl Default for ToolchainId {
    fn default() -> Self {
        Self(concat!("diffctx-snapshot/", env!("CARGO_PKG_VERSION")).to_string())
    }
}

pub struct AnalysisContext {
    pub root: RepoRoot,
    pub range: RangeSpec,
    pub base: SnapshotReader,
    pub head: SnapshotReader,
    pub changes: ChangeSet,
    pub workspace: WorkspaceModel,
    pub admission: Arc<Admission>,
    pub toolchain: ToolchainId,
    pub run: RunBudget,
}

impl AnalysisContext {
    pub fn new(root: RepoRoot, range: RangeSpec, run: RunBudget) -> Result<Self> {
        Self::with_admission(root, range, run, Arc::new(Admission::default()))
    }

    pub fn with_admission(
        root: RepoRoot,
        range: RangeSpec,
        run: RunBudget,
        admission: Arc<Admission>,
    ) -> Result<Self> {
        let (base_side, head_side) = match &range {
            RangeSpec::Review { base, head } => {
                (Side::Tree(base.clone()), Side::Tree(head.clone()))
            }
            RangeSpec::Pushed { old, new } => {
                if old.is_zero() || !is_ancestor(&root, old.clone(), new.clone())? {
                    return Err(SnapshotError::Range(
                        "a pushed range runs from an ancestor to its descendant; a rewritten \
                         ref is a `Rewritten` range"
                            .into(),
                    ));
                }
                (Side::Tree(old.clone()), Side::Tree(new.clone()))
            }
            RangeSpec::Rewritten { old, new } => (Side::Tree(old.clone()), Side::Tree(new.clone())),
            RangeSpec::Staged { base } => (Side::Tree(base.clone()), Side::Index),
            RangeSpec::WorkingTree { base } => (Side::Tree(base.clone()), Side::WorkingTree),
            RangeSpec::At(side) => (side.clone(), side.clone()),
        };
        let open =
            |side| SnapshotReader::with_budget(&root, side, Arc::clone(&admission), run.clone());
        let head = open(head_side)?;
        // One snapshot answers for both sides: the base is the head, shared,
        // and nothing changed.
        let (base, changes) = if matches!(range, RangeSpec::At(_)) {
            (head.clone(), ChangeSet::default())
        } else {
            let base = open(base_side)?;
            let changes = changes::compute(&base, &head)?;
            (base, changes)
        };
        head.set_role(SideTag::Head);
        base.set_role(SideTag::Base);
        let workspace =
            WorkspaceModel::read(&head).map_err(|e| SnapshotError::Workspace(e.to_string()))?;
        Ok(Self {
            root,
            range,
            base,
            head,
            changes,
            workspace,
            admission,
            toolchain: ToolchainId::default(),
            run,
        })
    }

    /// What the question did not get to see, as far as it is known now: the
    /// manifests the workspace model could not read and every read of either
    /// snapshot that failed. Listings add theirs (`Listing::losses`).
    pub fn losses(&self) -> Vec<Loss> {
        let mut losses = self.workspace.losses().to_vec();
        losses.extend(self.head.losses());
        if !matches!(self.range, RangeSpec::At(_)) {
            losses.extend(self.base.losses());
        }
        losses
    }

    /// `hash(base snapshot, head snapshot, admission, toolchain)`: two contexts
    /// of one question with the same identity read the same things.
    pub fn identity(&self) -> ContentId {
        ContentId(digest(&[
            self.base.id().0.as_bytes(),
            self.head.id().0.as_bytes(),
            &self.admission.hash().to_le_bytes(),
            self.toolchain.0.as_bytes(),
        ]))
    }
}
