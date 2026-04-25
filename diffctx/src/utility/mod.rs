pub mod needs;
pub mod scoring;

pub use needs::InformationNeed;
pub use scoring::{UtilityState, apply_fragment, compute_density, marginal_gain, utility_value};
