use once_cell::sync::Lazy;

use crate::config::env_overrides::{read_env_fraction, read_env_u32};

pub struct SelectionConfig {
    pub core_budget_fraction: f64,
    pub r_cap_min: f64,
    pub stopping_threshold: f64,
}

impl Default for SelectionConfig {
    fn default() -> Self {
        Self {
            core_budget_fraction: 0.70,
            r_cap_min: 0.01,
            stopping_threshold: 0.08,
        }
    }
}

pub struct RescueConfig {
    pub budget_fraction: f64,
    pub min_score_percentile: f64,
}

impl Default for RescueConfig {
    fn default() -> Self {
        Self {
            budget_fraction: 0.05,
            min_score_percentile: 0.80,
        }
    }
}

pub struct BoltzmannConfig {
    pub beta_lo: f64,
    pub beta_hi: f64,
    pub bisect_iters: u32,
    pub calibration_tolerance: f64,
}

impl Default for BoltzmannConfig {
    fn default() -> Self {
        Self {
            beta_lo: 1e-6,
            beta_hi: 1.0,
            bisect_iters: 24,
            calibration_tolerance: 0.05,
        }
    }
}

pub static SELECTION: Lazy<SelectionConfig> = Lazy::new(|| SelectionConfig {
    core_budget_fraction: read_env_fraction("DIFFCTX_OP_SELECTION_CORE_BUDGET_FRACTION", 0.70),
    r_cap_min: read_env_fraction("DIFFCTX_OP_SELECTION_R_CAP_MIN", 0.01),
    stopping_threshold: read_env_fraction("DIFFCTX_OP_SELECTION_STOPPING_THRESHOLD", 0.08),
});
pub static RESCUE: Lazy<RescueConfig> = Lazy::new(|| RescueConfig {
    budget_fraction: read_env_fraction("DIFFCTX_OP_RESCUE_BUDGET_FRACTION", 0.05),
    min_score_percentile: read_env_fraction("DIFFCTX_OP_RESCUE_MIN_SCORE_PERCENTILE", 0.80),
});
pub static BOLTZMANN: Lazy<BoltzmannConfig> = Lazy::new(|| BoltzmannConfig {
    calibration_tolerance: read_env_fraction("DIFFCTX_OP_BOLTZMANN_CALIBRATION_TOLERANCE", 0.05),
    bisect_iters: read_env_u32("DIFFCTX_OP_BOLTZMANN_BISECT_ITERS", 24).max(1),
    ..BoltzmannConfig::default()
});
