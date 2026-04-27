use once_cell::sync::Lazy;

pub struct BudgetConfig {
    pub unlimited: u32,
    pub auto_multiplier: f64,
    pub auto_min: u32,
    pub auto_max: u32,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            unlimited: 10_000_000,
            auto_multiplier: 5.0,
            auto_min: 8_000,
            auto_max: 124_000,
        }
    }
}

pub static BUDGET: Lazy<BudgetConfig> = Lazy::new(BudgetConfig::default);
