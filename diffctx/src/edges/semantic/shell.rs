use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::extensions::SHELL_EXTENSIONS;
use crate::config::weights::EDGE_WEIGHTS;
use crate::types::Fragment;

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, FragmentIndex, discover_files_by_refs, link_by_name};

fn is_shell_file(path: &Path) -> bool {
    let ext = base::file_ext(path);
    SHELL_EXTENSIONS.contains(ext.as_str())
}

static SOURCE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"(?m)^\s*(?:source|\.)\s+["']?([^"'\s;]+)"#).unwrap());
static SCRIPT_CALL_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?:bash|sh|python|python3|node|ruby|perl)\s+(\S+)").unwrap());
static EXEC_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\./(\S+)").unwrap());

fn extract_refs(content: &str) -> FxHashSet<String> {
    let mut refs = FxHashSet::default();
    for cap in SOURCE_RE.captures_iter(content) {
        refs.insert(cap[1].to_string());
    }
    for cap in SCRIPT_CALL_RE.captures_iter(content) {
        refs.insert(cap[1].to_string());
    }
    for cap in EXEC_RE.captures_iter(content) {
        refs.insert(cap[1].to_string());
    }
    refs
}

pub struct ShellEdgeBuilder;

impl EdgeBuilder for ShellEdgeBuilder {
    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict {
        let sh_frags: Vec<&Fragment> = fragments
            .iter()
            .filter(|f| is_shell_file(Path::new(f.path())))
            .collect();
        if sh_frags.is_empty() {
            return FxHashMap::default();
        }

        let source_weight = EDGE_WEIGHTS["shell_source"].forward;
        let script_weight = EDGE_WEIGHTS["shell_script"].forward;
        let source_reverse = EDGE_WEIGHTS["shell_source"].reverse_factor;
        let script_reverse = EDGE_WEIGHTS["shell_script"].reverse_factor;

        let idx = FragmentIndex::new(fragments, repo_root);

        let mut edges: EdgeDict = FxHashMap::default();

        for f in &sh_frags {
            let content = &f.content;

            for cap in SOURCE_RE.captures_iter(content) {
                let ref_path = &cap[1];
                link_by_name(
                    &f.id,
                    ref_path,
                    &idx,
                    &mut edges,
                    source_weight,
                    source_reverse,
                );
            }

            for cap in SCRIPT_CALL_RE.captures_iter(content) {
                let ref_path = &cap[1];
                link_by_name(
                    &f.id,
                    ref_path,
                    &idx,
                    &mut edges,
                    script_weight,
                    script_reverse,
                );
            }

            for cap in EXEC_RE.captures_iter(content) {
                let ref_path = &cap[1];
                link_by_name(
                    &f.id,
                    ref_path,
                    &idx,
                    &mut edges,
                    script_weight,
                    script_reverse,
                );
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
        let sh_changed: Vec<&PathBuf> = changed.iter().filter(|f| is_shell_file(f)).collect();
        if sh_changed.is_empty() {
            return vec![];
        }

        let mut all_refs = FxHashSet::default();
        for f in &sh_changed {
            let content = base::read_file_cached(f, file_cache);
            if let Some(c) = content {
                all_refs.extend(extract_refs(&c));
            }
        }

        discover_files_by_refs(&all_refs, changed, candidates, repo_root)
    }
}
