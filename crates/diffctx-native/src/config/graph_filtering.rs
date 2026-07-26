use once_cell::sync::Lazy;

pub struct GraphFilteringConfig {
    pub hub_out_degree_threshold: usize,
    pub fallback_max_files: usize,
    pub min_lines_for_signature: u32,
    pub max_cache_bytes: usize,
    pub git_rename_similarity_threshold: u32,
}

impl Default for GraphFilteringConfig {
    fn default() -> Self {
        Self {
            hub_out_degree_threshold: 3,
            fallback_max_files: 10_000,
            min_lines_for_signature: 5,
            max_cache_bytes: 200 * 1024 * 1024,
            git_rename_similarity_threshold: 100,
        }
    }
}

pub static GRAPH_FILTERING: Lazy<GraphFilteringConfig> = Lazy::new(GraphFilteringConfig::default);
