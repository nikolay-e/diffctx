use once_cell::sync::Lazy;

pub struct RenderConfig {
    pub section_symbol_max_chars: usize,
}

impl Default for RenderConfig {
    fn default() -> Self {
        Self {
            section_symbol_max_chars: 50,
        }
    }
}

pub static RENDER: Lazy<RenderConfig> = Lazy::new(RenderConfig::default);
