use once_cell::sync::Lazy;

/// Initial value of the git subprocess timeout; `git::set_git_timeout`
/// overwrites it per run from the CLI/MCP `--timeout`.
pub const DEFAULT_TIMEOUT_SECONDS: u64 = 60;
pub const CATFILE_TERMINATION_TIMEOUT_SECONDS: u64 = 5;

pub struct GitConfig {
    pub catfile_termination_timeout_seconds: u64,
}

impl Default for GitConfig {
    fn default() -> Self {
        Self {
            catfile_termination_timeout_seconds: CATFILE_TERMINATION_TIMEOUT_SECONDS,
        }
    }
}

pub static GIT: Lazy<GitConfig> = Lazy::new(GitConfig::default);
