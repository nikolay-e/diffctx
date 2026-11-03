//! One repository state per question. A [`SnapshotReader`] serves one
//! snapshot - a tree, the index, the working tree - through one door:
//! admission is decided inside it, a failed read is an `Err` and never an
//! absent file, and a tree side never falls back to the disk. An
//! [`AnalysisContext`] is built once from a resolved [`RangeSpec`] and carries
//! both readers, the change set and the admission policy.
//!
//! The module reaches no other module of the crate (`tests/snapshot_isolation.rs`
//! holds it), and the three methods that turn a failed read into an empty value
//! are denied here and nowhere else: a justified use carries
//! `#[allow(clippy::disallowed_methods, reason = "...")]`.

#![deny(clippy::disallowed_methods)]
#![allow(
    clippy::redundant_pub_crate,
    clippy::significant_drop_tightening,
    clippy::cognitive_complexity
)]

mod admission;
mod changes;
mod context;
mod error;
mod git;
mod ignore;
mod loss;
mod range;
mod reader;
mod repo;
mod types;
pub mod workspace;

pub use admission::{
    Admission, AdmissionPolicy, Decision, IgnoreSource, Meta, NotAnalysedReason, UntrackedPolicy,
    WithholdReason, is_secret_path,
};
pub use changes::{Change, ChangeKind, ChangeSet};
pub use context::{AnalysisContext, ToolchainId};
pub use error::{Result, SnapshotError};
pub(crate) use git::git_command;
pub use git::{DEFAULT_TIMEOUT_SECONDS, REPO_LOCATING_ENV, ReadFault, RunBudget, set_git_timeout};
pub use loss::{Loss, SideTag, Stage};
pub use range::{RangeSpec, commits, resolve, staged, working_tree};
pub use reader::{Entry, Listing, Side, SnapshotReader, hash_working_paths};
pub use repo::{
    Refs, RepoRoot, index_tree, is_ancestor, merge_base, open, reflog_since, refs, worktrees,
};
pub use types::{Bytes, ContentId, Cursor, Mode, Oid, RefName, ReflogEntry, Remote, RepoPath};
pub use workspace::WorkspaceModel;

/// The largest blob a reader hands out; a larger one is listed, not read.
pub const MAX_BLOB_READ_BYTES: u64 = 16_000_000;
