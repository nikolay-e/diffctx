use once_cell::sync::Lazy;

use crate::config::env_overrides::read_env_usize;

pub struct ModeConfig {
    pub bm25_top_k_primary: usize,
    pub bm25_top_k_off: usize,
    pub ego_depth_default: usize,
    pub ego_depth_extended: usize,
    pub hybrid_large_candidate_threshold: usize,
}

impl Default for ModeConfig {
    fn default() -> Self {
        Self {
            bm25_top_k_primary: 1,
            bm25_top_k_off: 0,
            ego_depth_default: 1,
            ego_depth_extended: 2,
            hybrid_large_candidate_threshold: 50,
        }
    }
}

pub static MODE: Lazy<ModeConfig> = Lazy::new(|| ModeConfig {
    hybrid_large_candidate_threshold: read_env_usize(
        "DIFFCTX_OP_MODE_HYBRID_LARGE_CANDIDATE_THRESHOLD",
        50,
    ),
    ..ModeConfig::default()
});
