// The two class-level properties every impact and symbol answer holds, each
// a class of false absence or false hedge black-box testing found:
//
//   A  enumeration, two-sided. The subject's name written into a file of the
//      snapshot is either a spelling the engine cannot read as a use or a
//      comment and a document:
//      - A-use: `foo(x)` in an unread file (unknown extension, none) and the
//        name as a string literal in a call of a parsed file. The answer
//        names that file or stays partial; a complete answer that nobody
//        uses the subject is a false absence.
//      - A-no-use: the name in a comment of a parsed file (the family's own
//        suffix, the definition's own file) or in documentation. The
//        answer's disclosure equals that of the run with a word nobody uses
//        in the name's place: a hedge over a comment breaks "no confident
//        and right answer becomes a hedge".
//   B  proof before not-ours: a caller that really calls the subject, stripped
//      of its imports, its type annotations or its package clause, may weaken
//      to a candidate or a gap, never to a complete answer that names nobody.
//
// This file holds what the synthetic suite (class_completeness.rs) and the
// corpus suite (class_completeness_corpus.rs) share: the verdicts, the text
// of an insertion and the three transformations of a caller. A transformation
// is a spelling change by file extension, never a parse: where it breaks the
// file, the answer owns up to a parse loss and the property holds.

use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::Path;

use regex::Regex;
use serde_json::{Value, json};

use super::families::{discloses, says_empty};

// ---- verdicts ----

// Sections that list what changed or describe the run, which name a file
// without saying anything about a use of the subject in it.
const LISTINGS: [&str; 5] = ["inventory", "range", "name", "schema", "provenance"];

// What a row of `definitions` says about the definition itself.
const OWN: [&str; 5] = ["path", "symbol", "lines", "kind", "change"];

fn names(value: &Value, path: &str) -> bool {
    match value {
        Value::String(text) => text == path || text.starts_with(&format!("{path}::")),
        Value::Array(items) => items.iter().any(|item| names(item, path)),
        Value::Object(map) => map.iter().any(|(key, item)| match key.as_str() {
            listed if LISTINGS.contains(&listed) => false,
            "definitions" => item.as_array().into_iter().flatten().any(|row| {
                row.as_object().is_some_and(|fields| {
                    fields
                        .iter()
                        .any(|(field, held)| !OWN.contains(&field.as_str()) && names(held, path))
                })
            }),
            _ => names(item, path),
        }),
        _ => false,
    }
}

// The answer lists `path` as a use, a candidate or a mention, or names it as
// a loss or an unread file: anywhere but the lists of changed files.
pub(crate) fn lists(doc: &Value, path: &str) -> bool {
    names(doc, path)
}

// A-use: the inserted file is listed, or the answer is not a complete
// statement that nobody uses the subject.
pub(crate) fn use_is_enumerated(doc: &Value, path: &str) -> bool {
    lists(doc, path) || !complete_empty(doc)
}

// What an answer discloses about its own reach: the coverage, the gaps and
// the files it names as unread, per definition and overall. Nothing in it
// lists what changed, so a changed file that only adds a comment leaves it
// alone.
pub(crate) fn disclosure(doc: &Value) -> Value {
    let definitions: Vec<Value> = doc["definitions"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|d| {
            json!({
                "symbol": d["symbol"],
                "coverage": d["coverage"],
                "gaps": d["gaps"],
                "missed_in": d["missed_in"],
            })
        })
        .collect();
    json!({
        "empty": doc["empty"],
        "coverage": doc["coverage"],
        "spelled": doc["spelled"],
        "unlisted": doc["unlisted"],
        "definitions": definitions,
    })
}

// A-no-use: the answer discloses what the run with the neutral word did.
pub(crate) fn unmoved_by_no_use(doc: &Value, baseline: &Value) -> bool {
    disclosure(doc) == disclosure(baseline)
}

// The answer is a complete statement that nobody uses the subject.
pub(crate) fn complete_empty(doc: &Value) -> bool {
    let rows = |value: &Value| value.as_array().is_some_and(|rows| !rows.is_empty());
    let definitions = doc["definitions"].as_array().cloned().unwrap_or_default();
    says_empty(doc)
        || (definitions.is_empty() && !discloses(doc))
        || definitions.iter().any(|d| {
            !rows(&d["rows"])
                && d["coverage"]["calls"]["complete"] == true
                && d["coverage"]["reads"]["complete"] == true
                && !rows(&d["missed_in"])
                && !rows(&d["gaps"])
                && !rows(&d["folded"])
                && !rows(&d["possibly_also_changed"])
                && !rows(&d["also_changed"])
        })
}

