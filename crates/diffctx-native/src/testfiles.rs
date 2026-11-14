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
pub(crate) fn is_test_path(path: &Path) -> bool {
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
    if let Some(rest) = lower.strip_prefix("test_") {
        return Some(rest.to_string());
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

/// The attributes a Rust function carries to be a test, by the last segment of
/// the path (`tokio::test`, `sqlx::test` are `test`): the built-in one and the
/// test-framework crates' (`rstest`, `test-case`, `test-strategy`, `quickcheck`,
/// `wasm-bindgen-test`).
pub(crate) const RUST_TEST_ATTRIBUTES: &[&str] = &[
    "test",
    "rstest",
    "test_case",
    "proptest",
    "quickcheck",
    "wasm_bindgen_test",
];

/// The macros whose block makes tests of the functions written inside it:
/// `proptest! { #[test] fn .. }`, `quickcheck! { fn .. }`.
pub(crate) const RUST_TEST_MACROS: &[&str] = &["proptest", "quickcheck"];

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
            if indent(line) <= own {
                break;
            }
            last = at + 1 + offset;
        }
        scopes.push((at as u32 + 1, last as u32 + 1));
        at = last + 1;
    }
    scopes
}

/// The names one line of example code calls: a name after a dot is a method of
/// something else, and is not taken.
fn called_names(example: &str) -> Vec<&str> {
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
    names
}

/// The names the `>>>` examples of `text` call, with the 0-based line of each
/// example: a doctest runs what it calls, in the file it documents.
pub(crate) fn doctest_calls(text: &str) -> Vec<(usize, Vec<&str>)> {
    text.lines()
        .enumerate()
        .filter_map(|(row, line)| {
            let names = called_names(line.trim_start().strip_prefix(">>>")?);
            (!names.is_empty()).then_some((row, names))
        })
        .collect()
}

/// A fence's info string says the example runs as a test (`cargo test --doc`):
/// plain, `rust`, `should_panic`, an edition. `ignore`, `no_run`, `compile_fail`
/// and other languages do not run.
fn fence_runs(info: &str) -> bool {
    info.split(',')
        .map(str::trim)
        .all(|word| matches!(word, "" | "rust" | "should_panic") || word.starts_with("edition"))
}

/// The names the fenced examples of a Rust file's `///` and `//!` comments call
/// that `cargo test --doc` runs, with the 0-based line of each.
pub(crate) fn rust_doc_example_calls(text: &str) -> Vec<(usize, Vec<&str>)> {
    let mut found = Vec::new();
    let mut fence: Option<bool> = None;
    for (row, line) in text.lines().enumerate() {
        let line = line.trim_start();
        let Some(doc) = line
            .strip_prefix("///")
            .or_else(|| line.strip_prefix("//!"))
        else {
            fence = None;
            continue;
        };
        let doc = doc.trim();
        if let Some(info) = doc.strip_prefix("```") {
            fence = fence.map_or_else(|| Some(fence_runs(info)), |_| None);
        } else if fence == Some(true) {
            let names = called_names(doc);
            if !names.is_empty() {
                found.push((row, names));
            }
        }
    }
    found
}

/// Where a runner's configuration file holds the keys that say which files are
/// tests.
enum Holder {
    /// An INI section (`[pytest]`, `[tool:pytest]`).
    Ini(&'static str),
    /// A TOML table (`[tool.pytest.ini_options]`).
    Toml(&'static [&'static str]),
    /// A JSON object, the document when the path is empty (`package.json`'s `jest`).
    Json(&'static [&'static str]),
    /// A script that exports an object: the key and its literals are read off the text.
    Script,
}

/// How the values of a key name files.
#[derive(Clone, Copy)]
enum Declares {
    /// Glob patterns (`testMatch`, `python_files`).
    Globs,
    /// Regular expressions over the path (`testRegex`).
    Regex,
}

/// A test runner whose configuration decides which files are tests: the
/// configuration files, the keys, and the files the patterns judge. A runner is a
/// row; a pattern with no `/` judges the file's name where `names_only`.
struct Runner {
    config_files: &'static [(&'static str, Holder)],
    keys: &'static [(&'static str, Declares)],
    extensions: &'static [&'static str],
    names_only: bool,
}

const SCRIPT_EXTENSIONS: &[&str] = &["js", "jsx", "mjs", "cjs", "ts", "tsx", "mts", "cts"];

