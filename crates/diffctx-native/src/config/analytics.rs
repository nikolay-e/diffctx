use once_cell::sync::Lazy;

pub struct AnalyticsConfig {
    pub hotspot_degree_weight: f64,
    pub hotspot_churn_weight: f64,
}

impl Default for AnalyticsConfig {
    fn default() -> Self {
        Self {
            hotspot_degree_weight: 0.5,
            hotspot_churn_weight: 0.5,
        }
    }
}

pub static ANALYTICS: Lazy<AnalyticsConfig> = Lazy::new(AnalyticsConfig::default);
