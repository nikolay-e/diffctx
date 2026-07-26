use std::collections::BTreeMap;
use std::path::Path;
use std::sync::OnceLock;

use serde::Deserialize;

use _diffctx::render::{DiffContextOutput, FragmentEntry};

#[derive(Debug, Deserialize, Clone, Default)]
pub struct TestCase {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub repo: Repo,
    #[serde(default)]
    pub fixtures: Fixtures,
    #[serde(default)]
    pub fragments: Vec<DeclaredFragment>,
    #[serde(default)]
    pub oracle: Oracle,
    #[serde(default)]
    pub accept: Accept,
    #[serde(default)]
    pub xfail: Option<XFail>,
    #[serde(default)]
    pub min_score: Option<f64>,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct Repo {
    #[serde(default)]
    pub initial_files: BTreeMap<String, String>,
    #[serde(default)]
    pub changed_files: BTreeMap<String, String>,
    #[serde(default = "default_commit_message")]
    pub commit_message: String,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct Fixtures {
    #[serde(default)]
    pub auto_garbage: bool,
    #[serde(default)]
    pub distractors: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct DeclaredFragment {
    pub id: String,
    #[serde(default)]
    pub selector: Selector,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct Selector {
    pub path: Option<String>,
    pub symbol: Option<String>,
    pub kind: Option<String>,
    pub anchor: Option<String>,
    pub any_of: Option<Vec<Selector>>,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct Oracle {
    #[serde(default)]
    pub required: Vec<String>,
    #[serde(default)]
    pub allowed: Vec<String>,
    #[serde(default)]
    pub forbidden: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Accept {
    #[serde(default = "default_symbol_match")]
    pub symbol_match: String,
    #[serde(default)]
    pub kind_must_match: bool,
    #[serde(default = "default_span_relation")]
    pub span_relation: String,
}

impl Default for Accept {
    fn default() -> Self {
        Self {
            symbol_match: default_symbol_match(),
            kind_must_match: false,
            span_relation: default_span_relation(),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct XFail {
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub issue: Option<String>,
}

impl XFail {
    pub fn is_active(&self) -> bool {
        self.reason.is_some() || self.category.is_some()
    }
}

fn default_symbol_match() -> String {
    "exact".into()
}
fn default_span_relation() -> String {
    "exact_or_enclosing".into()
}
fn default_commit_message() -> String {
    "Update files".into()
}

pub fn match_path(candidate: &str, target: &str) -> bool {
    candidate == target
        || candidate.ends_with(&format!("/{target}"))
        || candidate
            .replace('\\', "/")
            .ends_with(&format!("/{target}"))
}

pub fn symbol_matches(frag_symbol: &str, expected: &str, mode: &str) -> bool {
    match mode {
        "prefix" => frag_symbol.starts_with(expected),
        "substring" => frag_symbol.contains(expected),
        _ => frag_symbol == expected,
    }
}

pub fn matches_selector(fragment: &FragmentEntry, selector: &Selector, accept: &Accept) -> bool {
    if let Some(any) = &selector.any_of {
        return any.iter().any(|s| matches_selector(fragment, s, accept));
    }
    if let Some(path) = &selector.path {
        if !match_path(&fragment.path, path) {
            return false;
        }
    }
    if let Some(symbol) = &selector.symbol {
        let frag_symbol = fragment.symbol.as_deref().unwrap_or("");
        if !symbol_matches(frag_symbol, symbol, &accept.symbol_match) {
            return false;
        }
    }
    if let Some(kind) = &selector.kind {
        if accept.kind_must_match && fragment.kind != *kind {
            return false;
        }
    }
    if let Some(anchor) = &selector.anchor {
        let content = fragment.content.as_deref().unwrap_or("");
        if !content.contains(anchor) && !fragment.path.contains(anchor) {
            return false;
        }
    }
    true
}

pub struct OracleResult {
    pub passed: bool,
    pub score: f64,
    pub recall: f64,
    pub forbidden_rate: f64,
    pub missing_required: Vec<String>,
    pub hit_forbidden: Vec<String>,
}

pub fn evaluate_oracle(case: &TestCase, output: &DiffContextOutput) -> OracleResult {
    let mut matched_ids: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for entry in &output.fragments {
        for decl in &case.fragments {
            if matches_selector(entry, &decl.selector, &case.accept) {
                matched_ids.insert(decl.id.as_str());
            }
        }
    }

    let required_total = case.oracle.required.len();
    let missing_required: Vec<String> = case
        .oracle
        .required
        .iter()
        .filter(|id| !matched_ids.contains(id.as_str()))
        .cloned()
        .collect();

    let forbidden_total = case.oracle.forbidden.len();
    let hit_forbidden: Vec<String> = case
        .oracle
        .forbidden
        .iter()
        .filter(|id| matched_ids.contains(id.as_str()))
        .cloned()
        .collect();

    let recall = if required_total == 0 {
        1.0
    } else {
        (required_total - missing_required.len()) as f64 / required_total as f64
    };
    let forbidden_rate = if forbidden_total == 0 {
        0.0
    } else {
        hit_forbidden.len() as f64 / forbidden_total as f64
    };
    let score = 100.0 * recall * (1.0 - forbidden_rate);
    let passed = missing_required.is_empty() && hit_forbidden.is_empty();

    OracleResult {
        passed,
        score,
        recall,
        forbidden_rate,
        missing_required,
        hit_forbidden,
    }
}

pub fn calculate_budget(case: &TestCase) -> u32 {
    let overhead = _diffctx::config::limits::LIMITS.overhead_per_fragment;
    let mut all_files: BTreeMap<String, String> = case.repo.initial_files.clone();
    for (k, v) in &case.repo.changed_files {
        all_files.insert(k.clone(), v.clone());
    }
    for (k, v) in &case.fixtures.distractors {
        all_files.insert(k.clone(), v.clone());
    }

    let content_tokens: u32 = all_files
        .values()
        .map(|c| _diffctx::tokenizer::count_tokens(c))
        .sum();
    let n_frags = all_files.len().max(2) as u32;
    ((content_tokens + n_frags * overhead) * 5 / 2).max(500)
}

static GARBAGE_CACHE: OnceLock<BTreeMap<String, String>> = OnceLock::new();

pub fn garbage_files() -> &'static BTreeMap<String, String> {
    GARBAGE_CACHE.get_or_init(|| {
        let mut m = BTreeMap::new();
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("garbage");
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => return m,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n,
                None => continue,
            };
            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            m.insert(format!("unrelated_dir/{name}"), content);
        }
        m
    })
}
