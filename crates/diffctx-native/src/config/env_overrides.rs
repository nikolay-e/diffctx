//! Shared helpers for reading config parameters from environment variables.
//!
//! Used by `category_weights.rs` and the Group-C operational-parameter
//! overrides documented in `docs/engineering/parameter-strategy.md`. The pattern is:
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

// Every test above verifies the pure parser with a literal env value, never
// with the env-var *name* it is documented under -- so a typo'd name (e.g.
// `DIFFCTX_OP_EGO_PER_HOP_DECAY` instead of the real `DIFFCTX_EGO_PER_HOP_DECAY`
// read at `config/scoring.rs:16`) sweeps nothing and every test above still
// passes. These tests close that gap by checking the *set of names* rather
// than mutating process-global env state per variable (flaky under parallel
// `cargo test`).
#[cfg(test)]
mod override_name_consistency {
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    use regex::Regex;

    fn crate_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    fn extract(text: &str, re: &Regex) -> BTreeSet<String> {
        re.captures_iter(text).map(|c| c[1].to_string()).collect()
    }

    fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_rs_files(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    // Every `DIFFCTX_*` literal read through the `read_env_*` helpers
    // defined in this module -- the actual Group-C operational-parameter
    // plumbing, as opposed to the handful of unrelated toggles
    // (`DIFFCTX_OBJECTIVE`, `DIFFCTX_MAX_FRAGMENTS`,
    // `DIFFCTX_MAX_EDGES_PER_NODE`, `DIFFCTX_NO_COMMIT_SIGNAL`,
    // `DIFFCTX_TOKEN_CACHE_*`) that call `std::env::var`/`var_os` directly
    // and are explicitly out of scope per parameter-strategy.md's "other
    // internal toggles" callout. This file (`env_overrides.rs`) is excluded
    // from the scan: it defines the generic readers but never hardcodes a
    // `DIFFCTX_*` name itself, and excluding it keeps this test's own
    // literals (below) out of the scanned set.
    fn code_literal_set() -> BTreeSet<String> {
        let re = Regex::new(
            r#"read_env_(?:f64|fraction|open_fraction|usize|u32)\(\s*"(DIFFCTX_[A-Z0-9_]+)""#,
        )
        .unwrap();
        let mut files = Vec::new();
        collect_rs_files(&crate_root().join("src"), &mut files);
        let mut set = BTreeSet::new();
        for file in files {
            if file.file_name().and_then(|n| n.to_str()) == Some("env_overrides.rs") {
                continue;
            }
            let text =
                std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("reading {file:?}: {e}"));
            set.extend(extract(&text, &re));
        }
        set
    }

    fn script_param_set() -> BTreeSet<String> {
        let re = Regex::new(r#"\("(DIFFCTX_[A-Z0-9_]+)","#).unwrap();
        let path = crate_root().join("../../scripts/sensitivity_check.py");
        let text =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {path:?}: {e}"));
        extract(&text, &re)
    }

    // Names in the Tier-3 *table rows* only. Restricting to lines starting
    // with `|` excludes the prose blockquote earlier in the same section
    // (the "other internal toggles" callout), which names
    // `DIFFCTX_OBJECTIVE`/`DIFFCTX_MAX_FRAGMENTS`/`DIFFCTX_NO_COMMIT_SIGNAL`
    // in backticks as examples of what is deliberately *not* in the table.
    fn documented_tier3_set() -> BTreeSet<String> {
        let re = Regex::new(r"`(DIFFCTX_[A-Z0-9_]+)`").unwrap();
        let path = crate_root().join("../../docs/engineering/parameter-strategy.md");
        let text =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {path:?}: {e}"));
        let start = text
            .find("### Tier 3")
            .expect("parameter-strategy.md must have a '### Tier 3' section header");
        let end = text[start..]
            .find("The Tier-1 pair")
            .map(|i| start + i)
            .unwrap_or(text.len());
        let table_rows: String = text[start..end]
            .lines()
            .filter(|line| line.trim_start().starts_with('|'))
            .collect::<Vec<_>>()
            .join("\n");
        extract(&table_rows, &re)
    }

    // Read via `read_env_*` but intentionally not in the Tier-3 table:
    // `DIFFCTX_OP_SELECTION_CORE_BUDGET_FRACTION` -- `core_budget_fraction`
    // is Tier-1 (calibrated), exposed only so `scripts/sensitivity_check.py`
    // can sweep it (documented in prose, not the table -- see
    // parameter-strategy.md's "env-overridable ... for sweeps" note).
    const TIER1_EXTRAS_READ_BUT_NOT_TABLED: &[&str] =
        &["DIFFCTX_OP_SELECTION_CORE_BUDGET_FRACTION"];

    #[test]
    fn every_script_swept_name_is_actually_read_in_code() {
        let code = code_literal_set();
        let script = script_param_set();
        assert!(
            script.len() >= 10,
            "sensitivity_check.py's OPERATIONAL_PARAMS parsed too small ({}) \
             -- the extraction regex likely drifted from the source format",
            script.len()
        );
        let phantom: Vec<&String> = script.iter().filter(|n| !code.contains(*n)).collect();
        assert!(
            phantom.is_empty(),
            "scripts/sensitivity_check.py sweeps names never read by any \
             config module (dead sweep points, measuring the default at \
             every perturbation): {phantom:?}"
        );
    }

    #[test]
    fn documented_tier3_names_match_the_code_reads_that_back_them() {
        let code = code_literal_set();
        let tier3_code: BTreeSet<String> = code
            .into_iter()
            .filter(|n| !TIER1_EXTRAS_READ_BUT_NOT_TABLED.contains(&n.as_str()))
            .collect();
        let documented = documented_tier3_set();
        assert!(
            documented.len() >= 10,
            "parameter-strategy.md's Tier-3 table parsed too small ({}) \
             -- the section markers or extraction regex likely drifted",
            documented.len()
        );
        assert_eq!(
            tier3_code, documented,
            "crate's Tier-3 env-read set and parameter-strategy.md's table \
             disagree -- a var was added/renamed/removed on one side only"
        );
    }
}
