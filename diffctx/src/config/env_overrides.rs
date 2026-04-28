//! Shared helpers for reading config parameters from environment variables.
//!
//! Used by `category_weights.rs` and the Group-C operational-parameter
//! overrides documented in `docs/parameter-strategy.md`. The pattern is:
//! `Lazy::new` reads the env var once at first access; tests verify the
//! pure parser (`parse_*_or_default`) directly so they do not need to
//! mutate process-global env state.

pub fn parse_f64_or_default(raw: Option<String>, default: f64) -> f64 {
    raw.and_then(|s| s.parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v >= 0.0)
        .unwrap_or(default)
}

pub fn parse_fraction_or_default(raw: Option<String>, default: f64) -> f64 {
    parse_f64_or_default(raw, default).clamp(0.0, 1.0)
}

/// Parse a fraction strictly inside the open interval (0, 1).
/// Used for parameters where 0.0 or 1.0 produce algorithmic degeneracy
/// (e.g. PPR_ALPHA=1.0 makes restart probability zero, yielding all-zero rankings).
pub fn parse_open_fraction_or_default(raw: Option<String>, default: f64) -> f64 {
    const EPS: f64 = 1e-4;
    parse_f64_or_default(raw, default).clamp(EPS, 1.0 - EPS)
}

pub fn parse_usize_or_default(raw: Option<String>, default: usize) -> usize {
    raw.and_then(|s| s.parse::<usize>().ok()).unwrap_or(default)
}

pub fn parse_u32_or_default(raw: Option<String>, default: u32) -> u32 {
    raw.and_then(|s| s.parse::<u32>().ok()).unwrap_or(default)
}

pub fn read_env_f64(name: &str, default: f64) -> f64 {
    parse_f64_or_default(std::env::var(name).ok(), default)
}

pub fn read_env_fraction(name: &str, default: f64) -> f64 {
    parse_fraction_or_default(std::env::var(name).ok(), default)
}

pub fn read_env_open_fraction(name: &str, default: f64) -> f64 {
    parse_open_fraction_or_default(std::env::var(name).ok(), default)
}

pub fn read_env_usize(name: &str, default: usize) -> usize {
    parse_usize_or_default(std::env::var(name).ok(), default)
}

pub fn read_env_u32(name: &str, default: u32) -> u32 {
    parse_u32_or_default(std::env::var(name).ok(), default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f64_accepts_finite_nonneg() {
        assert_eq!(parse_f64_or_default(Some("0.42".into()), 1.0), 0.42);
        assert_eq!(parse_f64_or_default(Some("0".into()), 1.0), 0.0);
    }

    #[test]
    fn f64_rejects_negative_and_nonfinite() {
        assert_eq!(parse_f64_or_default(Some("-0.5".into()), 1.0), 1.0);
        assert_eq!(parse_f64_or_default(Some("nan".into()), 1.0), 1.0);
        assert_eq!(parse_f64_or_default(Some("inf".into()), 1.0), 1.0);
    }

    #[test]
    fn fraction_clamps_into_unit_interval() {
        assert_eq!(parse_fraction_or_default(Some("0.5".into()), 0.7), 0.5);
        assert_eq!(parse_fraction_or_default(Some("1.05".into()), 0.7), 1.0);
        assert_eq!(parse_fraction_or_default(Some("42".into()), 0.7), 1.0);
        assert_eq!(parse_fraction_or_default(Some("-0.5".into()), 0.7), 0.7);
        assert_eq!(parse_fraction_or_default(Some("nan".into()), 0.7), 0.7);
        assert_eq!(parse_fraction_or_default(None, 0.7), 0.7);
    }

    #[test]
    fn open_fraction_clamps_to_open_interval() {
        // Boundary 1.0 → degenerate (PPR α=1 zeros all rankings); must clamp.
        let v_one = parse_open_fraction_or_default(Some("1.0".into()), 0.6);
        assert!(
            v_one < 1.0,
            "open fraction must clamp 1.0 below 1; got {v_one}"
        );
        assert!(
            v_one > 0.99,
            "clamp must stay near 1.0, not collapse to default"
        );
        // Boundary 0.0 → also degenerate; must clamp above 0.
        let v_zero = parse_open_fraction_or_default(Some("0.0".into()), 0.6);
        assert!(
            v_zero > 0.0,
            "open fraction must clamp 0.0 above 0; got {v_zero}"
        );
        // Interior values pass through.
        assert_eq!(parse_open_fraction_or_default(Some("0.6".into()), 0.0), 0.6);
        // Above 1.0 also clamped.
        assert!(parse_open_fraction_or_default(Some("42".into()), 0.6) < 1.0);
    }

    #[test]
    fn f64_falls_back_on_missing_or_unparseable() {
        assert_eq!(parse_f64_or_default(None, 0.7), 0.7);
        assert_eq!(parse_f64_or_default(Some("hello".into()), 0.7), 0.7);
        assert_eq!(parse_f64_or_default(Some("".into()), 0.7), 0.7);
    }

    #[test]
    fn usize_parses_or_falls_back() {
        assert_eq!(parse_usize_or_default(Some("42".into()), 7), 42);
        assert_eq!(parse_usize_or_default(Some("-1".into()), 7), 7);
        assert_eq!(parse_usize_or_default(None, 7), 7);
    }

    #[test]
    fn u32_parses_or_falls_back() {
        assert_eq!(parse_u32_or_default(Some("24".into()), 8), 24);
        assert_eq!(parse_u32_or_default(Some("nope".into()), 8), 8);
    }
}
