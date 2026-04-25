#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoringMode {
    Hybrid,
    Ppr,
    Ego,
}

impl ScoringMode {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "ppr" => Self::Ppr,
            "ego" => Self::Ego,
            _ => Self::Hybrid,
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
}

#[derive(Debug, Clone)]
pub struct PipelineConfig {
    pub discovery: DiscoveryKind,
    pub scoring: ScoringKind,
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
                bm25_top_k: 1,
                ego_depth: 1,
                ppr_alpha: 0.60,
            },
            ScoringMode::Ego => Self {
                discovery: DiscoveryKind::Ensemble,
                scoring: ScoringKind::Ego,
                low_relevance_filter: false,
                bm25_top_k: 1,
                ego_depth: 2,
                ppr_alpha: 0.60,
            },
            ScoringMode::Hybrid => {
                let is_large = n_candidate_files > 50;
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
                    bm25_top_k: if is_large { 1 } else { 0 },
                    ego_depth: if is_large { 2 } else { 1 },
                    ppr_alpha: 0.60,
                }
            }
        }
    }
}
