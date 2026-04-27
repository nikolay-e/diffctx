use once_cell::sync::Lazy;

pub struct NeedsConfig {
    pub min_symbol_length: usize,
    pub background_min_ident_length: usize,

    pub definition_priority: f64,
    pub call_definition_priority: f64,
    pub signature_priority: f64,
    pub impact_priority: f64,
    pub invariant_priority: f64,
    pub test_priority: f64,
    pub background_priority: f64,
    pub concept_background_priority: f64,
    pub fallback_priority: f64,
    pub identifier_default_priority: f64,

    pub defines_scope_match: f64,
    pub defines_no_scope: f64,
    pub defines_other_scope: f64,
    pub impact_scope_match: f64,
    pub impact_mentions: f64,
    pub signature_defines: f64,
    pub test_mentions: f64,
    pub mentions_fallback: f64,

    pub min_rel_for_bonus: f64,
    pub relatedness_bonus: f64,
}

impl Default for NeedsConfig {
    fn default() -> Self {
        Self {
            min_symbol_length: 3,
            background_min_ident_length: 5,

            definition_priority: 0.9,
            call_definition_priority: 1.0,
            signature_priority: 0.7,
            impact_priority: 0.8,
            invariant_priority: 0.85,
            test_priority: 0.6,
            background_priority: 0.2,
            concept_background_priority: 0.3,
            fallback_priority: 0.5,
            identifier_default_priority: 0.5,

            defines_scope_match: 1.0,
            defines_no_scope: 0.5,
            defines_other_scope: 0.3,
            impact_scope_match: 0.15,
            impact_mentions: 0.8,
            signature_defines: 0.7,
            test_mentions: 0.6,
            mentions_fallback: 0.3,

            min_rel_for_bonus: 0.03,
            relatedness_bonus: 0.25,
        }
    }
}

pub static NEEDS: Lazy<NeedsConfig> = Lazy::new(NeedsConfig::default);
