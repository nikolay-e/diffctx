//! One answer to "is this a test file".
//!
//! There used to be two: a per-language dispatch in
//! `edges::structural::testing` gating `TestEdge` emission, and a flat suffix
//! list in `utility::needs` gating test-need match strength. They disagreed —
//! a `.kts` file was a test to one and not the other — so the two halves of
//! "this is a test for the changed code" could each hold independently (#182).
//!
//! Both also accepted any stem ending in `test`, because the JVM and Scala
//! conventions carry no separator (`FooTest.java`) and both lowercased the name
//! before comparing, which destroys the CamelCase boundary that makes the
//! convention readable. That matched `latest`, `greatest`, `contest` and
//! `attest`. The rule here is that a `test`/`spec` marker counts only at a
//! **word boundary**: its own segment between separators, or a capitalised
//! `Test`/`Spec` in the original name.
//!
//! Two families are accepted false positives, because no name-based rule
//! separates them: `PodSpec`/`JobSpec` (Kubernetes API types) look exactly like
//! `AuthSpec` (a Scala test), and `ABTest` (an experiment) looks exactly like
//! `FooTest`. Both over-classify — a K8s model contributes test-need strength
//! it should not — and both are preferred to under-classifying the conventions
//! they collide with, which are far more common in the corpora this feeds.

use std::path::Path;

/// Directory names that mark everything beneath them as test material.
const TEST_DIRS: &[&str] = &["test", "tests", "__tests__", "spec", "specs"];

/// Segment markers, compared case-insensitively against a whole segment.
const SEGMENT_MARKERS: &[&str] = &["test", "tests", "spec", "specs"];

/// CamelCase markers, compared against the original case — the capital letter
/// *is* the word boundary.
const CAMEL_MARKERS: &[&str] = &["Test", "Tests", "Spec", "Specs"];

fn has_test_directory(path: &Path) -> bool {
    path.components().any(|c| {
        let segment = c.as_os_str().to_string_lossy().to_lowercase();
        TEST_DIRS.contains(&segment.as_str())
    })
}

/// True when `stem` carries a test marker as its own `_`/`-`/`.`-delimited
/// segment: `test_auth`, `auth_test`, `widget.spec`, `widget-test`, `tests`.
fn has_marker_segment(stem: &str) -> bool {
    stem.split(['_', '-', '.'])
        .any(|segment| SEGMENT_MARKERS.contains(&segment.to_lowercase().as_str()))
}

/// True when `stem` carries a capitalised marker at a CamelCase boundary:
/// `FooTest`, `TestFoo`, `AuthSpec`. Deliberately case-sensitive — lowercasing
/// first is what made `latest` look like a test.
fn has_camel_marker(stem: &str) -> bool {
    for marker in CAMEL_MARKERS {
        // A capital `T` *is* the word boundary: in CamelCase an uppercase letter
        // always starts a new word, and these markers are compared case
        // sensitively, so a match cannot be the tail of a longer word.
        // `latest`/`contest`/`attest` carry a lowercase `t` and never reach here.
        //
        // This used to also require the preceding character to be lowercase,
        // which rejected every acronym-prefixed name the JVM conventions are
        // full of — `XMLTest`, `HTTPTest`, `DBTest`, `UITest`, `IOTest` were all
        // classified as ordinary source. The guard prevented no false positive:
        // the only stems it excluded were exactly those acronyms.
        if stem.strip_suffix(marker).is_some() {
            return true;
        }
        if let Some(rest) = stem.strip_prefix(marker) {
            // `TestFoo` — the next character starts a new word. A bare `Test`
            // stem is already covered by the segment rule.
            if rest.starts_with(char::is_uppercase) {
                return true;
            }
        }
    }
    false
}

/// Whether `path` is test material.
pub fn is_test_path(path: &Path) -> bool {
    if has_test_directory(path) {
        return true;
    }
    // `file_stem` strips one extension, so `widget.test.ts` yields
    // `widget.test` and the marker survives as a segment.
    let Some(stem) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
        return false;
    };
    has_marker_segment(&stem) || has_camel_marker(&stem)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn is_test(p: &str) -> bool {
        is_test_path(Path::new(p))
    }

    #[test]
    fn every_supported_naming_convention_is_recognised() {
        for path in [
            "tests/test_auth.py",
            "src/auth_test.py",
            "web/handler_test.go",
            "ui/widget.test.ts",
            "ui/widget.spec.tsx",
            "ui/widget-spec.js",
            "src/FooTest.java",
            "src/FooTest.kt",
            "src/AuthSpec.scala",
            "src/TestHelpers.kt",
            // Acronym + marker. Every one of these was misclassified as
            // ordinary source while the camel rule demanded a lowercase
            // character before the marker.
            "src/XMLTest.java",
            "src/HTTPTest.go",
            "src/DBTest.kt",
            "src/UITest.swift",
            "src/IOTest.scala",
            "src/MyXMLTest.java",
            "src/JSONSpec.scala",
            "crates/x/tests/integration.rs",
            "src/tests.rs",
            "app/__tests__/widget.jsx",
            "spec/models/user_spec.rb",
        ] {
            assert!(is_test(path), "not recognised as a test: {path}");
        }
    }

    /// The false positive both old implementations shared: a stem that merely
    /// *ends* in the letters `test`, with no word boundary.
    #[test]
    fn an_ordinary_word_ending_in_test_is_not_a_test_file() {
        for path in [
            "src/latest.rs",
            "src/latest.java",
            "src/latest.kt",
            "src/greatest.scala",
            "src/contest.py",
            "src/attest.go",
            "src/testing.rs",
            "src/tester.py",
            "src/manifest.json",
        ] {
            assert!(!is_test(path), "wrongly classified as a test: {path}");
        }
    }

    /// `conftest.py` is pytest infrastructure rather than a test module, and
    /// both previous implementations classified it as non-test. Corpus cases
    /// depend on that, so it must not change.
    #[test]
    fn conftest_is_not_itself_a_test_file() {
        assert!(!is_test("tests_helpers/conftest.py"));
        assert!(!is_test("src/conftest.py"));
    }

    /// A `tests/` directory anywhere in the path wins regardless of filename —
    /// that is how every ecosystem separates its test tree.
    #[test]
    fn a_test_directory_marks_everything_beneath_it() {
        assert!(is_test("tests/fixtures/data_loader.py"));
        assert!(is_test("crates/x/tests/common/mod.rs"));
        assert!(!is_test("src/testdata/loader.py"));
    }
}
