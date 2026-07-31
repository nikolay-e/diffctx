use once_cell::sync::Lazy;

use crate::config::env_overrides::read_env_f64;

pub struct FilteringConfig {
    pub proximity_floor_max: f64,
    pub proximity_half_decay: f64,
    pub definition_proximity_half_decay: f64,
    pub hub_reverse_threshold: usize,
    pub max_context_fragments_per_file: usize,
}

impl Default for FilteringConfig {
    fn default() -> Self {
        Self {
            proximity_floor_max: 0.04,
            proximity_half_decay: 50.0,
            definition_proximity_half_decay: 5.0,
            hub_reverse_threshold: 2,
            max_context_fragments_per_file: 30,
        }
    }
}

pub static FILTERING: Lazy<FilteringConfig> = Lazy::new(|| FilteringConfig {
    proximity_half_decay: read_env_f64("DIFFCTX_OP_FILTERING_PROXIMITY_HALF_DECAY", 50.0),
    definition_proximity_half_decay: read_env_f64(
        "DIFFCTX_OP_FILTERING_DEFINITION_PROXIMITY_HALF_DECAY",
        5.0,
    ),
    ..FilteringConfig::default()
});
