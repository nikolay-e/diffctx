//! What a run did not get to see, in one vocabulary. Every layer reports its
//! gaps as a [`Loss`] and every door says them in its own words; a gap that is
//! not a `Loss` is a gap nobody can be told about.

use std::fmt;

use crate::snapshot::admission::NotAnalysedReason;
use crate::snapshot::types::RepoPath;

/// Which end of the range a path belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub enum SideTag {
    Base,
    Head,
}

impl fmt::Display for SideTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Base => "base",
            Self::Head => "head",
        })
    }
}

/// The stage of a run a stop fell in.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Stage {
    Listing,
    Workspace,
    Extraction,
    Search,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Loss {
    /// A read the run needed and did not get.
    ReadFailed {
        path: RepoPath,
        side: SideTag,
    },
    /// Working-tree bytes changed after the listing was taken.
    SnapshotChanged {
        path: RepoPath,
    },
    TooLarge {
        path: RepoPath,
        size: u64,
    },
    /// Never a path (#509).
    Withheld {
        count: usize,
    },
    NotAnalysed {
        path: RepoPath,
        reason: NotAnalysedReason,
    },
    /// A manifest the workspace model could not read as written.
    ManifestUnread {
        path: RepoPath,
        reason: String,
    },
    ParseError {
        path: RepoPath,
        lines: Vec<(u32, u32)>,
    },
    /// No extractor for the file's language.
    Unsupported {
        path: RepoPath,
    },
    Deadline {
        stage: Stage,
    },
    /// A panic caught at the extractor boundary.
    ExtractorFailed {
        path: RepoPath,
    },
}

impl Loss {
    /// The loss's name, stable across versions: what a machine reader keys on.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::ReadFailed { .. } => "read_failed",
            Self::SnapshotChanged { .. } => "snapshot_changed",
            Self::TooLarge { .. } => "too_large",
            Self::Withheld { .. } => "withheld",
            Self::NotAnalysed { .. } => "not_analysed",
            Self::ManifestUnread { .. } => "manifest_unread",
            Self::ParseError { .. } => "parse_error",
            Self::Unsupported { .. } => "unsupported",
            Self::Deadline { .. } => "deadline",
            Self::ExtractorFailed { .. } => "extractor_failed",
        }
    }

    /// The path the loss is about; `None` for one that names none.
    pub fn path(&self) -> Option<&RepoPath> {
        match self {
            Self::ReadFailed { path, .. }
            | Self::SnapshotChanged { path }
            | Self::TooLarge { path, .. }
            | Self::NotAnalysed { path, .. }
            | Self::ManifestUnread { path, .. }
            | Self::ParseError { path, .. }
            | Self::Unsupported { path }
            | Self::ExtractorFailed { path } => Some(path),
            Self::Withheld { .. } | Self::Deadline { .. } => None,
        }
    }
}

impl fmt::Display for Loss {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadFailed { path, side } => {
                write!(f, "{path}: a read of the {side} side failed")
            }
            Self::SnapshotChanged { path } => {
                write!(f, "{path}: changed on disk after the snapshot was listed")
            }
            Self::TooLarge { path, size } => {
                write!(f, "{path}: {size} bytes, over the size cap")
            }
            Self::Withheld { count } => write!(f, "{count} path(s) withheld"),
            Self::NotAnalysed { path, reason } => {
                write!(f, "{path}: listed, not read as source ({reason:?})")
            }
            Self::ManifestUnread { path, reason } => write!(f, "{path}: {reason}"),
            Self::ParseError { path, lines } => {
                let spans: Vec<String> = lines
                    .iter()
                    .map(|(from, to)| {
                        if from == to {
                            from.to_string()
                        } else {
                            format!("{from}-{to}")
                        }
                    })
                    .collect();
                write!(f, "{path}: parse errors at lines {}", spans.join(", "))
            }
            Self::Unsupported { path } => write!(f, "{path}: no extractor for its language"),
            Self::Deadline { stage } => write!(f, "the run stopped in the {stage:?} stage"),
            Self::ExtractorFailed { path } => write!(f, "{path}: the extractor failed"),
        }
    }
}
