use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::edge_weights::ANSIBLE_SEMANTIC;
use crate::config::weights::EDGE_WEIGHTS;
use crate::types::{Fragment, FragmentId};

use super::super::EdgeDict;
use super::super::base::{
    self, EdgeBuilder, FragmentIndex, add_edge, discover_files_by_refs, file_representatives,
    link_by_name, link_by_path_match,
};

static ANSIBLE_EXTS: Lazy<FxHashSet<&str>> = Lazy::new(|| {
    [".yml", ".yaml", ".j2", ".jinja2", ".jinja"]
        .iter()
        .copied()
        .collect()
});

static INCLUDE_VARS_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?m)^\s*(?:include_vars|vars_files)\s*:\s*["']?([^\s"']{1,300})["']?"#).unwrap()
});
static INCLUDE_TASKS_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?m)^\s*(?:include_tasks|import_tasks|include_role|import_role|import_playbook)\s*:\s*["']?([^\s"']{1,300})["']?"#).unwrap()
});
static TEMPLATE_SRC_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?m)^\s*src\s*:\s*["']?([^\s"']{1,300}\.j(?:2|inja2?))["']?"#).unwrap()
});
static ROLES_LIST_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*-\s+(?:role:\s*)?([a-zA-Z_][\w.\-]{0,200})\s*$").unwrap());
static VARS_FILES_LIST_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"(?m)^\s*-\s+["']([^"']{1,300}\.ya?ml)["']"#).unwrap());

fn is_ansible_file(path: &Path) -> bool {
    ANSIBLE_EXTS.contains(base::file_ext(path).as_str())
}

fn get_role_name(path: &Path) -> Option<String> {
    let parts: Vec<&str> = path.iter().filter_map(|c| c.to_str()).collect();
    for (i, part) in parts.iter().enumerate() {
        if *part == "roles" && i + 1 < parts.len() {
            return Some(parts[i + 1].to_string());
        }
    }
    None
}

fn ref_to_filename(r: &str) -> String {
    r.trim_end_matches('/')
        .split('/')
        .next_back()
        .unwrap_or(r)
        .to_lowercase()
}

fn extract_refs(content: &str, file_path: &Path) -> FxHashSet<String> {
    let mut refs = FxHashSet::default();
    for m in INCLUDE_VARS_RE.captures_iter(content) {
        refs.insert(m[1].to_string());
    }
    for m in INCLUDE_TASKS_RE.captures_iter(content) {
        refs.insert(m[1].to_string());
    }
    for m in TEMPLATE_SRC_RE.captures_iter(content) {
        refs.insert(m[1].to_string());
    }
    for m in VARS_FILES_LIST_RE.captures_iter(content) {
        refs.insert(m[1].to_string());
    }

    if let Some(role) = get_role_name(file_path) {
        let path_str = file_path.to_string_lossy();
        if path_str.contains("/tasks/") {
            refs.insert(format!("roles/{}/handlers/main.yml", role));
            refs.insert(format!("roles/{}/templates/", role));
            refs.insert(format!("roles/{}/files/", role));
        }
    }
    refs
}

fn extract_role_refs(content: &str) -> FxHashSet<String> {
    let mut roles = FxHashSet::default();
    let mut in_roles = false;
    for line in content.lines() {
        let stripped = line.trim();
        if stripped.starts_with("roles:") {
            in_roles = true;
            continue;
        }
        if !in_roles {
            continue;
        }
        if stripped.starts_with("- ") {
            if let Some(c) = ROLES_LIST_RE.captures(line) {
                roles.insert(c[1].to_string());
            }
        } else if !stripped.is_empty() && !stripped.starts_with('#') && stripped.contains(':') {
            in_roles = false;
        }
    }
    roles
}

pub struct AnsibleEdgeBuilder;

impl EdgeBuilder for AnsibleEdgeBuilder {
    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict {
        let frags: Vec<&Fragment> = fragments
            .iter()
            .filter(|f| is_ansible_file(Path::new(f.path())))
            .collect();
        if frags.is_empty() {
            return FxHashMap::default();
        }

        let include_w = EDGE_WEIGHTS["ansible_include"].forward;
        let role_w = EDGE_WEIGHTS["ansible_role"].forward;
        let rev = EDGE_WEIGHTS["ansible_include"].reverse_factor;
        let sibling_w = include_w * ANSIBLE_SEMANTIC.sibling_modifier;

        let idx = FragmentIndex::new(fragments, repo_root);
        let mut edges: EdgeDict = FxHashMap::default();

        for af in &frags {
            let path = Path::new(af.path());
            for r in extract_refs(&af.content, path) {
                let filename = ref_to_filename(&r);
                link_by_name(&af.id, &filename, &idx, &mut edges, include_w, rev);
                link_by_path_match(&af.id, &r, &idx, &mut edges, include_w, rev);
            }

            for role in extract_role_refs(&af.content) {
                for subdir in ["tasks", "handlers", "templates"] {
                    let path_hint = format!("roles/{}/{}", role, subdir);
                    link_by_path_match(&af.id, &path_hint, &idx, &mut edges, role_w, rev);
                }
            }
        }

        // A role is a file-level relation: one edge per file pair through the
        // representatives, never fragment × fragment (two 150-fragment task
        // files were 22k emissions), never two fragments of one file, and
        // capped like the directory-sibling channel so a monster role does
        // not become naming-reachable from itself.
        let owned: Vec<Fragment> = frags.iter().map(|f| (*f).clone()).collect();
        let reps = file_representatives(&owned);
        let mut role_files: FxHashMap<String, Vec<&FragmentId>> = FxHashMap::default();
        for (path, rep) in &reps {
            if let Some(role) = get_role_name(Path::new(path)) {
                role_files.entry(role).or_default().push(rep);
            }
        }
        for group in role_files.values_mut() {
            group.sort();
            if group.len() > crate::config::limits::SIBLING.max_files_per_dir {
                continue;
            }
            for (i, r1) in group.iter().enumerate() {
                for r2 in &group[i + 1..] {
                    add_edge(&mut edges, r1, r2, sibling_w, rev);
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
        let ansible_changed: Vec<&PathBuf> =
            changed.iter().filter(|p| is_ansible_file(p)).collect();
        if ansible_changed.is_empty() {
            return vec![];
        }

        let mut refs = FxHashSet::default();
        let mut role_names = FxHashSet::default();

        for f in &ansible_changed {
            if let Some(content) = base::read_file_cached(f, file_cache) {
                for r in extract_refs(&content, f) {
                    refs.insert(ref_to_filename(&r));
                    refs.insert(r);
                }
                for role in extract_role_refs(&content) {
                    role_names.insert(role);
                }
            }
            if let Some(role) = get_role_name(f) {
                role_names.insert(role);
            }
        }

        for role in &role_names {
            refs.insert(format!("roles/{}/tasks/main.yml", role));
            refs.insert(format!("roles/{}/handlers/main.yml", role));
            refs.insert(format!("roles/{}/templates/", role));
        }

        discover_files_by_refs(&refs, changed, candidates, repo_root)
    }

    fn category_label(&self) -> Option<&str> {
        Some("semantic")
    }
}
