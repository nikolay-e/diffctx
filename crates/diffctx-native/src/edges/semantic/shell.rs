use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::extensions::SHELL_EXTENSIONS;
use crate::config::weights::EDGE_WEIGHTS;
use crate::types::Fragment;

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, FragmentIndex, link_by_name};

fn is_shell_file(path: &Path) -> bool {
    SHELL_EXTENSIONS.contains(base::file_ext(path).as_str())
}

fn is_shebang_shell(path: &Path, content: &str) -> bool {
    crate::languages::sniff_language(&path.to_string_lossy(), content) == Some("bash")
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
        let shebang_paths: FxHashSet<&str> = fragments
            .iter()
            .filter(|f| f.id.start_line == 1 && is_shebang_shell(Path::new(f.path()), &f.content))
            .map(Fragment::path)
            .collect();
        let Some(sh_frags) = base::frags_where(fragments, |f| {
            is_shell_file(Path::new(f.path())) || shebang_paths.contains(f.path())
        }) else {
            return FxHashMap::default();
        };

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
        base::discover_by_extracted_refs(
            changed,
            candidates,
            repo_root,
            file_cache,
            |p| {
                is_shell_file(p)
                    || base::read_file_cached(p, file_cache)
                        .is_some_and(|c| is_shebang_shell(p, &c))
            },
            extract_refs,
        )
    }
}
