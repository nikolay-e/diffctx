use once_cell::sync::Lazy;

pub struct TokenizationConfig {
    pub fragment_min_identifier_length: usize,
    pub query_min_identifier_length: usize,
    pub diff_context_radius: usize,
}

impl Default for TokenizationConfig {
    fn default() -> Self {
        Self {
            fragment_min_identifier_length: 2,
            query_min_identifier_length: 3,
            diff_context_radius: 3,
        }
    }
}

pub static TOKENIZATION: Lazy<TokenizationConfig> = Lazy::new(TokenizationConfig::default);
