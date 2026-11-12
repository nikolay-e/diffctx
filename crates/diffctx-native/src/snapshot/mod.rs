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
mod diff;
mod error;
mod git;
mod grep;
mod ignore;
mod loss;
mod range;
mod reader;
mod repo;
mod run;
#[cfg(test)]
pub(crate) mod testing;
mod types;
mod untracked;
pub mod workspace;

pub use admission::{
    Admission, AdmissionPolicy, Decision, IgnoreSource, Meta, NotAnalysedReason, UntrackedPolicy,
    WithholdReason, is_secret_path,
};
pub use changes::{Change, ChangeKind, ChangeSet};
pub use context::{AnalysisContext, ToolchainId};
pub use diff::{diff_text, validate_pathspec};
pub use error::{Result, SnapshotError};
pub use git::{
    DEFAULT_TIMEOUT_SECONDS, REPO_LOCATING_ENV, ReadFault, RunBudget, RunScope, enter_run,
    git_command, set_git_timeout,
};
pub use grep::{
    Row, files_with_declaration, files_with_word, lines_matching, needle_lines, word_first_lines,
    word_lines,
};
pub use loss::{Loss, SideTag, Stage};
pub use range::{
    RangeSpec, commits, duration_base, is_staged_keyword, resolve, split_diff_range, staged,
    working_tree,
};
pub use reader::{Entry, Listing, Side, SnapshotReader};
pub use repo::{
    PlanStep, Refs, RepoRoot, StagedSnapshot, capture_planned, empty_tree_id, find_toplevel,
    index_tree, is_ancestor, is_git_repo, open, reflog_since, refs, rev_oid,
};
pub use run::{
    commit_count, commit_message, commit_messages, hash_object_paths, run_git, run_git_or_none,
    run_git_z,
};
pub use types::{
    Bytes, ContentId, Cursor, Mode, Oid, RefName, ReflogEntry, Remote, RepoPath,
    contains_lexically, normalize_lexical,
};
pub use untracked::{Untracked, untracked};
pub use workspace::WorkspaceModel;

/// The largest blob a reader hands out; a larger one is listed, not read.
pub const MAX_BLOB_READ_BYTES: u64 = 16_000_000;
