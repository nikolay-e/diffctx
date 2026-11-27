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

pub static RRF: Lazy<RrfConfig> = Lazy::new(RrfConfig::default);

pub struct PitConfig {
    /// Weight on the structural component. `1 - blend` goes to the lexical one.
    pub blend: f64,
    /// Bonus added when both components place a fragment inside their own
    /// top-`agreement_top_k`.
    pub agreement_bonus: f64,
    pub agreement_top_k: usize,
}

impl Default for PitConfig {
    fn default() -> Self {
        Self {
            // Structural-leaning, because EGO is the measured stronger arm: it
            // sits 79 cases ahead of RRF on the oracle corpus. An even blend
            // would start the successor behind the mode it is replacing.
            blend: read_env_f64("DIFFCTX_PIT_BLEND", 0.65).clamp(0.0, 1.0),
            // Agreement is worth something but must not dominate a percentile:
            // both components are in [0, 1], so a bonus above ~0.2 would let
            // agreement alone outrank a fragment either signal ranks highly.
            agreement_bonus: read_env_f64("DIFFCTX_PIT_AGREEMENT_BONUS", 0.10).clamp(0.0, 1.0),
            agreement_top_k: read_env_f64("DIFFCTX_PIT_AGREEMENT_TOP_K", 20.0).max(1.0) as usize,
        }
    }
}

pub static PIT: Lazy<PitConfig> = Lazy::new(PitConfig::default);
