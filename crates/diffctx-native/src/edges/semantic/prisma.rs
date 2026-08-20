use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::weights::EDGE_WEIGHTS;
use crate::types::Fragment;

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, add_edge, add_edges_from_ids, discover_files_by_refs};

fn is_prisma_file(path: &Path) -> bool {
    base::has_ext(path, &[".prisma"])
}

fn is_prisma_consumer(path: &Path) -> bool {
    let ext = base::file_ext(path);
    matches!(ext.as_str(), ".ts" | ".js" | ".tsx" | ".jsx")
}

static MODEL_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^\s*model\s+(\w+)").unwrap());
static ENUM_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^\s*enum\s+(\w+)").unwrap());
static TYPE_REF_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b([A-Z]\w+)\b").unwrap());
static CLIENT_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?:prisma\.\w+|@prisma/client)").unwrap());

static PRISMA_BUILTINS: Lazy<FxHashSet<&str>> =
    Lazy::new(|| base::kw("String Int Float Boolean DateTime Json Bytes BigInt Decimal"));
fn extract_defs(content: &str) -> FxHashSet<String> {
    let mut defs = FxHashSet::default();
    defs.extend(base::captures1(&MODEL_RE, content));
    defs.extend(base::captures1(&ENUM_RE, content));
    defs
}

fn extract_type_refs(content: &str) -> FxHashSet<String> {
    base::captures1(&TYPE_REF_RE, content)
        .filter(|t| !PRISMA_BUILTINS.contains(t.as_str()))
        .collect()
}

pub struct PrismaEdgeBuilder;

impl EdgeBuilder for PrismaEdgeBuilder {
    fn build(&self, fragments: &[Fragment], _repo_root: Option<&Path>) -> EdgeDict {
        let schema_frags: Vec<&Fragment> = fragments
            .iter()
            .filter(|f| is_prisma_file(Path::new(f.path())))
            .collect();
        if schema_frags.is_empty() {
            return FxHashMap::default();
        }

        let schema_w = EDGE_WEIGHTS["prisma_schema"].forward;
        let client_w = EDGE_WEIGHTS["prisma_client"].forward;
        let schema_rev = EDGE_WEIGHTS["prisma_schema"].reverse_factor;
        let client_rev = EDGE_WEIGHTS["prisma_client"].reverse_factor;

        let mut name_to_defs: FxHashMap<String, Vec<_>> = FxHashMap::default();
        for f in &schema_frags {
            for name in extract_defs(&f.content) {
                name_to_defs
                    .entry(name.to_lowercase())
                    .or_default()
                    .push(f.id.clone());
            }
        }

        let mut edges: EdgeDict = FxHashMap::default();

        for f in &schema_frags {
            let self_defs = extract_defs(&f.content);
            for tref in extract_type_refs(&f.content) {
                if self_defs.contains(&tref) {
                    continue;
                }
                if let Some(targets) = name_to_defs.get(&tref.to_lowercase()) {
                    add_edges_from_ids(&mut edges, &f.id, &targets, schema_w, schema_rev);
                }
            }
        }

        let consumer_frags: Vec<&Fragment> = fragments
            .iter()
            .filter(|f| is_prisma_consumer(Path::new(f.path())) && CLIENT_RE.is_match(&f.content))
            .collect();
        for cf in &consumer_frags {
            for sf in &schema_frags {
                if cf.id != sf.id {
                    add_edge(&mut edges, &cf.id, &sf.id, client_w, client_rev);
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
        let prisma_changed: Vec<&PathBuf> = changed.iter().filter(|f| is_prisma_file(f)).collect();
        if prisma_changed.is_empty() {
            return vec![];
        }
        let mut refs = FxHashSet::default();
        for f in &prisma_changed {
            if let Some(content) = base::read_file_cached(f, file_cache) {
                refs.extend(extract_type_refs(&content));
            }
        }
        refs.insert("prisma".to_string());
        discover_files_by_refs(&refs, changed, candidates, repo_root)
    }
}
