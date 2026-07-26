use once_cell::sync::Lazy;

pub const DEFAULT_TIMEOUT_SECONDS: u64 = 60;
pub const POLL_INTERVAL_MS: u64 = 10;
pub const CATFILE_TERMINATION_TIMEOUT_SECONDS: u64 = 5;

pub struct GitConfig {
    pub default_timeout_seconds: u64,
    pub poll_interval_ms: u64,
    pub catfile_termination_timeout_seconds: u64,
}

impl Default for GitConfig {
    fn default() -> Self {
        Self {
            default_timeout_seconds: DEFAULT_TIMEOUT_SECONDS,
            poll_interval_ms: POLL_INTERVAL_MS,
            catfile_termination_timeout_seconds: CATFILE_TERMINATION_TIMEOUT_SECONDS,
        }
    }
}

pub static GIT: Lazy<GitConfig> = Lazy::new(GitConfig::default);
