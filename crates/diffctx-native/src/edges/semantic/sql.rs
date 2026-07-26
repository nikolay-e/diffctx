use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::weights::EDGE_WEIGHTS;
use crate::types::Fragment;

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, add_edge, discover_files_by_refs};

fn is_sql_file(path: &Path) -> bool {
    base::file_ext(path) == ".sql"
}

static CREATE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?mi)^\s*CREATE\s+(?:OR\s+REPLACE\s+)?(?:TABLE|VIEW|FUNCTION|PROCEDURE|TYPE|INDEX|TRIGGER|SEQUENCE|SCHEMA)\s+(?:IF\s+NOT\s+EXISTS\s+)?(?:[\w.]+\.)?(\w+)").unwrap()
});
static REFERENCES_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?mi)REFERENCES\s+(?:[\w.]+\.)?(\w+)").unwrap());
static TABLE_REF_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?mi)(?:FROM|JOIN|INTO|UPDATE|DELETE\s+FROM|ALTER\s+TABLE|DROP\s+TABLE|TRUNCATE\s+TABLE|INSERT\s+INTO)\s+(?:[\w.]+\.)?(\w+)").unwrap()
});

static SQL_KEYWORDS: Lazy<FxHashSet<&str>> = Lazy::new(|| {
    [
        "select", "from", "where", "and", "or", "not", "in", "exists", "null", "true", "false",
        "set", "values", "as", "on", "using", "left", "right", "inner", "outer", "cross", "group",
        "order", "by", "having", "limit", "offset", "union", "all", "distinct", "case", "when",
        "then", "else", "end", "if", "begin", "declare", "returns", "return",
    ]
    .iter()
    .copied()
    .collect()
});

fn extract_creates(content: &str) -> FxHashSet<String> {
    CREATE_RE
        .captures_iter(content)
        .map(|c| c[1].to_lowercase())
        .filter(|n| !SQL_KEYWORDS.contains(n.as_str()))
        .collect()
}

fn extract_table_refs(content: &str) -> FxHashSet<String> {
    let mut refs: FxHashSet<String> = TABLE_REF_RE
        .captures_iter(content)
        .map(|c| c[1].to_lowercase())
        .filter(|n| !SQL_KEYWORDS.contains(n.as_str()))
        .collect();
    refs.extend(
        REFERENCES_RE
            .captures_iter(content)
            .map(|c| c[1].to_lowercase())
            .filter(|n| !SQL_KEYWORDS.contains(n.as_str())),
    );
    refs
}

pub struct SqlEdgeBuilder;

impl EdgeBuilder for SqlEdgeBuilder {
    fn build(&self, fragments: &[Fragment], _repo_root: Option<&Path>) -> EdgeDict {
        let frags: Vec<&Fragment> = fragments
            .iter()
            .filter(|f| is_sql_file(Path::new(f.path())))
            .collect();
        if frags.is_empty() {
            return FxHashMap::default();
        }

        let fk_w = EDGE_WEIGHTS["sql_fk"].forward;
        let table_ref_w = EDGE_WEIGHTS["sql_table_ref"].forward;
        let reverse_factor = EDGE_WEIGHTS["sql_fk"].reverse_factor;

        let mut table_to_frags: FxHashMap<String, Vec<_>> = FxHashMap::default();
        for f in &frags {
            for name in extract_creates(&f.content) {
                table_to_frags.entry(name).or_default().push(f.id.clone());
            }
        }

        let mut edges: EdgeDict = FxHashMap::default();

        for f in &frags {
            let self_creates = extract_creates(&f.content);
            let has_fk = REFERENCES_RE.is_match(&f.content);
            for tref in extract_table_refs(&f.content) {
                if self_creates.contains(&tref) {
                    continue;
                }
                let w = if has_fk
                    && REFERENCES_RE
                        .captures_iter(&f.content)
                        .any(|c| c[1].to_lowercase() == tref)
                {
                    fk_w
                } else {
                    table_ref_w
                };
                if let Some(targets) = table_to_frags.get(&tref) {
                    for t in targets {
                        if t != &f.id {
                            add_edge(&mut edges, &f.id, t, w, reverse_factor);
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
        let sql_changed: Vec<&PathBuf> = changed.iter().filter(|f| is_sql_file(f)).collect();
        if sql_changed.is_empty() {
            return vec![];
        }
        let mut refs = FxHashSet::default();
        for f in &sql_changed {
            if let Some(content) = base::read_file_cached(f, file_cache) {
                refs.extend(extract_table_refs(&content));
            }
        }
        discover_files_by_refs(&refs, changed, candidates, repo_root)
    }
}