// B: the caller is neither listed nor covered by a gap, or the answer says
// the change reaches nothing.
pub(crate) fn proof_missing(doc: &Value, caller: &str) -> bool {
    says_empty(doc) || (!lists(doc, caller) && !discloses(doc))
}

// A short, readable cut of an answer for a failure table.
pub(crate) fn excerpt(doc: &Value) -> String {
    let paths = |rows: &Value| -> Vec<String> {
        rows.as_array()
            .into_iter()
            .flatten()
            .filter_map(|row| row["path"].as_str().map(str::to_string))
            .collect()
    };
    let definitions: Vec<Value> = doc["definitions"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|d| {
            json!({
                "symbol": d["symbol"],
                "callers": paths(&d["rows"]),
                "calls_complete": d["coverage"]["calls"]["complete"],
                "reads_complete": d["coverage"]["reads"]["complete"],
                "gaps": d["gaps"],
                "missed_in": d["missed_in"],
            })
        })
        .collect();
    let unlisted: serde_json::Map<String, Value> = doc["unlisted"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(key, held)| (key.clone(), held["total"].clone()))
        .collect();
    let text = json!({
        "empty": doc["empty"],
        "definitions": definitions,
        "unanalysed": doc["coverage"]["unanalysed"]["items"],
        "limits": doc["coverage"]["limits"],
        "spelled": doc["spelled"]["files"],
        "unlisted": unlisted,
    })
    .to_string();
    text.chars().take(480).collect()
}

// ---- findings ----

pub(crate) struct Finding {
    // `A` a use the answer calls complete and empty over, `H` an answer moved by a
    // spelling that is no use (a hedge), `B` a proof the answer lost, `E` a run
    // that failed before it could be judged (the corpus suite's `F` for B).
    pub class: char,
    pub family: String,
    pub case: String,
    pub variant: String,
    pub query: String,
    // The answer is a complete statement that nobody uses the subject.
    pub complete_empty: bool,
    pub excerpt: String,
}

impl Finding {
    // What the known-red list names: the test that found it and the variant.
    fn key(&self, test: &str) -> String {
        format!(
            "{test} | {} | {} | {} | {} | {}",
            self.class, self.family, self.case, self.variant, self.query
        )
    }

    fn row(&self, test: &str) -> String {
        format!(
            "{test}\t{}\t{}\t{}\t{}\t{}\tcomplete_empty={}\t{}",
            self.class,
            self.family,
            self.case,
            self.variant,
            self.query,
            self.complete_empty,
            self.excerpt
        )
    }
}

// The variants that are red today, one `key # note` a line (the report's
// first six columns, `|`-joined), held in both directions as
// known_below_threshold.txt is: a red variant that is not listed is a
// regression, and a listed variant that passes is a stale entry that would
// absorb a future regression. The release gate is an empty list.
// `DIFFCTX_CLASS_IGNORE_RED` reads the list as empty, to see every red variant.
fn known_red() -> BTreeSet<String> {
    if std::env::var_os("DIFFCTX_CLASS_IGNORE_RED").is_some() {
        return BTreeSet::new();
    }
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("class_completeness_known_red.txt");
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|line| line.split_once('#').map_or(line, |(key, _)| key).trim())
        .filter(|key| !key.is_empty())
        .map(str::to_string)
        .collect()
}

