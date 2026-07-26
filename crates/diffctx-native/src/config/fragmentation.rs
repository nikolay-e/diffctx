use once_cell::sync::Lazy;

pub struct FragmentationConfig {
    pub binary_detection_buffer_size: usize,
    pub generated_marker_header_lines: usize,
}

impl Default for FragmentationConfig {
    fn default() -> Self {
        Self {
            binary_detection_buffer_size: 8192,
            generated_marker_header_lines: 10,
        }
    }
}

pub static FRAGMENTATION: Lazy<FragmentationConfig> = Lazy::new(FragmentationConfig::default);
