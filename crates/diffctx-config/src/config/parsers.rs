use once_cell::sync::Lazy;

pub struct ParsersConfig {
    pub generic_max_lines: usize,
    pub sub_fragment_threshold_lines: u32,
    pub sub_fragment_target_lines: u32,
    pub max_sub_depth: u32,
    pub max_recursion_depth: u32,
    pub container_search_max_depth: u32,
    pub min_fragment_lines: u32,
}

impl Default for ParsersConfig {
    fn default() -> Self {
        Self {
            generic_max_lines: 200,
            sub_fragment_threshold_lines: 30,
            sub_fragment_target_lines: 20,
            max_sub_depth: 3,
            max_recursion_depth: 500,
            container_search_max_depth: 3,
            min_fragment_lines: 1,
        }
    }
}

pub static PARSERS: Lazy<ParsersConfig> = Lazy::new(ParsersConfig::default);
