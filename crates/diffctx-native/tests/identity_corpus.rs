// The accounting identity (relay arch, R38 B) over every case of the impact and
// symbol corpus: in every file that may name a changed definition, each text match
// of each of its names is exactly one record — a use, a declaration, a not-ours with
// its witness, a gap. The binary checks it itself in a debug build under
// `DIFFCTX_DEBUG_IDENTITY` (src/tokens/occurrence/identity.rs); this suite sets the
// knob per case, runs the case as the corpus does and fails on any violation line.
// The case's expectations are not read: the identity is a property of the account.
//
// The per-language tests are the measurement and are ignored until the identity
// holds on the whole corpus; the planted witness runs in every debug build.
//
// Knobs:
//   DIFFCTX_IDENTITY_FILTER  only the cases whose name contains the text
//   DIFFCTX_IDENTITY_REPORT  append every case's lines, tagged with the case, here
#![allow(dead_code)]

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::Value;
use tempfile::TempDir;
use walkdir::WalkDir;

#[path = "support/accounting.rs"]
mod accounting;
#[path = "support/fact_keys.rs"]
mod fact_keys;
#[path = "common/oracle/mod.rs"]
mod oracle;
mod support;

use oracle::case::{Case, Mode, parse};
use support::families::{Family, Files, Planted, Query, Repo};

const KNOB: &str = "DIFFCTX_DEBUG_IDENTITY";

fn cases_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("repository root")
        .join("tests")
        .join("cases")
}

fn knob(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn load(language: &str) -> Vec<(String, Case)> {
    let filter = knob("DIFFCTX_IDENTITY_FILTER");
    let mut loaded = Vec::new();
    for mode in [Mode::Impact, Mode::Symbol] {
        let dir = cases_root().join(mode.name()).join(language);
        let mut paths: Vec<PathBuf> = WalkDir::new(&dir)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_file())
            .map(walkdir::DirEntry::into_path)
            .filter(|p| p.extension().is_some_and(|x| x == "yaml" || x == "yml"))
            .collect();
        paths.sort();
        for path in paths {
            let rel = path.strip_prefix(&dir).unwrap_or(&path).with_extension("");
            let name = format!("{}/{language}/{}", mode.name(), rel.display());
            if filter.as_ref().is_some_and(|f| !name.contains(f.as_str())) {
                continue;
            }
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let raw = std::fs::read_to_string(&path).expect("a readable case");
            // A case that does not parse is the corpus gate's business.
            if let Ok(case) = parse(&raw, stem, mode) {
                loaded.push((name, case));
            }
        }
    }
    loaded
}

/// The distinct lines the knob wrote: the case asks the binary more than once.
fn lines_of(sink: &Path) -> Vec<Value> {
    let text = std::fs::read_to_string(sink).unwrap_or_default();
    let distinct: BTreeSet<&str> = text.lines().collect();
    distinct
        .into_iter()
        .map(|line| serde_json::from_str(line).expect("a JSON line"))
        .collect()
}

fn report(case: &str, lines: &[Value]) {
    let Some(path) = knob("DIFFCTX_IDENTITY_REPORT") else {
        return;
    };
    let mut out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("the report file");
    for line in lines {
        let mut tagged = line.clone();
        tagged["case"] = Value::from(case);
        writeln!(out, "{tagged}").expect("the report is written");
    }
}

fn is(line: &Value, kind: &str) -> bool {
    line["identity"] == kind
}

