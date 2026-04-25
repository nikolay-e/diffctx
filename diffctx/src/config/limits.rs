use once_cell::sync::Lazy;

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

pub struct PPRConfig {
    pub alpha: f64,
}

impl Default for PPRConfig {
    fn default() -> Self {
        Self { alpha: 0.60 }
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
}

impl Default for CochangeConfig {
    fn default() -> Self {
        Self {
            weight: 0.40,
            min_count: 2,
            max_files_per_commit: 30,
            commits_limit: 500,
            timeout_seconds: 10,
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
    pub gamma: f64,
    pub r_cap_sigma: f64,
    pub proximity_decay: f64,
    pub peripheral_cap: f64,
}

impl Default for UtilityConfig {
    fn default() -> Self {
        Self {
            eta: 0.20,
            gamma: 0.10,
            r_cap_sigma: 2.0,
            proximity_decay: 0.30,
            peripheral_cap: 0.15,
        }
    }
}

pub static LIMITS: Lazy<AlgorithmLimits> = Lazy::new(AlgorithmLimits::default);
pub static PPR: Lazy<PPRConfig> = Lazy::new(PPRConfig::default);
pub static LEXICAL: Lazy<LexicalConfig> = Lazy::new(LexicalConfig::default);
pub static COCHANGE: Lazy<CochangeConfig> = Lazy::new(CochangeConfig::default);
pub static SIBLING: Lazy<SiblingConfig> = Lazy::new(SiblingConfig::default);
pub static UTILITY: Lazy<UtilityConfig> = Lazy::new(UtilityConfig::default);