// Appends every finding to `DIFFCTX_CLASS_REPORT` when set, and fails the test
// when a finding is not on the known-red list or a listed variant passed. A
// sampled corpus run (DIFFCTX_CLASS_FILTER, DIFFCTX_CLASS_EVERY) cannot tell a
// stale entry from a case it did not run.
pub(crate) fn conclude(test: &str, checked: usize, findings: &[Finding]) {
    if let Some(path) = std::env::var_os("DIFFCTX_CLASS_REPORT") {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap_or_else(|e| panic!("open {path:?}: {e}"));
        for finding in findings {
            writeln!(file, "{}", finding.row(test)).expect("write the report");
        }
    }
    let red = known_red();
    let found: BTreeSet<String> = findings.iter().map(|f| f.key(test)).collect();
    let unexpected: Vec<String> = findings
        .iter()
        .filter(|f| !red.contains(&f.key(test)))
        .map(|f| f.row(test))
        .collect();
    let sampled = ["DIFFCTX_CLASS_FILTER", "DIFFCTX_CLASS_EVERY"]
        .iter()
        .any(|knob| std::env::var_os(knob).is_some());
    let prefix = format!("{test} | ");
    let stale: Vec<&String> = red
        .iter()
        .filter(|key| !sampled && key.starts_with(&prefix) && !found.contains(*key))
        .collect();
    if unexpected.is_empty() && stale.is_empty() {
        return;
    }
    panic!(
        "{test}: {} of {checked} checked variants are red and not listed in \
         class_completeness_known_red.txt, {} listed variants pass\n{}\n{}",
        unexpected.len(),
        stale.len(),
        unexpected.join("\n"),
        stale
            .iter()
            .map(|key| format!("stale: {key}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

// ---- the text of an insertion ----

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

pub(crate) fn extension(path: &str) -> &str {
    file_name(path).rsplit_once('.').map_or("", |(_, ext)| ext)
}

// How a comment is written in a file, as (opening, closing); none where the
// format has no comment.
pub(crate) fn comment_of(path: &str) -> Option<(&'static str, &'static str)> {
    let name = file_name(path);
    if name == "Dockerfile" || name.starts_with("Dockerfile.") || name == "Makefile" {
        return Some(("#", ""));
    }
    Some(match extension(path) {
        "py" | "sh" | "bash" | "zsh" | "yaml" | "yml" | "toml" | "tf" | "tfvars" | "rb" | "pl"
        | "r" | "ini" | "cfg" | "conf" | "properties" | "ex" | "exs" | "jl" | "nix" | "mk" => {
            ("#", "")
        }
        "rs" | "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "kt" | "kts" | "java" | "scala"
        | "groovy" | "gradle" | "go" | "php" | "c" | "h" | "cpp" | "hpp" | "cc" | "cxx" | "m"
        | "mm" | "swift" | "cs" | "dart" | "proto" => ("//", ""),
        "sql" | "lua" | "hs" => ("--", ""),
        "ml" | "mli" => ("(*", "*)"),
        "html" | "vue" | "svelte" | "astro" | "xml" | "md" => ("<!--", "-->"),
        _ => return None,
    })
}

// A line that spells the subject, in the way the file can hold it. A PHP file
// holds a comment only inside its tag.
pub(crate) fn mention(path: &str, symbol: &str) -> String {
    match comment_of(path) {
        Some((open, _)) if extension(path) == "php" => format!("<?php\n{open} {symbol}\n"),
        Some((open, close)) if extension(path) != "md" => {
            format!("{open} {symbol} {close}\n").replace("  \n", "\n")
        }
        _ => format!("see {symbol}(x) here\n"),
    }
}

// A call of the parsed file's language with the subject's name as a string
// literal argument: a spelling that may be a reflective use, which a reader
// that follows names alone cannot trace. None where the format has no call
// to write standing alone.
pub(crate) fn literal_call(path: &str, symbol: &str) -> Option<String> {
    let call = format!("register(\"{symbol}\")");
    Some(match lang(path)? {
        Lang::Python => format!("{call}\n"),
        Lang::Script => format!("{call};\n"),
        Lang::Php => format!("<?php\n{call};\n"),
        Lang::Rust => format!("fn zz_register() {{\n    {call};\n}}\n"),
        Lang::C => format!("void zz_register(void) {{\n    {call};\n}}\n"),
        Lang::Kotlin => format!("fun zzRegister() {{\n    {call}\n}}\n"),
        Lang::Swift => format!("func zzRegister() {{\n    {call}\n}}\n"),
        Lang::Java => {
            format!("class ZzInserted {{\n    void zz() {{\n        {call};\n    }}\n}}\n")
        }
        Lang::Go => format!("package zz\n\nfunc zzRegister() {{\n\t{call}\n}}\n"),
        Lang::Shell => format!("register \"{symbol}\"\n"),
        Lang::CSharp | Lang::Ruby | Lang::Terraform => return None,
    })
}

// ---- transformations of a caller ----

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lang {
    Python,
    Script,
    Rust,
    Kotlin,
    Java,
    Go,
    Php,
    C,
    Swift,
    Shell,
    CSharp,
    Ruby,
    Terraform,
}

fn lang(path: &str) -> Option<Lang> {
    if file_name(path) == "Dockerfile" {
        return None;
    }
    Some(match extension(path) {
        "py" => Lang::Python,
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "vue" | "svelte" | "astro" => Lang::Script,
        "rs" => Lang::Rust,
        "kt" | "kts" | "scala" => Lang::Kotlin,
        "java" | "groovy" => Lang::Java,
        "go" => Lang::Go,
        "php" => Lang::Php,
        "c" | "h" | "cpp" | "hpp" | "cc" | "cxx" | "m" | "mm" => Lang::C,
        "swift" => Lang::Swift,
        "sh" | "bash" | "zsh" => Lang::Shell,
        "cs" => Lang::CSharp,
        "rb" => Lang::Ruby,
        "tf" => Lang::Terraform,
        _ => return None,
    })
}

fn rx(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap_or_else(|e| panic!("{pattern}: {e}"))
}

// How an import statement that starts on a line ends.
enum Until {
    // The statement is the line.
    Line,
    // The first line that holds this character.
    Char(char),
}

fn import_start(lang: Lang, line: &str) -> Option<Until> {
    let t = line.trim_start();
    let indented = t.len() != line.len();
    match lang {
        Lang::Python => {
            let imports = t.starts_with("import ")
                || (t.starts_with("from ") && t.contains(" import "))
                || t.starts_with("from __future__");
            imports.then(|| {
                if t.contains('(') && !t.contains(')') {
                    Until::Char(')')
                } else {
                    Until::Line
                }
            })
        }
        Lang::Script => {
            if t.starts_with("import ") || t.starts_with("import{") {
                let closed = t.contains('\'') || t.contains('"');
                Some(if closed {
                    Until::Line
                } else {
                    Until::Char('\'')
                })
            } else {
                (t.contains("= require(") || t.starts_with("require(")).then_some(Until::Line)
            }
        }
        Lang::Rust => (t.starts_with("use ") || t.starts_with("extern crate ")).then(|| {
            if t.ends_with(';') {
                Until::Line
            } else {
                Until::Char(';')
            }
        }),
        Lang::Kotlin | Lang::Java => t.starts_with("import ").then_some(Until::Line),
        Lang::Go => {
            if t.starts_with("import (") {
                Some(Until::Char(')'))
            } else {
                t.starts_with("import ").then_some(Until::Line)
            }
        }
        Lang::Php => {
            let imports = (!indented && t.starts_with("use "))
                || ["require ", "require_once ", "include ", "include_once "]
                    .iter()
                    .any(|k| t.starts_with(k));
            imports.then(|| {
                if t.ends_with(';') {
                    Until::Line
                } else {
                    Until::Char(';')
                }
            })
        }
        Lang::C => (t.starts_with("#include") || t.starts_with("#import")).then_some(Until::Line),
        Lang::Swift => (t.starts_with("import ")
            || t.starts_with("@testable import ")
            || t.starts_with("@_exported import "))
        .then_some(Until::Line),
        Lang::Shell => (t.starts_with(". ") || t.starts_with("source ")).then_some(Until::Line),
        Lang::CSharp => (t.starts_with("using ")
            && t.ends_with(';')
            && !t.starts_with("using var ")
            && !t.starts_with("using ("))
        .then_some(Until::Line),
        Lang::Ruby => (t.starts_with("require ")
            || t.starts_with("require_relative ")
            || t.starts_with("load "))
        .then_some(Until::Line),
        Lang::Terraform => (t.starts_with("source")
            && t["source".len()..].trim_start().starts_with('='))
        .then_some(Until::Line),
    }
}

fn changed(old: &str, new: String) -> Option<String> {
    (new != old).then_some(new)
}

fn joined(old: &str, lines: &[String]) -> String {
    let mut text = lines.join("\n");
    if old.ends_with('\n') {
        text.push('\n');
    }
    text
}

// The file without its import statements.
pub(crate) fn strip_imports(path: &str, text: &str) -> Option<String> {
    let lang = lang(path)?;
    let mut kept: Vec<String> = Vec::new();
    let mut until: Option<char> = None;
    for line in text.lines() {
        if let Some(end) = until {
            if line.contains(end) {
                until = None;
            }
            continue;
        }
        match import_start(lang, line) {
            Some(Until::Line) => {}
            Some(Until::Char(end)) => {
                // The first line may close it already.
                let tail = line.split_once(['(', '{']).map_or("", |(_, rest)| rest);
                if !tail.contains(end) {
                    until = Some(end);
                }
            }
            None => kept.push(line.to_string()),
        }
    }
    changed(text, joined(text, &kept))
}

// The file without the types it declares: parameter and variable annotations,
// return types, and the types written before a name.
pub(crate) fn strip_annotations(path: &str, text: &str) -> Option<String> {
    let lang = lang(path)?;
    let colon =
        rx(r"(?P<n>[A-Za-z_$][\w$]*)\??\s*:\s*[A-Za-z_&*\[\(][^,;(){}=]*?(?P<e>\s*(?:[,)=;{]|$))");
    let arrow = rx(r"\)\s*->\s*[^:{;=]+");
    let after_paren = rx(r"\)\s*:\s*\??[A-Za-z_\\][\w<>\[\]|?\\ ,]*?(?:\s*(?:\{|=>|=|;|$))");
    let prefixed = rx(
        r"\b(?:final\s+|const\s+|static\s+|unsigned\s+|struct\s+)*(?:\w+\.)*(?:int|long|short|char|byte|float|double|boolean|bool|void|size_t|var|String|[A-Z]\w*(?:<[^<>]*>)?(?:::\w+)*)(?:\[\])*[\s*&]+(?P<n>[A-Za-z_]\w*)\s*(?P<e>[,)=;\[])",
    );
    let go_param = rx(r"(?P<n>\w+)\s+(?:\*|\[\]|\.\.\.)*[\w.]+(?P<e>\s*[,)])");
    let go_result = rx(r"\)\s+[\w\[\]*.]+\s*\{");
    let php_param = rx(r"\??[A-Za-z_\\][\w\\]*\s+(?P<v>\$\w+)");
    let php_result = rx(r"\)\s*:\s*\??[\w\\]+");
    let lines: Vec<String> = text
        .lines()
        .map(|line| {
            let t = line.trim_start();
            if t.starts_with("//") || t.starts_with('#') || t.starts_with('*') {
                return line.to_string();
            }
            match lang {
                Lang::Python | Lang::Script | Lang::Rust | Lang::Kotlin | Lang::Swift => {
                    let mut out = if line.contains('(') || has_declaration(line) {
                        colon.replace_all(line, "$n$e").into_owned()
                    } else {
                        line.to_string()
                    };
                    out = arrow.replace_all(&out, ")").into_owned();
                    if lang != Lang::Python {
                        out = after_paren.replace_all(&out, ") {").into_owned();
                    }
                    out
                }
                Lang::Java | Lang::CSharp | Lang::C => {
                    prefixed.replace_all(line, "$n$e").into_owned()
                }
                Lang::Go if t.starts_with("func") => {
                    let out = go_param.replace_all(line, "$n$e").into_owned();
                    go_result.replace_all(&out, ") {").into_owned()
                }
                Lang::Php if line.contains("function") => {
                    let out = php_param.replace_all(line, "$v").into_owned();
                    php_result.replace_all(&out, ")").into_owned()
                }
                _ => line.to_string(),
            }
        })
        .collect();
    changed(text, joined(text, &lines))
}

fn has_declaration(line: &str) -> bool {
    rx(r"\b(?:let|var|val|const)\b").is_match(line)
}

// The package, namespace or module clause renamed to one nothing declares.
pub(crate) fn rename_clause(path: &str, text: &str) -> Option<String> {
    let (pattern, to) = match lang(path)? {
        Lang::Kotlin | Lang::Java => (r"(?m)^(package\s+)[\w.]+", "${1}zz.renamed"),
        Lang::Go => (r"(?m)^(package\s+)\w+", "${1}zzrenamed"),
        Lang::Php => (r"(?m)^(namespace\s+)[\w\\]+", "${1}Zz\\Renamed"),
        _ => return None,
    };
    changed(text, rx(pattern).replace(text, to).into_owned())
}

// The transformations of a caller, by name.
pub(crate) const TRANSFORMS: [(&str, fn(&str, &str) -> Option<String>); 3] = [
    ("strip_imports", strip_imports),
    ("strip_annotations", strip_annotations),
    ("rename_clause", rename_clause),
];

// All three, in turn, where at least one applies.
pub(crate) fn strip_everything(path: &str, text: &str) -> Option<String> {
    let mut now = text.to_string();
    for (_, transform) in TRANSFORMS {
        if let Some(next) = transform(path, &now) {
            now = next;
        }
    }
    changed(text, now)
}
