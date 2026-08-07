use crate::config::env_overrides::{read_env_fraction, read_env_u32};

#[derive(Clone)]
pub struct SelectionConfig {
    pub core_budget_fraction: f64,
    pub r_cap_min: f64,
    /// Share of the run budget one file may consume while other files still
    /// have unplaced candidates (#194: a single +9k-line data blob monopolized
    /// 364 of 370 output sections). The ceiling binds only in the first
    /// placement phase — leftovers no other file claims flow back to whoever
    /// wants them, so a single-file change is unaffected.
    pub per_file_budget_fraction: f64,
}

impl Default for SelectionConfig {
    fn default() -> Self {
        Self {
            // v5 re-calibration under per-file admission: validated winner
            // (tau, cbf) = (0.05, 0.4); cbf margin over 0.5 is small but
            // consistent in direction on both the calibration and held-out
            // manifests. Surface flat and monotone, as in v4.
            core_budget_fraction: 0.4,
            r_cap_min: 0.01,
            per_file_budget_fraction: 0.25,
        }
    }
}

#[derive(Clone)]
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

#[derive(Clone)]
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

pub fn selection() -> SelectionConfig {
    SelectionConfig {
        core_budget_fraction: read_env_fraction("DIFFCTX_OP_SELECTION_CORE_BUDGET_FRACTION", 0.4),
        r_cap_min: read_env_fraction("DIFFCTX_OP_SELECTION_R_CAP_MIN", 0.01),
        per_file_budget_fraction: read_env_fraction(
            "DIFFCTX_OP_SELECTION_PER_FILE_BUDGET_FRACTION",
            0.25,
        ),
    }
}

pub fn rescue() -> RescueConfig {
    RescueConfig {
        budget_fraction: read_env_fraction("DIFFCTX_OP_RESCUE_BUDGET_FRACTION", 0.05),
        min_score_percentile: read_env_fraction("DIFFCTX_OP_RESCUE_MIN_SCORE_PERCENTILE", 0.80),
    }
}

pub fn boltzmann() -> BoltzmannConfig {
    BoltzmannConfig {
        calibration_tolerance: read_env_fraction(
            "DIFFCTX_OP_BOLTZMANN_CALIBRATION_TOLERANCE",
            0.05,
        ),
        bisect_iters: read_env_u32("DIFFCTX_OP_BOLTZMANN_BISECT_ITERS", 24).max(1),
        ..BoltzmannConfig::default()
    }
}
