//! The cases `edge_precision` and `key_links` read, and the truth they score a
//! link against: for a link with a changed endpoint, the file at its other end
//! holds a required fragment of the case (right), is forbidden (wrong), or is
//! neither (unlabelled — counted apart, never as a hit or a miss).

// Each suite compiles this module and uses a part of it.
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use _diffctx::in_memory_harness::{MemoryRepo, fragment_rows};
use rustc_hash::FxHashMap;
use walkdir::WalkDir;

use crate::common::{Accept, Selector, TestCase, garbage_files, match_path, symbol_matches};

pub struct Case {
    pub name: String,
    pub test: TestCase,
    pub repo: MemoryRepo,
    pub changed: BTreeSet<String>,
    /// The tree after the change.
    pub files: BTreeMap<String, String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Label {
    Right,
    Wrong,
    Unlabelled,
}

fn cases_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("repository root")
        .join("tests")
        .join("cases")
        .join("diff")
}

/// The directories under `tests/cases/diff` that `variable` names
/// (comma-separated), else `default`. A directory that does not exist is
/// skipped: a gate may name a stratum another branch has not landed yet.
pub fn case_dirs(variable: &str, default: &[&str]) -> Vec<PathBuf> {
    let listed = std::env::var(variable)
        .ok()
        .filter(|v| !v.trim().is_empty());
    let names: Vec<String> = match listed {
        Some(v) => v.split(',').map(|s| s.trim().to_string()).collect(),
        None => default.iter().map(|s| s.to_string()).collect(),
    };
    names
        .into_iter()
        .map(|n| cases_root().join(n))
        .filter(|p| p.is_dir())
        .collect()
}

pub fn load_cases(dirs: &[PathBuf]) -> Vec<Case> {
    let root = cases_root();
    let mut out = Vec::new();
    for dir in dirs {
        let mut paths: Vec<PathBuf> = WalkDir::new(dir)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_file())
            .map(|e| e.path().to_path_buf())
            .filter(|p| matches!(p.extension().and_then(|x| x.to_str()), Some("yaml" | "yml")))
            .collect();
        paths.sort();
        for path in paths {
            let raw = std::fs::read_to_string(&path).expect("read case");
            let test: TestCase =
                serde_yaml::from_str(&raw).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let name = path
                .strip_prefix(&root)
                .unwrap_or(&path)
                .with_extension("")
                .to_string_lossy()
                .to_string();
            out.push(build_case(name, test));
        }
    }
    out
}

/// The in-memory form of the case the way the git runner and
/// `examples/diffctx-test.rs` lay it out: distractors and garbage in both
/// trees, renames moved, deletions dropped from the final tree.
fn build_case(name: String, test: TestCase) -> Case {
    let mut initial: FxHashMap<String, String> = test
        .repo
        .initial_files
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let mut changed: FxHashMap<String, String> = test
        .repo
        .changed_files
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    for (k, v) in &initial.clone() {
        changed.entry(k.clone()).or_insert_with(|| v.clone());
    }
    for (k, v) in &test.fixtures.distractors {
        initial.insert(k.clone(), v.clone());
        changed.insert(k.clone(), v.clone());
    }
    if test.fixtures.auto_garbage {
        for (k, v) in garbage_files() {
            initial.insert(k.clone(), v.clone());
            changed.insert(k.clone(), v.clone());
        }
    }
    for (from, to) in &test.repo.renamed_files {
        if let Some(content) = changed.remove(from) {
            changed.entry(to.clone()).or_insert(content);
        }
    }
    for rel in &test.repo.deleted_files {
        changed.remove(rel);
    }

    let touched: BTreeSet<String> = changed
        .iter()
        .filter(|(p, c)| initial.get(*p) != Some(*c))
        .map(|(p, _)| p.clone())
        .chain(
            initial
                .keys()
                .filter(|p| !changed.contains_key(*p))
                .cloned(),
        )
        .collect();
    let files = changed
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    Case {
        name,
        repo: MemoryRepo {
            name: test.name.clone(),
            initial_files: initial,
            changed_files: changed,
        },
        test,
        changed: touched,
        files,
    }
}

