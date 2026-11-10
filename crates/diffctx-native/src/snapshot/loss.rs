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
    /// The pinned grammar reports these lines as errors, and a newer release of
    /// it reads them: the file is not malformed, the grammar is out of date.
    GrammarOutdated {
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
    /// A definition changed in the range has more than one possible
    /// counterpart on the other side, so no pairing is claimed.
    AmbiguousMatch {
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
            Self::GrammarOutdated { .. } => "grammar_outdated",
            Self::Unsupported { .. } => "unsupported",
            Self::Deadline { .. } => "deadline",
            Self::ExtractorFailed { .. } => "extractor_failed",
            Self::AmbiguousMatch { .. } => "ambiguous_match",
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
            | Self::GrammarOutdated { path, .. }
            | Self::Unsupported { path }
            | Self::ExtractorFailed { path }
            | Self::AmbiguousMatch { path } => Some(path),
            Self::Withheld { .. } | Self::Deadline { .. } => None,
        }
    }

    /// The same loss about `at`: how a layer that read a path-free blob names
    /// the file it was read for. A loss that names no path is returned as it is.
    #[must_use]
    pub fn at(self, at: &RepoPath) -> Self {
        let path = at.clone();
        match self {
            Self::ReadFailed { side, .. } => Self::ReadFailed { path, side },
            Self::SnapshotChanged { .. } => Self::SnapshotChanged { path },
            Self::TooLarge { size, .. } => Self::TooLarge { path, size },
            Self::NotAnalysed { reason, .. } => Self::NotAnalysed { path, reason },
            Self::ManifestUnread { reason, .. } => Self::ManifestUnread { path, reason },
            Self::ParseError { lines, .. } => Self::ParseError { path, lines },
            Self::GrammarOutdated { lines, .. } => Self::GrammarOutdated { path, lines },
            Self::Unsupported { .. } => Self::Unsupported { path },
            Self::ExtractorFailed { .. } => Self::ExtractorFailed { path },
            Self::AmbiguousMatch { .. } => Self::AmbiguousMatch { path },
            other @ (Self::Withheld { .. } | Self::Deadline { .. }) => other,
        }
    }
}

impl Loss {
    /// One loss of every variant, `path` the file it is about (a variant that
    /// names no path ignores it): what the test knob `DIFFCTX_TEST_LOSS` injects
    /// and the loss sweep walks, so that the list of variants is the enum's own.
    /// `index` is an exhaustive match: a new variant does not compile until it
    /// has a place here, and the sample at that place.
    pub fn samples(path: &RepoPath) -> Vec<Self> {
        let at = || path.clone();
        let samples = vec![
            Self::ReadFailed {
                path: at(),
                side: SideTag::Head,
            },
            Self::SnapshotChanged { path: at() },
            Self::TooLarge {
                path: at(),
                size: 1,
            },
            Self::Withheld { count: 1 },
            Self::NotAnalysed {
                path: at(),
                reason: NotAnalysedReason::Untracked,
            },
            Self::ManifestUnread {
                path: at(),
                reason: "injected".to_string(),
            },
            Self::ParseError {
                path: at(),
                lines: vec![(1, 1)],
            },
            Self::GrammarOutdated {
                path: at(),
                lines: vec![(1, 1)],
            },
            Self::Unsupported { path: at() },
            Self::Deadline {
                stage: Stage::Search,
            },
            Self::ExtractorFailed { path: at() },
            Self::AmbiguousMatch { path: at() },
        ];
        debug_assert!(
            samples
                .iter()
                .enumerate()
                .all(|(place, sample)| sample.index() == place)
        );
        samples
    }

    fn index(&self) -> usize {
        match self {
            Self::ReadFailed { .. } => 0,
            Self::SnapshotChanged { .. } => 1,
            Self::TooLarge { .. } => 2,
            Self::Withheld { .. } => 3,
            Self::NotAnalysed { .. } => 4,
            Self::ManifestUnread { .. } => 5,
            Self::ParseError { .. } => 6,
            Self::GrammarOutdated { .. } => 7,
            Self::Unsupported { .. } => 8,
            Self::Deadline { .. } => 9,
            Self::ExtractorFailed { .. } => 10,
            Self::AmbiguousMatch { .. } => 11,
        }
    }
}

fn spans(lines: &[(u32, u32)]) -> String {
    lines
        .iter()
        .map(|(from, to)| {
            if from == to {
                from.to_string()
            } else {
                format!("{from}-{to}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
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
                write!(f, "{path}: parse errors at lines {}", spans(lines))
            }
            Self::GrammarOutdated { path, lines } => write!(
                f,
                "{path}: the pinned grammar reads lines {} as errors and a newer release reads them",
                spans(lines)
            ),
            Self::Unsupported { path } => write!(f, "{path}: no extractor for its language"),
            Self::Deadline { stage } => write!(f, "the run stopped in the {stage:?} stage"),
            Self::ExtractorFailed { path } => write!(f, "{path}: the extractor failed"),
            Self::AmbiguousMatch { path } => {
                write!(
                    f,
                    "{path}: a changed definition has more than one counterpart"
                )
            }
        }
    }
}