static RUNNERS: &[Runner] = &[
    Runner {
        config_files: &[
            ("pytest.ini", Holder::Ini("pytest")),
            ("tox.ini", Holder::Ini("pytest")),
            ("setup.cfg", Holder::Ini("tool:pytest")),
            (
                "pyproject.toml",
                Holder::Toml(&["tool", "pytest", "ini_options"]),
            ),
        ],
        keys: &[("python_files", Declares::Globs)],
        extensions: &["py"],
        names_only: true,
    },
    Runner {
        config_files: &[
            ("package.json", Holder::Json(&["jest"])),
            ("jest.config.json", Holder::Json(&[])),
            ("jest.config.js", Holder::Script),
            ("jest.config.cjs", Holder::Script),
            ("jest.config.mjs", Holder::Script),
            ("jest.config.ts", Holder::Script),
        ],
        keys: &[
            ("testMatch", Declares::Globs),
            ("testRegex", Declares::Regex),
        ],
        extensions: SCRIPT_EXTENSIONS,
        names_only: false,
    },
];

/// One runner's patterns over the files beneath the directory of its configuration.
struct Declared {
    dir: String,
    extensions: &'static [&'static str],
    names_only: bool,
    patterns: Vec<regex::Regex>,
}

/// The test files the repository's runner configurations declare, beyond the
/// naming convention: a file `checks/check_a.py` is a test where `python_files`
/// says `check_*.py`. A configuration that cannot be read declares nothing.
#[derive(Default)]
pub(crate) struct DeclaredTests(Vec<Declared>);

impl DeclaredTests {
    /// Reads the configuration files among `paths` (repository-relative) through `read`.
    pub(crate) fn read(paths: &[&str], read: impl Fn(&str) -> Option<String>) -> Self {
        let mut declared = Vec::new();
        for path in paths {
            let (dir, name) = path.rsplit_once('/').unwrap_or(("", path));
            for runner in RUNNERS {
                let Some((_, holder)) = runner.config_files.iter().find(|(n, _)| *n == name) else {
                    continue;
                };
                let Some(text) = read(path) else { continue };
                let patterns: Vec<regex::Regex> = runner
                    .keys
                    .iter()
                    .flat_map(|&(key, how)| {
                        values_of(holder, &text, key)
                            .into_iter()
                            .filter_map(move |value| pattern_of(&value, how))
                    })
                    .collect();
                if !patterns.is_empty() {
                    declared.push(Declared {
                        dir: dir.to_string(),
                        extensions: runner.extensions,
                        names_only: runner.names_only,
                        patterns,
                    });
                }
            }
        }
        Self(declared)
    }

    pub(crate) fn is_test(&self, path: &str) -> bool {
        let Some(extension) = Path::new(path).extension().and_then(|e| e.to_str()) else {
            return false;
        };
        self.0.iter().any(|declared| {
            let Some(beneath) = (declared.dir.is_empty())
                .then_some(path)
                .or_else(|| path.strip_prefix(declared.dir.as_str())?.strip_prefix('/'))
            else {
                return false;
            };
            declared.extensions.contains(&extension)
                && declared.patterns.iter().any(|pattern| {
                    let name = beneath.rsplit('/').next().unwrap_or(beneath);
                    pattern.is_match(if declared.names_only { name } else { beneath })
                        || pattern.is_match(&format!("/{beneath}"))
                })
        })
    }
}

/// The values a runner's configuration gives `key`: a list of strings or one string
/// (split at whitespace where a glob list is written as words).
fn values_of(holder: &Holder, text: &str, key: &str) -> Vec<String> {
    match holder {
        Holder::Ini(section) => ini_values(text, section, key),
        Holder::Toml(at) => {
            let table = toml::from_str::<toml::Table>(text).ok();
            let value = table
                .as_ref()
                .and_then(|table| crate::snapshot::workspace::toml_at(table, at))
                .and_then(|holder| holder.get(key));
            crate::snapshot::workspace::toml_strings(value)
                .iter()
                .flat_map(|words| words.split_whitespace().map(str::to_string))
                .collect()
        }
        Holder::Json(at) => {
            let mut value = serde_json::from_str::<serde_json::Value>(text).ok();
            for segment in *at {
                value = value.and_then(|v| v.get(segment).cloned());
            }
            match value.and_then(|v| v.get(key).cloned()) {
                Some(serde_json::Value::String(one)) => vec![one],
                Some(serde_json::Value::Array(items)) => items
                    .into_iter()
                    .filter_map(|item| item.as_str().map(str::to_string))
                    .collect(),
                _ => Vec::new(),
            }
        }
        Holder::Script => script_values(text, key),
    }
}

