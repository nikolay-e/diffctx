use crate::config::limits::PPR;
use crate::config::mode::mode as mode_config;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoringMode {
    Ppr,
    Ego,
    Bm25,
    Rrf,
    Pit,
}

impl ScoringMode {
    pub fn from_str(s: &str) -> Result<Self, String> {
        match s.to_lowercase().as_str() {
            "ppr" => Ok(Self::Ppr),
            "ego" => Ok(Self::Ego),
            "bm25" => Ok(Self::Bm25),
            "rrf" => Ok(Self::Rrf),
            "pit" => Ok(Self::Pit),
            other => Err(format!(
                "unknown scoring_mode '{other}': expected one of {}",
                SCORING_MODE_NAMES.join("|")
            )),
        }
    }
}

/// The single source of truth for what `--scoring` accepts.
///
/// Both CLIs enumerate the accepted values for their own argument parsers, and
/// both silently kept their own copy: `pit` was reachable through the engine and
/// the eval harness while `diffctx --scoring pit` rejected it as invalid. A test
/// pins this array against `from_str` in both directions.
pub const SCORING_MODE_NAMES: &[&str] = &["ppr", "ego", "bm25", "rrf", "pit"];

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
    Rrf,
    Pit,
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
    pub bm25_top_k: usize,
    pub ego_depth: usize,
    pub ppr_alpha: f64,
}

impl PipelineConfig {
    pub fn from_mode(mode: ScoringMode) -> Self {
        let m = mode_config();
        match mode {
            ScoringMode::Ppr => Self {
                discovery: DiscoveryKind::Ensemble,
                scoring: ScoringKind::Ppr,
                bm25_top_k: m.bm25_top_k_primary,
                ego_depth: m.ego_depth_default,
                ppr_alpha: PPR.alpha,
                objective: ObjectiveMode::Submodular,
            },
            ScoringMode::Ego => Self {
                discovery: DiscoveryKind::Ensemble,
                scoring: ScoringKind::Ego,
                bm25_top_k: m.bm25_top_k_primary,
                ego_depth: m.ego_depth_extended,
                ppr_alpha: PPR.alpha,
                objective: ObjectiveMode::Submodular,
            },
            ScoringMode::Bm25 => Self {
                discovery: DiscoveryKind::Ensemble,
                scoring: ScoringKind::Bm25,
                bm25_top_k: m.bm25_top_k_off,
                ego_depth: m.ego_depth_default,
                ppr_alpha: PPR.alpha,
                objective: ObjectiveMode::Submodular,
            },
            // Discovery is deliberately identical to EGO's: the fusion gain
            // must come from ranking the same universe better, not from a
            // wider universe that would confound it with a discovery change.
            ScoringMode::Rrf => Self {
                discovery: DiscoveryKind::Ensemble,
                scoring: ScoringKind::Rrf,
                bm25_top_k: m.bm25_top_k_primary,
                ego_depth: m.ego_depth_extended,
                ppr_alpha: PPR.alpha,
                objective: ObjectiveMode::Submodular,
            },
            // Same universe and the same depth as Rrf, so the two fusion modes
            // differ only in how the two signals are combined. Comparing PIT
            // against RRF is then a comparison of blends, not of candidate
            // supply.
            ScoringMode::Pit => Self {
                discovery: DiscoveryKind::Ensemble,
                scoring: ScoringKind::Pit,
                bm25_top_k: m.bm25_top_k_primary,
                ego_depth: m.ego_depth_extended,
                ppr_alpha: PPR.alpha,
                objective: ObjectiveMode::Submodular,
            },
        }
    }
}

#[cfg(test)]
mod scoring_mode_name_tests {
    use super::{SCORING_MODE_NAMES, ScoringMode};

    #[test]
    fn every_advertised_name_parses() {
        for name in SCORING_MODE_NAMES {
            assert!(
                ScoringMode::from_str(name).is_ok(),
                "advertised scoring mode does not parse: {name}"
            );
        }
    }

    /// The other direction, which is the one that actually broke: a mode added
    /// to `from_str` but not to the advertised list is invisible to `--scoring`.
    #[test]
    fn every_parsable_mode_is_advertised() {
        for candidate in ["ppr", "ego", "bm25", "rrf", "pit"] {
            if ScoringMode::from_str(candidate).is_ok() {
                assert!(
                    SCORING_MODE_NAMES.contains(&candidate),
                    "{candidate} parses but is not advertised to the CLIs"
                );
            }
        }
    }
}
