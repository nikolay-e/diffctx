use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    #[error("{0}")]
    Git(String),
    #[error("not a git repository: {0}")]
    NotARepo(PathBuf),
    #[error("invalid range: {0}")]
    InvalidRange(String),
    #[error(
        "invalid duration '{0}': a window is whole <number><unit> components such as 24h, 8d, 90min or 1h30m"
    )]
    InvalidDuration(String),
    #[error("invalid path '{0}': paths stay inside the repository and carry no pathspec magic")]
    InvalidPath(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("timeout after {0}s")]
    Timeout(u64),
    #[error("cancelled")]
    Cancelled,
    #[error("cat-file protocol: {0}")]
    Protocol(String),
    #[error("{path} is listed with object {oid}, which the repository does not have")]
    MissingObject { path: String, oid: String },
    #[error("{path} is too large to read ({size} bytes)")]
    TooLarge { path: String, size: u64 },
    #[error("the reflog cursor is past the end of the log: the log was rewritten or expired")]
    StaleCursor,
    #[error("{0}")]
    Range(String),
    #[error("workspace model: {0}")]
    Workspace(String),
}

impl SnapshotError {
    /// The caller misspoke: a usage error, not a git or environment failure.
    pub fn is_usage(&self) -> bool {
        matches!(
            self,
            Self::InvalidRange(_) | Self::InvalidDuration(_) | Self::InvalidPath(_)
        )
    }

    /// The run ran out of time or was withdrawn: what follows is partial,
    /// never a finding about the file that was being read.
    pub fn is_interruption(&self) -> bool {
        matches!(self, Self::Timeout(_) | Self::Cancelled)
    }
}

pub type Result<T> = std::result::Result<T, SnapshotError>;