fn identity(language: &str) {
    let (mut checked, mut unchecked, mut failed) = (0, Vec::new(), Vec::new());
    for (name, mut case) in load(language) {
        let dir = TempDir::new().expect("tempdir");
        let sink = dir.path().join("identity.jsonl");
        let sink_text = sink.to_str().expect("a UTF-8 path").to_string();
        case.query.env.insert(KNOB.to_string(), sink_text);
        // A case's own assertion failing is no concern here: the lines are written
        // before the answer is judged.
        let _ = oracle::engine::run(&case, &name);
        let lines = lines_of(&sink);
        report(&name, &lines);
        if !lines.iter().any(|line| is(line, "account")) {
            unchecked.push(name);
            continue;
        }
        checked += 1;
        for line in lines.iter().filter(|line| is(line, "violation")) {
            failed.push(format!(
                "{name}\t{}\t{}\t{}\t{}\t{}",
                line["kind"].as_str().unwrap_or(""),
                line["def"].as_str().unwrap_or(""),
                line["name"].as_str().unwrap_or(""),
                line["file"].as_str().unwrap_or(""),
                line["detail"]
            ));
        }
    }
    eprintln!(
        "identity/{language}: {checked} case(s) checked, {} without an account, {} violation(s)",
        unchecked.len(),
        failed.len()
    );
    assert!(
        failed.is_empty(),
        "the accounting identity does not hold ({language}):\n\
         case\tkind\tdef\tname\tfile\tdetail\n{}",
        failed.join("\n")
    );
}

macro_rules! per_language {
    ($($name:ident),* $(,)?) => {
        mod corpus {
            use super::*;
            $(
                #[test]
                #[ignore = "the identity measurement; run with --ignored in a debug build"]
                fn $name() { identity(stringify!($name)); }
            )*
        }
    };
}

per_language! {
    python, typescript, rust, kotlin, java, php, c, cpp, swift, go, shell, config, terraform,
    dockerfile, web, sql, other,
}

#[test]
fn every_corpus_language_is_measured() {
    let measured = [
        "python",
        "typescript",
        "rust",
        "kotlin",
        "java",
        "php",
        "c",
        "cpp",
        "swift",
        "go",
        "shell",
        "config",
        "terraform",
        "dockerfile",
        "web",
        "sql",
        "other",
    ];
    for language in oracle::LANGUAGES {
        assert!(measured.contains(language), "{language} is not measured");
    }
}

// The comment on line 5 and the string on line 6 spell the subject's name and are
// no use; the identity holds only when each is a record (a not-ours of its region).
const CART: &str = "from shop.pricing import total


def checkout(items):
    # total is computed in pricing
    note = \"the total\"
    return total(items)
";

fn files(entries: &[(&str, &str)]) -> Files {
    entries
        .iter()
        .map(|(path, text)| ((*path).to_string(), (*text).to_string()))
        .collect()
}

#[test]
#[cfg_attr(
    not(debug_assertions),
    ignore = "the identity is checked by a debug binary only"
)]
fn a_comment_and_a_string_that_name_the_subject_are_records() {
    let pyproject = (
        "pyproject.toml",
        "[project]\nname = \"shop\"\nversion = \"0.1.0\"\n",
    );
    let planted = Planted::new(Repo {
        family: Family::Python,
        base: files(&[
            pyproject,
            (
                "shop/pricing.py",
                "def total(items):\n    return sum(items)\n",
            ),
            ("shop/cart.py", CART),
        ]),
        change: files(&[(
            "shop/pricing.py",
            "def total(items):\n    return sum(items) * 2\n",
        )]),
        caller: None,
        definition: "shop/pricing.py".to_string(),
        symbol: "total".to_string(),
    });
    let out = TempDir::new().expect("tempdir");
    let sink = out.path().join("identity.jsonl");
    let knob = [(KNOB, sink.to_str().expect("a path"))];
    planted.ask(Query::Impact, &knob).expect("the answer");
    let lines = lines_of(&sink);
    let account = lines
        .iter()
        .find(|line| {
            is(line, "account") && line["def"] == "shop/pricing.py::total" && line["side"] == "head"
        })
        .unwrap_or_else(|| panic!("no account of total at head: {lines:?}"));
    // def total, the import, the comment, the string, the call.
    assert_eq!(account["matches"], 5, "{account}");
    assert_eq!(account["remainder"], 0, "{account}");
    let violations: Vec<&Value> = lines.iter().filter(|line| is(line, "violation")).collect();
    assert!(violations.is_empty(), "{violations:#?}");
}
