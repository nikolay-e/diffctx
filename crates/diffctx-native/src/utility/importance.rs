//! File-importance prior I(f) for impact-need scoring.
//!
//! See `crate::config::importance` for the rationale behind the chosen
//! constants. This module computes I(f) ∈ (0, 1] from the fragment path
//! using the path/stem patterns defined in config.

use std::path::Path;
use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::importance::{
    DEFAULT_IMPORTANCE, GENERATED_CAP, GENERATED_DIRS, PERIPHERAL_CAP, PERIPHERAL_DIRS,
    PERIPHERAL_STEMS, PERIPHERAL_SUFFIXES,
};
use crate::types::Fragment;

fn path_components_lower(path: &Path) -> FxHashSet<String> {
    path.components()
        .filter_map(|c| c.as_os_str().to_str())
        .map(|s| s.to_lowercase())
        .collect()
}

fn is_peripheral(path: &Path) -> bool {
    let parts = path_components_lower(path);
    if PERIPHERAL_DIRS.iter().any(|d| parts.contains(*d)) {
        return true;
    }
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();
    if PERIPHERAL_STEMS.iter().any(|p| stem.starts_with(p)) {
        return true;
    }
    if PERIPHERAL_SUFFIXES.iter().any(|s| stem.ends_with(s)) {
        return true;
    }
    false
}

fn is_generated(path: &Path) -> bool {
    let parts = path_components_lower(path);
    GENERATED_DIRS.iter().any(|d| parts.contains(*d))
}

/// Compute I(f) for every fragment path in the universe.
///
/// Returns a map from path string to importance ∈ {GENERATED_CAP,
/// PERIPHERAL_CAP, DEFAULT_IMPORTANCE}. Generated takes precedence over
/// peripheral when both apply (a more conservative downweight).
pub fn compute_file_importance(fragments: &[Fragment]) -> FxHashMap<Arc<str>, f64> {
    let mut seen: FxHashSet<Arc<str>> = FxHashSet::default();
    let mut out: FxHashMap<Arc<str>, f64> = FxHashMap::default();
    for f in fragments {
        let path_str = f.path();
        let key: Arc<str> = Arc::from(path_str);
        if !seen.insert(key.clone()) {
            continue;
        }
        let path = Path::new(path_str);
        let imp = if is_generated(path) {
            GENERATED_CAP
        } else if is_peripheral(path) {
            PERIPHERAL_CAP
        } else {
            DEFAULT_IMPORTANCE
        };
        out.insert(key, imp);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classify(path_str: &str) -> f64 {
        let path = Path::new(path_str);
        if is_generated(path) {
            GENERATED_CAP
        } else if is_peripheral(path) {
            PERIPHERAL_CAP
        } else {
            DEFAULT_IMPORTANCE
        }
    }

    #[test]
    fn default_for_production_paths() {
        assert_eq!(classify("src/handler.ts"), DEFAULT_IMPORTANCE);
        assert_eq!(classify("lib/auth/login.rs"), DEFAULT_IMPORTANCE);
        assert_eq!(classify("server/main.go"), DEFAULT_IMPORTANCE);
    }

    #[test]
    fn peripheral_for_examples_and_demo() {
        assert_eq!(classify("examples/parsing.ts"), PERIPHERAL_CAP);
        assert_eq!(classify("demo/usage.py"), PERIPHERAL_CAP);
        assert_eq!(classify("vendor/lib/foo.go"), PERIPHERAL_CAP);
        assert_eq!(classify("fixtures/sample.json"), PERIPHERAL_CAP);
        assert_eq!(classify("docs/guide.md"), PERIPHERAL_CAP);
    }

    #[test]
    fn peripheral_for_stem_patterns() {
        assert_eq!(classify("tests/example_usage.py"), PERIPHERAL_CAP);
        assert_eq!(classify("src/module_demo.rs"), PERIPHERAL_CAP);
        assert_eq!(classify("scripts/sample_run.sh"), PERIPHERAL_CAP);
    }

    #[test]
    fn generated_takes_precedence() {
        assert_eq!(classify("generated/protobuf.rs"), GENERATED_CAP);
        assert_eq!(classify("src/__generated__/api.ts"), GENERATED_CAP);
        // generated/ in any component triggers, even nested under examples
        assert_eq!(classify("examples/__generated__/foo.py"), GENERATED_CAP);
    }

    #[test]
    fn case_insensitive_directory_matching() {
        assert_eq!(classify("Examples/foo.py"), PERIPHERAL_CAP);
        assert_eq!(classify("VENDOR/lib.go"), PERIPHERAL_CAP);
        assert_eq!(classify("Generated/api.ts"), GENERATED_CAP);
    }

    #[test]
    fn ordering_is_stable_under_priors() {
        // Production caller is at least 6.6x more important than peripheral.
        assert!(DEFAULT_IMPORTANCE / PERIPHERAL_CAP >= 6.0);
        // Peripheral is at least 1.4x more important than generated.
        assert!(PERIPHERAL_CAP / GENERATED_CAP >= 1.4);
        // Both caps are well below 1 (the cap must bite).
        assert!(GENERATED_CAP < 0.25);
        assert!(PERIPHERAL_CAP < 0.25);
        // Both caps are above 0 (schema-impact signal retained).
        assert!(GENERATED_CAP > 0.0);
        assert!(PERIPHERAL_CAP > 0.0);
    }
}
