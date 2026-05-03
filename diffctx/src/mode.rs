use crate::config::limits::PPR;
use crate::config::mode::MODE;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoringMode {
    Hybrid,
    Ppr,
    Ego,
    Bm25,
}

impl ScoringMode {
    pub fn from_str(s: &str) -> Result<Self, String> {
        match s.to_lowercase().as_str() {
            "hybrid" => Ok(Self::Hybrid),
            "ppr" => Ok(Self::Ppr),
            "ego" => Ok(Self::Ego),
            "bm25" => Ok(Self::Bm25),
            other => Err(format!(
                "unknown scoring_mode '{other}': expected one of hybrid|ppr|ego|bm25"
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryKind {
    Default,
    Ensemble,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoringKind {
    Ppr,
    Ego,
    Bm25,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectiveMode {
    Submodular,
    BoltzmannModular,
}

impl ObjectiveMode {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "boltzmann" | "boltzmann_modular" | "modular_boltzmann" => Self::BoltzmannModular,
            _ => Self::Submodular,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PipelineConfig {
    pub discovery: DiscoveryKind,
    pub scoring: ScoringKind,
    pub objective: ObjectiveMode,
    pub low_relevance_filter: bool,
    pub bm25_top_k: usize,
    pub ego_depth: usize,
    pub ppr_alpha: f64,
}

impl PipelineConfig {
    pub fn from_mode(mode: ScoringMode, n_candidate_files: usize) -> Self {
        match mode {
            ScoringMode::Ppr => Self {
                discovery: DiscoveryKind::Ensemble,
                scoring: ScoringKind::Ppr,
                low_relevance_filter: false,
                bm25_top_k: MODE.bm25_top_k_primary,
                ego_depth: MODE.ego_depth_default,
                ppr_alpha: PPR.alpha,
                objective: ObjectiveMode::Submodular,
            },
            ScoringMode::Ego => Self {
                discovery: DiscoveryKind::Ensemble,
                scoring: ScoringKind::Ego,
                low_relevance_filter: false,
                bm25_top_k: MODE.bm25_top_k_primary,
                ego_depth: MODE.ego_depth_extended,
                ppr_alpha: PPR.alpha,
                objective: ObjectiveMode::Submodular,
            },
            ScoringMode::Bm25 => Self {
                discovery: DiscoveryKind::Ensemble,
                scoring: ScoringKind::Bm25,
                low_relevance_filter: false,
                bm25_top_k: MODE.bm25_top_k_off,
                ego_depth: MODE.ego_depth_default,
                ppr_alpha: PPR.alpha,
                objective: ObjectiveMode::Submodular,
            },
            ScoringMode::Hybrid => {
                let is_large = n_candidate_files > MODE.hybrid_large_candidate_threshold;
                Self {
                    discovery: if is_large {
                        DiscoveryKind::Ensemble
                    } else {
                        DiscoveryKind::Default
                    },
                    scoring: if is_large {
                        ScoringKind::Ego
                    } else {
                        ScoringKind::Ppr
                    },
                    low_relevance_filter: !is_large,
                    bm25_top_k: if is_large {
                        MODE.bm25_top_k_primary
                    } else {
                        MODE.bm25_top_k_off
                    },
                    ego_depth: if is_large {
                        MODE.ego_depth_extended
                    } else {
                        MODE.ego_depth_default
                    },
                    ppr_alpha: PPR.alpha,
                    objective: ObjectiveMode::Submodular,
                }
            }
        }
    }
}
