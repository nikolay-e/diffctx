use once_cell::sync::Lazy;

use super::env_overrides::read_env_open_fraction;

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
            per_hop_decay: read_env_open_fraction("DIFFCTX_EGO_PER_HOP_DECAY", 0.5),
        }
    }
}

pub static EGO: Lazy<EgoScoringConfig> = Lazy::new(EgoScoringConfig::default);
