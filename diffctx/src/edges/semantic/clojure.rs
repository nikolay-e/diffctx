use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::weights::EDGE_WEIGHTS;
use crate::types::Fragment;

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, add_edge, add_edges_from_ids, discover_files_by_refs};

fn is_clojure_file(path: &Path) -> bool {
    let ext = base::file_ext(path);
    matches!(ext.as_str(), ".clj" | ".cljs" | ".cljc" | ".edn")
}

static NS_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\(ns\s+([\w.\-]+)").unwrap());
static REQUIRE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r":require\s*\[([^\]]+)\]").unwrap());
static REQUIRE_SINGLE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\[?([\w.\-]+)(?:\s+:as\s+(\w+))?").unwrap());
static USE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r":use\s*\[([^\]]+)\]").unwrap());
static IMPORT_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r":import\s*\[([^\]]+)\]").unwrap());
static DEFN_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^\s*\((?:defn-?|def|defmacro|defprotocol|defrecord|deftype|defmulti|defmethod|defonce)\s+([\w\-!?*+<>=]+)").unwrap()
});
static CALL_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\(([\w.\-]+/[\w\-!?*+<>=]+)").unwrap());

static CLJ_KEYWORDS: Lazy<FxHashSet<&str>> = Lazy::new(|| {
    [
        "if", "do", "let", "fn", "def", "defn", "defmacro", "when", "cond", "case", "loop",
        "recur", "throw", "try", "catch", "finally", "quote", "var", "ns", "require", "use",
        "import", "in-ns", "refer", "nil", "true", "false", "println", "pr", "prn", "str", "first",
        "rest", "cons", "conj", "assoc", "dissoc", "get", "count", "map", "filter", "reduce",
        "apply", "partial", "comp", "identity", "not",
    ]
    .iter()
    .copied()
    .collect()
});

fn extract_requires(content: &str) -> FxHashSet<String> {
    let mut refs = FxHashSet::default();
    for cap in REQUIRE_RE.captures_iter(content) {
        for single in REQUIRE_SINGLE_RE.captures_iter(&cap[1]) {
            let name = &single[1];
            if !name.starts_with(':') {
                refs.insert(name.to_string());
                if let Some(leaf) = name.split('.').last() {
                    refs.insert(leaf.to_string());
                }
            }
        }
    }
    for cap in USE_RE.captures_iter(content) {
        for single in REQUIRE_SINGLE_RE.captures_iter(&cap[1]) {
            refs.insert(single[1].to_string());
        }
    }
    for cap in IMPORT_RE.captures_iter(content) {
        for word in cap[1].split_whitespace() {
            let w = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '.');
            if !w.is_empty() {
                refs.insert(w.to_string());
            }
        }
    }
    refs
}

fn extract_ns(content: &str) -> Option<String> {
    NS_RE.captures(content).map(|c| c[1].to_string())
}

fn extract_defs(content: &str) -> FxHashSet<String> {
    DEFN_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect()
}

pub struct ClojureEdgeBuilder;

impl EdgeBuilder for ClojureEdgeBuilder {
    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict {
        let frags: Vec<&Fragment> = fragments
            .iter()
            .filter(|f| is_clojure_file(Path::new(f.path())))
            .collect();
        if frags.is_empty() {
            return FxHashMap::default();
        }

        let require_w = EDGE_WEIGHTS["clojure_require"].forward;
        let fn_w = EDGE_WEIGHTS["clojure_fn"].forward;
        let _proto_w = EDGE_WEIGHTS["clojure_protocol"].forward;
        let reverse_factor = EDGE_WEIGHTS["clojure_require"].reverse_factor;

        let idx = base::FragmentIndex::new(fragments, repo_root);
        let mut ns_to_frags: FxHashMap<String, Vec<_>> = FxHashMap::default();
        let mut name_to_defs: FxHashMap<String, Vec<_>> = FxHashMap::default();
        for f in &frags {
            if let Some(ns) = extract_ns(&f.content) {
                let leaf = ns.split('.').last().unwrap_or(&ns).to_lowercase();
                ns_to_frags.entry(leaf).or_default().push(f.id.clone());
                ns_to_frags
                    .entry(ns.to_lowercase())
                    .or_default()
                    .push(f.id.clone());
            }
            for name in extract_defs(&f.content) {
                name_to_defs
                    .entry(name.to_lowercase())
                    .or_default()
                    .push(f.id.clone());
            }
        }

        let mut edges: EdgeDict = FxHashMap::default();

        for f in &frags {
            let self_defs = extract_defs(&f.content);
            for req in extract_requires(&f.content) {
                let leaf = req.split('.').last().unwrap_or(&req).to_lowercase();
                if let Some(targets) = ns_to_frags.get(&leaf) {
                    add_edges_from_ids(&mut edges, &f.id, targets, require_w, reverse_factor);
                } else {
                    base::link_by_name(&f.id, &req, &idx, &mut edges, require_w, reverse_factor);
                }
            }
            for cap in CALL_RE.captures_iter(&f.content) {
                let full = &cap[1];
                if let Some(func) = full.split('/').last() {
                    if self_defs.contains(func) || CLJ_KEYWORDS.contains(func) {
                        continue;
                    }
                    if let Some(targets) = name_to_defs.get(&func.to_lowercase()) {
                        for t in targets {
                            if t != &f.id {
                                add_edge(&mut edges, &f.id, t, fn_w, reverse_factor);
                            }
                        }
                    }
                }
            }
            for id in &f.identifiers {
                if self_defs.contains(id) || CLJ_KEYWORDS.contains(id.as_str()) {
                    continue;
                }
                if let Some(targets) = name_to_defs.get(&id.to_lowercase()) {
                    for t in targets {
                        if t != &f.id {
                            add_edge(&mut edges, &f.id, t, fn_w, reverse_factor);
                        }
                    }
                }
            }
        }
        edges
    }

    fn discover_related_files(
        &self,
        changed: &[PathBuf],
        candidates: &[PathBuf],
        repo_root: Option<&Path>,
        file_cache: Option<&FxHashMap<PathBuf, String>>,
    ) -> Vec<PathBuf> {
        let clj_changed: Vec<&PathBuf> = changed.iter().filter(|f| is_clojure_file(f)).collect();
        if clj_changed.is_empty() {
            return vec![];
        }
        let mut refs = FxHashSet::default();
        for f in &clj_changed {
            if let Some(content) = base::read_file_cached(f, file_cache) {
                refs.extend(extract_requires(&content));
            }
        }
        discover_files_by_refs(&refs, changed, candidates, repo_root)
    }
}
