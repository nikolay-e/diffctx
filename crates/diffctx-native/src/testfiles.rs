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

use rustc_hash::FxHashMap;

/// Directory names that mark everything beneath them as test material.
const TEST_DIRS: &[&str] = &["test", "tests", "__tests__", "spec", "specs"];

/// Segment markers, compared case-insensitively against a whole segment.
/// `selftest` is a tool's own suite (`selftest.mjs`, `lib-selftest.mjs`, #428).
const SEGMENT_MARKERS: &[&str] = &["test", "tests", "spec", "specs", "selftest", "selftests"];

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

/// Whether `path` is a test file the way the test-naming convention reads one:
/// `is_test_path`, and a Perl `.t` script.
pub(crate) fn is_test_file(path: &Path) -> bool {
    is_test_path(path)
        || path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("t"))
}

/// The stem a test file's name asks for: `test_foo`, `foo_test`, `foo.spec`,
/// `TestFoo`, `FooTests` all name `foo`.
pub(crate) fn target_name_of_test(test_name: &str) -> Option<String> {
    let lower = test_name.to_lowercase();
    if lower.starts_with("test_") {
        return Some(lower[5..].to_string());
    }
    if lower.ends_with("_test") {
        return Some(lower[..lower.len() - 5].to_string());
    }
    if lower.contains(".test") {
        return Some(lower.split(".test").next()?.to_string());
    }
    if lower.contains(".spec") {
        return Some(lower.split(".spec").next()?.to_string());
    }
    if test_name.starts_with("Test")
        && test_name.len() > 4
        && test_name.as_bytes()[4].is_ascii_uppercase()
    {
        return Some(test_name[4..].to_lowercase());
    }
    if test_name.ends_with("Tests") && test_name.len() > 5 {
        return Some(test_name[..test_name.len() - 5].to_lowercase());
    }
    // Kotest/Spek `FooSpec`, Maven Failsafe `FooIT`.
    if test_name.ends_with("Spec") && test_name.len() > 4 {
        return Some(test_name[..test_name.len() - 4].to_lowercase());
    }
    if test_name.ends_with("IT")
        && test_name.len() > 2
        && test_name.as_bytes()[test_name.len() - 3].is_ascii_lowercase()
    {
        return Some(test_name[..test_name.len() - 2].to_lowercase());
    }
    if test_name.ends_with("Test") && test_name.len() > 4 {
        return Some(test_name[..test_name.len() - 4].to_lowercase());
    }
    None
}

/// JVM test classes are named after their subject: `FooTest`, `FooTests`,
/// `FooSpec`, `FooIT` (stems lower-cased).
pub(crate) const JVM_TEST_SUFFIXES: &[&str] = &["test", "tests", "spec", "it"];
pub(crate) const JVM_TEST_SUFFIX_EXTENSIONS: &[&str] =
    &[".java", ".kt", ".kts", ".scala", ".groovy"];

/// Same ambiguity bar as the c-family and path-reference caps: a stem carried
/// by more files than this cannot name a target.
const MAX_GLOBAL_TARGET_FILES: usize = 8;

/// The (test file, source file) pairs the naming convention links, over the files
/// in view: a test's target name picks the source files of that stem, in the
/// test's own directory first, then anywhere while the stem stays rare enough to
/// identify a file. `config_test` names `config`, but envoy holds 520 files with
/// that stem. Files come in as the run lists them, and pairs go out in that order.
pub(crate) fn naming_links<'a>(paths: &[&'a str]) -> Vec<(&'a str, &'a str)> {
    let stem = |path: &str| {
        Path::new(path)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let mut tests: Vec<&str> = Vec::new();
    let mut by_stem: FxHashMap<String, Vec<&str>> = FxHashMap::default();
    for &path in paths {
        if is_test_file(Path::new(path)) {
            tests.push(path);
        } else {
            by_stem
                .entry(stem(path).to_lowercase())
                .or_default()
                .push(path);
        }
    }
    let mut links = Vec::new();
    for test in tests {
        let Some(target) = target_name_of_test(&stem(test)) else {
            continue;
        };
        let candidates = by_stem.get(&target).map_or(&[][..], Vec::as_slice);
        let dir = Path::new(test).parent();
        let beside: Vec<&str> = candidates
            .iter()
            .copied()
            .filter(|source| Path::new(source).parent() == dir)
            .collect();
        let chosen = if beside.is_empty() {
            if candidates.len() > MAX_GLOBAL_TARGET_FILES {
                continue;
            }
            candidates.to_vec()
        } else {
            beside
        };
        links.extend(chosen.into_iter().map(|source| (test, source)));
    }
    links
}

/// The words a source file spells when its language lets a test live beside the
/// code it tests, as the token index asks for them before the file is read; none
/// where the language has no such convention and the path alone decides. The
/// scopes themselves are [`in_source_test_scopes`].
pub(crate) fn in_source_markers(path: &Path) -> &'static [&'static str] {
    match path
        .extension()
        .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
        .as_deref()
    {
        Some("ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs") => &["vitest"],
        Some("py") => &["TestCase"],
        Some("cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx") => {
            &["TEST_CASE", "TEST_CASE_TEMPLATE", "SCENARIO", "TEST_SUITE"]
        }
        _ => &[],
    }
}

