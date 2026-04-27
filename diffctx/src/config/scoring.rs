use once_cell::sync::Lazy;

use crate::config::env_overrides::read_env_f64;

pub struct EgoScoringConfig {
    pub identifier_overlap_epsilon: f64,
    pub identifier_overlap_cap: usize,
    pub per_hop_decay: f64,
}

impl Default for EgoScoringConfig {
    fn default() -> Self {
        Self {
            identifier_overlap_epsilon: 0.1,
            identifier_overlap_cap: 10,
            per_hop_decay: 1.0,
        }
    }
}

pub static EGO: Lazy<EgoScoringConfig> = Lazy::new(|| EgoScoringConfig {
    per_hop_decay: read_env_f64("DIFFCTX_OP_EGO_PER_HOP_DECAY", 1.0),
    ..EgoScoringConfig::default()
});