/// `key = a b` of an INI section, with the indented lines that continue it.
fn ini_values(text: &str, section: &str, key: &str) -> Vec<String> {
    let mut in_section = false;
    let mut found = false;
    let mut words = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if found {
            if line.starts_with([' ', '\t']) && !trimmed.is_empty() {
                words.extend(trimmed.split_whitespace().map(str::to_string));
                continue;
            }
            break;
        }
        if let Some(name) = trimmed.strip_prefix('[').and_then(|t| t.strip_suffix(']')) {
            in_section = name.trim() == section;
        } else if in_section {
            if let Some((name, value)) = trimmed.split_once(['=', ':']) {
                if name.trim() == key {
                    found = true;
                    words.extend(value.split_whitespace().map(str::to_string));
                }
            }
        }
    }
    words
}

/// The string literals that follow `key:` in a script, up to the end of its list
/// (`testMatch: ["a", 'b']`) or the one literal (`testRegex: "x"`).
fn script_values(text: &str, key: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find(key) {
        let after = rest[at + key.len()..]
            .trim_start_matches(['"', '\''])
            .trim_start();
        rest = &rest[at + key.len()..];
        let Some(value) = after.strip_prefix(':') else {
            continue;
        };
        let value = value.trim_start();
        let (body, many) = match value.strip_prefix('[') {
            Some(list) => (list, true),
            None => (value, false),
        };
        let mut chars = body.chars();
        while let Some(quote) = chars.next() {
            if !matches!(quote, '"' | '\'' | '`') {
                // Commas and blanks between a list's literals; anything else
                // (a variable, the closing bracket) ends what is read.
                if many && (quote == ',' || quote.is_whitespace()) {
                    continue;
                }
                break;
            }
            let mut literal = String::new();
            let mut escaped = false;
            for c in chars.by_ref() {
                if escaped {
                    if !matches!(c, '\\' | '"' | '\'' | '`') {
                        literal.push('\\');
                    }
                    literal.push(c);
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == quote {
                    break;
                } else {
                    literal.push(c);
                }
            }
            values.push(literal);
            if !many {
                break;
            }
        }
    }
    values
}

fn pattern_of(value: &str, how: Declares) -> Option<regex::Regex> {
    match how {
        Declares::Regex => regex::Regex::new(value).ok(),
        Declares::Globs => glob_regex(
            value
                .trim_start_matches("<rootDir>/")
                .trim_start_matches("./"),
        ),
    }
}

/// A glob as an anchored regular expression: `*` and `?` stay within a segment,
/// `**/` spans segments, `[..]` classes, `{a,b}` alternatives and the extglob
/// groups `?(..)`, `*(..)`, `+(..)`, `@(..)`. A negated group `!(..)` is not
/// read: the pattern then declares nothing.
fn glob_regex(glob: &str) -> Option<regex::Regex> {
    let chars: Vec<char> = glob.chars().collect();
    let mut out = String::from("^");
    let mut closers: Vec<&str> = Vec::new();
    let mut at = 0;
    while at < chars.len() {
        let c = chars[at];
        let next = chars.get(at + 1).copied();
        match c {
            '*' if next == Some('*') => {
                at += 1;
                if chars.get(at + 1) == Some(&'/') {
                    at += 1;
                    out.push_str("(?:.*/)?");
                } else {
                    out.push_str(".*");
                }
            }
            '*' | '?' | '+' | '@' | '!' if next == Some('(') => {
                if c == '!' {
                    return None;
                }
                at += 1;
                out.push_str("(?:");
                closers.push(match c {
                    '?' => ")?",
                    '*' => ")*",
                    '+' => ")+",
                    _ => ")",
                });
            }
            '*' => out.push_str("[^/]*"),
            '?' => out.push_str("[^/]"),
            '{' => {
                out.push_str("(?:");
                closers.push(")");
            }
            '(' => {
                out.push_str("(?:");
                closers.push(")");
            }
            '}' | ')' => out.push_str(closers.pop()?),
            '|' | ',' if !closers.is_empty() => out.push('|'),
            '[' => {
                let close = chars[at..].iter().position(|&c| c == ']')? + at;
                let class: String = chars[at + 1..close].iter().collect();
                out.push('[');
                out.push_str(&class.replacen('!', "^", usize::from(class.starts_with('!'))));
                out.push(']');
                at = close;
            }
            other => out.push_str(&regex::escape(&other.to_string())),
        }
        at += 1;
    }
    if !closers.is_empty() {
        return None;
    }
    out.push('$');
    regex::Regex::new(&out).ok()
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
