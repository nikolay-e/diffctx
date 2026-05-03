use crate::config::env_overrides::{read_env_fraction, read_env_u32};

#[derive(Clone)]
pub struct SelectionConfig {
    pub core_budget_fraction: f64,
    pub r_cap_min: f64,
}

impl Default for SelectionConfig {
    fn default() -> Self {
        Self {
            // Calibrated on the v1 manifest (2119 instances across
            // SWE-bench Lite, PolyBench, Multi-SWE-bench, ContextBench)
            // with the pebble-fixed pool. Winner of the (tau, cbf) grid
            // = (0.12, 0.5) at min(per_benchmark file_recall) = 0.1092.
            // Surface is flat (top-3 within 0.001), so the choice is
            // robust rather than tuned to a sharp optimum.
            core_budget_fraction: 0.5,
            r_cap_min: 0.01,
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
        core_budget_fraction: read_env_fraction("DIFFCTX_OP_SELECTION_CORE_BUDGET_FRACTION", 0.5),
        r_cap_min: read_env_fraction("DIFFCTX_OP_SELECTION_R_CAP_MIN", 0.01),
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