fn selector_matches_file(
    selector: &Selector,
    accept: &Accept,
    path: &str,
    content: &str,
    symbols: &[String],
) -> bool {
    if let Some(any) = &selector.any_of {
        return any
            .iter()
            .any(|s| selector_matches_file(s, accept, path, content, symbols));
    }
    if let Some(p) = &selector.path {
        if !match_path(path, p) {
            return false;
        }
    }
    if let Some(sym) = &selector.symbol {
        if !symbols
            .iter()
            .any(|s| symbol_matches(s, sym, &accept.symbol_match))
        {
            return false;
        }
    }
    if let Some(anchor) = &selector.anchor {
        if !content.contains(anchor.as_str()) && !path.contains(anchor.as_str()) {
            return false;
        }
    }
    true
}

impl Case {
    fn declared_ids_in(&self, path: &str) -> BTreeSet<&str> {
        let Some(content) = self.files.get(path) else {
            return BTreeSet::new();
        };
        let symbols: Vec<String> = fragment_rows(path, content)
            .into_iter()
            .filter_map(|r| r.symbol)
            .collect();
        self.test
            .fragments
            .iter()
            .filter(|d| {
                selector_matches_file(&d.selector, &self.test.accept, path, content, &symbols)
            })
            .map(|d| d.id.as_str())
            .collect()
    }

    /// Right when the file holds a required fragment, wrong when it holds a
    /// forbidden one, unlabelled otherwise. A file that is both is right: the
    /// case then asks for the file and names a part of it unwanted, which is
    /// the case's problem to settle, not this count's.
    pub fn label(&self, path: &str) -> Label {
        let ids = self.declared_ids_in(path);
        if self
            .test
            .oracle
            .required
            .iter()
            .any(|r| ids.contains(r.as_str()))
        {
            Label::Right
        } else if self
            .test
            .oracle
            .forbidden
            .iter()
            .any(|f| ids.contains(f.as_str()))
        {
            Label::Wrong
        } else {
            Label::Unlabelled
        }
    }

    /// The files a good selection must add: holding a required fragment, and
    /// not themselves changed.
    pub fn required_files(&self) -> BTreeSet<String> {
        self.files
            .keys()
            .filter(|p| !self.changed.contains(*p) && self.label(p) == Label::Right)
            .cloned()
            .collect()
    }
}

/// The other end of a link with exactly one changed endpoint.
pub fn far_end<'a>(case: &Case, a: &'a str, b: &'a str) -> Option<&'a str> {
    match (case.changed.contains(a), case.changed.contains(b)) {
        (true, false) => Some(b),
        (false, true) => Some(a),
        _ => None,
    }
}

#[derive(Default, Clone, Copy, Debug)]
pub struct Tally {
    pub right: usize,
    pub wrong: usize,
    pub unlabelled: usize,
}

impl Tally {
    pub fn add(&mut self, label: Label) {
        match label {
            Label::Right => self.right += 1,
            Label::Wrong => self.wrong += 1,
            Label::Unlabelled => self.unlabelled += 1,
        }
    }

    pub fn counted(&self) -> usize {
        self.right + self.wrong
    }

    pub fn precision(&self) -> Option<f64> {
        (self.counted() > 0).then(|| self.right as f64 / self.counted() as f64)
    }

    /// Wilson 95% interval of the precision.
    pub fn wilson(&self) -> Option<(f64, f64)> {
        let n = self.counted() as f64;
        if n == 0.0 {
            return None;
        }
        let z = 1.96_f64;
        let p = self.right as f64 / n;
        let denom = 1.0 + z * z / n;
        let centre = (p + z * z / (2.0 * n)) / denom;
        let half = z * (p * (1.0 - p) / n + z * z / (4.0 * n * n)).sqrt() / denom;
        Some(((centre - half).max(0.0), (centre + half).min(1.0)))
    }

    pub fn describe(&self) -> String {
        match (self.precision(), self.wilson()) {
            (Some(p), Some((lo, hi))) => format!(
                "right {:>3}  wrong {:>3}  unlabelled {:>3}  n {:>3}  precision {:.3}  wilson95 [{:.3}, {:.3}]",
                self.right,
                self.wrong,
                self.unlabelled,
                self.counted(),
                p,
                lo,
                hi
            ),
            _ => format!(
                "right {:>3}  wrong {:>3}  unlabelled {:>3}  n   0  precision  n/a",
                self.right, self.wrong, self.unlabelled
            ),
        }
    }
}
