use once_cell::sync::Lazy;

use super::env_overrides::{read_env_f64, read_env_fraction, read_env_open_fraction};

pub struct EgoScoringConfig {
    pub identifier_overlap_epsilon: f64,
    pub identifier_overlap_cap: usize,
    pub per_hop_decay: f64,
}

impl Default for EgoScoringConfig {
    fn default() -> Self {
        Self {
            identifier_overlap_epsilon: read_env_fraction("DIFFCTX_EGO_LEXICAL_EPS", 0.1),
            identifier_overlap_cap: 10,
            per_hop_decay: read_env_open_fraction("DIFFCTX_EGO_PER_HOP_DECAY", 0.5),
        }
    }
}

pub static EGO: Lazy<EgoScoringConfig> = Lazy::new(EgoScoringConfig::default);

pub struct RrfConfig {
    pub k: f64,
}

impl Default for RrfConfig {
    fn default() -> Self {
        Self {
            // k=60 is the Cormack et al. 2009 constant; it damps the top of
            // each component list so a rank-1 hit in one signal cannot alone
            // outrank an item both signals agree on at ranks 2-3.
            k: read_env_f64("DIFFCTX_RRF_K", 60.0).max(1.0),
        }
    }
}

pub fn rrf() -> RrfConfig {
    RrfConfig::default()
}
