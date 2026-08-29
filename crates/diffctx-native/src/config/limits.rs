use once_cell::sync::Lazy;

use crate::config::env_overrides::{read_env_f64, read_env_fraction, read_env_open_fraction};

pub struct AlgorithmLimits {
    pub max_file_size: usize,
    pub max_changed_file_size: usize,
    pub max_fragments: usize,
    pub max_generated_fragments: usize,
    pub max_generated_lines: usize,
    pub skip_expensive_threshold: usize,
    pub rare_identifier_threshold: usize,
    pub overhead_per_fragment: u32,
}

impl Default for AlgorithmLimits {
    fn default() -> Self {
        let max_fragments = std::env::var("DIFFCTX_MAX_FRAGMENTS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|&v| v >= 1)
            .unwrap_or(200);
        Self {
            max_file_size: 100_000,
            max_changed_file_size: 5_000_000,
            max_fragments,
            max_generated_fragments: 5,
            max_generated_lines: 30,
            skip_expensive_threshold: 2000,
            rare_identifier_threshold: 3,
            // Measured against real YAML serialization (path/lines/kind/symbol
            // keys + quoting): actual scaffold runs ~40-45 tokens/fragment,
            // not 18 - the old estimate let --budget overshoot by ~18%.
            overhead_per_fragment: 40,
        }
    }
}

/// Hard ceiling for a single `git cat-file` blob read. Sized with headroom
/// above `max_changed_file_size` (5 MB) so legitimate large changed files still
/// load, while a pathological multi-hundred-MB blob is drained and rejected
/// instead of allocated up front.
pub const MAX_BLOB_READ_BYTES: usize = 16_000_000;

/// The scoring mode every entry point ships. Named rather than repeated as a
/// literal in the CLI arg, three pyo3 signatures and the Python layers: those
/// copies are what would make a future default change land in some entry points
/// and not others.
pub const DEFAULT_SCORING: &str = "ego";

pub const DEFAULT_PPR_ALPHA: f64 = 0.60;
/// v5 re-calibration under per-file admission (4x3 grid, v1 calibration
/// manifest, held-out validated): winner (tau, cbf) = (0.05, 0.4) at
/// min(per_benchmark file_recall) = 0.6815 vs 0.6684 for the old point.
/// Confirmed on the test splits (500x3): file_recall parity with the old
/// default, contextbench file_precision +0.041 (CI excludes zero), and on
/// dcbench-372 paired nontrivial recall within noise at -4% tokens. The
/// weak stop only ships together with the admission gate (#65): without it,
/// tau=0.05 re-admits the diffuse tail the gate exists to block.
pub const DEFAULT_STOPPING_THRESHOLD: f64 = 0.05;
/// The pre-admission calibration point. A scorer that ships no admission
/// gate — BM25 has no graph to walk, and `DIFFCTX_FILE_ADMISSION=0` turns the
/// gate off for the others — gets this instead of the weak stop above, so the
/// two never ship apart. An explicit `--tau` still wins.
pub const UNGATED_STOPPING_THRESHOLD: f64 = 0.12;
pub const DEFAULT_PIPELINE_TIMEOUT_SECONDS: u64 = 300;

pub struct PPRConfig {
    pub alpha: f64,
    pub default_seed_epsilon: f64,
    pub push_scale_factor: usize,
    pub max_pushes_cap: usize,
    pub convergence_tolerance: f64,
    pub forward_blend: f64,
}

impl Default for PPRConfig {
    fn default() -> Self {
        Self {
            alpha: DEFAULT_PPR_ALPHA,
            default_seed_epsilon: 0.1,
            push_scale_factor: 100,
            max_pushes_cap: 2_000_000,
            convergence_tolerance: 1e-4,
            forward_blend: 0.4,
        }
    }
}

pub struct LexicalConfig {
    pub min_similarity: f64,
    pub top_k_neighbors: usize,
    pub max_df_ratio: f64,
    pub min_idf: f64,
    pub max_postings: usize,
    pub weight_min: f64,
    pub weight_max: f64,
    pub backward_factor: f64,
}

impl Default for LexicalConfig {
    fn default() -> Self {
        Self {
            min_similarity: 0.30,
            top_k_neighbors: 5,
            max_df_ratio: 0.15,
            min_idf: 2.0,
            max_postings: 100,
            weight_min: 0.05,
            weight_max: 0.15,
            backward_factor: 0.5,
        }
    }
}

pub struct CochangeConfig {
    pub min_count: usize,
    pub max_files_per_commit: usize,
    pub commits_limit: usize,
    pub log_scale_factor: f64,
}

impl Default for CochangeConfig {
    fn default() -> Self {
        Self {
            min_count: 2,
            max_files_per_commit: 30,
            commits_limit: 500,
            log_scale_factor: 0.1,
        }
    }
}

pub struct SiblingConfig {
    pub max_files_per_dir: usize,
}

impl Default for SiblingConfig {
    fn default() -> Self {
        Self {
            max_files_per_dir: 20,
        }
    }
}

pub struct UtilityConfig {
    pub eta: f64,
    pub structural_bonus_weight: f64,
    pub r_cap_sigma: f64,
    pub proximity_decay: f64,
}

impl Default for UtilityConfig {
    fn default() -> Self {
        Self {
            eta: 0.20,
            structural_bonus_weight: 0.10,
            r_cap_sigma: 2.0,
            proximity_decay: 0.30,
        }
    }
}

pub static LIMITS: Lazy<AlgorithmLimits> = Lazy::new(AlgorithmLimits::default);
pub static PPR: Lazy<PPRConfig> = Lazy::new(|| PPRConfig {
    alpha: read_env_open_fraction("DIFFCTX_OP_PPR_ALPHA", DEFAULT_PPR_ALPHA),
    forward_blend: read_env_fraction("DIFFCTX_OP_PPR_FORWARD_BLEND", 0.4),
    ..PPRConfig::default()
});
pub static LEXICAL: Lazy<LexicalConfig> = Lazy::new(LexicalConfig::default);
pub static COCHANGE: Lazy<CochangeConfig> = Lazy::new(CochangeConfig::default);
pub static SIBLING: Lazy<SiblingConfig> = Lazy::new(SiblingConfig::default);
pub static UTILITY: Lazy<UtilityConfig> = Lazy::new(|| UtilityConfig {
    eta: read_env_f64("DIFFCTX_OP_UTILITY_ETA", 0.20),
    structural_bonus_weight: read_env_f64("DIFFCTX_OP_UTILITY_STRUCTURAL_BONUS_WEIGHT", 0.10),
    r_cap_sigma: read_env_f64("DIFFCTX_OP_UTILITY_R_CAP_SIGMA", 2.0),
    proximity_decay: read_env_f64("DIFFCTX_OP_UTILITY_PROXIMITY_DECAY", 0.30),
});
