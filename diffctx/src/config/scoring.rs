use once_cell::sync::Lazy;

pub struct EgoScoringConfig {
    pub identifier_overlap_epsilon: f64,
    pub identifier_overlap_cap: usize,
}

impl Default for EgoScoringConfig {
    fn default() -> Self {
        Self {
            identifier_overlap_epsilon: 0.1,
            identifier_overlap_cap: 10,
        }
    }
}

pub static EGO: Lazy<EgoScoringConfig> = Lazy::new(EgoScoringConfig::default);
