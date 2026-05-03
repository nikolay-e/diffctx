//! Per-`EdgeCategory` weight multipliers — the calibratable `w_τ` from the
//! paper.
//!
//! Each fine-grained edge type (130+ in `weights.rs`) carries a default
//! domain-prior weight. On top, every edge is scaled by a per-category
//! multiplier `w_τ ∈ R_{≥0}`, one per `EdgeCategory` variant (10 total).
//! These ten scalars are the parameters intended for offline calibration
//! (Bayesian opt / grid search) against a labeled corpus, as described in
//! paper Section 4.3 (Edge-Type Weight Calibration).
//!
//! Default for every variant is 1.0 (no scaling — the fine-grained
//! prior weights from `weights.rs` apply unchanged).

use once_cell::sync::Lazy;

use crate::graph::EdgeCategory;

#[derive(Debug, Clone, Copy)]
pub struct CategoryWeights {
    pub semantic: f64,
    pub structural: f64,
    pub sibling: f64,
    pub config: f64,
    pub config_generic: f64,
    pub document: f64,
    pub similarity: f64,
    pub history: f64,
    pub test_edge: f64,
    pub generic: f64,
}

impl Default for CategoryWeights {
    fn default() -> Self {
        Self {
            semantic: 1.0,
            structural: 1.0,
            sibling: 1.0,
            config: 1.0,
            config_generic: 1.0,
            document: 1.0,
            similarity: 1.0,
            history: 1.0,
            test_edge: 1.0,
            generic: 1.0,
        }
    }
}

impl CategoryWeights {
    /// Multiplier applied to every edge of the given category before scoring.
    pub fn multiplier(&self, category: EdgeCategory) -> f64 {
        match category {
            EdgeCategory::Semantic => self.semantic,
            EdgeCategory::Structural => self.structural,
            EdgeCategory::Sibling => self.sibling,
            EdgeCategory::Config => self.config,
            EdgeCategory::ConfigGeneric => self.config_generic,
            EdgeCategory::Document => self.document,
            EdgeCategory::Similarity => self.similarity,
            EdgeCategory::History => self.history,
            EdgeCategory::TestEdge => self.test_edge,
            EdgeCategory::Generic => self.generic,
        }
    }
}

pub static CATEGORY_WEIGHTS: Lazy<CategoryWeights> = Lazy::new(CategoryWeights::default);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_unity() {
        let w = CategoryWeights::default();
        for cat in [
            EdgeCategory::Semantic,
            EdgeCategory::Structural,
            EdgeCategory::Sibling,
            EdgeCategory::Config,
            EdgeCategory::ConfigGeneric,
            EdgeCategory::Document,
            EdgeCategory::Similarity,
            EdgeCategory::History,
            EdgeCategory::TestEdge,
            EdgeCategory::Generic,
        ] {
            assert_eq!(w.multiplier(cat), 1.0);
        }
    }

    #[test]
    fn multiplier_reaches_each_variant() {
        let w = CategoryWeights {
            semantic: 0.1,
            structural: 0.2,
            sibling: 0.3,
            config: 0.4,
            config_generic: 0.5,
            document: 0.6,
            similarity: 0.7,
            history: 0.8,
            test_edge: 0.9,
            generic: 0.05,
        };
        assert_eq!(w.multiplier(EdgeCategory::Semantic), 0.1);
        assert_eq!(w.multiplier(EdgeCategory::Structural), 0.2);
        assert_eq!(w.multiplier(EdgeCategory::Sibling), 0.3);
        assert_eq!(w.multiplier(EdgeCategory::Config), 0.4);
        assert_eq!(w.multiplier(EdgeCategory::ConfigGeneric), 0.5);
        assert_eq!(w.multiplier(EdgeCategory::Document), 0.6);
        assert_eq!(w.multiplier(EdgeCategory::Similarity), 0.7);
        assert_eq!(w.multiplier(EdgeCategory::History), 0.8);
        assert_eq!(w.multiplier(EdgeCategory::TestEdge), 0.9);
        assert_eq!(w.multiplier(EdgeCategory::Generic), 0.05);
    }
}
