use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::types::{Fragment, FragmentId};

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, add_edge};

const WEIGHT: f64 = 0.70;
const REVERSE_FACTOR: f64 = 0.45;

static HELM_VALUES_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\{\{-?\s*\.Values\.([a-zA-Z0-9_.]+)").unwrap());
static HELM_INCLUDE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"\{\{\s*(?:include|template)\s+"([^"]+)""#).unwrap());
static HELM_DEFINE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"\{\{-?\s*define\s+"([^"]+)""#).unwrap());
static YAML_KEY_PATH_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^(\s*)([a-zA-Z_][a-zA-Z0-9_-]*):").unwrap());

fn is_helm_template(path: &Path) -> bool {
    let path_str = path.to_string_lossy();
    if !path_str.contains("templates") {
        return false;
    }
    let ext = base::file_ext(path);
    ext == ".yaml" || ext == ".yml" || ext == ".tpl"
}

fn is_helm_values(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    name == "values.yaml"
        || name == "values.yml"
        || name.starts_with("values-")
        || name.starts_with("values_")
}

fn is_chart_yaml(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    name == "chart.yaml" || name == "chart.yml"
}

fn extract_yaml_keys(content: &str, max_depth: usize) -> FxHashSet<String> {
    let mut keys = FxHashSet::default();
    let mut path_stack: Vec<(usize, String)> = Vec::new();

    for cap in YAML_KEY_PATH_RE.captures_iter(content) {
        let indent = cap[1].len();
        let key = cap[2].to_string();

        while let Some(last) = path_stack.last() {
            if last.0 >= indent {
                path_stack.pop();
            } else {
                break;
            }
        }

        path_stack.push((indent, key.clone()));

        if path_stack.len() <= max_depth {
            let full_path: String = path_stack
                .iter()
                .map(|(_, k)| k.as_str())
                .collect::<Vec<_>>()
                .join(".");
            keys.insert(full_path);
            keys.insert(key);
        }
    }

    keys
}

fn get_chart_root(path: &Path) -> Option<PathBuf> {
    let mut current = path.parent()?.to_path_buf();
    for _ in 0..5 {
        if current.join("Chart.yaml").exists() || current.join("chart.yaml").exists() {
            return Some(current);
        }
        let parent = current.parent()?;
        if parent == current {
            break;
        }
        current = parent.to_path_buf();
    }
    None
}

fn collect_chart_roots(paths: &[&PathBuf]) -> FxHashSet<PathBuf> {
    let mut roots = FxHashSet::default();
    for p in paths {
        if let Some(root) = get_chart_root(p) {
            roots.insert(root);
        }
    }
    roots
}

fn is_in_chart(candidate: &Path, chart_roots: &FxHashSet<PathBuf>) -> bool {
    for chart_root in chart_roots {
        if candidate.starts_with(chart_root) {
            return true;
        }
    }
    false
}

fn get_chart_root_from_fragment(path: &Path) -> Option<PathBuf> {
    get_chart_root(path)
}

fn build_values_index(
    values_files: &[&Fragment],
) -> FxHashMap<PathBuf, FxHashMap<String, Vec<FragmentId>>> {
    let mut idx: FxHashMap<PathBuf, FxHashMap<String, Vec<FragmentId>>> = FxHashMap::default();
    for vf in values_files {
        let path = Path::new(vf.path());
        let chart_root = get_chart_root_from_fragment(path)
            .unwrap_or_else(|| path.parent().unwrap_or(Path::new("")).to_path_buf());
        let chart_entry = idx.entry(chart_root).or_default();
        for key in extract_yaml_keys(&vf.content, 4) {
            chart_entry.entry(key).or_default().push(vf.id.clone());
        }
    }
    idx
}

fn build_define_index(templates: &[&Fragment]) -> FxHashMap<String, Vec<FragmentId>> {
    let mut defs: FxHashMap<String, Vec<FragmentId>> = FxHashMap::default();
    for tmpl in templates {
        for cap in HELM_DEFINE_RE.captures_iter(&tmpl.content) {
            defs.entry(cap[1].to_string())
                .or_default()
                .push(tmpl.id.clone());
        }
    }
    defs
}

fn link_longest_match(
    tmpl_id: &FragmentId,
    parts: &[&str],
    values_keys: &FxHashMap<String, Vec<FragmentId>>,
    edges: &mut EdgeDict,
) {
    for i in (1..=parts.len()).rev() {
        let partial = parts[..i].join(".");
        if let Some(values_ids) = values_keys.get(&partial) {
            if let Some(first) = values_ids.first() {
                add_edge(edges, tmpl_id, first, WEIGHT, REVERSE_FACTOR);
                return;
            }
        }
    }
}

fn link_root_key(
    tmpl_id: &FragmentId,
    root_key: &str,
    values_keys: &FxHashMap<String, Vec<FragmentId>>,
    edges: &mut EdgeDict,
) {
    if let Some(values_ids) = values_keys.get(root_key) {
        for vid in values_ids {
            add_edge(edges, tmpl_id, vid, WEIGHT * 0.8, REVERSE_FACTOR);
        }
    }
}

fn add_values_ref_edges(
    tmpl: &Fragment,
    values_keys: &FxHashMap<String, Vec<FragmentId>>,
    edges: &mut EdgeDict,
) {
    for cap in HELM_VALUES_RE.captures_iter(&tmpl.content) {
        let ref_str = &cap[1];
        let parts: Vec<&str> = ref_str.split('.').collect();
        link_longest_match(&tmpl.id, &parts, values_keys, edges);
        if let Some(root) = parts.first() {
            link_root_key(&tmpl.id, root, values_keys, edges);
        }
    }
}

fn add_include_edges(
    tmpl: &Fragment,
    define_defs: &FxHashMap<String, Vec<FragmentId>>,
    edges: &mut EdgeDict,
) {
    for cap in HELM_INCLUDE_RE.captures_iter(&tmpl.content) {
        if let Some(def_ids) = define_defs.get(&cap[1]) {
            for def_id in def_ids {
                if *def_id != tmpl.id {
                    add_edge(edges, &tmpl.id, def_id, WEIGHT * 0.9, REVERSE_FACTOR);
                }
            }
        }
    }
}

fn add_chart_file_edges(
    tmpl: &Fragment,
    chart_root: &Path,
    chart_files: &[&Fragment],
    edges: &mut EdgeDict,
) {
    for cf in chart_files {
        let cf_root = get_chart_root_from_fragment(Path::new(cf.path())).unwrap_or_else(|| {
            Path::new(cf.path())
                .parent()
                .unwrap_or(Path::new(""))
                .to_path_buf()
        });
        if cf_root == chart_root {
            add_edge(edges, &tmpl.id, &cf.id, WEIGHT * 0.5, REVERSE_FACTOR);
        }
    }
}

fn add_template_edges(
    tmpl: &Fragment,
    values_idx: &FxHashMap<PathBuf, FxHashMap<String, Vec<FragmentId>>>,
    define_defs: &FxHashMap<String, Vec<FragmentId>>,
    chart_files: &[&Fragment],
    edges: &mut EdgeDict,
) {
    let tmpl_path = Path::new(tmpl.path());
    let chart_root = get_chart_root_from_fragment(tmpl_path).unwrap_or_else(|| {
        tmpl_path
            .parent()
            .and_then(|p| p.parent())
            .unwrap_or(Path::new(""))
            .to_path_buf()
    });

    let empty_keys = FxHashMap::default();
    let values_keys = values_idx.get(&chart_root).unwrap_or(&empty_keys);

    add_values_ref_edges(tmpl, values_keys, edges);
    add_include_edges(tmpl, define_defs, edges);
    add_chart_file_edges(tmpl, &chart_root, chart_files, edges);
}

pub struct HelmEdgeBuilder;

impl EdgeBuilder for HelmEdgeBuilder {
    fn build(&self, fragments: &[Fragment], _repo_root: Option<&Path>) -> EdgeDict {
        let templates: Vec<&Fragment> = fragments
            .iter()
            .filter(|f| is_helm_template(Path::new(f.path())))
            .collect();
        let values_files: Vec<&Fragment> = fragments
            .iter()
            .filter(|f| is_helm_values(Path::new(f.path())))
            .collect();
        let chart_files: Vec<&Fragment> = fragments
            .iter()
            .filter(|f| is_chart_yaml(Path::new(f.path())))
            .collect();

        if templates.is_empty() && values_files.is_empty() {
            return FxHashMap::default();
        }

        let mut edges: EdgeDict = FxHashMap::default();
        let values_idx = build_values_index(&values_files);
        let define_defs = build_define_index(&templates);

        for tmpl in &templates {
            add_template_edges(tmpl, &values_idx, &define_defs, &chart_files, &mut edges);
        }

        edges
    }

    fn discover_related_files(
        &self,
        changed: &[PathBuf],
        candidates: &[PathBuf],
        _repo_root: Option<&Path>,
        _file_cache: Option<&FxHashMap<PathBuf, String>>,
    ) -> Vec<PathBuf> {
        let templates: Vec<&PathBuf> = changed.iter().filter(|f| is_helm_template(f)).collect();
        let values: Vec<&PathBuf> = changed.iter().filter(|f| is_helm_values(f)).collect();

        if templates.is_empty() && values.is_empty() {
            return vec![];
        }

        let all_helm: Vec<&PathBuf> = templates.iter().chain(values.iter()).copied().collect();
        let chart_roots = collect_chart_roots(&all_helm);
        if chart_roots.is_empty() {
            return vec![];
        }

        let changed_set: FxHashSet<&PathBuf> = changed.iter().collect();
        let mut discovered = Vec::new();

        for candidate in candidates {
            if changed_set.contains(candidate) {
                continue;
            }
            if !is_helm_template(candidate)
                && !is_helm_values(candidate)
                && !is_chart_yaml(candidate)
            {
                continue;
            }
            if is_in_chart(candidate, &chart_roots) {
                discovered.push(candidate.clone());
            }
        }

        discovered
    }
}
