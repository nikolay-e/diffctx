use once_cell::sync::Lazy;

pub struct BudgetConfig {
    pub unlimited: u32,
    pub auto_multiplier: f64,
    pub auto_min: u32,
    pub auto_max: u32,
    /// Per-core-fragment ceiling when sizing the auto budget. A core fragment
    /// that is a whole-file `Chunk` (e.g. a 2000-line unparsed template touched
    /// by one hunk) would otherwise inflate the budget by its entire byte
    /// weight, dragging in the whole repo as "context". Capping each core
    /// fragment's budget contribution decouples "how big are the files I edited"
    /// from "how much of the repo do I pull in". Does not affect whether the
    /// changed file is included (it always is) — only the context budget it earns.
    pub core_token_cap_per_fragment: u32,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            unlimited: 10_000_000,
            auto_multiplier: 3.0,
            auto_min: 8_000,
            auto_max: 48_000,
            core_token_cap_per_fragment: 1_500,
        }
    }
}

pub static BUDGET: Lazy<BudgetConfig> = Lazy::new(BudgetConfig::default);