/// The line spans (1-based, inclusive) of `text` that are test scopes by the
/// framework's own rule although the file is no test file by name: Vitest's
/// in-source block (`if (import.meta.vitest) {`), a `unittest.TestCase`
/// subclass, a doctest or Catch2 `TEST_CASE`/`SCENARIO` block. A test context is
/// the scope that holds an occurrence, not the file's path. Rust's inline
/// `#[test]` functions are found by the Rust reader.
pub(crate) fn in_source_test_scopes(path: &Path, text: &str) -> Vec<(u32, u32)> {
    let lines: Vec<&str> = text.lines().collect();
    match path
        .extension()
        .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
        .as_deref()
    {
        Some("ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs") => {
            braced_scopes(&lines, |line| {
                line.trim_start().starts_with("if") && line.contains("import.meta.vitest")
            })
        }
        Some("py") => indented_scopes(&lines, |line| {
            line.trim_start().starts_with("class ") && line.contains("TestCase")
        }),
        Some("cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx") => braced_scopes(&lines, |line| {
            let line = line.trim_start();
            [
                "TEST_CASE(",
                "TEST_CASE_TEMPLATE(",
                "SCENARIO(",
                "TEST_SUITE(",
            ]
            .iter()
            .any(|opener| line.starts_with(*opener))
        }),
        _ => Vec::new(),
    }
}

/// The spans that start at a line `opens` accepts and end where the brace the
/// block opens is closed again. Braces in strings and comments count, so a block
/// that never balances is no scope: a test is never claimed by guess.
fn braced_scopes(lines: &[&str], opens: impl Fn(&str) -> bool) -> Vec<(u32, u32)> {
    let mut scopes = Vec::new();
    let mut at = 0;
    while at < lines.len() {
        if !opens(lines[at]) {
            at += 1;
            continue;
        }
        let mut depth = 0i32;
        let mut opened = false;
        let mut end = None;
        for (offset, line) in lines[at..].iter().enumerate() {
            for byte in line.bytes() {
                match byte {
                    b'{' => {
                        depth += 1;
                        opened = true;
                    }
                    b'}' => depth -= 1,
                    _ => {}
                }
            }
            if opened && depth <= 0 {
                end = Some(at + offset);
                break;
            }
        }
        match end {
            Some(last) => {
                scopes.push((at as u32 + 1, last as u32 + 1));
                at = last + 1;
            }
            None => at += 1,
        }
    }
    scopes
}

/// The spans that start at a line `opens` accepts and last through every later
/// line indented deeper than it (blank lines inside belong to it).
fn indented_scopes(lines: &[&str], opens: impl Fn(&str) -> bool) -> Vec<(u32, u32)> {
    let indent = |line: &str| line.len() - line.trim_start().len();
    let mut scopes = Vec::new();
    let mut at = 0;
    while at < lines.len() {
        if !opens(lines[at]) {
            at += 1;
            continue;
        }
        let own = indent(lines[at]);
        let mut last = at;
        for (offset, line) in lines[at + 1..].iter().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            if indent(*line) <= own {
                break;
            }
            last = at + 1 + offset;
        }
        scopes.push((at as u32 + 1, last as u32 + 1));
        at = last + 1;
    }
    scopes
}

/// The names the `>>>` examples of `text` call, with the 0-based line of each
/// example: a doctest runs what it calls, in the file it documents. A name after
/// a dot is a method of something else and is not taken.
pub(crate) fn doctest_calls(text: &str) -> Vec<(usize, Vec<&str>)> {
    let mut found = Vec::new();
    for (row, line) in text.lines().enumerate() {
        let Some(example) = line.trim_start().strip_prefix(">>>") else {
            continue;
        };
        let bytes = example.as_bytes();
        let mut names = Vec::new();
        for (open, byte) in bytes.iter().enumerate() {
            if *byte != b'(' {
                continue;
            }
            let start = bytes[..open]
                .iter()
                .rposition(|b| !(b.is_ascii_alphanumeric() || *b == b'_'))
                .map_or(0, |at| at + 1);
            let name = &example[start..open];
            let after_a_dot = start > 0 && bytes[start - 1] == b'.';
            if !name.is_empty() && !name.starts_with(|c: char| c.is_ascii_digit()) && !after_a_dot {
                names.push(name);
            }
        }
        if !names.is_empty() {
            found.push((row, names));
        }
    }
    found
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
