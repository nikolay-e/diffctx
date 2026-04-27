use once_cell::sync::Lazy;

use crate::config::env_overrides::{read_env_f64, read_env_fraction};

pub struct AlgorithmLimits {
    pub max_file_size: usize,
    pub max_fragments: usize,
    pub max_generated_fragments: usize,
    pub max_generated_lines: usize,
    pub skip_expensive_threshold: usize,
    pub rare_identifier_threshold: usize,
    pub overhead_per_fragment: u32,
}

impl Default for AlgorithmLimits {
    fn default() -> Self {
        let max_fragments = std::env::var("TREEMAPPER_MAX_FRAGMENTS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(200);
        Self {
            max_file_size: 100_000,
            max_fragments,
            max_generated_fragments: 5,
            max_generated_lines: 30,
            skip_expensive_threshold: 2000,
            rare_identifier_threshold: 3,
            overhead_per_fragment: 18,
        }
    }
}

pub const DEFAULT_PPR_ALPHA: f64 = 0.60;
pub const DEFAULT_STOPPING_THRESHOLD: f64 = 0.08;
pub const DEFAULT_PIPELINE_TIMEOUT_SECONDS: u64 = 300;
pub const DEFAULT_BUDGET_TOKENS: u32 = 4096;

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
    pub weight: f64,
    pub min_count: usize,
    pub max_files_per_commit: usize,
    pub commits_limit: usize,
    pub timeout_seconds: u64,
    pub log_scale_factor: f64,
}

impl Default for CochangeConfig {
    fn default() -> Self {
        Self {
            weight: 0.40,
            min_count: 2,
            max_files_per_commit: 30,
            commits_limit: 500,
            timeout_seconds: 10,
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
    alpha: read_env_fraction("DIFFCTX_OP_PPR_ALPHA", DEFAULT_PPR_ALPHA),
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
